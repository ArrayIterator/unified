//! billing - school fees charges.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::json;
use unified_core::pagination::PaginationParams;
use actix_web::{web, HttpRequest, HttpResponse};
use serde_json::{json, Value};
use std::sync::Arc;
use super::state::*;
use super::helpers::*;
use super::school_fees_types::*;
use super::school_fees::*;
use super::school_fees_handlers::*;

pub(crate) async fn get_fee_receipt(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<BillingState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = fee_caller(&req)?;
        fee_require_view(&ctx, &caller)?;
        let charge = state
            .fee_charges
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Fee charge not found"))?;
        require_record_in_company_scope(&charge.company_id, &_scope)?;
        fee_require_charge_access(&ctx, &state, &caller, &charge)?;
        let receipt = state
            .fee_receipts
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|receipt| receipt.charge_id == charge.id)
            .max_by_key(|receipt| receipt.issued_at)
            .ok_or_else(|| {
                UnifiedError::not_found("No receipt issued for this charge yet")
            })?;
        let fee_type_name = state
            .fee_types
            .get(&charge.fee_type_id)
            .map(|entry| entry.value().name.clone())
            .unwrap_or_default();
        Ok(json!({
            "receipt": receipt,
            "charge": view_fee_charge(&charge),
            "print": fee_receipt_print_payload(&receipt, &charge, &fee_type_name),
        }))
    })();
    json::respond(result, ctx.is_debug())
}

/// True when a receipt already exists for this charge covering
/// exactly `amount_minor`. Issuing again for the same paid amount
/// would create two receipts for the same money.
pub(crate) fn existing_fee_receipt_for_amount(
    state: &BillingState,
    charge_id: &str,
    amount_minor: i64,
) -> bool {
    state.fee_receipts.iter().any(|entry| {
        let receipt = entry.value();
        receipt.charge_id == charge_id && receipt.amount_minor == amount_minor
    })
}

/// Issue a receipt for the amount paid so far and request
/// printing: the receipt is data, and the print request goes out
/// as `school_fees.receipt.print_requested` for the printing
/// module. This endpoint never renders or prints anything itself.
pub(crate) async fn issue_fee_receipt(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<BillingState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = require_company_for_write(&_tenant, &req)?;
        let caller = fee_caller(&req)?;
        fee_require_manage(&ctx, &caller)?;
        let charge = state
            .fee_charges
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Fee charge not found"))?;
        require_record_in_company_scope(&charge.company_id, &_scope)?;
        if charge.amount_paid_minor <= 0 {
            return Err(UnifiedError::unprocessable(
                "Cannot issue a receipt for a charge with no payment recorded",
            ));
        }
        if existing_fee_receipt_for_amount(&state, &charge.id, charge.amount_paid_minor) {
            return Err(UnifiedError::conflict(
                "A receipt already exists for the amount paid on this charge",
            ));
        }
        let fee_type_name = state
            .fee_types
            .get(&charge.fee_type_id)
            .map(|entry| entry.value().name.clone())
            .unwrap_or_default();
        let receipt = FeeReceipt {
            id: new_id(),
            number: state.next_fee_receipt_number(),
            charge_id: charge.id.clone(),
            student_user_id: charge.student_user_id.clone(),
            amount_minor: charge.amount_paid_minor,
            currency: charge.currency.clone(),
            method: None,
            reference: charge.invoice_id.clone(),
            issued_by: caller.actor.clone(),
            issued_at: now(),
        };
        state
            .fee_receipts
            .insert(receipt.id.clone(), receipt.clone());
        let payload = fee_receipt_print_payload(&receipt, &charge, &fee_type_name);
        state.emit(
            "school_fees.receipt.issued",
            serde_json::to_value(&receipt).unwrap_or(Value::Null),
        );
        state.emit("school_fees.receipt.print_requested", payload.clone());
        Ok(json!({
            "receipt": receipt,
            "charge": view_fee_charge(&charge),
            "print": payload,
        }))
    })();
    json::respond(result, ctx.is_debug())
}

/// Send one manual reminder for a charge to the student and their
/// linked guardians.
pub(crate) async fn remind_fee_charge(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<BillingState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = fee_caller(&req)?;
        fee_require_manage(&ctx, &caller)?;
        let charge = state
            .fee_charges
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Fee charge not found"))?;
        require_record_in_company_scope(&charge.company_id, &_scope)?;
        if charge.total_minor - charge.amount_paid_minor <= 0
            || matches!(charge.status.as_str(), "paid" | "void")
        {
            return Err(UnifiedError::unprocessable(
                "Fee charge has no outstanding balance to remind about",
            ));
        }
        let fee_type_name = state
            .fee_types
            .get(&charge.fee_type_id)
            .map(|entry| entry.value().name.clone())
            .unwrap_or_default();
        let remaining = charge.total_minor - charge.amount_paid_minor;
        let title = format!("Fee reminder: {fee_type_name} ({})", charge.period_label);
        let body = format!(
            "An amount of {} {} for {} ({}) is due on {}. Outstanding: {} {}.",
            minor_to_decimal_string(charge.total_minor, &charge.currency),
            charge.currency,
            fee_type_name,
            charge.period_label,
            charge.due_date.format("%Y-%m-%d"),
            minor_to_decimal_string(remaining, &charge.currency),
            charge.currency,
        );
        let mut sent = 0usize;
        for recipient in fee_reminder_recipients(&state, &charge) {
            let key = format!("{}:0:{}", charge.id, recipient);
            if state.fee_reminders.contains_key(&key) {
                continue;
            }
            state.request_notification(&recipient, &title, &body, "school_fee_reminder");
            state.fee_reminders.insert(
                key,
                FeeReminder {
                    charge_id: charge.id.clone(),
                    offset_days: 0,
                    recipient_user_id: recipient.clone(),
                    sent_at: now(),
                },
            );
            state.emit(
                "school_fees.fee.reminder",
                json!({
                    "charge_id": charge.id,
                    "student_user_id": charge.student_user_id,
                    "recipient_user_id": recipient,
                    "offset_days": 0,
                }),
            );
            sent += 1;
        }
        Ok(json!({ "reminders_sent": sent, "charge_id": charge.id }))
    })();
    json::respond(result, ctx.is_debug())
}

/// Run the fee-reminder scan: for every open charge, send the
/// reminders that fall due today (before and after the due date,
/// per the settings) to the student and their linked guardians.
/// Each reminder is sent once per recipient.
pub(crate) async fn run_fee_reminders(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<BillingState>>,
    req: HttpRequest,
) -> HttpResponse {
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = fee_caller(&req)?;
        fee_require_manage(&ctx, &caller)?;
        let settings = state.fee_settings();
        let at = now();
        // A scoped trigger only reminds its own company's
        // charges.
        let charges: Vec<FeeCharge> = state
            .fee_charges
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|charge| in_company_scope(&charge.company_id, &_scope))
            .collect();
        let mut sent = 0usize;
        let mut charges_touched = 0usize;
        let mut newly_overdue = 0usize;
        for charge in &charges {
            let due_offsets = fee_reminders_due(charge, &settings, at);
            if due_offsets.is_empty() {
                continue;
            }
            let fee_type_name = state
                .fee_types
                .get(&charge.fee_type_id)
                .map(|entry| entry.value().name.clone())
                .unwrap_or_default();
            let remaining = charge.total_minor - charge.amount_paid_minor;
            let mut touched = false;
            for offset in due_offsets {
                let title = format!(
                    "Fee reminder: {fee_type_name} ({})",
                    charge.period_label
                );
                let body = if offset > 0 {
                    format!(
                        "An amount of {} {} for {} ({}) is due in {offset} day(s) on {}. Outstanding: {} {}.",
                        minor_to_decimal_string(charge.total_minor, &charge.currency),
                        charge.currency,
                        fee_type_name,
                        charge.period_label,
                        charge.due_date.format("%Y-%m-%d"),
                        minor_to_decimal_string(remaining, &charge.currency),
                        charge.currency,
                    )
                } else {
                    format!(
                        "An amount of {} {} for {} ({}) was due on {} and is {} day(s) overdue. Outstanding: {} {}.",
                        minor_to_decimal_string(charge.total_minor, &charge.currency),
                        charge.currency,
                        fee_type_name,
                        charge.period_label,
                        charge.due_date.format("%Y-%m-%d"),
                        offset.abs(),
                        minor_to_decimal_string(remaining, &charge.currency),
                        charge.currency,
                    )
                };
                for recipient in fee_reminder_recipients(&state, charge) {
                    let key = format!("{}:{offset}:{}", charge.id, recipient);
                    if state.fee_reminders.contains_key(&key) {
                        continue;
                    }
                    state.request_notification(
                        &recipient,
                        &title,
                        &body,
                        "school_fee_reminder",
                    );
                    state.fee_reminders.insert(
                        key,
                        FeeReminder {
                            charge_id: charge.id.clone(),
                            offset_days: offset,
                            recipient_user_id: recipient.clone(),
                            sent_at: at,
                        },
                    );
                    state.emit(
                        "school_fees.fee.reminder",
                        json!({
                            "charge_id": charge.id,
                            "student_user_id": charge.student_user_id,
                            "recipient_user_id": recipient,
                            "offset_days": offset,
                        }),
                    );
                    sent += 1;
                    touched = true;
                }
            }
            if touched {
                charges_touched += 1;
            }
            if fee_charge_is_overdue(charge, at) {
                newly_overdue += 1;
            }
        }
        state.emit(
            "school_fees.reminders.completed",
            json!({
                "reminders_sent": sent,
                "charges": charges_touched,
                "overdue_charges": newly_overdue,
            }),
        );
        Ok(json!({
            "reminders_sent": sent,
            "charges": charges_touched,
            "overdue_charges": newly_overdue,
        }))
    })();
    json::respond(result, ctx.is_debug())
}

pub(crate) async fn list_fee_arrears(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<BillingState>>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let _tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return json::error_response(&error, ctx.is_debug()),
    };
    let _scope = match caller_company(&_tenant, &req) {
        Ok(scope) => scope,
        Err(error) => return json::error_response(&error, ctx.is_debug()),
    };
    let caller = match fee_caller(&req) {
        Ok(caller) => caller,
        Err(error) => return json::error_response(&error, ctx.is_debug()),
    };
    if let Err(error) = fee_require_view(&ctx, &caller) {
        return json::error_response(&error, ctx.is_debug());
    }
    let is_manager = fee_is_manager(&ctx, &caller);
    let visible = fee_visible_student_ids(&state, &caller);
    let params = query.into_inner();
    let settings = state.fee_settings();
    let at = now();
    let mut by_student: std::collections::BTreeMap<String, StudentArrears> =
        std::collections::BTreeMap::new();
    for entry in state.fee_charges.iter() {
        let charge = entry.value();
        if !fee_charge_is_overdue(charge, at) {
            continue;
        }
        if !is_manager && !visible.iter().any(|id| id == &charge.student_user_id) {
            continue;
        }
        if !in_company_scope(&charge.company_id, &_scope) {
            continue;
        }
        if let Some(student) = params.filters.get("student_user_id") {
            if charge.student_user_id != *student {
                continue;
            }
        }
        if let Some(period) = params.filters.get("academic_period_id") {
            if charge.academic_period_id != *period {
                continue;
            }
        }
        let overdue_minor = charge.total_minor - charge.amount_paid_minor;
        let days = fee_days_overdue(charge, at);
        let bucket = fee_aging_bucket(&settings.aging_bucket_days, days);
        let summary = by_student
            .entry(charge.student_user_id.clone())
            .or_insert_with(|| StudentArrears {
                student_user_id: charge.student_user_id.clone(),
                total_overdue_minor: 0,
                currency: charge.currency.clone(),
                charge_count: 0,
                oldest_due_date: None,
                buckets: Vec::new(),
            });
        summary.total_overdue_minor += overdue_minor;
        summary.charge_count += 1;
        if summary
            .oldest_due_date
            .is_none_or(|oldest| charge.due_date < oldest)
        {
            summary.oldest_due_date = Some(charge.due_date);
        }
        match summary.buckets.iter_mut().find(|item| item.bucket == bucket) {
            Some(item) => {
                item.amount_minor += overdue_minor;
                item.charge_count += 1;
            }
            None => summary.buckets.push(ArrearsBucket {
                bucket,
                amount_minor: overdue_minor,
                charge_count: 1,
            }),
        }
    }
    let mut items: Vec<StudentArrears> = by_student.into_values().collect();
    items.sort_by(|a, b| b.total_overdue_minor.cmp(&a.total_overdue_minor));
    if params.normalized().order == "asc" {
        items.reverse();
    }
    let page = paginate(items, &params);
    json::success_response(page, 200)
}

pub(crate) async fn list_fee_discounts(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<BillingState>>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let _tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return json::error_response(&error, ctx.is_debug()),
    };
    let _scope = match caller_company(&_tenant, &req) {
        Ok(scope) => scope,
        Err(error) => return json::error_response(&error, ctx.is_debug()),
    };
    let caller = match fee_caller(&req) {
        Ok(caller) => caller,
        Err(error) => return json::error_response(&error, ctx.is_debug()),
    };
    if let Err(error) = fee_require_view(&ctx, &caller) {
        return json::error_response(&error, ctx.is_debug());
    }
    let is_manager = fee_is_manager(&ctx, &caller);
    let visible = fee_visible_student_ids(&state, &caller);
    let params = query.into_inner();
    let sort = params.allowed_sort(&["created_at"], "created_at");
    let mut items: Vec<FeeDiscount> = state
        .fee_discounts
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|discount| {
            (is_manager || visible.iter().any(|id| id == &discount.student_user_id))
                && params
                    .filters
                    .get("student_user_id")
                    .is_none_or(|student| discount.student_user_id == *student)
                && params
                    .filters
                    .get("fee_type_id")
                    .is_none_or(|fee_type| {
                        discount.fee_type_id.as_deref() == Some(fee_type.as_str())
                    })
                && params
                    .filters
                    .get("status")
                    .is_none_or(|status| discount.status == *status)
                && params.filters.get("approved").is_none_or(|approved| {
                    discount.approved == (*approved == "true")
                })
        })
        .collect();
    match sort.as_str() {
        _ => items.sort_by(|a, b| a.created_at.cmp(&b.created_at)),
    }
    if params.normalized().order == "desc" {
        items.reverse();
    }
    let page = paginate(items, &params);
    json::success_response(page, 200)
}

pub(crate) async fn create_fee_discount(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<BillingState>>,
    req: HttpRequest,
    body: web::Json<CreateFeeDiscountRequest>,
) -> HttpResponse {
    let result: UnifiedResult<FeeDiscount> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = require_company_for_write(&_tenant, &req)?;
        let caller = fee_caller(&req)?;
        fee_require_manage(&ctx, &caller)?;
        let input = body.into_inner();
        let settings = state.fee_settings();
        let student_user_id = fee_require_text(&input.student_user_id, "student_user_id", 128)?;
        let fee_type_id = match &input.fee_type_id {
            Some(raw) => {
                let id = fee_require_text(raw, "fee_type_id", 128)?;
                if !state.fee_types.contains_key(id.as_str()) {
                    return Err(UnifiedError::not_found("Fee type not found"));
                }
                Some(id)
            }
            None => None,
        };
        let kind = fee_require_text(&input.kind, "kind", 32)?;
        if !matches!(kind.as_str(), "percentage" | "fixed") {
            return Err(UnifiedError::bad_request(
                "kind must be percentage or fixed",
            ));
        }
        let (percent_basis_points, amount_minor) = if kind == "percentage" {
            let basis = input.percent_basis_points.ok_or_else(|| {
                UnifiedError::bad_request(
                    "percent_basis_points is required for a percentage discount",
                )
            })?;
            if basis <= 0 || basis > 10_000 {
                return Err(UnifiedError::bad_request(
                    "percent_basis_points must be between 1 and 10000",
                ));
            }
            (basis, 0)
        } else {
            let amount = input.amount_minor.ok_or_else(|| {
                UnifiedError::bad_request("amount_minor is required for a fixed discount")
            })?;
            if amount <= 0 {
                return Err(UnifiedError::bad_request(
                    "amount_minor must be positive",
                ));
            }
            (0, amount)
        };
        if let (Some(from), Some(to)) = (input.valid_from, input.valid_to) {
            if to < from {
                return Err(UnifiedError::bad_request(
                    "valid_to must not be before valid_from",
                ));
            }
        }
        let reason = fee_optional_text(&input.reason, "reason", 2000)?;
        // When approval is configured, a new discount starts
        // unapproved; a manager approves it with an update.
        let approved = !settings.discount_requires_approval;
        let discount = FeeDiscount {
            id: new_id(),
            student_user_id,
            fee_type_id,
            kind,
            percent_basis_points,
            amount_minor,
            valid_from: input.valid_from,
            valid_to: input.valid_to,
            approved,
            approved_by: None,
            reason,
            status: "active".to_string(),
            created_at: now(),
            updated_at: now(),
        };
        state
            .fee_discounts
            .insert(discount.id.clone(), discount.clone());
        state.emit(
            "school_fees.discount.created",
            serde_json::to_value(&discount).unwrap_or(Value::Null),
        );
        Ok(discount)
    })();
    json::respond(result, ctx.is_debug())
}

pub(crate) async fn get_fee_discount(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<BillingState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<FeeDiscount> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = fee_caller(&req)?;
        fee_require_view(&ctx, &caller)?;
        let discount = state
            .fee_discounts
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Fee discount not found"))?;
        if !fee_can_view_student(&ctx, &state, &caller, &discount.student_user_id) {
            return Err(UnifiedError::forbidden(
                "Fee discount belongs to a different student",
            ));
        }
        Ok(discount)
    })();
    json::respond(result, ctx.is_debug())
}

pub(crate) async fn update_fee_discount(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<BillingState>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdateFeeDiscountRequest>,
) -> HttpResponse {
    let result: UnifiedResult<FeeDiscount> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = require_company_for_write(&_tenant, &req)?;
        let caller = fee_caller(&req)?;
        fee_require_manage(&ctx, &caller)?;
        let input = body.into_inner();
        let mut discount = state
            .fee_discounts
            .get_mut(path.as_str())
            .ok_or_else(|| UnifiedError::not_found("Fee discount not found"))?;
        let was_approved = discount.approved;
        if let Some(approved) = input.approved {
            discount.approved = approved;
            if approved {
                discount.approved_by = Some(caller.actor.clone());
            } else {
                discount.approved_by = None;
            }
        }
        if let Some(status) = input.status {
            if !matches!(status.as_str(), "active" | "revoked") {
                return Err(UnifiedError::bad_request(
                    "status must be active or revoked",
                ));
            }
            discount.status = status;
        }
        if let Some(valid_to) = input.valid_to {
            if let Some(from) = discount.valid_from {
                if valid_to < from {
                    return Err(UnifiedError::bad_request(
                        "valid_to must not be before valid_from",
                    ));
                }
            }
            discount.valid_to = Some(valid_to);
        }
        if let Some(reason) = input.reason {
            discount.reason = if reason.trim().is_empty() {
                None
            } else {
                Some(fee_require_text(&reason, "reason", 2000)?)
            };
        }
        discount.updated_at = now();
        let updated = discount.clone();
        let newly_approved = !was_approved && updated.approved;
        drop(discount);
        if newly_approved {
            state.emit(
                "school_fees.discount.approved",
                serde_json::to_value(&updated).unwrap_or(Value::Null),
            );
        }
        Ok(updated)
    })();
    json::respond(result, ctx.is_debug())
}

pub(crate) async fn delete_fee_discount(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<BillingState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = require_company_for_write(&_tenant, &req)?;
        let caller = fee_caller(&req)?;
        fee_require_manage(&ctx, &caller)?;
        if !state.fee_discounts.contains_key(path.as_str()) {
            return Err(UnifiedError::not_found("Fee discount not found"));
        }
        state.fee_discounts.remove(path.as_str());
        state.emit(
            "school_fees.discount.deleted",
            json!({ "discount_id": path.as_str() }),
        );
        Ok(json!({ "deleted": true, "id": path.as_str() }))
    })();
    json::respond(result, ctx.is_debug())
}

pub(crate) async fn list_fee_guardians(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<BillingState>>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let _tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return json::error_response(&error, ctx.is_debug()),
    };
    let _scope = match caller_company(&_tenant, &req) {
        Ok(scope) => scope,
        Err(error) => return json::error_response(&error, ctx.is_debug()),
    };
    let caller = match fee_caller(&req) {
        Ok(caller) => caller,
        Err(error) => return json::error_response(&error, ctx.is_debug()),
    };
    if let Err(error) = fee_require_view(&ctx, &caller) {
        return json::error_response(&error, ctx.is_debug());
    }
    let is_manager = fee_is_manager(&ctx, &caller);
    let params = query.into_inner();
    let mut items: Vec<FeeGuardianLink> = state
        .fee_guardians
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|link| {
            (is_manager
                || link.guardian_user_id == caller.actor
                || fee_visible_student_ids(&state, &caller)
                    .iter()
                    .any(|id| id == &link.student_user_id))
                && params
                    .filters
                    .get("student_user_id")
                    .is_none_or(|student| link.student_user_id == *student)
                && params
                    .filters
                    .get("guardian_user_id")
                    .is_none_or(|guardian| link.guardian_user_id == *guardian)
                && params.filters.get("active").is_none_or(|active| {
                    link.active == (*active == "true")
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

pub(crate) async fn create_fee_guardian(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<BillingState>>,
    req: HttpRequest,
    body: web::Json<CreateFeeGuardianRequest>,
) -> HttpResponse {
    let result: UnifiedResult<FeeGuardianLink> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = require_company_for_write(&_tenant, &req)?;
        let caller = fee_caller(&req)?;
        fee_require_manage(&ctx, &caller)?;
        let input = body.into_inner();
        let guardian_user_id =
            fee_require_text(&input.guardian_user_id, "guardian_user_id", 128)?;
        let student_user_id =
            fee_require_text(&input.student_user_id, "student_user_id", 128)?;
        if guardian_user_id == student_user_id {
            return Err(UnifiedError::bad_request(
                "guardian_user_id must differ from student_user_id",
            ));
        }
        let relationship = input
            .relationship
            .map(|raw| fee_require_text(&raw, "relationship", 64))
            .transpose()?
            .unwrap_or_else(|| "guardian".to_string());
        if state.fee_guardians.iter().any(|entry| {
            let link = entry.value();
            link.active
                && link.guardian_user_id == guardian_user_id
                && link.student_user_id == student_user_id
        }) {
            return Err(UnifiedError::conflict(
                "Guardian is already linked to this student",
            ));
        }
        let link = FeeGuardianLink {
            id: new_id(),
            guardian_user_id,
            student_user_id,
            relationship,
            active: true,
            created_at: now(),
        };
        state
            .fee_guardians
            .insert(link.id.clone(), link.clone());
        state.emit(
            "school_fees.guardian.linked",
            serde_json::to_value(&link).unwrap_or(Value::Null),
        );
        Ok(link)
    })();
    json::respond(result, ctx.is_debug())
}

pub(crate) async fn delete_fee_guardian(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<BillingState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = require_company_for_write(&_tenant, &req)?;
        let caller = fee_caller(&req)?;
        fee_require_manage(&ctx, &caller)?;
        let mut link = state
            .fee_guardians
            .get_mut(path.as_str())
            .ok_or_else(|| UnifiedError::not_found("Guardian link not found"))?;
        if !link.active {
            return Err(UnifiedError::conflict("Guardian link is already inactive"));
        }
        link.active = false;
        let updated = link.clone();
        drop(link);
        state.emit(
            "school_fees.guardian.unlinked",
            serde_json::to_value(&updated).unwrap_or(Value::Null),
        );
        Ok(json!({ "deleted": true, "id": path.as_str() }))
    })();
    json::respond(result, ctx.is_debug())
}

/// Fee charges for the caller: the student's own charges, or their
/// linked children's charges for a guardian.
pub(crate) async fn list_my_fee_charges(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<BillingState>>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let _tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return json::error_response(&error, ctx.is_debug()),
    };
    let _scope = match caller_company(&_tenant, &req) {
        Ok(scope) => scope,
        Err(error) => return json::error_response(&error, ctx.is_debug()),
    };
    let caller = match fee_caller(&req) {
        Ok(caller) => caller,
        Err(error) => return json::error_response(&error, ctx.is_debug()),
    };
    if let Err(error) = fee_require_view(&ctx, &caller) {
        return json::error_response(&error, ctx.is_debug());
    }
    let visible = fee_visible_student_ids(&state, &caller);
    let params = query.into_inner();
    let sort = params.allowed_sort(&["created_at", "due_date", "total_minor"], "created_at");
    let at = now();
    let mut items: Vec<FeeCharge> = state
        .fee_charges
        .iter()
        .map(|entry| view_fee_charge(entry.value()))
        .filter(|charge| {
            visible.iter().any(|id| id == &charge.student_user_id)
                && in_company_scope(&charge.company_id, &_scope)
                && params
                    .filters
                    .get("status")
                    .is_none_or(|status| fee_charge_effective_status(charge, at) == *status)
                && params
                    .filters
                    .get("academic_period_id")
                    .is_none_or(|period| charge.academic_period_id == *period)
        })
        .collect();
    match sort.as_str() {
        "due_date" => items.sort_by(|a, b| a.due_date.cmp(&b.due_date)),
        "total_minor" => items.sort_by(|a, b| a.total_minor.cmp(&b.total_minor)),
        _ => items.sort_by(|a, b| a.created_at.cmp(&b.created_at)),
    }
    if params.normalized().order == "desc" {
        items.reverse();
    }
    let page = paginate(items, &params);
    json::success_response(page, 200)
}
