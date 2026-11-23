//! payments - hooks.

use unified_core::hooks::{HookContext};
use unified_core::json;
use chrono::{DateTime, Utc};
use serde_json::{json, Value};
use std::sync::{Arc};
use uuid::Uuid;
use super::{events::*, card_validation::*, card_settings::*, limits::*, providers::*, types::*, reference_ids::*, credentials::*, records::*, adapter::*, dispatch::*, stripe::*, paypal::*, xendit::*, midtrans::*, hyperpay::*, twocheckout::*, mpgs::*, generic_card::*, custom_provider::*, wallets::*, state::*, requests::*, handlers_catalog::*, handlers_intents::*, handlers_charges::*, handlers_methods::*, handlers_subscriptions::*, handlers_checkout::*, handlers_webhooks::*, handlers_split_bills::*, module::*};




// ---------------------------------------------------------------------------
// Hook handlers (synchronous, like every other module's listeners).
// ---------------------------------------------------------------------------
/// A capture completed — through a gateway charge, an intent capture,
/// or a bank-transfer administrator approval. When the payload
/// carries `split_id` and `part_id`, mark that split part paid (the
/// single place where parts move to `paid`, so gateway and manual
/// payments share one code path), emit `payment.split.part.paid`,
/// and emit `payment.split.completed` when the full total is covered.
/// Captures without split context are ignored. The transition is
/// idempotent: an already-paid part is left untouched.
pub(crate) fn on_payment_captured_for_split(state: Arc<PaymentsState>, ctx: &mut HookContext) {
    let split_id = ctx
        .get("split_id")
        .and_then(|value| value.as_str())
        .and_then(|value| Uuid::parse_str(value).ok());
    let part_id = ctx
        .get("part_id")
        .and_then(|value| value.as_str())
        .and_then(|value| Uuid::parse_str(value).ok());
    let (Some(split_id), Some(part_id)) = (split_id, part_id) else {
        return;
    };
    let transaction_id = ctx
        .get("transaction_id")
        .and_then(|value| value.as_str())
        .and_then(|value| Uuid::parse_str(value).ok());
    let Some(mut split) = splits_store()
        .get(&split_id)
        .map(|entry| entry.value().clone())
    else {
        return;
    };
    if split.status != "active" {
        return;
    }
    let Some(part) = split.parts.iter_mut().find(|part| part.id == part_id) else {
        return;
    };
    if part.status != "pending" {
        return;
    }
    part.status = "paid".to_string();
    part.paid_transaction_id = transaction_id;
    part.paid_at = Some(Utc::now());
    let part_amount = part.amount_minor;
    let part_provider = part.provider.clone();
    let payer_id = part.payer_id.clone();
    recompute_split_totals(&mut split);
    let completed = split.remaining_minor == 0;
    if completed {
        split.status = "completed".to_string();
        split.updated_at = Utc::now();
    }
    let split_value = serde_json::to_value(&split).unwrap_or(Value::Null);
    splits_store().insert(split_id, split.clone());
    state.emit(
        PAYMENT_SPLIT_PART_PAID,
        json!({
            "split_id": split_id,
            "part_id": part_id,
            "order_id": split.order_id.clone(),
            "invoice_id": split.invoice_id.clone(),
            "customer_id": split.customer_id.clone(),
            "payer_id": payer_id,
            "provider": part_provider,
            "amount_minor": part_amount,
            "currency": split.currency.clone(),
            "transaction_id": transaction_id,
            "split": split_value.clone(),
        }),
    );
    if completed {
        state.emit(
            PAYMENT_SPLIT_COMPLETED,
            json!({
                "split_id": split_id,
                "order_id": split.order_id.clone(),
                "invoice_id": split.invoice_id.clone(),
                "customer_id": split.customer_id.clone(),
                "total_minor": split.total_minor,
                "currency": split.currency.clone(),
                "split": split_value,
            }),
        );
    }
}


/// The store started a checkout through
/// `POST /api/v1/store/orders/{id}/pay` and emitted
/// `payment.initiated` with `{ "order", "provider", "amount" }`.
/// Hook handlers are synchronous, so the gateway cannot charge here;
/// the checkout is recorded so a later confirm / pay call can answer
/// it, and the result tells the store the gateway accepted the
/// handoff.
pub(crate) fn on_payment_initiated(state: Arc<PaymentsState>, ctx: &mut HookContext) {
    let _ = state;
    let order_id = ctx
        .get("order")
        .and_then(|order| order.get("id"))
        .and_then(|id| id.as_str())
        .map(str::to_string)
        .or_else(|| {
            ctx.get("order_id")
                .and_then(|id| id.as_str())
                .map(str::to_string)
        });
    let Some(order_id) = order_id else {
        ctx.add_result(json!({
            "gateway": "payments",
            "status": "rejected",
            "error": "payment.initiated carries no order id",
        }));
        return;
    };
    pending_checkouts_store().insert(order_id.clone(), ctx.data.clone());
    ctx.add_result(json!({
        "gateway": "payments",
        "status": "accepted",
        "order_id": order_id,
        "note": "Checkout recorded. Confirm with POST /api/v1/payments/orders/{order_id}/pay (charge) or POST /api/v1/payments/intents/{id}/confirm (intent).",
    }));
}


/// The store refunded an order (`store.order.refunded`) after marking
/// it refunded. The gateway-first refund path emits
/// `payment.refunded` itself; this listener re-emits it for the
/// store-first path, so downstream listeners that only know
/// `payment.refunded` (billing, licensing) still fire. Licensing
/// revocation is idempotent (an already-revoked license stays
/// revoked), so a double emit is safe.
pub(crate) fn on_store_order_refunded(state: Arc<PaymentsState>, ctx: &mut HookContext) {
    let order_id = ctx
        .get("order_id")
        .and_then(|id| id.as_str())
        .map(str::to_string)
        .or_else(|| {
            ctx.get("order")
                .and_then(|order| order.get("id"))
                .and_then(|id| id.as_str())
                .map(str::to_string)
        });
    let Some(order_id) = order_id else {
        return;
    };
    // A store-refunded order is a completed refund by construction
    // (the store only marks it refunded on the completion path), so
    // the re-emitted event carries an explicit completed status and
    // the refund identifiers the billing listener needs for its
    // status gate and its redelivery dedupe.
    state.emit(
        PAYMENT_REFUNDED,
        json!({
            "order_id": order_id,
            "license_ids": ctx.get("license_ids").cloned().unwrap_or(Value::Null),
            "reason": ctx.get("reason").cloned().unwrap_or(Value::Null),
            "transaction_id": ctx.get("transaction_id").cloned().unwrap_or(Value::Null),
            "provider_refund_id": ctx.get("provider_refund_id").cloned().unwrap_or(Value::Null),
            "amount_minor": ctx.get("amount_minor").cloned().unwrap_or(Value::Null),
            "currency": ctx.get("currency").cloned().unwrap_or(Value::Null),
            "status": "refunded",
            "fully_refunded": true,
            "source": "store.order.refunded",
        }),
    );
}


/// A renewal was requested for a subscription
/// (`payment.subscription.renew_requested`, emitted by an operator,
/// a cron, or the scheduler once it knows the kind).
///
/// Two branches, and they must not be confused: an
/// operator-initiated request only schedules the charge by asking
/// the scheduler to run a `payment.subscription.renew` job at
/// `renew_at`. A scheduler-dispatched request carries
/// `via_scheduler: true` and is the terminal step: it runs the
/// charge instead of re-enqueuing. Without that distinction each
/// dispatch spawns one new queue item forever and the renewal never
/// runs, because the handler would answer its own dispatched event
/// by enqueuing the same job again.
pub(crate) fn on_subscription_renew_requested(state: Arc<PaymentsState>, ctx: &mut HookContext) {
    let Some(subscription_id) = ctx
        .get("subscription_id")
        .and_then(|id| id.as_str())
        .and_then(|id| Uuid::parse_str(id).ok())
    else {
        ctx.add_result(json!({
            "gateway": "payments",
            "status": "rejected",
            "error": "payment.subscription.renew_requested needs subscription_id",
        }));
        return;
    };
    // Scheduler-dispatched request: run the charge, never re-enqueue.
    // The charge is asynchronous and this handler is synchronous, so
    // it is spawned (the outcome is reported through the renewal
    // events: `payment.subscription.renewed` on success,
    // `payment.failed` on failure). The scheduler's 30-second
    // per-subscription dedupe stays in place as a second guard
    // against duplicate dispatches.
    if ctx
        .get("via_scheduler")
        .and_then(|value| value.as_bool())
        == Some(true)
    {
        let worker_state = state.clone();
        spawn_renewal_worker(async move {
            let _ = renew_one(&worker_state, subscription_id).await;
        });
        ctx.add_result(json!({
            "gateway": "payments",
            "subscription_id": subscription_id,
            "via_scheduler": true,
            "queued": false,
            "renewal": "started",
        }));
        return;
    }
    let renew_at = ctx
        .get("renew_at")
        .and_then(|value| value.as_str())
        .and_then(|value| DateTime::parse_from_rfc3339(value).ok())
        .map(|dt| dt.with_timezone(&Utc))
        .unwrap_or_else(Utc::now);
    let queued = enqueue_renewal(&state, subscription_id, renew_at);
    ctx.add_result(json!({
        "gateway": "payments",
        "subscription_id": subscription_id,
        "queued": queued,
    }));
}
