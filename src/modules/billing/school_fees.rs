//! billing - school fees.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::HookContext;
use actix_web::HttpRequest;
use chrono::{DateTime, Duration, Utc};
use serde_json::{json, Value};
use std::sync::Arc;
use super::types::*;
use super::state::*;
use super::reference::*;
use super::helpers::*;
use super::school_fees_types::*;

/// Caller identity: the server-verified caller (user id plus
/// server-resolved role, both required for capability checks),
/// exactly like the e-learning module.
pub(crate) struct FeeCaller {
    pub(crate) actor: String,
    role: String,
}

pub(crate) fn fee_caller(req: &HttpRequest) -> UnifiedResult<FeeCaller> {
    let verified = unified_core::caller::require_caller(req)?;
    let actor = verified.user_id.clone();
    let role = verified.role().to_string();
    Ok(FeeCaller { actor, role })
}

/// Ask the roles module whether `role` holds `permission` through
/// the `role.check` hook. Fails closed: no answer means not allowed
/// (same fail-closed shape as the e-learning module).
pub(crate) fn fee_check_capability(ctx: &AppContext, role: &str, permission: &str) -> bool {
    let context = ctx.hooks.emit(
        "role.check",
        json!({ "role": role, "permission": permission, "allowed": false }),
    );
    context
        .get("allowed")
        .and_then(|value| value.as_bool())
        .unwrap_or(false)
}

pub(crate) fn fee_require_permission(
    ctx: &AppContext,
    role: &str,
    permission: &str,
) -> UnifiedResult<()> {
    if fee_check_capability(ctx, role, permission) {
        Ok(())
    } else {
        Err(UnifiedError::forbidden(format!(
            "Role \"{role}\" does not hold permission \"{permission}\""
        )))
    }
}

pub(crate) fn fee_require_view(ctx: &AppContext, caller: &FeeCaller) -> UnifiedResult<()> {
    fee_require_permission(ctx, &caller.role, PERM_SCHOOL_FEES_VIEW)
}

pub(crate) fn fee_require_manage(ctx: &AppContext, caller: &FeeCaller) -> UnifiedResult<()> {
    fee_require_permission(ctx, &caller.role, PERM_SCHOOL_FEES_MANAGE)
}

pub(crate) fn fee_is_manager(ctx: &AppContext, caller: &FeeCaller) -> bool {
    fee_check_capability(ctx, &caller.role, PERM_SCHOOL_FEES_MANAGE)
}

/// Student ids the caller may see: the caller's own id plus the
/// students linked to them as a guardian. Managers bypass this
/// and see everything.
pub(crate) fn fee_visible_student_ids(state: &Arc<BillingState>, caller: &FeeCaller) -> Vec<String> {
    let mut ids = vec![caller.actor.clone()];
    for entry in state.fee_guardians.iter() {
        let link = entry.value();
        if link.active
            && link.guardian_user_id == caller.actor
            && !ids.contains(&link.student_user_id)
        {
            ids.push(link.student_user_id.clone());
        }
    }
    ids
}

pub(crate) fn fee_can_view_student(
    ctx: &AppContext,
    state: &Arc<BillingState>,
    caller: &FeeCaller,
    student_user_id: &str,
) -> bool {
    fee_is_manager(ctx, caller)
        || fee_visible_student_ids(state, caller)
            .iter()
            .any(|id| id == student_user_id)
}

pub(crate) fn fee_require_charge_access(
    ctx: &AppContext,
    state: &Arc<BillingState>,
    caller: &FeeCaller,
    charge: &FeeCharge,
) -> UnifiedResult<()> {
    if fee_can_view_student(ctx, state, caller, &charge.student_user_id) {
        Ok(())
    } else {
        Err(UnifiedError::forbidden(
            "Fee charge belongs to a different student",
        ))
    }
}

pub(crate) fn fee_require_text(value: &str, field: &str, max: usize) -> UnifiedResult<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err(UnifiedError::bad_request(format!(
            "{field} must not be empty"
        )));
    }
    if trimmed.len() > max {
        return Err(UnifiedError::bad_request(format!(
            "{field} must be at most {max} characters"
        )));
    }
    Ok(trimmed.to_string())
}

pub(crate) fn fee_optional_text(
    value: &Option<String>,
    field: &str,
    max: usize,
) -> UnifiedResult<Option<String>> {
    match value {
        Some(raw) => Ok(Some(fee_require_text(raw, field, max)?)),
        None => Ok(None),
    }
}

pub(crate) fn fee_require_currency(value: &str) -> UnifiedResult<String> {
    let code = fee_require_text(value, "currency", 3)?;
    if code.len() != 3 || !code.chars().all(|c| c.is_ascii_alphabetic()) {
        return Err(UnifiedError::bad_request(
            "currency must be a 3-letter ISO code",
        ));
    }
    Ok(code.to_ascii_uppercase())
}

/// True when a charge is past its due date with a balance still
/// outstanding. Paid and void charges are never overdue.
pub(crate) fn fee_charge_is_overdue(charge: &FeeCharge, at: DateTime<Utc>) -> bool {
    matches!(charge.status.as_str(), "open" | "partially_paid")
        && charge.total_minor - charge.amount_paid_minor > 0
        && charge.due_date < at
}

/// The status callers see: an unpaid charge past its due date reads
/// as `overdue`. The stored status is left untouched.
pub(crate) fn fee_charge_effective_status(charge: &FeeCharge, at: DateTime<Utc>) -> String {
    if fee_charge_is_overdue(charge, at) {
        "overdue".to_string()
    } else {
        charge.status.clone()
    }
}

/// A copy of a charge with the derived `overdue` flag and effective
/// status filled in. All read endpoints return this view.
pub(crate) fn view_fee_charge(charge: &FeeCharge) -> FeeCharge {
    let mut view = charge.clone();
    view.overdue = fee_charge_is_overdue(charge, now());
    if view.overdue {
        view.status = "overdue".to_string();
    }
    view
}

/// Days a charge is overdue (zero when it is not yet due).
pub(crate) fn fee_days_overdue(charge: &FeeCharge, at: DateTime<Utc>) -> i64 {
    if charge.due_date >= at {
        0
    } else {
        (at.date_naive() - charge.due_date.date_naive()).num_days()
    }
}

/// Aging bucket label for a number of days overdue, from the
/// configured cut-offs. Cut-offs `[30, 60, 90]` give `1-30`,
/// `31-60`, `61-90` and `over_90`.
pub(crate) fn fee_aging_bucket(cutoffs: &[i32], days_overdue: i64) -> String {
    let mut lower = 0i64;
    for &cutoff in cutoffs {
        if days_overdue <= i64::from(cutoff) {
            return format!("{}-{}", lower + 1, cutoff);
        }
        lower = i64::from(cutoff);
    }
    let last = cutoffs.last().copied().unwrap_or(0);
    format!("over_{last}")
}

/// Discounts that may apply to a new charge for this student and
/// fee type right now: active, approved and inside their validity
/// period.
pub(crate) fn fee_applicable_discounts(
    state: &Arc<BillingState>,
    student_user_id: &str,
    fee_type_id: &str,
    at: DateTime<Utc>,
) -> Vec<FeeDiscount> {
    state
        .fee_discounts
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|discount| {
            discount.student_user_id == student_user_id
                && discount.status == "active"
                && discount.approved
                && discount
                    .fee_type_id
                    .as_deref()
                    .is_none_or(|id| id == fee_type_id)
                && discount.valid_from.is_none_or(|from| from <= at)
                && discount.valid_to.is_none_or(|to| to >= at)
        })
        .collect()
}

/// Discount amount in minor units for one gross amount. Percentage
/// discounts use basis points; fixed discounts are capped at the
/// gross amount so a charge can never go negative.
pub(crate) fn fee_discount_amount(gross_minor: i64, discount: &FeeDiscount) -> i64 {
    match discount.kind.as_str() {
        "percentage" => {
            (gross_minor * discount.percent_basis_points.clamp(0, 10_000)) / 10_000
        }
        _ => discount.amount_minor.max(0).min(gross_minor),
    }
}

/// Reminder offsets that fall due today for one charge, in days
/// relative to the due date (positive = days before due, negative =
/// days after due). Mirrors the invoice reminder schedule.
pub(crate) fn fee_reminders_due(
    charge: &FeeCharge,
    settings: &SchoolFeeSettings,
    at: DateTime<Utc>,
) -> Vec<i32> {
    if charge.total_minor - charge.amount_paid_minor <= 0
        || matches!(charge.status.as_str(), "paid" | "void")
    {
        return Vec::new();
    }
    let today = at.date_naive();
    let due_day = charge.due_date.date_naive();
    let mut due_offsets: Vec<i32> = Vec::new();
    for &days in &settings.reminder_before_days {
        if days > 0 && today == due_day - Duration::days(i64::from(days)) {
            due_offsets.push(days);
        }
    }
    for &days in &settings.reminder_after_days {
        if days > 0 && today == due_day + Duration::days(i64::from(days)) {
            due_offsets.push(-days);
        }
    }
    due_offsets
}

/// Recipients for a fee reminder: the student plus every active
/// linked guardian.
pub(crate) fn fee_reminder_recipients(state: &Arc<BillingState>, charge: &FeeCharge) -> Vec<String> {
    let mut recipients = vec![charge.student_user_id.clone()];
    for entry in state.fee_guardians.iter() {
        let link = entry.value();
        if link.active
            && link.student_user_id == charge.student_user_id
            && !recipients.contains(&link.guardian_user_id)
        {
            recipients.push(link.guardian_user_id.clone());
        }
    }
    recipients
}

/// Build and store the billing invoice for a fee charge. The
/// invoice is the payment record: every payment against the charge
/// is a payment against this invoice. The invoice carries the
/// charge's tenant / company id and is routed through pricing
/// reconciliation exactly like a renewal invoice: on success it is
/// returned `issued`; on any validation failure -- no pricing
/// answer, a mismatch, or no tenant id on the charge -- it is
/// returned as a `draft` with the reason in its notes, so no charge
/// may be requested for it until it is issued manually (which
/// validates with the request's tenant context).
pub(crate) fn build_fee_invoice(
    state: &Arc<BillingState>,
    charge: &FeeCharge,
    fee_type: &SchoolFeeType,
    period_label: &str,
) -> (Invoice, UnifiedResult<()>) {
    let total = charge.total_minor;
    let mut invoice = Invoice {
        id: new_id(),
        number: state.next_invoice_number(),
        reference: fresh_invoice_reference(&**state),
        order_id: None,
        subscription_id: None,
        user_id: charge.student_user_id.clone(),
        company_id: charge.company_id.clone(),
        items: vec![InvoiceItem {
            name: format!("{} ({})", fee_type.name, period_label),
            quantity: 1,
            unit_price: total,
            line_total: total,
            tax_rate: 0.0,
            tax_amount: 0,
        }],
        subtotal: total,
        tax_total: 0,
        total,
        currency: charge.currency.clone(),
        status: "issued".to_string(),
        overdue: false,
        tax_inclusive: false,
        amount_paid: 0,
        credit_total: 0,
        balance: total,
        issue_date: Some(now()),
        due_date: Some(charge.due_date),
        payment_terms_days: None,
        paid_at: None,
        notes: Some(format!("School fee charge {}", charge.id)),
        reminders_sent: Vec::new(),
        created_at: now(),
        updated_at: now(),
    };
    let validation = match &charge.company_id {
        Some(tenant_id) if !tenant_id.trim().is_empty() => {
            state.validate_invoice_with_pricing(tenant_id.trim(), &invoice)
        }
        _ => Err(UnifiedError::unprocessable(
            "Invoice reconciliation unavailable: no tenant id on the fee charge",
        )),
    };
    match validation {
        Ok(()) => {
            state
                .invoices
                .insert(invoice.id.clone(), invoice.clone());
            (invoice, Ok(()))
        }
        Err(error) => {
            invoice.status = "draft".to_string();
            invoice.notes = Some(format!(
                "Held as draft: invoice reconciliation did not pass ({}) - issue manually after review.",
                error.message
            ));
            invoice.updated_at = now();
            state
                .invoices
                .insert(invoice.id.clone(), invoice.clone());
            (invoice, Err(error))
        }
    }
}

/// Record a payment against a fee charge's linked invoice,
/// mirroring the `record_payment` validation and bookkeeping
/// (balance cap, partial/full status, `invoice.payment.recorded`
/// and `invoice.paid` events). The charge itself is updated by
/// the invoice-event sync handlers, which run synchronously inside
/// the emits below.
pub(crate) fn record_fee_invoice_payment(
    state: &Arc<BillingState>,
    charge: &FeeCharge,
    amount_minor: i64,
    method: &str,
    reference: Option<String>,
) -> UnifiedResult<InvoicePayment> {
    let invoice_id = charge.invoice_id.clone().ok_or_else(|| {
        UnifiedError::unprocessable("Fee charge has no linked invoice to pay")
    })?;
    let mut invoice = state
        .invoices
        .get_mut(&invoice_id)
        .ok_or_else(|| UnifiedError::not_found("Linked invoice not found"))?;
    // A draft must be issued first, the same as the manual invoice
    // payment paths: paying it would bypass pricing reconciliation.
    check_payment_allowed(&invoice)?;
    let amount = amount_minor;
    if amount > invoice.balance {
        return Err(UnifiedError::unprocessable(format!(
            "Payment of {} exceeds the outstanding balance of {}",
            minor_to_decimal_string(amount_minor, &charge.currency),
            minor_to_decimal_string(invoice.balance, &charge.currency),
        )));
    }
    let payment = InvoicePayment {
        id: new_id(),
        invoice_id: invoice.id.clone(),
        amount,
        method: method.to_string(),
        reference,
        recorded_at: now(),
    };
    state.payments.insert(payment.id.clone(), payment.clone());
    invoice.amount_paid += amount;
    recompute_balance(&mut invoice);
    if invoice.balance == 0 {
        invoice.status = "paid".to_string();
        invoice.paid_at = Some(now());
    } else {
        invoice.status = "partially_paid".to_string();
    }
    invoice.updated_at = now();
    let fully_paid = invoice.status == "paid";
    let invoice_value = serde_json::to_value(&*invoice).unwrap_or(Value::Null);
    let payment_value = serde_json::to_value(&payment).unwrap_or(Value::Null);
    drop(invoice);
    state.emit("invoice.payment.recorded", payment_value);
    if fully_paid {
        state.emit(
            "invoice.paid",
            json!({ "invoice": invoice_value, "method": method }),
        );
    }
    Ok(payment)
}

/// Mirror one invoice's payment state onto its linked fee charge.
/// Returns the updated charge plus whether it just became paid or
/// just became void, so the event handlers can emit exactly once.
pub(crate) fn sync_fee_charge_from_invoice(
    state: &Arc<BillingState>,
    invoice: &Invoice,
) -> (Option<FeeCharge>, bool, bool) {
    let mut found: Option<FeeCharge> = None;
    for mut entry in state.fee_charges.iter_mut() {
        if entry.value().invoice_id.as_deref() == Some(invoice.id.as_str()) {
            let was_paid = entry.status == "paid";
            let was_void = entry.status == "void";
            entry.amount_paid_minor = invoice.amount_paid;
            if invoice.status == "paid" {
                entry.status = "paid".to_string();
                entry.paid_at = invoice.paid_at;
            } else if invoice.status == "void" {
                entry.status = "void".to_string();
            } else if invoice.status == "credited" {
                // A full refund of a paid fee invoice reopens the
                // charge: it is open again when nothing is left
                // paid, partially paid when some payment remains.
                // Without this arm the charge kept status `paid`
                // while its paid amount had dropped below the total.
                entry.status = if entry.amount_paid_minor > 0 {
                    "partially_paid".to_string()
                } else {
                    "open".to_string()
                };
                entry.paid_at = None;
            } else if entry.amount_paid_minor > 0 {
                entry.status = "partially_paid".to_string();
            }
            entry.updated_at = now();
            let updated = entry.value().clone();
            let became_paid = !was_paid && updated.status == "paid";
            let became_void = !was_void && updated.status == "void";
            found = Some(updated);
            return (found, became_paid, became_void);
        }
    }
    (None, false, false)
}

/// Print payload for a receipt: the shape the printing module
/// consumes from `school_fees.receipt.print_requested` (and from
/// the read receipt endpoint, without emitting).
pub(crate) fn fee_receipt_print_payload(
    receipt: &FeeReceipt,
    charge: &FeeCharge,
    fee_type_name: &str,
) -> Value {
    json!({
        "document_kind": "school_fee_receipt",
        "receipt_number": receipt.number,
        "student_user_id": receipt.student_user_id,
        "fee_type": fee_type_name,
        "period_label": charge.period_label,
        "academic_period_id": charge.academic_period_id,
        "amount_minor": receipt.amount_minor,
        "amount_text": minor_to_decimal_string(receipt.amount_minor, &receipt.currency),
        "currency": receipt.currency,
        "method": receipt.method,
        "reference": receipt.reference,
        "charge_id": charge.id,
        "invoice_id": charge.invoice_id,
        "due_date": charge.due_date,
        "issued_at": receipt.issued_at,
        "issued_by": receipt.issued_by,
    })
}

/// `academic.enrollment.approved`: create schedules for every
/// active fee type marked for auto-assignment, unless a schedule
/// already exists for that student, fee type and academic period.
/// The academic module owns the student, program, level and
/// period; billing only records which fees the enrolment implies.
/// The event carries `{enrollment_id, student_id, subject_id,
/// semester_id}` -- no program, level, or period fields, and
/// `student_id` is the academic student record id, not the user
/// id. The student key is read from `student_user_id` (or
/// `user_id`) when an emitter supplies it, falling back to
/// `student_id`; schedules created from the fallback are keyed by
/// the academic student id until academic echoes the user id.
pub(crate) fn on_academic_enrollment_approved(state: Arc<BillingState>, ctx: &mut HookContext) {
    let Some(student_user_id) = ctx
        .get("student_user_id")
        .and_then(|value| value.as_str())
        .map(str::to_string)
        .or_else(|| {
            ctx.get("user_id")
                .and_then(|value| value.as_str())
                .map(str::to_string)
        })
        .or_else(|| {
            ctx.get("student_id")
                .and_then(|value| value.as_str())
                .map(str::to_string)
        })
        .filter(|id| !id.trim().is_empty())
    else {
        ctx.add_result(json!({
            "billing": "skipped",
            "reason": "academic.enrollment.approved carries no student id",
        }));
        return;
    };
    let program_code = ctx
        .get("program_code")
        .and_then(|value| value.as_str())
        .unwrap_or("")
        .to_string();
    let level_code = ctx
        .get("level_code")
        .and_then(|value| value.as_str())
        .unwrap_or("")
        .to_string();
    let academic_period_id = ctx
        .get("academic_period_id")
        .and_then(|value| value.as_str())
        .unwrap_or("")
        .to_string();
    let period_start = ctx
        .get("period_start")
        .and_then(|value| value.as_str())
        .and_then(|raw| raw.parse::<DateTime<Utc>>().ok());
    let due_in_days = i64::from(state.fee_settings().auto_assign_due_in_days);
    let base = period_start.unwrap_or_else(now);
    let due_date = base + Duration::days(due_in_days);
    let mut created = 0usize;
    let auto_types: Vec<SchoolFeeType> = state
        .fee_types
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|fee_type| fee_type.active && fee_type.auto_assign_on_enrolment)
        .collect();
    for fee_type in auto_types {
        let already_scheduled = state.fee_schedules.iter().any(|entry| {
            let schedule = entry.value();
            schedule.student_user_id == student_user_id
                && schedule.fee_type_id == fee_type.id
                && schedule.academic_period_id == academic_period_id
                && schedule.status == "active"
        });
        if already_scheduled {
            continue;
        }
        let schedule = StudentFeeSchedule {
            id: new_id(),
            student_user_id: student_user_id.clone(),
            program_code: program_code.clone(),
            level_code: level_code.clone(),
            academic_period_id: academic_period_id.clone(),
            fee_type_id: fee_type.id.clone(),
            amount_minor: fee_type.default_amount_minor,
            currency: fee_type.currency.clone(),
            due_date,
            status: "active".to_string(),
            notes: None,
            created_at: now(),
            updated_at: now(),
        };
        state
            .fee_schedules
            .insert(schedule.id.clone(), schedule.clone());
        state.emit(
            "school_fees.schedule.created",
            serde_json::to_value(&schedule).unwrap_or(Value::Null),
        );
        created += 1;
    }
    ctx.add_result(json!({
        "billing": "ok",
        "student_user_id": student_user_id,
        "schedules_created": created,
    }));
}

/// `academic.enrollment.dropped`: cancel the student's active fee
/// schedules and void their open charges (voiding the linked
/// invoice, which the sync handler mirrors onto the charge). The
/// student key follows the same resolution as
/// `on_academic_enrollment_approved` (see its docs).
pub(crate) fn on_academic_enrollment_dropped(state: Arc<BillingState>, ctx: &mut HookContext) {
    let Some(student_user_id) = ctx
        .get("student_user_id")
        .and_then(|value| value.as_str())
        .map(str::to_string)
        .or_else(|| {
            ctx.get("user_id")
                .and_then(|value| value.as_str())
                .map(str::to_string)
        })
        .or_else(|| {
            ctx.get("student_id")
                .and_then(|value| value.as_str())
                .map(str::to_string)
        })
        .filter(|id| !id.trim().is_empty())
    else {
        ctx.add_result(json!({
            "billing": "skipped",
            "reason": "academic.enrollment.dropped carries no student id",
        }));
        return;
    };
    let mut cancelled = 0usize;
    let mut voided = 0usize;
    for mut entry in state.fee_schedules.iter_mut() {
        if entry.student_user_id == student_user_id && entry.status == "active" {
            entry.status = "cancelled".to_string();
            entry.updated_at = now();
            let value = serde_json::to_value(&*entry).unwrap_or(Value::Null);
            drop(entry);
            state.emit("school_fees.schedule.cancelled", value);
            cancelled += 1;
        }
    }
    let open_charge_ids: Vec<String> = state
        .fee_charges
        .iter()
        .filter(|entry| {
            let charge = entry.value();
            charge.student_user_id == student_user_id
                && matches!(charge.status.as_str(), "open" | "partially_paid")
        })
        .map(|entry| entry.key().clone())
        .collect();
    for charge_id in open_charge_ids {
        let Some(charge) = state
            .fee_charges
            .get(&charge_id)
            .map(|entry| entry.value().clone())
        else {
            continue;
        };
        match &charge.invoice_id {
            Some(invoice_id) => {
                if let Some(mut invoice) = state.invoices.get_mut(invoice_id) {
                    if !matches!(invoice.status.as_str(), "paid" | "void" | "credited") {
                        invoice.status = "void".to_string();
                        invoice.updated_at = now();
                        let invoice_value =
                            serde_json::to_value(&*invoice).unwrap_or(Value::Null);
                        drop(invoice);
                        state.emit("invoice.voided", invoice_value);
                        voided += 1;
                    }
                }
            }
            None => {
                if let Some(mut stored) = state.fee_charges.get_mut(&charge_id) {
                    stored.status = "void".to_string();
                    stored.updated_at = now();
                    let value = serde_json::to_value(&*stored).unwrap_or(Value::Null);
                    drop(stored);
                    state.emit("school_fees.charge.voided", value);
                    voided += 1;
                }
            }
        }
    }
    ctx.add_result(json!({
        "billing": "ok",
        "student_user_id": student_user_id,
        "schedules_cancelled": cancelled,
        "charges_voided": voided,
    }));
}

/// `invoice.payment.recorded`: mirror the paid amount onto the
/// linked fee charge (partial payments included).
pub(crate) fn on_invoice_payment_recorded_for_fee(state: Arc<BillingState>, ctx: &mut HookContext) {
    let Some(invoice_id) = ctx
        .get("invoice_id")
        .and_then(|value| value.as_str())
        .map(str::to_string)
    else {
        return;
    };
    let Some(invoice) = state
        .invoices
        .get(&invoice_id)
        .map(|entry| entry.value().clone())
    else {
        return;
    };
    let (updated, _, _) = sync_fee_charge_from_invoice(&state, &invoice);
    if let Some(charge) = updated {
        ctx.add_result(json!({
            "billing": "ok",
            "charge_id": charge.id,
            "amount_paid_minor": charge.amount_paid_minor,
        }));
    }
}

/// `invoice.paid`: mark the linked fee charge paid and announce it
/// for the academic module and the notification flow.
pub(crate) fn on_invoice_paid_for_fee(state: Arc<BillingState>, ctx: &mut HookContext) {
    let Some(invoice_id) = ctx
        .get("invoice")
        .and_then(|value| value.get("id"))
        .and_then(|value| value.as_str())
        .map(str::to_string)
    else {
        return;
    };
    let Some(invoice) = state
        .invoices
        .get(&invoice_id)
        .map(|entry| entry.value().clone())
    else {
        return;
    };
    let (updated, became_paid, _) = sync_fee_charge_from_invoice(&state, &invoice);
    if let Some(charge) = updated {
        if became_paid {
            state.emit(
                "school_fees.charge.paid",
                serde_json::to_value(&charge).unwrap_or(Value::Null),
            );
        }
        ctx.add_result(json!({
            "billing": "ok",
            "charge_id": charge.id,
            "status": charge.status,
        }));
    }
}

/// `invoice.voided`: mirror the void onto the linked fee charge.
pub(crate) fn on_invoice_voided_for_fee(state: Arc<BillingState>, ctx: &mut HookContext) {
    let Some(invoice_id) = ctx
        .get("id")
        .and_then(|value| value.as_str())
        .map(str::to_string)
    else {
        return;
    };
    let Some(invoice) = state
        .invoices
        .get(&invoice_id)
        .map(|entry| entry.value().clone())
    else {
        return;
    };
    let (updated, _, became_void) = sync_fee_charge_from_invoice(&state, &invoice);
    if let Some(charge) = updated {
        if became_void {
            state.emit(
                "school_fees.charge.voided",
                serde_json::to_value(&charge).unwrap_or(Value::Null),
            );
        }
        ctx.add_result(json!({
            "billing": "ok",
            "charge_id": charge.id,
            "status": charge.status,
        }));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration;

    fn test_discount(kind: &str, percent_basis_points: i64, amount_minor: i64) -> FeeDiscount {
        FeeDiscount {
            id: "disc-1".to_string(),
            student_user_id: "stu-1".to_string(),
            fee_type_id: None,
            kind: kind.to_string(),
            percent_basis_points,
            amount_minor,
            valid_from: None,
            valid_to: None,
            approved: true,
            approved_by: None,
            reason: None,
            status: "active".to_string(),
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }

    fn test_charge(status: &str, total_minor: i64, paid_minor: i64, due: DateTime<Utc>) -> FeeCharge {
        FeeCharge {
            id: "chg-1".to_string(),
            schedule_id: "sch-1".to_string(),
            student_user_id: "stu-1".to_string(),
            fee_type_id: "ft-1".to_string(),
            academic_period_id: "2026-11".to_string(),
            period_label: "2026-11".to_string(),
            amount_minor: total_minor,
            discount_minor: 0,
            total_minor,
            amount_paid_minor: paid_minor,
            currency: "IDR".to_string(),
            company_id: None,
            invoice_id: None,
            discount_ids: Vec::new(),
            due_date: due,
            status: status.to_string(),
            overdue: false,
            paid_at: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }

    #[test]
    fn fee_discount_percentage_scales_with_gross() {
        let discount = test_discount("percentage", 2500, 0);
        assert_eq!(fee_discount_amount(10000, &discount), 2500);
    }

    #[test]
    fn fee_discount_percentage_clamps_at_full_amount() {
        let discount = test_discount("percentage", 15000, 0);
        assert_eq!(fee_discount_amount(10000, &discount), 10000);
    }

    #[test]
    fn fee_discount_fixed_caps_at_gross_and_floors_at_zero() {
        assert_eq!(fee_discount_amount(10000, &test_discount("fixed", 0, 3000)), 3000);
        assert_eq!(fee_discount_amount(10000, &test_discount("fixed", 0, 99999)), 10000);
        assert_eq!(fee_discount_amount(10000, &test_discount("fixed", 0, -100)), 0);
    }

    #[test]
    fn fee_discount_unknown_kind_falls_back_to_fixed_amount() {
        assert_eq!(fee_discount_amount(10000, &test_discount("other", 0, 500)), 500);
    }

    #[test]
    fn fee_aging_bucket_uses_cutoff_boundaries() {
        let cutoffs = [30, 60, 90];
        assert_eq!(fee_aging_bucket(&cutoffs, 0), "1-30");
        assert_eq!(fee_aging_bucket(&cutoffs, 30), "1-30");
        assert_eq!(fee_aging_bucket(&cutoffs, 31), "31-60");
        assert_eq!(fee_aging_bucket(&cutoffs, 90), "61-90");
        assert_eq!(fee_aging_bucket(&cutoffs, 91), "over_90");
        assert_eq!(fee_aging_bucket(&[], 5), "over_0");
    }

    #[test]
    fn fee_charge_is_overdue_for_open_unpaid_charge_past_due() {
        let charge = test_charge("open", 10000, 0, Utc::now() - Duration::days(3));
        assert!(fee_charge_is_overdue(&charge, Utc::now()));
    }

    #[test]
    fn fee_charge_is_not_overdue_when_paid_or_current() {
        let paid = test_charge("paid", 10000, 10000, Utc::now() - Duration::days(3));
        assert!(!fee_charge_is_overdue(&paid, Utc::now()));
        let current = test_charge("open", 10000, 0, Utc::now() + Duration::days(3));
        assert!(!fee_charge_is_overdue(&current, Utc::now()));
    }

    #[test]
    fn fee_days_overdue_counts_whole_days_only() {
        let charge = test_charge("open", 10000, 0, Utc::now() - Duration::days(3));
        assert_eq!(fee_days_overdue(&charge, Utc::now()), 3);
        let current = test_charge("open", 10000, 0, Utc::now() + Duration::days(3));
        assert_eq!(fee_days_overdue(&current, Utc::now()), 0);
    }

    #[test]
    fn fee_charge_effective_status_reports_overdue_and_paid() {
        let overdue = test_charge("open", 10000, 0, Utc::now() - Duration::days(1));
        assert_eq!(fee_charge_effective_status(&overdue, Utc::now()), "overdue");
        let paid = test_charge("paid", 10000, 10000, Utc::now() - Duration::days(1));
        assert_eq!(fee_charge_effective_status(&paid, Utc::now()), "paid");
    }
}
