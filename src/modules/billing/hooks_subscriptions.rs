//! billing - hooks subscriptions.

use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::HookContext;
use unified_core::json;
use unified_core::pagination::{PageResult, PaginationParams};
use actix_web::{web, HttpRequest, HttpResponse};
use chrono::Duration;
use serde_json::{json, Value};
use std::sync::Arc;
use super::helpers::*;
use super::reference::*;
use super::school_fees_types::value_to_minor;
use super::state::*;
use super::types::*;

pub(crate) const PAYMENT_FAILED: &str = "payment.failed";

pub(crate) const SUBSCRIPTION_CREATED: &str = "subscription.created";

pub(crate) const SUBSCRIPTION_RENEWED: &str = "subscription.renewed";

pub(crate) const SUBSCRIPTION_TRIAL_ENDED: &str = "subscription.trial_ended";

pub(crate) const SUBSCRIPTION_UPGRADED: &str = "subscription.upgraded";

pub(crate) const SUBSCRIPTION_DOWNGRADED: &str = "subscription.downgraded";

pub(crate) const SUBSCRIPTION_PAST_DUE: &str = "subscription.past_due";

pub(crate) const SUBSCRIPTION_CANCELLED: &str = "subscription.cancelled";

/// Mirror a new subscription from `subscription.created` (emitted by
/// the store when a subscription order is paid).
pub(crate) fn on_subscription_created(state: Arc<BillingState>, ctx: &mut HookContext) {
    match state.sync_subscription_lifecycle(&ctx.data) {
        Some(mirror) => ctx.add_result(json!({
            "billing": "subscription_mirrored",
            "subscription_id": mirror.subscription_id,
            "status": mirror.status,
        })),
        None => ctx.add_result(json!({
            "billing": "skipped",
            "reason": "subscription.created payload lacks the fields a lifecycle mirror needs",
        })),
    }
}

/// Keep the mirror in sync with every later `subscription.*` event.
/// For `subscription.upgraded` / `subscription.downgraded` the
/// proration lines on the event are invoiced: a charge line for the
/// new plan and a credit line for the unused time on the old plan
/// (see `build_proration_invoice`). `no_prorate` changes carry no
/// lines and invoice nothing.
pub(crate) fn on_subscription_synced(state: Arc<BillingState>, ctx: &mut HookContext) {
    let Some(mirror) = state.sync_subscription_lifecycle(&ctx.data) else {
        ctx.add_result(json!({
            "billing": "skipped",
            "reason": "subscription event payload lacks the fields a lifecycle mirror needs",
        }));
        return;
    };
    if !matches!(ctx.event.as_str(), "subscription.upgraded" | "subscription.downgraded") {
        ctx.add_result(json!({
            "billing": "subscription_mirrored",
            "subscription_id": mirror.subscription_id,
            "status": mirror.status,
        }));
        return;
    }
    // A plan upgrade is the billing plan change the licensing
    // module listens for (`billing.plan.changed`): the license ids
    // are not known here, so the handler resolves them from the
    // order through the store (`store.order.licenses`). Emitted
    // before proration invoicing so a `no_prorate` upgrade — which
    // invoices nothing — still reaches licensing.
    let previous_product_id = ctx
        .get("previous_product_id")
        .and_then(|value| value.as_str())
        .unwrap_or("")
        .to_string();
    if ctx.event.as_str() == "subscription.upgraded" {
        state.emit(
            "billing.plan.changed",
            json!({
                "order_id": mirror.order_id,
                "subscription_id": mirror.subscription_id,
                "new_product": mirror.product_id,
                "previous_product_id": previous_product_id,
            }),
        );
    }
    let proration = ctx.get("proration").cloned().unwrap_or(Value::Null);
    // The store emits proration amounts as integer minor units
    // (legacy emitters sent major-unit floats, still accepted);
    // convert once at this boundary and keep minor units below.
    // The emitted `subscription.proration_invoiced` event keeps
    // the original values.
    let credit = proration
        .get("credit")
        .and_then(|value| value_to_minor(value, &mirror.currency))
        .unwrap_or(0);
    let charge = proration
        .get("charge")
        .and_then(|value| value_to_minor(value, &mirror.currency))
        .unwrap_or(0);
    if credit <= 0 && charge <= 0 {
        ctx.add_result(json!({
            "billing": "subscription_mirrored",
            "subscription_id": mirror.subscription_id,
            "status": mirror.status,
            "proration_invoice": Value::Null,
            "reason": "no proration amount to invoice",
        }));
        return;
    }
    // The proration invoice is reconciled before it is issued:
    // when validation fails it is stored as a draft instead and
    // no `subscription.proration_invoiced` event is emitted, so
    // no period or balance moves on an unreconciled invoice.
    let invoice = state.build_proration_invoice(&mirror, &previous_product_id, credit, charge);
    let (invoice, validation) = state.validate_proration_invoice(&mirror, invoice);
    let invoice_id = invoice.id.clone();
    let issued = validation.is_ok();
    state.invoices.insert(invoice.id.clone(), invoice.clone());
    state.emit(
        "invoice.created",
        serde_json::to_value(&invoice).unwrap_or(Value::Null),
    );
    if issued {
        // The event carries major units; `credit`/`charge` above
        // are minor units, converted with the mirror currency.
        let credit_major = minor_to_major(credit, &mirror.currency);
        let charge_major = minor_to_major(charge, &mirror.currency);
        state.emit(
            "subscription.proration_invoiced",
            json!({
                "subscription_id": mirror.subscription_id,
                "invoice_id": invoice_id,
                "credit": credit_major,
                "charge": charge_major,
            }),
        );
    }
    ctx.add_result(json!({
        "billing": "subscription_mirrored",
        "subscription_id": mirror.subscription_id,
        "status": mirror.status,
        "proration_invoice_id": invoice_id,
        "invoice_status": invoice.status,
        "reconciliation": if issued { Value::Null } else { json!(validation.err().map(|error| error.message)) },
    }));
}

/// Handle a failed payment (`payment.failed`) for a subscription:
/// record the dunning attempt, move the subscription to `past_due`
/// through the store, and request a notification -- on every failure
/// and, when the retries are exhausted, a final one. The
/// subscription is found by `subscription_id` when the event names
/// this commercial subscription, otherwise by `order_id` (the
/// payments module's failure carries its own gateway subscription
/// id, which billing cannot resolve directly). Cancellation itself
/// waits for the grace period to expire in the scan; dunning only
/// stops retrying once the attempts are exhausted.
pub(crate) fn on_payment_failed(state: Arc<BillingState>, ctx: &mut HookContext) {
    let subscription_id = ctx
        .get("subscription_id")
        .and_then(|value| value.as_str())
        .map(str::to_string)
        .or_else(|| {
            ctx.get("order_id")
                .and_then(|value| value.as_str())
                .and_then(|order_id| {
                    state
                        .subscription_lifecycles
                        .iter()
                        .find(|entry| entry.value().order_id == order_id)
                        .map(|entry| entry.key().clone())
                })
        });
    let Some(subscription_id) = subscription_id else {
        ctx.add_result(json!({
            "billing": "skipped",
            "reason": "payment.failed names no subscription or order billing mirrors",
        }));
        return;
    };
    let Some(mirror) = state
        .subscription_lifecycles
        .get(&subscription_id)
        .map(|entry| entry.value().clone())
    else {
        ctx.add_result(json!({
            "billing": "skipped",
            "subscription_id": subscription_id,
            "reason": "no mirrored subscription for this payment failure",
        }));
        return;
    };
    if mirror.status == "cancelled" {
        ctx.add_result(json!({
            "billing": "skipped",
            "subscription_id": subscription_id,
            "reason": "subscription is cancelled",
        }));
        return;
    }
    let reason = ctx
        .get("reason")
        .and_then(|value| value.as_str())
        .unwrap_or("payment failed")
        .to_string();
    let attempt_number = mirror.dunning_attempts + 1;
    let attempt = DunningAttempt {
        id: new_id(),
        subscription_id: subscription_id.clone(),
        attempt_number,
        reason: reason.clone(),
        amount_minor: ctx.get("amount_minor").and_then(|value| value.as_i64()),
        created_at: now(),
    };
    state
        .dunning_attempts
        .insert(attempt.id.clone(), attempt.clone());
    let next_retry_at = now() + Duration::days(i64::from(mirror.dunning_retry_interval_days));
    if let Some(mut stored) = state.subscription_lifecycles.get_mut(&subscription_id) {
        stored.dunning_attempts = attempt_number;
        stored.last_failure_at = Some(now());
        stored.next_retry_at = Some(next_retry_at);
        stored.updated_at = now();
    }
    state.emit(
        "subscription.lifecycle.apply",
        json!({
            "subscription_id": subscription_id,
            "action": "mark_past_due",
            "reason": reason,
        }),
    );
    state.emit(
        "subscription.payment_failed",
        json!({
            "subscription_id": subscription_id,
            "attempt_number": attempt_number,
            "reason": reason,
        }),
    );
    let exhausted = attempt_number >= mirror.dunning_max_retries;
    if exhausted {
        state.request_notification(
            &mirror.user_id,
            "Subscription payment failed — no retries left",
            &format!(
                "The renewal payment for your subscription (product {}) failed: {reason}. All {} retry attempt(s) are exhausted; the subscription will be cancelled when its grace period ends on {}. Please update your payment method.",
                mirror.product_id,
                mirror.dunning_max_retries,
                (mirror.current_period_end
                    + Duration::days(i64::from(mirror.grace_period_days)))
                .to_rfc3339()
            ),
            "subscription.payment_failed",
        );
    } else {
        state.request_notification(
            &mirror.user_id,
            "Subscription payment failed",
            &format!(
                "The renewal payment for your subscription (product {}) failed: {reason}. Retry attempt {attempt_number} of {} is scheduled for {}. Please update your payment method if the failure was yours.",
                mirror.product_id,
                mirror.dunning_max_retries,
                next_retry_at.to_rfc3339()
            ),
            "subscription.payment_failed",
        );
    }
    ctx.add_result(json!({
        "billing": "dunning_recorded",
        "subscription_id": subscription_id,
        "attempt_number": attempt_number,
        "dunning_exhausted": exhausted,
    }));
}

/// Fulfil `billing.invoice.requested` (admissions registration
/// fees, completed appointments): create one invoice for the
/// requested amount. The requester is identified by the reference
/// key it carries -- `application_id` (admissions) or
/// `appointment_id` (appointments) -- which becomes the invoice's
/// `order_id`, so `invoice.paid` links the payment back to the
/// requester (admissions matches on `invoice.order_id`). The
/// invoice is created as a `draft`, never `issued`: issuing runs
/// pricing reconciliation in `update_invoice`, and a hook payload
/// carries no tenant context to validate against. The caller's
/// own `reference` string (for example `admissions:<id>`) is not
/// a shared-format reference, so it is kept in the notes instead
/// of the invoice `reference`. A repeat request for the same key
/// returns the existing invoice instead of duplicating it.
pub(crate) fn on_invoice_requested(state: Arc<BillingState>, ctx: &mut HookContext) {
    let reference_key = ctx
        .get("application_id")
        .and_then(|value| value.as_str())
        .map(str::to_string)
        .or_else(|| {
            ctx.get("appointment_id")
                .and_then(|value| value.as_str())
                .map(str::to_string)
        });
    let Some(reference_key) = reference_key.filter(|key| !key.trim().is_empty()) else {
        ctx.add_result(json!({
            "billing": "skipped",
            "reason": "billing.invoice.requested carries no application_id or appointment_id",
        }));
        return;
    };
    if let Some(existing) = state
        .invoices
        .iter()
        .find(|entry| entry.value().order_id.as_deref() == Some(reference_key.as_str()))
        .map(|entry| entry.value().clone())
    {
        ctx.add_result(json!({
            "billing": "invoice_already_created",
            "invoice_id": existing.id,
            "order_id": reference_key,
            "status": existing.status,
        }));
        return;
    }
    let Some(user_id) = ctx
        .get("customer_user_id")
        .and_then(|value| value.as_str())
        .map(str::to_string)
        .filter(|id| !id.trim().is_empty())
    else {
        ctx.add_result(json!({
            "billing": "skipped",
            "order_id": reference_key,
            "reason": "billing.invoice.requested carries no customer_user_id",
        }));
        return;
    };
    let currency = ctx
        .get("currency")
        .and_then(|value| value.as_str())
        .unwrap_or("USD")
        .to_string();
    if currency.len() != 3 || !currency.chars().all(|c| c.is_ascii_alphabetic()) {
        ctx.add_result(json!({
            "billing": "skipped",
            "order_id": reference_key,
            "reason": "billing.invoice.requested currency must be a 3-letter code",
        }));
        return;
    }
    // The amount arrives as integer minor units (legacy emitters
    // sent major-unit floats, still accepted). An out-of-range
    // amount is not a number this handler can use.
    let Some(amount_minor) = ctx
        .get("amount")
        .and_then(|value| value_to_minor(value, &currency))
    else {
        ctx.add_result(json!({
            "billing": "skipped",
            "order_id": reference_key,
            "reason": "billing.invoice.requested carries no numeric amount",
        }));
        return;
    };
    if amount_minor < 0 {
        ctx.add_result(json!({
            "billing": "skipped",
            "order_id": reference_key,
            "reason": "billing.invoice.requested amount must be a non-negative number",
        }));
        return;
    }
    let description = ctx
        .get("description")
        .and_then(|value| value.as_str())
        .filter(|text| !text.trim().is_empty())
        .unwrap_or("Requested invoice")
        .to_string();
    let caller_reference = ctx
        .get("reference")
        .and_then(|value| value.as_str())
        .map(str::to_string);
    let settings = state.settings();
    let (net, tax) = line_tax(
        amount_minor,
        1,
        settings.default_tax_rate,
        settings.tax_inclusive,
    );
    let line_total = if settings.tax_inclusive {
        net + tax
    } else {
        net
    };
    let total = net + tax;
    let created = now();
    let invoice = Invoice {
        id: new_id(),
        number: state.next_invoice_number(),
        reference: fresh_invoice_reference(&state),
        order_id: Some(reference_key.clone()),
        subscription_id: None,
        user_id,
        company_id: None,
        items: vec![InvoiceItem {
            name: description,
            quantity: 1,
            unit_price: amount_minor,
            line_total,
            tax_rate: settings.default_tax_rate,
            tax_amount: tax,
        }],
        subtotal: net,
        tax_total: tax,
        total,
        currency: currency.to_uppercase(),
        status: "draft".to_string(),
        overdue: false,
        tax_inclusive: settings.tax_inclusive,
        amount_paid: 0,
        credit_total: 0,
        balance: total,
        issue_date: Some(created),
        due_date: Some(created + Duration::days(i64::from(settings.payment_terms_days))),
        payment_terms_days: Some(settings.payment_terms_days),
        paid_at: None,
        notes: caller_reference,
        reminders_sent: Vec::new(),
        created_at: created,
        updated_at: created,
    };
    state.invoices.insert(invoice.id.clone(), invoice.clone());
    state.emit(
        "invoice.created",
        serde_json::to_value(&invoice).unwrap_or(Value::Null),
    );
    ctx.add_result(json!({
        "billing": "invoice_created",
        "invoice_id": invoice.id,
        "order_id": reference_key,
        "status": "draft",
        "total": minor_to_major(invoice.total, &invoice.currency),
        "currency": invoice.currency,
    }));
}

/// Pay out an approved expense reimbursement
/// (`expenses.reimbursement.requested`
/// `{reimbursement_id, report_id, employee_id, amount_minor,
/// currency, payable_reference}`): record the disbursement in
/// billing's ledger and confirm it with
/// `expenses.reimbursement.paid` `{reimbursement_id, paid_at}`
/// plus the generic `payment.captured` carrying the same
/// `reimbursement_id`, so both of the expenses module's outcome
/// listeners see it. A repeat request for an already-paid
/// reimbursement answers `already_paid` without recording a
/// second disbursement; a failed one may be re-requested (the
/// expenses retry re-emits this event). An invalid payload is
/// answered with `expenses.reimbursement.failed`
/// `{reimbursement_id, reason}` and the report stays `approved`.
pub(crate) fn on_reimbursement_requested(state: Arc<BillingState>, ctx: &mut HookContext) {
    let fail = |state: &Arc<BillingState>,
                ctx: &mut HookContext,
                reimbursement_id: Option<String>,
                reason: &str| {
        if let Some(reimbursement_id) = &reimbursement_id {
            state.emit(
                "expenses.reimbursement.failed",
                json!({
                    "reimbursement_id": reimbursement_id,
                    "reason": reason,
                }),
            );
        }
        ctx.add_result(json!({
            "billing": "reimbursement_failed",
            "reimbursement_id": reimbursement_id,
            "reason": reason,
        }));
    };
    let Some(reimbursement_id) = ctx
        .get("reimbursement_id")
        .and_then(|value| value.as_str())
        .map(str::to_string)
        .filter(|id| !id.trim().is_empty())
    else {
        ctx.add_result(json!({
            "billing": "skipped",
            "reason": "expenses.reimbursement.requested carries no reimbursement_id",
        }));
        return;
    };
    if let Some(existing) = state
        .reimbursement_payouts
        .get(&reimbursement_id)
        .map(|entry| entry.value().clone())
    {
        if existing.status == "paid" {
            ctx.add_result(json!({
                "billing": "reimbursement_already_paid",
                "reimbursement_id": reimbursement_id,
                "paid_at": existing.paid_at,
            }));
            return;
        }
    }
    let amount_minor = ctx
        .get("amount_minor")
        .and_then(|value| value.as_i64());
    let Some(amount_minor) = amount_minor.filter(|amount| *amount > 0) else {
        fail(
            &state,
            ctx,
            Some(reimbursement_id),
            "Reimbursement amount_minor must be a positive integer",
        );
        return;
    };
    let currency = ctx
        .get("currency")
        .and_then(|value| value.as_str())
        .unwrap_or("")
        .to_string();
    if currency.len() != 3 || !currency.chars().all(|c| c.is_ascii_alphabetic()) {
        fail(
            &state,
            ctx,
            Some(reimbursement_id),
            "Reimbursement currency must be a 3-letter code",
        );
        return;
    }
    let report_id = ctx
        .get("report_id")
        .and_then(|value| value.as_str())
        .map(str::to_string);
    let employee_id = ctx
        .get("employee_id")
        .and_then(|value| value.as_str())
        .map(str::to_string);
    let paid_at = now();
    let attempts = state
        .reimbursement_payouts
        .get(&reimbursement_id)
        .map(|entry| entry.value().attempts + 1)
        .unwrap_or(1);
    let payout = ReimbursementPayout {
        id: reimbursement_id.clone(),
        report_id: report_id.clone(),
        employee_id: employee_id.clone(),
        amount_minor,
        currency: currency.to_uppercase(),
        status: "paid".to_string(),
        failure_reason: None,
        paid_at: Some(paid_at),
        attempts,
        created_at: state
            .reimbursement_payouts
            .get(&reimbursement_id)
            .map(|entry| entry.value().created_at)
            .unwrap_or(paid_at),
        updated_at: paid_at,
    };
    state
        .reimbursement_payouts
        .insert(reimbursement_id.clone(), payout);
    state.emit(
        "expenses.reimbursement.paid",
        json!({
            "reimbursement_id": reimbursement_id,
            "paid_at": paid_at.to_rfc3339(),
        }),
    );
    state.emit(
        "payment.captured",
        json!({
            "reimbursement_id": reimbursement_id,
            "report_id": report_id,
            "employee_id": employee_id,
            "amount_minor": amount_minor,
            "currency": currency.to_uppercase(),
            "status": "captured",
        }),
    );
    ctx.add_result(json!({
        "billing": "reimbursement_paid",
        "reimbursement_id": reimbursement_id,
        "report_id": report_id,
        "amount_minor": amount_minor,
        "currency": currency.to_uppercase(),
        "paid_at": paid_at.to_rfc3339(),
    }));
}

/// Record a digital-product royalty payout request
/// (`store.digital.payout.requested`, the serialized
/// `DigitalPayoutRequest`): billing holds the obligation in its
/// ledger so finance can see it. No money moves on request -- the
/// payout is disbursed on `store.digital.payout.approved`.
pub(crate) fn on_digital_payout_requested(state: Arc<BillingState>, ctx: &mut HookContext) {
    let Some(payout_id) = ctx
        .get("id")
        .and_then(|value| value.as_str())
        .map(str::to_string)
        .filter(|id| !id.trim().is_empty())
    else {
        ctx.add_result(json!({
            "billing": "skipped",
            "reason": "store.digital.payout.requested carries no payout id",
        }));
        return;
    };
    if state.digital_payouts.contains_key(&payout_id) {
        ctx.add_result(json!({
            "billing": "digital_payout_already_recorded",
            "payout_id": payout_id,
        }));
        return;
    }
    let currency = ctx
        .get("currency")
        .and_then(|value| value.as_str())
        .unwrap_or("")
        .to_string();
    if currency.len() != 3 || !currency.chars().all(|c| c.is_ascii_alphabetic()) {
        ctx.add_result(json!({
            "billing": "skipped",
            "payout_id": payout_id,
            "reason": "store.digital.payout.requested currency must be a 3-letter code",
        }));
        return;
    }
    // The store emits the payout amount as integer minor units
    // (the serialized `DigitalPayoutRequest`); legacy emitters sent
    // major-unit floats, still accepted. Convert at this boundary.
    let Some(amount_minor) = ctx
        .get("amount")
        .and_then(|value| value_to_minor(value, &currency))
        .filter(|value| *value > 0)
    else {
        ctx.add_result(json!({
            "billing": "skipped",
            "payout_id": payout_id,
            "reason": "store.digital.payout.requested carries no positive amount",
        }));
        return;
    };
    let payout = BillingDigitalPayout {
        id: payout_id.clone(),
        payee: ctx
            .get("payee")
            .and_then(|value| value.as_str())
            .map(str::to_string),
        amount: amount_minor,
        currency: currency.to_uppercase(),
        royalty_ids: ctx
            .get("royalty_ids")
            .and_then(|value| value.as_array())
            .map(|items| {
                items
                    .iter()
                    .filter_map(|item| item.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default(),
        status: "requested".to_string(),
        paid_at: None,
        created_at: now(),
        updated_at: now(),
    };
    state.digital_payouts.insert(payout_id.clone(), payout);
    state.emit(
        "billing.digital.payout.recorded",
        json!({
            "payout_id": payout_id,
            "amount": minor_to_major(amount_minor, &currency),
            "currency": currency.to_uppercase(),
            "status": "requested",
        }),
    );
    ctx.add_result(json!({
        "billing": "digital_payout_recorded",
        "payout_id": payout_id,
        "status": "requested",
    }));
}

/// Disburse an approved digital-product royalty payout
/// (`store.digital.payout.approved`, the serialized payout):
/// record the disbursement in billing's ledger and emit
/// `billing.digital.payout.paid`. The store's mark-paid endpoint
/// stays the step that flips the store-side record to `paid`.
/// A payout billing never saw requested is recorded directly
/// from the approval payload instead of being dropped.
pub(crate) fn on_digital_payout_approved(state: Arc<BillingState>, ctx: &mut HookContext) {
    let Some(payout_id) = ctx
        .get("id")
        .and_then(|value| value.as_str())
        .map(str::to_string)
        .filter(|id| !id.trim().is_empty())
    else {
        ctx.add_result(json!({
            "billing": "skipped",
            "reason": "store.digital.payout.approved carries no payout id",
        }));
        return;
    };
    if let Some(existing) = state
        .digital_payouts
        .get(&payout_id)
        .map(|entry| entry.value().clone())
    {
        if existing.status == "paid" {
            ctx.add_result(json!({
                "billing": "digital_payout_already_paid",
                "payout_id": payout_id,
                "paid_at": existing.paid_at,
            }));
            return;
        }
    }
    let currency = ctx
        .get("currency")
        .and_then(|value| value.as_str())
        .map(str::to_string)
        .or_else(|| {
            state
                .digital_payouts
                .get(&payout_id)
                .map(|entry| entry.value().currency.clone())
        })
        .unwrap_or_else(|| "USD".to_string());
    // The approval payload carries the amount as integer minor
    // units (legacy: major-unit float); a stored payout already
    // carries integer minor units.
    let amount_minor = ctx
        .get("amount")
        .and_then(|value| value_to_minor(value, &currency))
        .filter(|value| *value > 0)
        .or_else(|| {
            state
                .digital_payouts
                .get(&payout_id)
                .map(|entry| entry.value().amount)
        });
    let Some(amount_minor) = amount_minor else {
        ctx.add_result(json!({
            "billing": "skipped",
            "payout_id": payout_id,
            "reason": "store.digital.payout.approved carries no positive amount",
        }));
        return;
    };
    let paid_at = now();
    let payout = BillingDigitalPayout {
        id: payout_id.clone(),
        payee: ctx
            .get("payee")
            .and_then(|value| value.as_str())
            .map(str::to_string)
            .or_else(|| {
                state
                    .digital_payouts
                    .get(&payout_id)
                    .and_then(|entry| entry.value().payee.clone())
            }),
        amount: amount_minor,
        currency: currency.to_uppercase(),
        royalty_ids: ctx
            .get("royalty_ids")
            .and_then(|value| value.as_array())
            .map(|items| {
                items
                    .iter()
                    .filter_map(|item| item.as_str().map(str::to_string))
                    .collect()
            })
            .or_else(|| {
                state
                    .digital_payouts
                    .get(&payout_id)
                    .map(|entry| entry.value().royalty_ids.clone())
            })
            .unwrap_or_default(),
        status: "paid".to_string(),
        paid_at: Some(paid_at),
        created_at: state
            .digital_payouts
            .get(&payout_id)
            .map(|entry| entry.value().created_at)
            .unwrap_or(paid_at),
        updated_at: paid_at,
    };
    state.digital_payouts.insert(payout_id.clone(), payout);
    state.emit(
        "billing.digital.payout.paid",
        json!({
            "payout_id": payout_id,
            "amount": minor_to_major(amount_minor, &currency),
            "currency": currency.to_uppercase(),
            "paid_at": paid_at.to_rfc3339(),
        }),
    );
    ctx.add_result(json!({
        "billing": "digital_payout_paid",
        "payout_id": payout_id,
        "amount": minor_to_major(amount_minor, &currency),
        "currency": currency.to_uppercase(),
        "paid_at": paid_at.to_rfc3339(),
    }));
}

/// Renew a subscription on `billing.subscription.renew_requested`
/// (SaaS packages): issue the renewal invoice for the current
/// period and request its charge, exactly as the lifecycle scan
/// does. A renewal invoice already issued for this period end is
/// not duplicated. When reconciliation does not pass, the invoice
/// is held as a draft and no charge is requested.
pub(crate) fn on_subscription_renew_requested(state: Arc<BillingState>, ctx: &mut HookContext) {
    let Some(subscription_id) = ctx
        .get("subscription_id")
        .and_then(|value| value.as_str())
        .map(str::to_string)
        .filter(|id| !id.trim().is_empty())
    else {
        ctx.add_result(json!({
            "billing": "skipped",
            "reason": "billing.subscription.renew_requested carries no subscription_id",
        }));
        return;
    };
    let Some(mirror) = state
        .subscription_lifecycles
        .get(&subscription_id)
        .map(|entry| entry.value().clone())
    else {
        ctx.add_result(json!({
            "billing": "skipped",
            "subscription_id": subscription_id,
            "reason": "no subscription mirror matches this subscription_id",
        }));
        return;
    };
    if mirror.status == "cancelled" {
        ctx.add_result(json!({
            "billing": "skipped",
            "subscription_id": subscription_id,
            "reason": "subscription is cancelled",
        }));
        return;
    }
    if mirror.renewal_invoice_for_period_end == Some(mirror.current_period_end) {
        ctx.add_result(json!({
            "billing": "renewal_invoice_already_issued",
            "subscription_id": subscription_id,
            "invoice_id": mirror.renewal_invoice_id,
        }));
        return;
    }
    let (invoice, validation) = state.build_validated_renewal_invoice(&mirror);
    let invoice_id = invoice.id.clone();
    let issued = validation.is_ok();
    state.invoices.insert(invoice.id.clone(), invoice.clone());
    state.emit(
        "invoice.created",
        serde_json::to_value(&invoice).unwrap_or(Value::Null),
    );
    if issued {
        if let Some(mut stored) = state.subscription_lifecycles.get_mut(&subscription_id) {
            stored.renewal_invoice_id = Some(invoice_id.clone());
            stored.renewal_invoice_for_period_end = Some(mirror.current_period_end);
            stored.updated_at = now();
        }
        state.request_renewal_charge(&mirror, &invoice_id, false);
    }
    ctx.add_result(json!({
        "billing": if issued { "renewal_invoice_created" } else { "renewal_invoice_held_draft" },
        "subscription_id": subscription_id,
        "invoice_id": invoice_id,
        "invoice_status": invoice.status,
        "reason": if issued { Value::Null } else { json!(validation.err().map(|error| error.message)) },
    }));
}

/// Cancel a subscription on `billing.subscription.cancel_requested`
/// (SaaS packages): billing never drives a status change on its
/// mirror directly -- the decision goes out as
/// `subscription.lifecycle.apply` with action `cancel`, the store
/// applies it, and the mirror follows on `subscription.cancelled`.
pub(crate) fn on_subscription_cancel_requested(state: Arc<BillingState>, ctx: &mut HookContext) {
    let Some(subscription_id) = ctx
        .get("subscription_id")
        .and_then(|value| value.as_str())
        .map(str::to_string)
        .filter(|id| !id.trim().is_empty())
    else {
        ctx.add_result(json!({
            "billing": "skipped",
            "reason": "billing.subscription.cancel_requested carries no subscription_id",
        }));
        return;
    };
    let Some(mirror) = state
        .subscription_lifecycles
        .get(&subscription_id)
        .map(|entry| entry.value().clone())
    else {
        ctx.add_result(json!({
            "billing": "skipped",
            "subscription_id": subscription_id,
            "reason": "no subscription mirror matches this subscription_id",
        }));
        return;
    };
    if mirror.status == "cancelled" {
        ctx.add_result(json!({
            "billing": "subscription_already_cancelled",
            "subscription_id": subscription_id,
        }));
        return;
    }
    let reason = ctx
        .get("reason")
        .and_then(|value| value.as_str())
        .unwrap_or("cancelled by package purchase")
        .to_string();
    state.emit(
        "subscription.lifecycle.apply",
        json!({
            "subscription_id": subscription_id,
            "action": "cancel",
            "reason": reason,
        }),
    );
    ctx.add_result(json!({
        "billing": "subscription_cancel_requested",
        "subscription_id": subscription_id,
        "reason": reason,
    }));
}

pub(crate) async fn list_subscriptions(
    state: web::Data<Arc<BillingState>>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let _tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return json::error_response(&error, false),
    };
    let _scope = match caller_company(&_tenant, &req) {
        Ok(scope) => scope,
        Err(error) => return json::error_response(&error, false),
    };
    let params = query.into_inner();
    let mut items: Vec<SubscriptionLifecycle> = state
        .subscription_lifecycles
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|mirror| {
            in_company_scope(&mirror.company_id, &_scope)
                && matches_search(
                &format!(
                    "{} {} {}",
                    mirror.subscription_id, mirror.user_id, mirror.product_id
                ),
                &params,
            ) && params
                .filters
                .get("status")
                .is_none_or(|status| mirror.status == *status)
                && params
                    .filters
                    .get("user_id")
                    .is_none_or(|user_id| mirror.user_id == *user_id)
                && params
                    .filters
                    .get("product_id")
                    .is_none_or(|product_id| mirror.product_id == *product_id)
                && params.filters.get("module_id").is_none_or(|module_id| {
                    mirror.module_id.as_deref() == Some(module_id.as_str())
                })
        })
        .collect();
    items.sort_by(|a, b| a.created_at.cmp(&b.created_at));
    if params.normalized().order == "desc" {
        items.reverse();
    }
    let page = paginate(items, &params);
    json::success_response(page, 200)
}

pub(crate) async fn get_subscription(
    state: web::Data<Arc<BillingState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let _tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return json::error_response(&error, false),
    };
    let _scope = match caller_company(&_tenant, &req) {
        Ok(scope) => scope,
        Err(error) => return json::error_response(&error, false),
    };
    let result: UnifiedResult<SubscriptionLifecycle> = state
        .subscription_lifecycles
        .get(path.as_str())
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found("Subscription not found"))
        .and_then(|mirror| {
            require_record_in_company_scope(&mirror.company_id, &_scope)?;
            Ok(mirror)
        });
    json::respond(result, false)
}

pub(crate) async fn list_dunning_attempts(
    state: web::Data<Arc<BillingState>>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let _tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return json::error_response(&error, false),
    };
    let _scope = match caller_company(&_tenant, &req) {
        Ok(scope) => scope,
        Err(error) => return json::error_response(&error, false),
    };
    let params = query.into_inner();
    let mirror = match state.subscription_lifecycles.get(path.as_str()) {
        Some(entry) => entry.value().clone(),
        None => {
            return json::respond(
                UnifiedResult::<PageResult<DunningAttempt>>::Err(UnifiedError::not_found(
                    "Subscription not found",
                )),
                false,
            );
        }
    };
    if let Err(error) = require_record_in_company_scope(&mirror.company_id, &_scope) {
        return json::respond(
            UnifiedResult::<PageResult<DunningAttempt>>::Err(error),
            false,
        );
    }
    let mut items: Vec<DunningAttempt> = state
        .dunning_attempts
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|attempt| attempt.subscription_id == path.as_str())
        .collect();
    items.sort_by(|a, b| a.attempt_number.cmp(&b.attempt_number));
    let page = paginate(items, &params);
    json::success_response(page, 200)
}

pub(crate) async fn run_subscription_scan_endpoint(
    state: web::Data<Arc<BillingState>>,
    req: HttpRequest,
) -> HttpResponse {
    let _tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return json::error_response(&error, false),
    };
    let _scope = match caller_company(&_tenant, &req) {
        Ok(scope) => scope,
        Err(error) => return json::error_response(&error, false),
    };
    let summary = state.run_subscription_scan(&_scope);
    state.emit("billing.subscriptions.scan_completed", summary.clone());
    json::success_response(summary, 200)
}
