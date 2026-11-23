//! payments - handlers webhooks.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::json;
use unified_core::pagination::{PageResult, PaginationParams};
use actix_web::{web, HttpRequest, HttpResponse};
use chrono::{DateTime, Utc};
use serde::{Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::sync::{Arc};
use super::{events::*, card_validation::*, card_settings::*, limits::*, providers::*, types::*, reference_ids::*, credentials::*, records::*, adapter::*, dispatch::*, stripe::*, paypal::*, xendit::*, midtrans::*, hyperpay::*, twocheckout::*, mpgs::*, generic_card::*, custom_provider::*, wallets::*, state::*, requests::*, handlers_catalog::*, handlers_intents::*, handlers_charges::*, handlers_methods::*, handlers_subscriptions::*, handlers_checkout::*, handlers_split_bills::*, hooks::*, module::*};



#[derive(Debug, Clone, Serialize)]
// ---------------------------------------------------------------------------
// Handlers: webhooks.
// ---------------------------------------------------------------------------
/// One processed webhook event, for the webhook event list.
pub(crate) struct WebhookEventView {
    pub(crate) event_id: String,
    pub(crate) received_at: DateTime<Utc>,
}


/// What a verified webhook event means, after the server-side
/// outcome (when the provider offers one) and the body's own status
/// fields have been consulted.
pub(crate) enum WebhookDisposition {
    Chargeback,
    Refund(RefundDisposition),
    Failed,
    Captured,
    Ignored(&'static str),
}


/// What a webhook refund event actually says. Read from the event's
/// own status fields, never from the event type alone: a
/// `refund.failed` event must not mark anything refunded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RefundDisposition {
    Completed,
    Partial,
    Pending,
    Failed,
}


/// Read the refund disposition from the event's status fields
/// (`data.object.status`, `status`, `refund_status`), falling back
/// to the event type only when the event carries no usable status.
pub(crate) fn webhook_refund_disposition(event: &Value, event_type: &str) -> RefundDisposition {
    let raw = event
        .get("data")
        .and_then(|data| data.get("object"))
        .and_then(|object| json_str(object, &["status", "refund_status"]))
        .or_else(|| json_str(event, &["status", "refund_status"]))
        .map(str::to_lowercase);
    let text = raw.as_deref().unwrap_or("");
    if text.contains("pending") || text.contains("processing") {
        return RefundDisposition::Pending;
    }
    if text.contains("fail")
        || text.contains("cancel")
        || text.contains("denied")
        || text.contains("reversed")
    {
        return RefundDisposition::Failed;
    }
    if text.contains("succeed") || text.contains("complete") || text.contains("refunded") {
        return RefundDisposition::Completed;
    }
    let lower = event_type.to_lowercase();
    if lower.contains("fail") || lower.contains("cancel") {
        RefundDisposition::Failed
    } else if lower.contains("pending") || lower.contains("processing") {
        RefundDisposition::Pending
    } else {
        RefundDisposition::Completed
    }
}


/// The currency an event claims, when it carries one.
pub(crate) fn event_currency(event: &Value) -> Option<String> {
    event
        .get("data")
        .and_then(|data| data.get("object"))
        .and_then(|object| json_str(object, &["currency"]))
        .or_else(|| json_str(event, &["currency"]))
        .map(str::to_string)
}


/// A major-unit amount field (a JSON number, or a numeric string
/// such as Midtrans's `gross_amount`) converted to minor units
/// with the currency's exponent. String amounts convert with
/// integer arithmetic (`decimal_str_to_minor`); JSON numbers go
/// through `major_to_minor`, which formats to the currency's
/// decimal places first so binary-float error cannot shift the
/// amount. Caveat, shared with the module's other money helpers:
/// three-decimal currencies (BHD, KWD, OMR) are converted as
/// two-decimal — a pre-existing limitation, not introduced here.
fn json_major_to_minor(body: &Value, keys: &[&str], currency: &str) -> Option<i64> {
    for key in keys {
        match body.get(key) {
            Some(Value::Number(number)) => {
                return number.as_f64().map(|major| major_to_minor(major, currency));
            }
            Some(Value::String(text)) => return decimal_str_to_minor(text, currency).ok(),
            _ => continue,
        }
    }
    None
}


/// A minor-unit amount from the event, when the event carries one in
/// units this module can trust: the `amount_minor` key (this
/// module's own convention), or Stripe's `data.object.amount`,
/// which is minor units. Other providers report major units, so
/// their amounts are converted with the event's currency instead
/// of being ignored: PayPal's `amount.value` (a major-unit decimal
/// string), Xendit's `amount` (major units), Midtrans's
/// `gross_amount` (a major-unit string, IDR when the event carries
/// no currency). A provider whose shape is not listed here yields
/// `None` — an amount-less event — and the caller must treat it as
/// unquantified, never as the full original amount.
pub(crate) fn event_amount_minor(event: &Value, kind: PaymentProviderKind) -> Option<i64> {
    if let Some(amount) = json_i64(event, &["amount_minor"]) {
        return Some(amount);
    }
    if kind == PaymentProviderKind::Stripe {
        return event
            .get("data")
            .and_then(|data| data.get("object"))
            .and_then(|object| json_i64(object, &["amount"]));
    }
    match kind {
        PaymentProviderKind::PayPal => {
            let amount_object = event
                .get("resource")
                .and_then(|resource| resource.get("amount"))
                .or_else(|| {
                    event
                        .get("data")
                        .and_then(|data| data.get("object"))
                        .and_then(|object| object.get("amount"))
                })
                .or_else(|| event.get("amount"))?;
            let currency = amount_object
                .get("currency_code")
                .and_then(|code| code.as_str())
                .map(str::to_string)
                .or_else(|| event_currency(event))
                .unwrap_or_default();
            json_major_to_minor(amount_object, &["value"], &currency)
        }
        PaymentProviderKind::Xendit => {
            let currency = event
                .get("data")
                .and_then(|data| json_str(data, &["currency"]))
                .map(str::to_string)
                .or_else(|| event_currency(event))
                .unwrap_or_default();
            event
                .get("data")
                .and_then(|data| json_major_to_minor(data, &["amount"], &currency))
                .or_else(|| json_major_to_minor(event, &["amount"], &currency))
        }
        PaymentProviderKind::Midtrans => {
            // Midtrans settles in IDR and its notifications carry
            // no currency field; fall back to IDR when absent.
            let currency = event_currency(event).unwrap_or_else(|| "IDR".to_string());
            event
                .get("data")
                .and_then(|data| json_major_to_minor(data, &["gross_amount"], &currency))
                .or_else(|| json_major_to_minor(event, &["gross_amount"], &currency))
        }
        _ => None,
    }
}


/// Idempotency key for the refund row a webhook records, so a
/// redelivered event can never record the same refund twice.
pub(crate) fn webhook_refund_idempotency_key(event_id: &str) -> String {
    format!("webhook-refund:{event_id}")
}


/// Build the refund transaction row for a webhook-applied refund.
/// The row is what the over-refund guard sums
/// (`refunded_amount_minor` counts only `kind == "refund"` rows
/// linked through `original_transaction_id`): without it, a
/// webhook refund is invisible to the guard and a later API
/// refund can refund the same money again.
///
/// The amount comes from the event when it carries one. An event
/// with NO usable amount records a `pending` row with amount 0 —
/// unquantified — and NEVER the full original amount: a partial
/// refund without an amount is not a full refund, and treating it
/// as one would mark the charge `refunded` and emit
/// `payment.refunded` for money that may still be owed. The caller
/// leaves the original in `refund_pending` until the amount is
/// known, and the API refund path refuses new refunds while an
/// unquantified row is pending.
pub(crate) fn webhook_refund_transaction(
    original: &PaymentTransaction,
    kind: PaymentProviderKind,
    event: &Value,
    event_id: &str,
    fully_refunded: bool,
) -> PaymentTransaction {
    let amount = event_amount_minor(event, kind);
    let (status, amount_minor) = match amount {
        Some(amount) => (
            if fully_refunded {
                PaymentStatus::Refunded
            } else {
                PaymentStatus::PartiallyRefunded
            },
            amount,
        ),
        None => (PaymentStatus::Pending, 0),
    };
    let mut refund = new_transaction(
        original.connection_id,
        &original.provider,
        "refund",
        &original.customer_id,
        original.order_id.clone(),
        original.invoice_id.clone(),
        amount_minor,
        &original.currency,
        status,
        // The event id is the provider-issued identifier for this
        // refund. The charge's provider transaction id is
        // deliberately NOT reused here: a refund row sharing it
        // could shadow the original charge in
        // `stored_transaction` lookups by provider id.
        event_id.to_string(),
        webhook_refund_idempotency_key(event_id),
    );
    refund.provider_refund_id = Some(event_id.to_string());
    refund.subscription_id = original.subscription_id;
    refund.original_transaction_id = Some(original.id);
    refund
}


/// Receive a webhook for one provider. The signature is verified
/// against every enabled connection of that kind before any
/// processing; the first connection whose secret verifies wins, so one
/// endpoint serves both the sandbox and the live connection of a
/// provider. A redelivered event id is acknowledged but not processed
/// twice.
pub(crate) async fn webhook_receiver(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<PaymentsState>>,
    provider: web::Path<String>,
    req: HttpRequest,
    body: web::Bytes,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| async {
        let kind = PaymentProviderKind::from_str(&provider).ok_or_else(|| {
            UnifiedError::not_found(format!("Unknown payment provider \"{provider}\""))
        })?;
        let raw_body = body.as_ref();
        let event: Value = serde_json::from_slice(raw_body).map_err(|_| {
            UnifiedError::bad_request("Webhook body is not valid JSON")
        })?;
        // `webhook_verify` takes reqwest headers; convert the actix
        // header map once (names/values that do not parse are skipped).
        let mut webhook_headers = reqwest::header::HeaderMap::new();
        for (name, value) in req.headers().iter() {
            let Ok(header_name) =
                reqwest::header::HeaderName::from_bytes(name.as_str().as_bytes())
            else {
                continue;
            };
            let Ok(header_value) = reqwest::header::HeaderValue::from_bytes(value.as_bytes())
            else {
                continue;
            };
            webhook_headers.insert(header_name, header_value);
        }
        // Verify against every enabled connection of this kind.
        let mut verified_connection: Option<PaymentConnection> = None;
        for entry in connections_store().iter() {
            let connection = entry.value().clone();
            if connection.kind != kind || !connection.enabled {
                continue;
            }
            let adapter = payment_registry().build(&connection)?;
            match adapter.webhook_verify(&webhook_headers, raw_body).await {
                Ok(true) => {
                    verified_connection = Some(connection);
                    break;
                }
                Ok(false) => continue,
                Err(_) => continue,
            }
        }
        let connection = verified_connection.ok_or_else(|| {
            UnifiedError::unauthorized(format!(
                "Webhook signature verification failed for provider \"{}\"",
                kind.as_str()
            ))
        })?;
        // The event id is the provider's id when the body carries
        // one. Without one, the id is a hash of the raw body, so an
        // identical redelivery still deduplicates instead of getting
        // a fresh id (and a fresh application) every time.
        let event_id = json_str(&event, &["id", "event_id"])
            .map(str::to_string)
            .unwrap_or_else(|| {
                format!(
                    "{}-sha256-{}",
                    kind.as_str(),
                    hex_encode(&Sha256::digest(raw_body))
                )
            });
        let event_type = json_str(&event, &["type", "event_type", "event"])
            .unwrap_or("unknown")
            .to_string();
        let duplicate = {
            // Claim the event id atomically, BEFORE processing: two
            // concurrent deliveries of the same event must not both
            // apply it. `insert` returns the previous entry, so the
            // first delivery wins and the second is a duplicate.
            // The claim is released when processing fails below, so
            // the provider's retry is processed instead of dismissed
            // as a duplicate. (The claim is per-process, like the
            // idempotency claims — see `claim_idempotency`; the
            // durable form is the `webhook_event_claims` table from
            // migration 2026-11-22-000112.)
            webhook_events_store()
                .insert(event_id.clone(), Utc::now())
                .is_some()
        };
        state.emit(
            PAYMENT_WEBHOOK_RECEIVED,
            json!({
                "provider": kind.as_str(),
                "connection_id": connection.id,
                "event_id": event_id,
                "event_type": event_type,
                "duplicate": duplicate,
            }),
        );
        if duplicate {
            return Ok(json!({
                "received": true,
                "duplicate": true,
                "event_id": event_id,
            }));
        }
        // Providers without signed webhooks (HyperPay, 2Checkout) are
        // verified by re-fetching the resource; the body's event type
        // is unauthenticated there, so the outcome is read from the
        // provider instead. A failed confirmation fails closed: the
        // webhook is rejected without any state change, and the
        // event-id claim is released so a retry can be processed.
        let adapter = match payment_registry().build(&connection) {
            Ok(adapter) => adapter,
            Err(error) => {
                webhook_events_store().remove(&event_id);
                return Err(error);
            }
        };
        let verified_outcome = match adapter.webhook_outcome(raw_body).await {
            Ok(outcome) => outcome,
            Err(error) => {
                webhook_events_store().remove(&event_id);
                return Err(error);
            }
        };
        // Map the event onto a transaction when the body names one,
        // and emit the type-specific event. The provider transaction
        // id is looked up in the usual nested places first.
        let provider_transaction_id = event
            .get("data")
            .and_then(|data| data.get("object"))
            .and_then(|object| json_str(object, &["id", "payment_intent", "charge"]))
            .or_else(|| {
                event
                    .get("resource")
                    .and_then(|resource| json_str(resource, &["id", "parent_payment"]))
            })
            .or_else(|| json_str(&event, &["transaction_id", "order_id", "charge_id", "payment_id"]))
            .map(str::to_string);
        let matched = provider_transaction_id
            .as_deref()
            .and_then(stored_transaction);
        let event_lower = event_type.to_lowercase();
        let customer_id = matched
            .as_ref()
            .map(|transaction| transaction.customer_id.clone())
            .unwrap_or_default();
        // Decide what the event means. A server-side outcome wins
        // over the body's event type; otherwise the event type string
        // decides, with the refund disposition read from the event's
        // own status fields (a `refund.failed` event must never mark
        // anything refunded).
        let disposition = match verified_outcome {
            Some(PaymentStatus::Captured) => WebhookDisposition::Captured,
            Some(PaymentStatus::Refunded) => {
                WebhookDisposition::Refund(RefundDisposition::Completed)
            }
            Some(PaymentStatus::PartiallyRefunded) => {
                WebhookDisposition::Refund(RefundDisposition::Partial)
            }
            Some(PaymentStatus::Pending) => WebhookDisposition::Ignored("pending"),
            Some(PaymentStatus::Chargeback) => WebhookDisposition::Chargeback,
            Some(
                PaymentStatus::Failed
                | PaymentStatus::Cancelled
                | PaymentStatus::Voided,
            ) => WebhookDisposition::Failed,
            Some(_) => WebhookDisposition::Ignored("authorized"),
            None if event_lower.contains("chargeback") || event_lower.contains("dispute") => {
                WebhookDisposition::Chargeback
            }
            None if event_lower.contains("refund") => {
                WebhookDisposition::Refund(webhook_refund_disposition(&event, &event_type))
            }
            None if event_lower.contains("fail") || event_lower.contains("declined") => {
                WebhookDisposition::Failed
            }
            None
                if event_lower.contains("succeed")
                    || event_lower.contains("captured")
                    || event_lower.contains("completed")
                    || event_lower.contains("settlement") =>
            {
                WebhookDisposition::Captured
            }
            None => WebhookDisposition::Ignored("unknown event type"),
        };
        let mut outcome = "ignored";
        match disposition {
            WebhookDisposition::Chargeback => {
                if let Some(transaction) = &matched {
                    if let Some(mut stored) = transactions_store().get_mut(&transaction.id) {
                        stored.status = "chargeback".to_string();
                    }
                }
                state.emit(
                    PAYMENT_CHARGEBACK,
                    json!({
                        "provider": kind.as_str(),
                        "connection_id": connection.id,
                        "event_id": event_id,
                        "event_type": event_type,
                        "transaction_id": matched.as_ref().map(|t| t.id),
                        "provider_transaction_id": provider_transaction_id,
                        "customer_id": customer_id,
                        "order_id": matched.as_ref().and_then(|t| t.order_id.clone()),
                        "invoice_id": matched.as_ref().and_then(|t| t.invoice_id.clone()),
                        "amount_minor": matched.as_ref().map(|t| t.amount_minor),
                        "currency": matched.as_ref().map(|t| t.currency.clone()),
                    }),
                );
                notify(
                    &state,
                    &customer_id,
                    "payment_chargeback",
                    "Chargeback received",
                    "A chargeback was filed for one of your payments. The order and license are under review.",
                );
                outcome = "chargeback";
            }
            WebhookDisposition::Refund(refund_disposition) => {
                match refund_disposition {
                    RefundDisposition::Completed | RefundDisposition::Partial => {
                        // The amount is read once: an event with no
                        // usable amount is unquantified, never the
                        // full original amount.
                        let refund_amount = event_amount_minor(&event, kind);
                        // A completed refund is only full when the
                        // event carries an amount at or above the
                        // charge. A partial disposition is never
                        // full, and an amount-less event is never
                        // full either.
                        let fully_refunded = refund_disposition == RefundDisposition::Completed
                            && refund_amount.is_some_and(|amount| {
                                matched.as_ref().is_none_or(|transaction| {
                                    amount >= transaction.amount_minor
                                })
                            });
                        let applied = matches!(
                            matched.as_ref().map(|t| t.status.as_str()),
                            Some("captured")
                                | Some("partially_refunded")
                                | Some("refund_pending")
                        );
                        if applied {
                            if let Some(transaction) = &matched {
                                // Record the refund row BEFORE
                                // updating the original's status, so
                                // the over-refund guard sees this
                                // webhook refund. The idempotency
                                // key keeps a redelivery from
                                // recording the same refund twice.
                                // The record + the original's status
                                // update run under the refund
                                // admission lock, so an API refund
                                // admitted at the same moment cannot
                                // spend the same remainder.
                                let _admission = refund_admission_lock()
                                    .lock()
                                    .expect("refund admission lock poisoned");
                                if find_by_idempotency(
                                    transaction.connection_id,
                                    &webhook_refund_idempotency_key(&event_id),
                                )
                                .is_none()
                                {
                                    let refund_row = webhook_refund_transaction(
                                        transaction,
                                        kind,
                                        &event,
                                        &event_id,
                                        fully_refunded,
                                    );
                                    record_transaction(refund_row);
                                }
                                if let Some(mut stored) =
                                    transactions_store().get_mut(&transaction.id)
                                {
                                    stored.status = match refund_amount {
                                        Some(_) if fully_refunded => "refunded".to_string(),
                                        Some(_) => "partially_refunded".to_string(),
                                        // Unquantified: the true
                                        // size is unknown, so the
                                        // charge waits in
                                        // `refund_pending` instead
                                        // of being marked refunded.
                                        None => "refund_pending".to_string(),
                                    };
                                }
                            }
                            state.emit(
                                PAYMENT_REFUNDED,
                                json!({
                                    "provider": kind.as_str(),
                                    "connection_id": connection.id,
                                    "event_id": event_id,
                                    "event_type": event_type,
                                    "transaction_id": matched.as_ref().map(|t| t.id),
                                    "provider_transaction_id": provider_transaction_id,
                                    "customer_id": customer_id,
                                    "order_id": matched.as_ref().and_then(|t| t.order_id.clone()),
                                    "invoice_id": matched.as_ref().and_then(|t| t.invoice_id.clone()),
                                    "amount_minor": refund_amount,
                                    "currency": matched.as_ref().map(|t| t.currency.clone()),
                                    "status": match refund_amount {
                                        Some(_) if fully_refunded => "refunded",
                                        Some(_) => "partially_refunded",
                                        None => "pending",
                                    },
                                    "fully_refunded": fully_refunded,
                                    "source": "webhook",
                                }),
                            );
                            outcome = match refund_amount {
                                Some(_) if fully_refunded => "refunded",
                                Some(_) => "partially_refunded",
                                None => "refund_pending",
                            };
                        } else {
                            outcome = "refund_ignored";
                        }
                    }
                    RefundDisposition::Pending => {
                        // A refund is in flight: the charge waits in
                        // `refund_pending` until the refund completes
                        // or fails. The refunded event carries a
                        // pending status, so listeners never mark the
                        // order or invoice refunded on it.
                        if let Some(transaction) = &matched {
                            if matches!(
                                transaction.status.as_str(),
                                "captured" | "partially_refunded"
                            ) {
                                if let Some(mut stored) =
                                    transactions_store().get_mut(&transaction.id)
                                {
                                    stored.status = "refund_pending".to_string();
                                }
                            }
                        }
                        state.emit(
                            PAYMENT_REFUNDED,
                            json!({
                                "provider": kind.as_str(),
                                "connection_id": connection.id,
                                "event_id": event_id,
                                "event_type": event_type,
                                "transaction_id": matched.as_ref().map(|t| t.id),
                                "provider_transaction_id": provider_transaction_id,
                                "customer_id": customer_id,
                                "order_id": matched.as_ref().and_then(|t| t.order_id.clone()),
                                "invoice_id": matched.as_ref().and_then(|t| t.invoice_id.clone()),
                                "amount_minor": event_amount_minor(&event, kind),
                                "currency": matched.as_ref().map(|t| t.currency.clone()),
                                "status": "pending",
                                "fully_refunded": false,
                                "source": "webhook",
                            }),
                        );
                        outcome = "refund_pending";
                    }
                    RefundDisposition::Failed => {
                        // The refund died: a charge that was waiting
                        // in `refund_pending` goes back to captured.
                        if let Some(transaction) = &matched {
                            if transaction.status == "refund_pending" {
                                if let Some(mut stored) =
                                    transactions_store().get_mut(&transaction.id)
                                {
                                    stored.status = "captured".to_string();
                                }
                            }
                        }
                        outcome = "refund_failed";
                    }
                }
            }
            WebhookDisposition::Failed => {
                // Only an open transaction can fail: a capture that
                // already succeeded is never rewritten to failed by a
                // later event.
                let applied = matches!(
                    matched.as_ref().map(|t| t.status.as_str()),
                    Some("authorized") | Some("requires_confirmation") | Some("pending")
                );
                if applied {
                    if let Some(transaction) = &matched {
                        if let Some(mut stored) = transactions_store().get_mut(&transaction.id) {
                            stored.status = "failed".to_string();
                        }
                    }
                    notify(
                        &state,
                        &customer_id,
                        "payment_failed",
                        "Payment failed",
                        "A payment failed. Please check your payment method and try again.",
                    );
                }
                state.emit(
                    PAYMENT_FAILED,
                    json!({
                        "provider": kind.as_str(),
                        "connection_id": connection.id,
                        "event_id": event_id,
                        "event_type": event_type,
                        "transaction_id": matched.as_ref().map(|t| t.id),
                        "provider_transaction_id": provider_transaction_id,
                        "customer_id": customer_id,
                        "order_id": matched.as_ref().and_then(|t| t.order_id.clone()),
                        "invoice_id": matched.as_ref().and_then(|t| t.invoice_id.clone()),
                    }),
                );
                outcome = "failed";
            }
            WebhookDisposition::Captured => {
                // Amount and currency guards: a capture whose currency
                // differs from the stored transaction, or whose
                // minor-unit amount is below it, is not applied. A
                // partial capture leaves the transaction alone, and a
                // transaction that already reached a terminal state is
                // never rewritten to captured.
                let currency_mismatch = event_currency(&event)
                    .zip(matched.as_ref().map(|t| t.currency.clone()))
                    .is_some_and(|(event_currency, stored_currency)| {
                        !event_currency.eq_ignore_ascii_case(&stored_currency)
                    });
                let underpaid = event_amount_minor(&event, kind)
                    .zip(matched.as_ref().map(|t| t.amount_minor))
                    .is_some_and(|(amount, stored)| amount < stored);
                let terminal = matches!(
                    matched.as_ref().map(|t| t.status.as_str()),
                    Some("refunded")
                        | Some("partially_refunded")
                        | Some("chargeback")
                        | Some("voided")
                );
                if currency_mismatch {
                    outcome = "currency_mismatch";
                } else if underpaid {
                    outcome = "partial_capture";
                } else if terminal {
                    outcome = "already_terminal";
                } else {
                    if let Some(transaction) = &matched {
                        if let Some(mut stored) = transactions_store().get_mut(&transaction.id) {
                            stored.status = "captured".to_string();
                        }
                    }
                    state.emit(
                        PAYMENT_CAPTURED,
                        json!({
                            "provider": kind.as_str(),
                            "connection_id": connection.id,
                            "event_id": event_id,
                            "event_type": event_type,
                            "transaction_id": matched.as_ref().map(|t| t.id),
                            "provider_transaction_id": provider_transaction_id,
                            "customer_id": customer_id,
                            "order_id": matched.as_ref().and_then(|t| t.order_id.clone()),
                            "invoice_id": matched.as_ref().and_then(|t| t.invoice_id.clone()),
                            "amount_minor": matched.as_ref().map(|t| t.amount_minor),
                            "currency": matched.as_ref().map(|t| t.currency.clone()),
                            "status": "captured",
                            "source": "webhook",
                        }),
                    );
                    outcome = "captured";
                }
            }
            WebhookDisposition::Ignored(reason) => {
                outcome = reason;
            }
        }
        // Processing finished without error: the claim recorded at
        // receipt stands as the seen marker. Failed deliveries
        // release the claim above, so a provider retry is processed
        // instead of dismissed as a duplicate.
        prune_webhook_events();
        Ok(json!({
            "received": true,
            "duplicate": false,
            "event_id": event_id,
            "event_type": event_type,
            "provider": kind.as_str(),
            "outcome": outcome,
        }))
    })()
    .await;
    json::respond(result, debug)
}


pub(crate) async fn list_webhook_events(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    params: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PageResult<WebhookEventView>> = (|| {
        let _tenant = require_tenant(&req)?;
        // Processed webhook event ids are an operator surface:
        // they name provider-side events, so they need the
        // payments admin capability, not just any caller.
        require_payments_admin(&ctx, &req)?;
        let mut items: Vec<WebhookEventView> = webhook_events_store()
            .iter()
            .map(|entry| WebhookEventView {
                event_id: entry.key().clone(),
                received_at: *entry.value(),
            })
            .collect();
        items.sort_by_key(|event| event.received_at);
        if params.normalized().order == "desc" {
            items.reverse();
        }
        Ok(paginate(items, &params))
    })();
    json::respond(result, debug)
}

#[cfg(test)]
mod tests {
    use super::*;
    use unified_core::config::AppConfig;
    use unified_core::hooks::HookBus;
    use unified_core::modules::ModuleRegistry;
    use actix_web::http::StatusCode;
    use actix_web::test::TestRequest;

    fn test_context() -> AppContext {
        AppContext::new(
            AppConfig::default(),
            Arc::new(HookBus::new()),
            Arc::new(ModuleRegistry::new()),
        )
    }

    fn stored_stripe_connection() -> PaymentConnection {
        let now = Utc::now();
        let mut credentials = PaymentCredentials::default();
        credentials.webhook_secret = Some("whsec-test".to_string());
        let connection = PaymentConnection {
            id: Uuid::now_v7(),
            name: "test stripe".to_string(),
            kind: PaymentProviderKind::Stripe,
            mode: GatewayMode::Sandbox,
            endpoint: None,
            credentials,
            custom_config: None,
            delegate_connection_id: None,
            enabled: true,
            last_tested_at: None,
            last_status: None,
            created_at: now,
            updated_at: now,
        };
        connections_store().insert(connection.id, connection.clone());
        connection
    }

    fn signed_body(event_id: &str) -> (Vec<u8>, String) {
        let body = serde_json::json!({
            "id": event_id,
            "type": "charge.succeeded",
            "data": { "object": { "id": "ch-no-such-charge", "amount": 1000, "currency": "usd" } },
        })
        .to_string();
        let timestamp = Utc::now().timestamp();
        let signed = format!("{timestamp}.{body}");
        let signature = hmac_sha256_hex("whsec-test".as_bytes(), signed.as_bytes());
        let header = format!("t={timestamp},v1={signature}");
        (body.into_bytes(), header)
    }

    #[tokio::test]
    async fn webhook_event_is_seen_only_after_successful_processing() {
        let connection = stored_stripe_connection();
        let event_id = format!("evt-p5-ok-{}", Uuid::now_v7());
        let (raw_body, signature) = signed_body(&event_id);
        let request = TestRequest::default()
            .insert_header(("Stripe-Signature", signature))
            .to_http_request();
        let response = webhook_receiver(
            web::Data::new(Arc::new(test_context())),
            web::Data::new(Arc::new(PaymentsState::new())),
            web::Path::from("stripe".to_string()),
            request,
            web::Bytes::from(raw_body),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        // The verified event was processed and is now recorded seen.
        assert!(webhook_events_store().contains_key(&event_id));
        // A redelivery is acknowledged as a duplicate, not reapplied.
        let (raw_body, signature) = signed_body(&event_id);
        let request = TestRequest::default()
            .insert_header(("Stripe-Signature", signature))
            .to_http_request();
        let response = webhook_receiver(
            web::Data::new(Arc::new(test_context())),
            web::Data::new(Arc::new(PaymentsState::new())),
            web::Path::from("stripe".to_string()),
            request,
            web::Bytes::from(raw_body),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        webhook_events_store().remove(&event_id);
        connections_store().remove(&connection.id);
    }

    #[tokio::test]
    async fn failed_webhook_is_not_recorded_as_seen() {
        let connection = stored_stripe_connection();
        let event_id = format!("evt-p5-bad-{}", Uuid::now_v7());
        let (raw_body, _) = signed_body(&event_id);
        // A tampered signature: verification fails, the event is
        // never processed, and its id must not be recorded seen —
        // otherwise a later genuine redelivery would be dismissed
        // as a duplicate without ever being applied.
        let request = TestRequest::default()
            .insert_header(("Stripe-Signature", "t=1,v1=deadbeef"))
            .to_http_request();
        let response = webhook_receiver(
            web::Data::new(Arc::new(test_context())),
            web::Data::new(Arc::new(PaymentsState::new())),
            web::Path::from("stripe".to_string()),
            request,
            web::Bytes::from(raw_body),
        )
        .await;
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        assert!(!webhook_events_store().contains_key(&event_id));
        connections_store().remove(&connection.id);
    }

    #[test]
    fn webhook_refund_row_is_visible_to_the_over_refund_guard() {
        let original = record_transaction(new_transaction(
            Uuid::now_v7(),
            "stripe",
            "charge",
            "cust-p6",
            None,
            None,
            1000,
            "USD",
            PaymentStatus::Captured,
            "ch-p6".to_string(),
            format!("p6-orig-{}", Uuid::now_v7()),
        ));
        let event_id = format!("evt-p6-{}", Uuid::now_v7());
        let event = serde_json::json!({ "amount_minor": 400 });
        let refund = webhook_refund_transaction(
            &original,
            PaymentProviderKind::Stripe,
            &event,
            &event_id,
            false,
        );
        assert_eq!(refund.kind, "refund");
        assert_eq!(refund.original_transaction_id, Some(original.id));
        assert_eq!(refund.amount_minor, 400);
        assert_eq!(refund.status, "partially_refunded");
        // The row must not share the charge's provider id, or it
        // would shadow the original in provider-id lookups.
        assert_eq!(refund.provider_transaction_id, event_id);
        assert_ne!(refund.provider_transaction_id, original.provider_transaction_id);
        assert_eq!(
            refund.idempotency_key,
            webhook_refund_idempotency_key(&event_id)
        );
        let refund = record_transaction(refund);
        // The guard now sees the webhook refund: only 600 of the
        // 1000 remains refundable.
        assert_eq!(refunded_amount_minor(original.id), 400);
        // An event with no amount is unquantified — pending with
        // amount 0 — even when the disposition says completed. It
        // is NEVER a full refund of the original amount.
        let full = webhook_refund_transaction(
            &original,
            PaymentProviderKind::Stripe,
            &serde_json::json!({}),
            &format!("evt-p6-full-{}", Uuid::now_v7()),
            true,
        );
        assert_eq!(full.status, "pending");
        assert_eq!(full.amount_minor, 0);
        let full = record_transaction(full);
        assert_eq!(refunded_amount_minor(original.id), 400);
        assert!(has_unquantified_pending_refund(original.id));
        transactions_store().remove(&full.id);
        transactions_store().remove(&refund.id);
        transactions_store().remove(&original.id);
    }

    #[test]
    fn event_amount_minor_converts_provider_major_unit_amounts() {
        // PayPal reports major-unit decimals: "10.50" USD is 1050
        // minor units.
        let paypal = serde_json::json!({
            "resource": { "amount": { "currency_code": "USD", "value": "10.50" } },
        });
        assert_eq!(
            event_amount_minor(&paypal, PaymentProviderKind::PayPal),
            Some(1050)
        );
        // Midtrans reports a major-unit string in IDR (a
        // zero-decimal currency): "10000" is 10000.
        let midtrans = serde_json::json!({ "gross_amount": "10000" });
        assert_eq!(
            event_amount_minor(&midtrans, PaymentProviderKind::Midtrans),
            Some(10000)
        );
        // Xendit reports a major-unit number.
        let xendit = serde_json::json!({ "data": { "amount": 25000, "currency": "IDR" } });
        assert_eq!(
            event_amount_minor(&xendit, PaymentProviderKind::Xendit),
            Some(25000)
        );
        // A provider shape this module does not know yields no
        // amount: the caller must treat it as unquantified.
        let unknown = serde_json::json!({ "amount": 999 });
        assert_eq!(
            event_amount_minor(&unknown, PaymentProviderKind::HyperPay),
            None
        );
    }
}
