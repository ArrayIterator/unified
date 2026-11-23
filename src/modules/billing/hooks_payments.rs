//! billing - hooks payments.

use unified_core::hooks::HookContext;
use serde_json::{json, Value};
use std::sync::Arc;
use super::types::*;
use super::state::*;
use super::helpers::*;
use super::handlers_invoices::*;
use super::school_fees_types::*;

pub(crate) fn on_order_created(state: Arc<BillingState>, ctx: &mut HookContext) {
    let order = ctx.data.clone();
    // Remember the order so `from-order` can build from it later.
    if let Some(order_id) = order.get("id").and_then(|v| v.as_str()) {
        state
            .seen_orders
            .insert(order_id.to_string(), order.clone());
    }
    // Auto-create a draft invoice, unless one already exists for the order.
    let Some(invoice) = state.invoice_from_order_value(&order) else {
        return;
    };
    let order_id = invoice.order_id.clone();
    let already_exists = state.invoices.iter().any(|entry| {
        entry.value().order_id == order_id && entry.value().status != "void"
    });
    if already_exists {
        return;
    }
    state.invoices.insert(invoice.id.clone(), invoice.clone());
    state.emit(
        "invoice.created",
        serde_json::to_value(&invoice).unwrap_or(Value::Null),
    );
}

/// Event names this module listens to (the payments module owns the
/// canonical constants; the literals are repeated here so billing
/// stays decoupled from the payments module).
pub(crate) const PAYMENT_CAPTURED: &str = "payment.captured";

pub(crate) const PAYMENT_REFUNDED: &str = "payment.refunded";

/// The store marks an order paid (`order.paid`) and billing settles
/// the matching invoice from it -- including the manual mark-paid
/// path, which emits no `payment.captured`.
pub(crate) const ORDER_PAID: &str = "order.paid";

/// The invoice a payment event belongs to: by invoice id when the
/// event carries one, otherwise by order id. Billing auto-creates a
/// draft invoice on `order.created`, so a missing invoice here means
/// the order was never seen by billing, not a lost payment.
pub(crate) fn invoice_for_payment_event(state: &BillingState, ctx: &HookContext) -> Option<Invoice> {
    if let Some(invoice_id) = ctx.get("invoice_id").and_then(|value| value.as_str()) {
        if let Some(invoice) = state
            .invoices
            .get(invoice_id)
            .map(|entry| entry.value().clone())
        {
            return Some(invoice);
        }
    }
    // A payment naming the commercial subscription (renewal or
    // proration) belongs to that subscription's open invoice.
    if let Some(subscription_id) = ctx
        .get("subscription_id")
        .and_then(|value| value.as_str())
    {
        if let Some(invoice) = state
            .invoices
            .iter()
            .find(|entry| {
                entry.value().subscription_id.as_deref() == Some(subscription_id)
                    && !matches!(
                        entry.value().status.as_str(),
                        "paid" | "void" | "credited"
                    )
            })
            .map(|entry| entry.value().clone())
        {
            return Some(invoice);
        }
    }
    let order_id = ctx
        .get("order_id")
        .and_then(|value| value.as_str())
        .or_else(|| {
            ctx.get("order")
                .and_then(|order| order.get("id"))
                .and_then(|id| id.as_str())
        })?;
    // A renewal invoice and the original order invoice share the
    // order id: prefer the open subscription-linked invoice, so a
    // renewal payment lands on the renewal invoice, not the already
    // paid original.
    if let Some(invoice) = state
        .invoices
        .iter()
        .find(|entry| {
            entry.value().order_id.as_deref() == Some(order_id)
                && entry.value().subscription_id.is_some()
                && !matches!(
                    entry.value().status.as_str(),
                    "paid" | "void" | "credited"
                )
        })
        .map(|entry| entry.value().clone())
    {
        return Some(invoice);
    }
    state
        .invoices
        .iter()
        .find(|entry| entry.value().order_id.as_deref() == Some(order_id))
        .map(|entry| entry.value().clone())
}

/// Dedupe key for a payment event: the refund / transaction id when
/// the event carries one, otherwise the order id plus amount, so a
/// redelivered event never applies twice.
pub(crate) fn payment_event_key(ctx: &HookContext, kind: &str) -> String {
    let id = ctx
        .get("provider_refund_id")
        .and_then(|value| value.as_str())
        .map(str::to_string)
        .or_else(|| {
            ctx.get("transaction_id")
                .and_then(|value| value.as_str())
                .map(str::to_string)
        })
        .or_else(|| {
            ctx.get("provider_transaction_id")
                .and_then(|value| value.as_str())
                .map(str::to_string)
        });
    match id {
        Some(id) => format!("{kind}:{id}"),
        None => {
            let order_id = ctx
                .get("order_id")
                .and_then(|value| value.as_str())
                .unwrap_or("");
            let amount = ctx
                .get("amount_minor")
                .and_then(|value| value.as_i64())
                .map(|amount| amount.to_string())
                .unwrap_or_default();
            format!("{kind}:order:{order_id}:{amount}")
        }
    }
}

/// An approved payment (`payment.captured`) records against the
/// matching invoice: amount paid, remaining balance, and status
/// `paid` or `partially_paid`, with the gateway / transaction
/// reference on the payment record. The existing `invoice.paid`
/// event is emitted when the balance reaches zero, exactly as the
/// manual pay path does. Redeliveries are deduplicated, and a
/// currency mismatch is never applied. An overpayment is never
/// silently clamped away: the balance is settled in full and the
/// excess is recorded as a credit note plus an
/// `invoice.overpayment` event, so the money the gateway actually
/// captured stays on the books.
pub(crate) fn on_payment_captured(state: Arc<BillingState>, ctx: &mut HookContext) {
    if let Some(status) = ctx.get("status").and_then(|value| value.as_str()) {
        if status != "captured" {
            ctx.add_result(json!({
                "billing": "skipped",
                "reason": format!("payment.captured carries status \"{status}\", not \"captured\""),
            }));
            return;
        }
    }
    let Some(invoice) = invoice_for_payment_event(&state, ctx) else {
        ctx.add_result(json!({
            "billing": "no_invoice",
            "reason": "no invoice exists for this order; billing creates invoices on order.created, so this order was never seen",
        }));
        return;
    };
    if matches!(invoice.status.as_str(), "void" | "credited") {
        ctx.add_result(json!({
            "billing": "skipped",
            "invoice_id": invoice.id,
            "reason": format!("invoice is {}", invoice.status),
        }));
        return;
    }
    let Some(amount_minor) = ctx.get("amount_minor").and_then(|value| value.as_i64()) else {
        ctx.add_result(json!({
            "billing": "skipped",
            "invoice_id": invoice.id,
            "reason": "payment.captured carries no amount",
        }));
        return;
    };
    let currency = ctx
        .get("currency")
        .and_then(|value| value.as_str())
        .unwrap_or("");
    if !currency.is_empty() && !currency.eq_ignore_ascii_case(&invoice.currency) {
        ctx.add_result(json!({
            "billing": "skipped",
            "invoice_id": invoice.id,
            "reason": format!(
                "captured currency {currency} does not match invoice currency {}",
                invoice.currency
            ),
        }));
        return;
    }
    let key = payment_event_key(ctx, "capture");
    if state.processed_payment_events.contains_key(&key) {
        ctx.add_result(json!({
            "billing": "duplicate",
            "invoice_id": invoice.id,
            "reason": "this capture was already applied",
        }));
        return;
    }
    let method = ctx
        .get("provider")
        .and_then(|value| value.as_str())
        .unwrap_or("gateway")
        .to_string();
    let reference = ctx
        .get("provider_transaction_id")
        .and_then(|value| value.as_str())
        .map(str::to_string)
        .or_else(|| {
            ctx.get("transaction_id")
                .and_then(|value| value.as_str())
                .map(str::to_string)
        });
    let captured = amount_minor;
    if captured <= 0 {
        ctx.add_result(json!({
            "billing": "skipped",
            "invoice_id": invoice.id,
            "reason": "payment.captured carries no positive amount",
        }));
        return;
    }
    // The captured amount settles at most the outstanding balance;
    // anything above it is an overpayment the gateway really took,
    // so it is recorded as a credit note below instead of dropped.
    let (amount, excess) = split_capture(captured, invoice.balance);
    if amount <= 0 && excess <= 0 {
        ctx.add_result(json!({
            "billing": "skipped",
            "invoice_id": invoice.id,
            "reason": "invoice has no outstanding balance",
        }));
        return;
    }
    let mut overpayment_note: Option<CreditNote> = None;
    if amount > 0 {
        let payment = InvoicePayment {
            id: new_id(),
            invoice_id: invoice.id.clone(),
            amount,
            method: method.clone(),
            reference,
            recorded_at: now(),
        };
        state.payments.insert(payment.id.clone(), payment.clone());
        let mut stored = state
            .invoices
            .get_mut(&invoice.id)
            .expect("invoice still present");
        stored.amount_paid += amount;
        recompute_balance(&mut stored);
        if stored.balance == 0 {
            stored.status = "paid".to_string();
            stored.paid_at = Some(now());
        } else {
            stored.status = "partially_paid".to_string();
        }
        stored.updated_at = now();
        let fully_paid = stored.status == "paid";
        let invoice_value = serde_json::to_value(&*stored).unwrap_or(Value::Null);
        let payment_value = serde_json::to_value(&payment).unwrap_or(Value::Null);
        drop(stored);
        state.emit("invoice.payment.recorded", payment_value);
        if fully_paid {
            state.emit(
                "invoice.paid",
                json!({ "invoice": invoice_value, "method": method }),
            );
        }
    }
    if excess > 0 {
        let note = CreditNote {
            id: new_id(),
            invoice_id: invoice.id.clone(),
            number: state.next_credit_note_number(),
            amount: excess,
            reason: format!(
                "Overpayment captured on invoice {} ({} {})",
                invoice.number,
                minor_to_major(captured, &invoice.currency),
                invoice.currency
            ),
            created_at: now(),
        };
        state
            .credit_notes
            .insert(note.id.clone(), note.clone());
        let mut stored = state
            .invoices
            .get_mut(&invoice.id)
            .expect("invoice still present");
        // The balance is already settled by the applied part, so the
        // overpayment credit only records the excess owed back; it
        // must not flip a paid invoice to `credited`.
        stored.credit_total += excess;
        recompute_balance(&mut stored);
        stored.updated_at = now();
        drop(stored);
        state.emit(
            "invoice.overpayment",
            json!({
                "invoice_id": invoice.id,
                "captured": minor_to_major(captured, &invoice.currency),
                "applied": minor_to_major(amount, &invoice.currency),
                "overpayment": minor_to_major(excess, &invoice.currency),
                "currency": invoice.currency,
                "credit_note_id": note.id,
            }),
        );
        state.emit(
            "invoice.credit_note.created",
            serde_json::to_value(&note).unwrap_or(Value::Null),
        );
        overpayment_note = Some(note);
    }
    state
        .processed_payment_events
        .insert(key, now());
    // A fully paid renewal invoice advances the subscription period
    // (see `maybe_renew_subscription`); other invoices never do.
    if let Some(updated_invoice) = state
        .invoices
        .get(&invoice.id)
        .map(|entry| view_invoice(entry.value()))
    {
        state.maybe_renew_subscription(&updated_invoice);
    }
    let fully_paid = state
        .invoices
        .get(&invoice.id)
        .map(|entry| entry.value().status == "paid")
        .unwrap_or(false);
    ctx.add_result(json!({
        "billing": if amount <= 0 {
            "overpayment"
        } else if fully_paid {
            "paid"
        } else {
            "partially_paid"
        },
        "invoice_id": invoice.id,
        "amount": minor_to_major(amount, &invoice.currency),
        "overpayment": minor_to_major(excess, &invoice.currency),
        "overpayment_credit_note_id": overpayment_note.map(|note| note.id),
    }));
}

/// A completed refund (`payment.refunded` with a `refunded` or
/// `partially_refunded` status) returns money on the matching
/// invoice: a credit note records the refund, `amount_paid` falls by
/// the refunded amount, and an invoice whose paid amount reaches zero
/// reads as `credited`. A partial refund of a paid invoice leaves it
/// as `partially_refunded` instead of still reading as fully paid:
/// the refunded part is visible in the status, not just in the
/// credit-note history. While the refund is still pending the invoice
/// is left untouched: the pending refund lives in the payments
/// transaction record, and only the refund-completion path finishes
/// it here.
pub(crate) fn on_payment_refunded(state: Arc<BillingState>, ctx: &mut HookContext) {
    let status = ctx
        .get("status")
        .and_then(|value| value.as_str())
        .unwrap_or("");
    match status {
        "refunded" | "partially_refunded" => {}
        "pending" => {
            ctx.add_result(json!({
                "billing": "skipped",
                "reason": "refund is still pending; the invoice is untouched until the refund completes",
            }));
            return;
        }
        other => {
            ctx.add_result(json!({
                "billing": "skipped",
                "reason": format!("refund status \"{other}\" is not a completed refund"),
            }));
            return;
        }
    }
    let Some(invoice) = invoice_for_payment_event(&state, ctx) else {
        ctx.add_result(json!({
            "billing": "no_invoice",
            "reason": "no invoice exists for this order; nothing to credit",
        }));
        return;
    };
    if invoice.status == "void" {
        ctx.add_result(json!({
            "billing": "skipped",
            "invoice_id": invoice.id,
            "reason": "invoice is void",
        }));
        return;
    }
    // Manual store refunds carry no minor-unit amount; the order
    // total (integer minor units, legacy major-unit float accepted)
    // is the refunded amount there.
    let refund = ctx
        .get("amount_minor")
        .and_then(|value| value.as_i64())
        .or_else(|| {
            ctx.get("order")
                .and_then(|order| order.get("total"))
                .and_then(|total| value_to_minor(total, &invoice.currency))
        });
    let Some(refund) = refund else {
        ctx.add_result(json!({
            "billing": "skipped",
            "invoice_id": invoice.id,
            "reason": "payment.refunded carries no amount",
        }));
        return;
    };
    let key = payment_event_key(ctx, "refund");
    if state.processed_payment_events.contains_key(&key) {
        ctx.add_result(json!({
            "billing": "duplicate",
            "invoice_id": invoice.id,
            "reason": "this refund was already applied",
        }));
        return;
    }
    // A refund can only return money that was paid.
    let refund = refund.min(invoice.amount_paid).max(0);
    if refund <= 0 {
        ctx.add_result(json!({
            "billing": "skipped",
            "invoice_id": invoice.id,
            "reason": "invoice has no paid amount to refund",
        }));
        return;
    }
    let reason = ctx
        .get("reason")
        .and_then(|value| value.as_str())
        .filter(|reason| !reason.trim().is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| {
            ctx.get("provider_refund_id")
                .and_then(|value| value.as_str())
                .map(|id| format!("Refund {id}"))
                .unwrap_or_else(|| "Refund".to_string())
        });
    let credit_note = CreditNote {
        id: new_id(),
        invoice_id: invoice.id.clone(),
        number: state.next_credit_note_number(),
        amount: refund,
        reason,
        created_at: now(),
    };
    state
        .credit_notes
        .insert(credit_note.id.clone(), credit_note.clone());
    let mut stored = state
        .invoices
        .get_mut(&invoice.id)
        .expect("invoice still present");
    stored.credit_total += refund;
    stored.amount_paid = (stored.amount_paid - refund).max(0);
    recompute_balance(&mut stored);
    if stored.amount_paid == 0 {
        stored.status = "credited".to_string();
    } else if invoice.status == "paid" {
        // A paid invoice that only gave some money back is neither
        // fully paid any more nor fully credited.
        stored.status = "partially_refunded".to_string();
    }
    stored.updated_at = now();
    drop(stored);
    state.processed_payment_events.insert(key, now());
    state.emit(
        "invoice.credit_note.created",
        serde_json::to_value(&credit_note).unwrap_or(Value::Null),
    );
    ctx.add_result(json!({
        "billing": "refunded",
        "invoice_id": invoice.id,
        "credit_note_id": credit_note.id,
        "amount": minor_to_major(refund, &invoice.currency),
    }));
}

/// `order.paid`: the store marks an order paid and billing settles
/// the matching invoice from it. The payload is the serialized
/// order (`id`, `total` in major units, `currency`,
/// `payment_reference`, `provider`), so it is translated into a
/// `payment.captured` context and handled by
/// `on_payment_captured`: the same invoice lookup, currency check,
/// dedupe key and overpayment recording apply. The payment
/// reference is carried as the transaction id, so a capture the
/// gateway already reported (and billing already applied) is
/// recognised as a duplicate instead of applied twice; the manual
/// mark-paid path, which emits no `payment.captured`, is the case
/// this listener exists for -- without it the invoice would stay
/// open forever. The order `status` is deliberately not copied:
/// it is `paid`, not `captured`, and would make the capture handler
/// skip the event.
pub(crate) fn on_order_paid(state: Arc<BillingState>, ctx: &mut HookContext) {
    let Some(order_id) = ctx
        .get("id")
        .and_then(|value| value.as_str())
        .map(str::to_string)
        .filter(|id| !id.trim().is_empty())
    else {
        ctx.add_result(json!({
            "billing": "skipped",
            "reason": "order.paid carries no order id",
        }));
        return;
    };
    let currency = ctx
        .get("currency")
        .and_then(|value| value.as_str())
        .unwrap_or("")
        .to_string();
    // The order total arrives as integer minor units (legacy
    // emitters sent major-unit floats, still accepted). An
    // out-of-range total cannot be recorded.
    let Some(amount_minor) = ctx
        .get("total")
        .and_then(|value| value_to_minor(value, &currency))
    else {
        ctx.add_result(json!({
            "billing": "skipped",
            "order_id": order_id,
            "reason": "order.paid carries no valid total",
        }));
        return;
    };
    let mut data = json!({
        "order_id": order_id,
        "amount_minor": amount_minor,
        "currency": currency,
    });
    if let Some(provider) = ctx.get("provider").and_then(|value| value.as_str()) {
        data["provider"] = Value::String(provider.to_string());
    }
    if let Some(reference) = ctx
        .get("payment_reference")
        .and_then(|value| value.as_str())
        .filter(|reference| !reference.trim().is_empty())
    {
        data["transaction_id"] = Value::String(reference.to_string());
        data["provider_transaction_id"] = Value::String(reference.to_string());
    }
    let mut capture_ctx = HookContext::new(PAYMENT_CAPTURED, data);
    on_payment_captured(state, &mut capture_ctx);
    for result in capture_ctx.results {
        ctx.add_result(result);
    }
}

/// Run the overdue-reminder scan when it is requested through the
/// hook bus (for example by a scheduler job emitting
/// `billing.reminders.run`).
pub(crate) fn on_reminders_run(state: Arc<BillingState>, _ctx: &mut HookContext) {
    let summary = run_reminder_scan(&state, &None);
    state.emit("billing.invoice.reminders_completed", summary);
}

/// The scheduler's daily cleanup also runs the reminder scan, so
/// reminders go out without a dedicated scheduler job kind.
pub(crate) fn on_cleanup_requested(state: Arc<BillingState>, _ctx: &mut HookContext) {
    let summary = run_reminder_scan(&state, &None);
    state.emit("billing.invoice.reminders_completed", summary);
    let subscription_summary = state.run_subscription_scan(&None);
    state.emit(
        "billing.subscriptions.scan_completed",
        subscription_summary,
    );
}

/// Run the subscription lifecycle scan when it is requested through
/// the hook bus (for example by a scheduler job emitting
/// `billing.subscriptions.scan`).
pub(crate) fn on_subscriptions_scan(state: Arc<BillingState>, _ctx: &mut HookContext) {
    let summary = state.run_subscription_scan(&None);
    state.emit("billing.subscriptions.scan_completed", summary);
}
