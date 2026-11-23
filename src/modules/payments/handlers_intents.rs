//! payments - handlers intents.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::json;
use unified_core::reference_id;
use actix_web::{web, HttpRequest, HttpResponse};
use chrono::{Utc};
use serde_json::{json, Value};
use std::sync::{Arc};
use uuid::Uuid;
use super::{events::*, card_validation::*, card_settings::*, limits::*, providers::*, types::*, reference_ids::*, credentials::*, records::*, adapter::*, dispatch::*, stripe::*, paypal::*, xendit::*, midtrans::*, hyperswitch::*, twocheckout::*, mpgs::*, generic_card::*, custom_provider::*, wallets::*, state::*, requests::*, handlers_catalog::*, handlers_charges::*, handlers_methods::*, handlers_subscriptions::*, handlers_checkout::*, handlers_webhooks::*, handlers_split_bills::*, hooks::*, module::*};



// ---------------------------------------------------------------------------
// Handlers: intents.
// ---------------------------------------------------------------------------
pub(crate) async fn create_intent(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    body: web::Json<CreateIntentRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PaymentIntent> = (|| {
        let _tenant = require_tenant(&req)?;
        let input = body.into_inner();
        if input.customer_id.trim().is_empty() {
            return Err(UnifiedError::bad_request("customer_id is required"));
        }
        require_customer(&req, &input.customer_id, "customer")?;
        validate_money(input.amount_minor, &input.currency)?;
        check_pending_checkout_amount(
            input.order_id.as_deref(),
            input.amount_minor,
            &input.currency,
        )?;
        let connection = find_connection(input.connection_id)?;
        if !connection.enabled {
            return Err(UnifiedError::bad_request("Payment connection is disabled"));
        }
        // Wallet kinds charge through the delegate; the delegate's
        // currency list is checked at confirm time, so the intent
        // only checks the wallet's own list loosely here.
        if !connection.kind.needs_delegate()
            && !currencies_for(connection.kind).contains(&input.currency.as_str())
        {
            return Err(UnifiedError::bad_request(format!(
                "Currency \"{}\" is not supported by provider \"{}\"",
                input.currency,
                connection.kind.as_str()
            )));
        }
        let capture_method = input.capture_method.clone().unwrap_or_else(|| "automatic".to_string());
        if capture_method != "automatic" && capture_method != "manual" {
            return Err(UnifiedError::bad_request(
                "capture_method must be automatic or manual",
            ));
        }
        if capture_method == "manual" && !capabilities_for(connection.kind).supports_authorize {
            return Err(UnifiedError::bad_request(format!(
                "Provider \"{}\" does not support manual capture (authorize)",
                connection.kind.as_str()
            )));
        }
        let token = resolve_token(
            input.payment_method_token.as_deref(),
            input.payment_method_id,
            &input.customer_id,
        )?;
        let now = Utc::now();
        let intent = PaymentIntent {
            id: Uuid::now_v7(),
            connection_id: connection.id,
            provider: connection.kind.as_str().to_string(),
            customer_id: input.customer_id,
            order_id: input.order_id,
            invoice_id: input.invoice_id,
            amount_minor: input.amount_minor,
            currency: input.currency,
            status: "requires_confirmation".to_string(),
            capture_method,
            provider_transaction_id: None,
            fee_minor: None,
            payment_method_token: token,
            payment_method_id: input.payment_method_id,
            idempotency_key: idempotency_key(input.idempotency_key),
            created_at: now,
            updated_at: now,
        };
        intents_store().insert(intent.id, intent.clone());
        Ok(intent)
    })();
    match result {
        Ok(intent) => json::success_response(intent, 201),
        Err(error) => json::error_response(&error, debug),
    }
}


pub(crate) async fn get_intent(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    id: web::Path<Uuid>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PaymentIntent> = (|| {
        let _tenant = require_tenant(&req)?;
        let intent = intents_store()
            .get(&id.into_inner())
            .map(|entry| entry.clone())
            .ok_or_else(|| UnifiedError::not_found("Payment intent not found"))?;
        require_customer(&req, &intent.customer_id, "intent")?;
        Ok(intent)
    })();
    json::respond(result, debug)
}


/// How one provider status maps onto the confirm outcome. Every
/// status is mapped explicitly: a pending (or still
/// requires-confirmation) result keeps the intent in
/// `requires_confirmation` with no event and a 202 pending
/// outcome, while a terminal provider status that is neither a
/// capture nor a plain decline (refunded, voided, chargeback,
/// cancelled) fails the intent and returns an error — the confirm
/// never reports success for those, even though the transaction
/// row keeps the provider's own status for the audit trail.
pub(crate) struct ConfirmMapping {
    pub(crate) kind: &'static str,
    pub(crate) event: Option<&'static str>,
    pub(crate) intent_status: &'static str,
    pub(crate) terminal_error: bool,
}


pub(crate) fn confirm_intent_mapping(status: PaymentStatus) -> ConfirmMapping {
    match status {
        PaymentStatus::Authorized => ConfirmMapping {
            kind: "authorize",
            event: Some(PAYMENT_AUTHORIZED),
            intent_status: "authorized",
            terminal_error: false,
        },
        PaymentStatus::Captured => ConfirmMapping {
            kind: "charge",
            event: Some(PAYMENT_CAPTURED),
            intent_status: "captured",
            terminal_error: false,
        },
        PaymentStatus::Failed => ConfirmMapping {
            kind: "charge",
            event: Some(PAYMENT_FAILED),
            intent_status: "failed",
            terminal_error: false,
        },
        PaymentStatus::Pending | PaymentStatus::RequiresConfirmation => ConfirmMapping {
            kind: "charge",
            event: None,
            intent_status: "requires_confirmation",
            terminal_error: false,
        },
        PaymentStatus::Refunded
        | PaymentStatus::PartiallyRefunded
        | PaymentStatus::Voided
        | PaymentStatus::Chargeback
        | PaymentStatus::Cancelled => ConfirmMapping {
            kind: "charge",
            event: if status == PaymentStatus::Chargeback {
                Some(PAYMENT_CHARGEBACK)
            } else {
                None
            },
            intent_status: "failed",
            terminal_error: true,
        },
    }
}


/// Confirm an intent: authorize (manual capture) or charge
/// (automatic capture), record the transaction, and emit the matching
/// event. A repeated confirm with the same idempotency key returns the
/// original transaction instead of charging twice.
pub(crate) async fn confirm_intent(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<PaymentsState>>,
    req: HttpRequest,
    id: web::Path<Uuid>,
    body: web::Json<ConfirmIntentRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PaymentTransaction> = (|| async {
        let _tenant = require_tenant(&req)?;
        let id = id.into_inner();
        let request = body.into_inner();
        let mut intent = intents_store()
            .get(&id)
            .map(|entry| entry.clone())
            .ok_or_else(|| UnifiedError::not_found("Payment intent not found"))?;
        require_customer(&req, &intent.customer_id, "intent")?;
        // Idempotency first: a repeated confirm with the same key
        // returns the original transaction instead of charging twice
        // or failing on the already-confirmed status below.
        let connection = find_connection(intent.connection_id)?;
        if let Some(existing) = find_by_idempotency(
            connection.id,
            &request.idempotency_key.clone().unwrap_or_else(|| intent.idempotency_key.clone()),
        ) {
            return Ok(replay_for_customer(existing, &intent.customer_id)?);
        }
        if intent.status != "requires_confirmation" {
            return Err(UnifiedError::conflict(format!(
                "Payment intent in status \"{}\" cannot be confirmed",
                intent.status
            )));
        }
        let token = resolve_token(
            request.payment_method_token.as_deref(),
            request.payment_method_id,
            &intent.customer_id,
        )?
        .or_else(|| intent.payment_method_token.clone());
        // Batch 18 (S1): validate the card before any provider
        // call. Raw details on the request run in `full` mode; a
        // stored method id (from the request or the intent) runs
        // in `method` mode; an opaque token alone skips the gate.
        request_card_validation(&state, &req, request.card.as_ref(), request.payment_method_id.or(intent.payment_method_id), None, &connection, &intent.customer_id, intent.order_id.as_deref(), Some(intent.id))?;
        let adapter = adapter_for(&connection)?;
        let charge_input = ChargeInput {
            amount_minor: intent.amount_minor,
            currency: intent.currency.clone(),
            customer_id: intent.customer_id.clone(),
            order_id: intent.order_id.clone(),
            reference_id: pending_checkout_reference(intent.order_id.as_deref()),
            invoice_id: intent.invoice_id.clone(),
            description: None,
            payment_method_token: token,
            idempotency_key: idempotency_key(request.idempotency_key),
            three_ds: false,
            billing: None,
        };
        // Claim the idempotency key BEFORE the provider call: a
        // concurrent confirm with the same key waits for this
        // request's transaction instead of charging twice.
        let claim = match claim_idempotency(connection.id, &charge_input.idempotency_key) {
            Some(sentinel) => sentinel,
            None => {
                if let Some(existing) =
                    wait_for_idempotency(connection.id, &charge_input.idempotency_key).await
                {
                    return Ok(replay_for_customer(existing, &intent.customer_id)?);
                }
                return Err(UnifiedError::conflict(
                    "A confirm with this idempotency key is already in progress",
                ));
            }
        };
        // Atomically move the intent to `confirming`, so a
        // concurrent confirm of the same intent cannot charge
        // twice even with a different idempotency key. The status
        // is restored on any provider failure below.
        let transitioned = match intents_store().get_mut(&id) {
            Some(mut stored) if stored.status == "requires_confirmation" => {
                stored.status = "confirming".to_string();
                true
            }
            _ => false,
        };
        if !transitioned {
            release_idempotency_claim(connection.id, &charge_input.idempotency_key, claim);
            return Err(UnifiedError::conflict(format!(
                "Payment intent in status \"{}\" cannot be confirmed",
                intents_store()
                    .get(&id)
                    .map(|entry| entry.status.clone())
                    .unwrap_or_default()
            )));
        }
        let result = if intent.capture_method == "manual" {
            adapter.authorize(&charge_input).await
        } else {
            adapter.charge(&charge_input).await
        };
        let result = match result {
            Ok(result) => result,
            Err(error) => {
                if let Some(mut stored) = intents_store().get_mut(&id) {
                    stored.status = "requires_confirmation".to_string();
                }
                release_idempotency_claim(connection.id, &charge_input.idempotency_key, claim);
                return Err(error);
            }
        };
        let mapping = confirm_intent_mapping(result.status);
        let (kind, status, event) = (mapping.kind, result.status, mapping.event);
        let mut transaction = new_transaction(
            connection.id,
            &intent.provider,
            kind,
            &intent.customer_id,
            intent.order_id.clone(),
            intent.invoice_id.clone(),
            intent.amount_minor,
            &intent.currency,
            status,
            result.provider_transaction_id.clone(),
            charge_input.idempotency_key.clone(),
        );
        transaction.intent_id = Some(intent.id);
        transaction.fee_minor = result.fee_minor;
        let transaction = record_transaction(transaction);
        intent.provider_transaction_id = Some(result.provider_transaction_id.clone());
        intent.fee_minor = result.fee_minor;
        intent.status = mapping.intent_status.to_string();
        intent.updated_at = Utc::now();
        intents_store().insert(intent.id, intent.clone());
        if let Some(event) = event {
            state.emit(
                event,
                json!({
                    "intent_id": intent.id,
                    "transaction_id": transaction.id,
                    "provider_transaction_id": result.provider_transaction_id,
                    "provider": intent.provider,
                    "connection_id": connection.id,
                    "customer_id": intent.customer_id,
                    "order_id": intent.order_id,
                    "invoice_id": intent.invoice_id,
                    "amount_minor": intent.amount_minor,
                    "currency": intent.currency,
                    "fee_minor": result.fee_minor,
                    "status": transaction.status,
                }),
            );
            if event == PAYMENT_FAILED {
                notify(
                    &state,
                    &intent.customer_id,
                    "payment_failed",
                    "Payment failed",
                    &format!(
                        "Your payment of {} {} could not be completed.",
                        intent.amount_minor, intent.currency
                    ),
                );
            }
        }
        if result.status == PaymentStatus::Failed {
            return Err(UnifiedError::bad_request(format!(
                "Provider \"{}\" declined the payment",
                intent.provider
            ))
            .with_metadata("provider", Value::String(intent.provider.clone()))
            .with_metadata("transaction_id", Value::String(transaction.id.to_string())));
        }
        if mapping.terminal_error {
            // The provider answered with a terminal status that is
            // not a capture: the transaction row above keeps the
            // provider's status, the intent is failed, and the
            // confirm itself is an error — never a success.
            return Err(UnifiedError::unprocessable(format!(
                "Provider \"{}\" returned status \"{}\"; the payment was not confirmed",
                intent.provider,
                result.status.as_str()
            ))
            .with_metadata("provider", Value::String(intent.provider.clone()))
            .with_metadata("transaction_id", Value::String(transaction.id.to_string())));
        }
        Ok(transaction)
    })()
    .await;
    match result {
        // Captured confirms are 201; pending (and authorized)
        // confirms are 202, so callers can tell "paid" from
        // "pending" by status code.
        Ok(transaction) => {
            let status = charge_success_status(&transaction.status);
            json::success_response(transaction, status)
        }
        Err(error) => json::error_response(&error, debug),
    }
}


/// Capture a previously authorized intent.
pub(crate) async fn capture_intent(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<PaymentsState>>,
    req: HttpRequest,
    id: web::Path<Uuid>,
    body: web::Json<CaptureIntentRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PaymentTransaction> = (|| async {
        let _tenant = require_tenant(&req)?;
        let id = id.into_inner();
        let request = body.into_inner();
        let mut intent = intents_store()
            .get(&id)
            .map(|entry| entry.clone())
            .ok_or_else(|| UnifiedError::not_found("Payment intent not found"))?;
        require_customer(&req, &intent.customer_id, "intent")?;
        if let Some(amount) = request.amount_minor {
            if amount <= 0 || amount > intent.amount_minor {
                return Err(UnifiedError::bad_request(
                    "Capture amount must be greater than zero and no more than the authorized amount",
                ));
            }
        }
        if intent.status != "authorized" {
            return Err(UnifiedError::conflict(format!(
                "Only an authorized intent can be captured (status is \"{}\")",
                intent.status
            )));
        }
        let provider_transaction_id = intent.provider_transaction_id.clone().ok_or_else(|| {
            UnifiedError::internal("Authorized intent has no provider transaction id")
        })?;
        let connection = find_connection(intent.connection_id)?;
        let adapter = adapter_for(&connection)?;
        // Atomically move the intent to `capturing`, so a
        // concurrent capture of the same authorization cannot
        // capture twice. The status is restored on any failure
        // below, so the caller can retry.
        let transitioned = match intents_store().get_mut(&id) {
            Some(mut stored) if stored.status == "authorized" => {
                stored.status = "capturing".to_string();
                true
            }
            _ => false,
        };
        if !transitioned {
            return Err(UnifiedError::conflict(format!(
                "Only an authorized intent can be captured (status is \"{}\")",
                intents_store()
                    .get(&id)
                    .map(|entry| entry.status.clone())
                    .unwrap_or_default()
            )));
        }
        let result = match adapter
            .capture(&provider_transaction_id, request.amount_minor)
            .await
        {
            Ok(result) => result,
            Err(error) => {
                if let Some(mut stored) = intents_store().get_mut(&id) {
                    stored.status = "authorized".to_string();
                }
                return Err(error);
            }
        };
        // Only a successful capture moves the intent; a failed
        // capture leaves it authorized so the caller can retry.
        if result.status != PaymentStatus::Captured {
            if let Some(mut stored) = intents_store().get_mut(&id) {
                stored.status = "authorized".to_string();
            }
            return Err(UnifiedError::bad_request(format!(
                "Provider \"{}\" capture failed with status \"{}\"",
                intent.provider,
                result.status.as_str()
            ))
            .with_metadata("provider", Value::String(intent.provider.clone())));
        }
        let mut transaction = new_transaction(
            connection.id,
            &intent.provider,
            "capture",
            &intent.customer_id,
            intent.order_id.clone(),
            intent.invoice_id.clone(),
            request.amount_minor.unwrap_or(intent.amount_minor),
            &intent.currency,
            result.status,
            result.provider_transaction_id.clone(),
            Uuid::now_v7().to_string(),
        );
        transaction.intent_id = Some(intent.id);
        transaction.fee_minor = result.fee_minor;
        let transaction = record_transaction(transaction);
        intent.status = "captured".to_string();
        intent.fee_minor = result.fee_minor;
        intent.updated_at = Utc::now();
        intents_store().insert(intent.id, intent.clone());
        state.emit(
            PAYMENT_CAPTURED,
            json!({
                "intent_id": intent.id,
                "transaction_id": transaction.id,
                "provider_transaction_id": result.provider_transaction_id,
                "provider": intent.provider,
                "connection_id": connection.id,
                "customer_id": intent.customer_id,
                "order_id": intent.order_id,
                "invoice_id": intent.invoice_id,
                "amount_minor": transaction.amount_minor,
                "currency": intent.currency,
                "fee_minor": result.fee_minor,
                "status": transaction.status,
            }),
        );
        Ok(transaction)
    })()
    .await;
    json::respond(result, debug)
}


/// Cancel an intent. An authorized intent is also voided at the
/// provider when the provider supports void; a void failure leaves
/// the intent authorized and returns the provider error.
pub(crate) async fn cancel_intent(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    id: web::Path<Uuid>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PaymentIntent> = (|| async {
        let _tenant = require_tenant(&req)?;
        let id = id.into_inner();
        let mut intent = intents_store()
            .get(&id)
            .map(|entry| entry.clone())
            .ok_or_else(|| UnifiedError::not_found("Payment intent not found"))?;
        require_customer(&req, &intent.customer_id, "intent")?;
        if intent.status != "requires_confirmation" && intent.status != "authorized" {
            return Err(UnifiedError::conflict(format!(
                "Payment intent in status \"{}\" cannot be cancelled",
                intent.status
            )));
        }
        if intent.status == "authorized" {
            let connection = find_connection(intent.connection_id)?;
            let adapter = adapter_for(&connection)?;
            if adapter.capabilities().supports_void {
                if let Some(provider_transaction_id) = &intent.provider_transaction_id {
                    adapter.void(provider_transaction_id).await?;
                }
            }
        }
        intent.status = "cancelled".to_string();
        intent.updated_at = Utc::now();
        intents_store().insert(intent.id, intent.clone());
        Ok(intent)
    })()
    .await;
    json::respond(result, debug)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn confirm_mapping_keeps_intent_and_transaction_in_agreement() {
        let captured = confirm_intent_mapping(PaymentStatus::Captured);
        assert_eq!(captured.intent_status, "captured");
        assert_eq!(captured.event, Some(PAYMENT_CAPTURED));
        assert!(!captured.terminal_error);
        let authorized = confirm_intent_mapping(PaymentStatus::Authorized);
        assert_eq!(authorized.kind, "authorize");
        assert_eq!(authorized.intent_status, "authorized");
        assert!(!authorized.terminal_error);
        // A pending provider answer is not a failure and not a
        // success: the intent stays open for confirmation.
        for status in [PaymentStatus::Pending, PaymentStatus::RequiresConfirmation] {
            let mapping = confirm_intent_mapping(status);
            assert_eq!(mapping.intent_status, "requires_confirmation");
            assert_eq!(mapping.event, None);
            assert!(!mapping.terminal_error);
        }
        // Terminal non-capture statuses fail the intent and the
        // confirm: they must never be reported as a success.
        for status in [
            PaymentStatus::Refunded,
            PaymentStatus::PartiallyRefunded,
            PaymentStatus::Voided,
            PaymentStatus::Chargeback,
            PaymentStatus::Cancelled,
        ] {
            let mapping = confirm_intent_mapping(status);
            assert_eq!(mapping.intent_status, "failed");
            assert!(mapping.terminal_error);
        }
        assert_eq!(
            confirm_intent_mapping(PaymentStatus::Chargeback).event,
            Some(PAYMENT_CHARGEBACK)
        );
        let failed = confirm_intent_mapping(PaymentStatus::Failed);
        assert_eq!(failed.intent_status, "failed");
        assert!(!failed.terminal_error);
    }
}
