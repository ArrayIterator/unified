//! billing - school fees handlers.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::json;
use unified_core::pagination::PaginationParams;
use actix_web::{web, HttpRequest, HttpResponse};
use chrono::{DateTime, Utc};
use serde::Deserialize;
use serde_json::{json, Value};
use std::sync::Arc;
use super::state::*;
use super::helpers::*;
use super::school_fees_types::*;
use super::school_fees::*;

#[derive(Debug, Clone, Deserialize)]
pub struct CreateFeeTypeRequest {
    pub code: String,
    pub name: String,
    pub description: Option<String>,
    pub category: String,
    pub billing_period: String,
    pub default_amount_minor: i64,
    pub currency: Option<String>,
    #[serde(default)]
    pub auto_assign_on_enrolment: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct UpdateFeeTypeRequest {
    pub name: Option<String>,
    pub description: Option<String>,
    pub category: Option<String>,
    pub billing_period: Option<String>,
    pub default_amount_minor: Option<i64>,
    pub currency: Option<String>,
    pub auto_assign_on_enrolment: Option<bool>,
    pub active: Option<bool>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct UpdateFeeSettingsRequest {
    pub currency: Option<String>,
    pub auto_assign_due_in_days: Option<u32>,
    pub reminder_before_days: Option<Vec<i32>>,
    pub reminder_after_days: Option<Vec<i32>>,
    pub aging_bucket_days: Option<Vec<i32>>,
    pub fee_categories: Option<Vec<String>>,
    pub billing_periods: Option<Vec<String>>,
    pub receipt_prefix: Option<String>,
    pub discount_requires_approval: Option<bool>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CreateFeeScheduleRequest {
    pub student_user_id: String,
    pub program_code: String,
    pub level_code: String,
    pub academic_period_id: String,
    pub fee_type_id: String,
    pub amount_minor: Option<i64>,
    pub due_date: DateTime<Utc>,
    pub notes: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct UpdateFeeScheduleRequest {
    pub amount_minor: Option<i64>,
    pub due_date: Option<DateTime<Utc>>,
    pub status: Option<String>,
    pub notes: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct GenerateFeeChargeRequest {
    pub period_label: String,
    pub due_date: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct PayFeeChargeRequest {
    pub amount_minor: i64,
    pub method: Option<String>,
    pub reference: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CreateFeeDiscountRequest {
    pub student_user_id: String,
    pub fee_type_id: Option<String>,
    pub kind: String,
    pub percent_basis_points: Option<i64>,
    pub amount_minor: Option<i64>,
    pub valid_from: Option<DateTime<Utc>>,
    pub valid_to: Option<DateTime<Utc>>,
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct UpdateFeeDiscountRequest {
    pub approved: Option<bool>,
    pub status: Option<String>,
    pub valid_to: Option<DateTime<Utc>>,
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CreateFeeGuardianRequest {
    pub guardian_user_id: String,
    pub student_user_id: String,
    pub relationship: Option<String>,
}

pub(crate) fn fee_validate_day_list(
    days: &[i32],
    field: &str,
    max: i32,
) -> UnifiedResult<Vec<i32>> {
    if days.len() > 31 {
        return Err(UnifiedError::bad_request(format!(
            "{field} must have at most 31 entries"
        )));
    }
    for &day in days {
        if day <= 0 || day > max {
            return Err(UnifiedError::bad_request(format!(
                "{field} entries must be between 1 and {max}"
            )));
        }
    }
    let mut sorted = days.to_vec();
    sorted.sort_unstable();
    sorted.dedup();
    Ok(sorted)
}

pub(crate) async fn list_fee_types(
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
    let params = query.into_inner();
    let sort = params.allowed_sort(&["created_at", "name", "code"], "created_at");
    let mut items: Vec<SchoolFeeType> = state
        .fee_types
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|fee_type| {
            matches_search(&format!("{} {} {}", fee_type.code, fee_type.name, fee_type.category), &params)
                && params
                    .filters
                    .get("category")
                    .is_none_or(|category| fee_type.category == *category)
                && params
                    .filters
                    .get("billing_period")
                    .is_none_or(|period| fee_type.billing_period == *period)
                && params.filters.get("active").is_none_or(|active| {
                    fee_type.active == (*active == "true")
                })
        })
        .collect();
    match sort.as_str() {
        "name" => items.sort_by(|a, b| a.name.cmp(&b.name)),
        "code" => items.sort_by(|a, b| a.code.cmp(&b.code)),
        _ => items.sort_by(|a, b| a.created_at.cmp(&b.created_at)),
    }
    if params.normalized().order == "desc" {
        items.reverse();
    }
    let page = paginate(items, &params);
    json::success_response(page, 200)
}

pub(crate) async fn create_fee_type(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<BillingState>>,
    req: HttpRequest,
    body: web::Json<CreateFeeTypeRequest>,
) -> HttpResponse {
    let result: UnifiedResult<SchoolFeeType> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = require_company_for_write(&_tenant, &req)?;
        let caller = fee_caller(&req)?;
        fee_require_manage(&ctx, &caller)?;
        let input = body.into_inner();
        let settings = state.fee_settings();
        let code = fee_require_text(&input.code, "code", 64)?;
        if state
            .fee_types
            .iter()
            .any(|entry| entry.value().code == code)
        {
            return Err(UnifiedError::conflict(format!(
                "Fee type code \"{code}\" already exists"
            )));
        }
        let name = fee_require_text(&input.name, "name", 200)?;
        let description = fee_optional_text(&input.description, "description", 2000)?;
        let category = fee_require_text(&input.category, "category", 64)?;
        if !settings.fee_categories.iter().any(|item| item == &category) {
            return Err(UnifiedError::bad_request(format!(
                "category \"{category}\" is not allowed; allowed values: {}",
                settings.fee_categories.join(", ")
            )));
        }
        let billing_period = fee_require_text(&input.billing_period, "billing_period", 64)?;
        if !settings.billing_periods.iter().any(|item| item == &billing_period) {
            return Err(UnifiedError::bad_request(format!(
                "billing_period \"{billing_period}\" is not allowed; allowed values: {}",
                settings.billing_periods.join(", ")
            )));
        }
        if input.default_amount_minor < 0 {
            return Err(UnifiedError::bad_request(
                "default_amount_minor must not be negative",
            ));
        }
        let currency = match &input.currency {
            Some(raw) => fee_require_currency(raw)?,
            None => settings.currency.clone(),
        };
        let fee_type = SchoolFeeType {
            id: new_id(),
            code,
            name,
            description,
            category,
            billing_period,
            default_amount_minor: input.default_amount_minor,
            currency,
            auto_assign_on_enrolment: input.auto_assign_on_enrolment,
            active: true,
            created_at: now(),
            updated_at: now(),
        };
        state
            .fee_types
            .insert(fee_type.id.clone(), fee_type.clone());
        state.emit(
            "school_fees.type.created",
            serde_json::to_value(&fee_type).unwrap_or(Value::Null),
        );
        Ok(fee_type)
    })();
    json::respond(result, ctx.is_debug())
}

pub(crate) async fn get_fee_type(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<BillingState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<SchoolFeeType> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = fee_caller(&req)?;
        fee_require_view(&ctx, &caller)?;
        state
            .fee_types
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Fee type not found"))
    })();
    json::respond(result, ctx.is_debug())
}

pub(crate) async fn update_fee_type(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<BillingState>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdateFeeTypeRequest>,
) -> HttpResponse {
    let result: UnifiedResult<SchoolFeeType> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = require_company_for_write(&_tenant, &req)?;
        let caller = fee_caller(&req)?;
        fee_require_manage(&ctx, &caller)?;
        let input = body.into_inner();
        let settings = state.fee_settings();
        let mut fee_type = state
            .fee_types
            .get_mut(path.as_str())
            .ok_or_else(|| UnifiedError::not_found("Fee type not found"))?;
        if let Some(name) = input.name {
            fee_type.name = fee_require_text(&name, "name", 200)?;
        }
        if let Some(description) = input.description {
            fee_type.description = if description.trim().is_empty() {
                None
            } else {
                Some(fee_require_text(&description, "description", 2000)?)
            };
        }
        if let Some(category) = input.category {
            let category = fee_require_text(&category, "category", 64)?;
            if !settings.fee_categories.iter().any(|item| item == &category) {
                return Err(UnifiedError::bad_request(format!(
                    "category \"{category}\" is not allowed; allowed values: {}",
                    settings.fee_categories.join(", ")
                )));
            }
            fee_type.category = category;
        }
        if let Some(billing_period) = input.billing_period {
            let billing_period =
                fee_require_text(&billing_period, "billing_period", 64)?;
            if !settings
                .billing_periods
                .iter()
                .any(|item| item == &billing_period)
            {
                return Err(UnifiedError::bad_request(format!(
                    "billing_period \"{billing_period}\" is not allowed; allowed values: {}",
                    settings.billing_periods.join(", ")
                )));
            }
            fee_type.billing_period = billing_period;
        }
        if let Some(amount) = input.default_amount_minor {
            if amount < 0 {
                return Err(UnifiedError::bad_request(
                    "default_amount_minor must not be negative",
                ));
            }
            fee_type.default_amount_minor = amount;
        }
        if let Some(currency) = input.currency {
            fee_type.currency = fee_require_currency(&currency)?;
        }
        if let Some(auto) = input.auto_assign_on_enrolment {
            fee_type.auto_assign_on_enrolment = auto;
        }
        if let Some(active) = input.active {
            fee_type.active = active;
        }
        fee_type.updated_at = now();
        let updated = fee_type.clone();
        drop(fee_type);
        state.emit(
            "school_fees.type.updated",
            serde_json::to_value(&updated).unwrap_or(Value::Null),
        );
        Ok(updated)
    })();
    json::respond(result, ctx.is_debug())
}

pub(crate) async fn delete_fee_type(
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
        if !state.fee_types.contains_key(path.as_str()) {
            return Err(UnifiedError::not_found("Fee type not found"));
        }
        if state.fee_schedules.iter().any(|entry| {
            entry.value().fee_type_id == path.as_str()
        }) {
            return Err(UnifiedError::unprocessable(
                "Fee type is used by fee schedules; deactivate it instead of deleting it",
            ));
        }
        state.fee_types.remove(path.as_str());
        state.emit(
            "school_fees.type.deleted",
            json!({ "fee_type_id": path.as_str() }),
        );
        Ok(json!({ "deleted": true, "id": path.as_str() }))
    })();
    json::respond(result, ctx.is_debug())
}

pub(crate) async fn get_fee_settings(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<BillingState>>,
    req: HttpRequest,
) -> HttpResponse {
    let result: UnifiedResult<SchoolFeeSettings> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = fee_caller(&req)?;
        fee_require_view(&ctx, &caller)?;
        Ok(state.fee_settings())
    })();
    json::respond(result, ctx.is_debug())
}

pub(crate) async fn update_fee_settings(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<BillingState>>,
    req: HttpRequest,
    body: web::Json<UpdateFeeSettingsRequest>,
) -> HttpResponse {
    let result: UnifiedResult<SchoolFeeSettings> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = fee_caller(&req)?;
        fee_require_manage(&ctx, &caller)?;
        let input = body.into_inner();
        let mut settings = state
            .fee_settings
            .write()
            .expect("school fee settings lock poisoned");
        if let Some(currency) = input.currency {
            settings.currency = fee_require_currency(&currency)?;
        }
        if let Some(days) = input.auto_assign_due_in_days {
            if days > 365 {
                return Err(UnifiedError::bad_request(
                    "auto_assign_due_in_days must be at most 365",
                ));
            }
            settings.auto_assign_due_in_days = days;
        }
        if let Some(days) = input.reminder_before_days {
            settings.reminder_before_days = fee_validate_day_list(&days, "reminder_before_days", 365)?;
        }
        if let Some(days) = input.reminder_after_days {
            settings.reminder_after_days = fee_validate_day_list(&days, "reminder_after_days", 365)?;
        }
        if let Some(days) = input.aging_bucket_days {
            let cutoffs = fee_validate_day_list(&days, "aging_bucket_days", 3650)?;
            if cutoffs.is_empty() {
                return Err(UnifiedError::bad_request(
                    "aging_bucket_days must not be empty",
                ));
            }
            settings.aging_bucket_days = cutoffs;
        }
        if let Some(categories) = input.fee_categories {
            if categories.is_empty() {
                return Err(UnifiedError::bad_request(
                    "fee_categories must not be empty",
                ));
            }
            settings.fee_categories = categories
                .into_iter()
                .map(|category| fee_require_text(&category, "fee_categories", 64))
                .collect::<UnifiedResult<Vec<String>>>()?;
        }
        if let Some(periods) = input.billing_periods {
            if periods.is_empty() {
                return Err(UnifiedError::bad_request(
                    "billing_periods must not be empty",
                ));
            }
            settings.billing_periods = periods
                .into_iter()
                .map(|period| fee_require_text(&period, "billing_periods", 64))
                .collect::<UnifiedResult<Vec<String>>>()?;
        }
        if let Some(prefix) = input.receipt_prefix {
            settings.receipt_prefix = fee_require_text(&prefix, "receipt_prefix", 16)?;
        }
        if let Some(requires) = input.discount_requires_approval {
            settings.discount_requires_approval = requires;
        }
        let updated = settings.clone();
        drop(settings);
        state.emit(
            "school_fees.settings.updated",
            serde_json::to_value(&updated).unwrap_or(Value::Null),
        );
        Ok(updated)
    })();
    json::respond(result, ctx.is_debug())
}

pub(crate) async fn create_fee_schedule(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<BillingState>>,
    req: HttpRequest,
    body: web::Json<CreateFeeScheduleRequest>,
) -> HttpResponse {
    let result: UnifiedResult<StudentFeeSchedule> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = require_company_for_write(&_tenant, &req)?;
        let caller = fee_caller(&req)?;
        fee_require_manage(&ctx, &caller)?;
        let input = body.into_inner();
        let student_user_id = fee_require_text(&input.student_user_id, "student_user_id", 128)?;
        let program_code = fee_require_text(&input.program_code, "program_code", 128)?;
        let level_code = fee_require_text(&input.level_code, "level_code", 128)?;
        let academic_period_id =
            fee_require_text(&input.academic_period_id, "academic_period_id", 128)?;
        let fee_type = state
            .fee_types
            .get(&input.fee_type_id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Fee type not found"))?;
        if !fee_type.active {
            return Err(UnifiedError::unprocessable(
                "Fee type is not active",
            ));
        }
        let amount_minor = match input.amount_minor {
            Some(amount) => {
                if amount < 0 {
                    return Err(UnifiedError::bad_request(
                        "amount_minor must not be negative",
                    ));
                }
                amount
            }
            None => fee_type.default_amount_minor,
        };
        let notes = fee_optional_text(&input.notes, "notes", 2000)?;
        let schedule = StudentFeeSchedule {
            id: new_id(),
            student_user_id,
            program_code,
            level_code,
            academic_period_id,
            fee_type_id: fee_type.id,
            amount_minor,
            currency: fee_type.currency,
            due_date: input.due_date,
            status: "active".to_string(),
            notes,
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
        Ok(schedule)
    })();
    json::respond(result, ctx.is_debug())
}

pub(crate) async fn list_fee_schedules(
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
    let sort = params.allowed_sort(&["created_at", "due_date"], "created_at");
    let mut items: Vec<StudentFeeSchedule> = state
        .fee_schedules
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|schedule| {
            (is_manager || visible.iter().any(|id| id == &schedule.student_user_id))
                && params
                    .filters
                    .get("student_user_id")
                    .is_none_or(|student| schedule.student_user_id == *student)
                && params
                    .filters
                    .get("fee_type_id")
                    .is_none_or(|fee_type| schedule.fee_type_id == *fee_type)
                && params
                    .filters
                    .get("academic_period_id")
                    .is_none_or(|period| schedule.academic_period_id == *period)
                && params
                    .filters
                    .get("status")
                    .is_none_or(|status| schedule.status == *status)
        })
        .collect();
    match sort.as_str() {
        "due_date" => items.sort_by(|a, b| a.due_date.cmp(&b.due_date)),
        _ => items.sort_by(|a, b| a.created_at.cmp(&b.created_at)),
    }
    if params.normalized().order == "desc" {
        items.reverse();
    }
    let page = paginate(items, &params);
    json::success_response(page, 200)
}

pub(crate) async fn get_fee_schedule(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<BillingState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<StudentFeeSchedule> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = fee_caller(&req)?;
        fee_require_view(&ctx, &caller)?;
        let schedule = state
            .fee_schedules
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Fee schedule not found"))?;
        if !fee_can_view_student(&ctx, &state, &caller, &schedule.student_user_id) {
            return Err(UnifiedError::forbidden(
                "Fee schedule belongs to a different student",
            ));
        }
        Ok(schedule)
    })();
    json::respond(result, ctx.is_debug())
}

pub(crate) async fn update_fee_schedule(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<BillingState>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdateFeeScheduleRequest>,
) -> HttpResponse {
    let result: UnifiedResult<StudentFeeSchedule> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = require_company_for_write(&_tenant, &req)?;
        let caller = fee_caller(&req)?;
        fee_require_manage(&ctx, &caller)?;
        let input = body.into_inner();
        let mut schedule = state
            .fee_schedules
            .get_mut(path.as_str())
            .ok_or_else(|| UnifiedError::not_found("Fee schedule not found"))?;
        if let Some(amount) = input.amount_minor {
            if amount < 0 {
                return Err(UnifiedError::bad_request(
                    "amount_minor must not be negative",
                ));
            }
            schedule.amount_minor = amount;
        }
        if let Some(due_date) = input.due_date {
            schedule.due_date = due_date;
        }
        if let Some(status) = input.status {
            if !matches!(status.as_str(), "active" | "paused" | "cancelled") {
                return Err(UnifiedError::bad_request(
                    "status must be active, paused or cancelled",
                ));
            }
            schedule.status = status;
        }
        if let Some(notes) = input.notes {
            schedule.notes = if notes.trim().is_empty() {
                None
            } else {
                Some(fee_require_text(&notes, "notes", 2000)?)
            };
        }
        schedule.updated_at = now();
        let updated = schedule.clone();
        drop(schedule);
        state.emit(
            "school_fees.schedule.updated",
            serde_json::to_value(&updated).unwrap_or(Value::Null),
        );
        Ok(updated)
    })();
    json::respond(result, ctx.is_debug())
}

/// Generate one fee charge for a schedule and period, with its
/// linked billing invoice. Approved, in-period discounts for the
/// student apply automatically. A fully discounted charge needs no
/// invoice and is recorded as paid.
pub(crate) async fn generate_fee_charges(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<BillingState>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<GenerateFeeChargeRequest>,
) -> HttpResponse {
    let result: UnifiedResult<FeeCharge> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = fee_caller(&req)?;
        fee_require_manage(&ctx, &caller)?;
        let input = body.into_inner();
        let period_label = fee_require_text(&input.period_label, "period_label", 32)?;
        let schedule = state
            .fee_schedules
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Fee schedule not found"))?;
        if schedule.status != "active" {
            return Err(UnifiedError::unprocessable(format!(
                "Cannot generate charges for a {} schedule",
                schedule.status
            )));
        }
        let fee_type = state
            .fee_types
            .get(&schedule.fee_type_id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Fee type not found"))?;
        if state.fee_charges.iter().any(|entry| {
            let charge = entry.value();
            charge.schedule_id == schedule.id
                && charge.period_label == period_label
                && charge.status != "void"
        }) {
            return Err(UnifiedError::conflict(format!(
                "A charge already exists for period \"{period_label}\" on this schedule"
            )));
        }
        let at = now();
        let discounts = fee_applicable_discounts(
            &state,
            &schedule.student_user_id,
            &schedule.fee_type_id,
            at,
        );
        let gross = schedule.amount_minor;
        let mut discount_minor = 0i64;
        let mut discount_ids: Vec<String> = Vec::new();
        for discount in &discounts {
            discount_minor += fee_discount_amount(gross, discount);
            discount_ids.push(discount.id.clone());
        }
        let discount_minor = discount_minor.min(gross);
        let total_minor = gross - discount_minor;
        let due_date = input.due_date.unwrap_or(schedule.due_date);
        // The tenant / company id comes from the guard-resolved
        // tenant context above, never from a client header: it is
        // carried on the charge so the linked invoice can be routed
        // through pricing reconciliation. A context with no company
        // falls back to the tenant id itself, exactly as before;
        // the silent `.ok()` fallback to no company is gone — the
        // guard already rejected context-less requests.
        let company_id = _tenant
            .company_id
            .clone()
            .or_else(|| Some(_tenant.tenant_id.to_string()));
        let mut charge = FeeCharge {
            id: new_id(),
            schedule_id: schedule.id.clone(),
            student_user_id: schedule.student_user_id.clone(),
            fee_type_id: schedule.fee_type_id.clone(),
            academic_period_id: schedule.academic_period_id.clone(),
            period_label: period_label.clone(),
            amount_minor: gross,
            discount_minor,
            total_minor,
            amount_paid_minor: 0,
            currency: schedule.currency.clone(),
            company_id,
            invoice_id: None,
            discount_ids,
            due_date,
            status: "open".to_string(),
            overdue: false,
            paid_at: None,
            created_at: at,
            updated_at: at,
        };
        if total_minor == 0 {
            // Fully discounted: nothing to collect.
            charge.status = "paid".to_string();
            charge.paid_at = Some(at);
            state
                .fee_charges
                .insert(charge.id.clone(), charge.clone());
            state.emit(
                "school_fees.charge.created",
                serde_json::to_value(&charge).unwrap_or(Value::Null),
            );
            state.emit(
                "school_fees.charge.paid",
                serde_json::to_value(&charge).unwrap_or(Value::Null),
            );
            return Ok(view_fee_charge(&charge));
        }
        // The linked invoice is reconciled before it is issued:
        // when validation fails it is stored as a draft (and the
        // charge is still created, so the obligation is not lost),
        // and it must be issued manually after review.
        let (invoice, _validation) =
            build_fee_invoice(&state, &charge, &fee_type, &period_label);
        charge.invoice_id = Some(invoice.id.clone());
        state
            .fee_charges
            .insert(charge.id.clone(), charge.clone());
        state.emit(
            "school_fees.charge.created",
            serde_json::to_value(&charge).unwrap_or(Value::Null),
        );
        Ok(view_fee_charge(&charge))
    })();
    json::respond(result, ctx.is_debug())
}

pub(crate) async fn list_fee_charges(
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
    let sort = params.allowed_sort(&["created_at", "due_date", "total_minor"], "created_at");
    let at = now();
    let mut items: Vec<FeeCharge> = state
        .fee_charges
        .iter()
        .map(|entry| view_fee_charge(entry.value()))
        .filter(|charge| {
            (is_manager || visible.iter().any(|id| id == &charge.student_user_id))
                && in_company_scope(&charge.company_id, &_scope)
                && matches_search(
                    &format!("{} {} {}", charge.period_label, charge.id, charge.student_user_id),
                    &params,
                )
                && params
                    .filters
                    .get("student_user_id")
                    .is_none_or(|student| charge.student_user_id == *student)
                && params
                    .filters
                    .get("fee_type_id")
                    .is_none_or(|fee_type| charge.fee_type_id == *fee_type)
                && params
                    .filters
                    .get("academic_period_id")
                    .is_none_or(|period| charge.academic_period_id == *period)
                && params
                    .filters
                    .get("status")
                    .is_none_or(|status| fee_charge_effective_status(charge, at) == *status)
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

pub(crate) async fn get_fee_charge(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<BillingState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<FeeCharge> = (|| {
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
        Ok(view_fee_charge(&charge))
    })();
    json::respond(result, ctx.is_debug())
}

/// Pay a fee charge. The payment is recorded against the linked
/// billing invoice through the shared invoice payment path, so
/// gateway captures, manual invoice payments and fee payments all
/// land in the same payment history.
pub(crate) async fn pay_fee_charge(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<BillingState>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<PayFeeChargeRequest>,
) -> HttpResponse {
    let result: UnifiedResult<FeeCharge> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = require_company_for_write(&_tenant, &req)?;
        let caller = fee_caller(&req)?;
        fee_require_view(&ctx, &caller)?;
        let input = body.into_inner();
        if input.amount_minor <= 0 {
            return Err(UnifiedError::bad_request(
                "amount_minor must be a positive number",
            ));
        }
        let method = input
            .method
            .unwrap_or_else(|| "manual".to_string());
        if method.trim().is_empty() {
            return Err(UnifiedError::bad_request(
                "Payment method must not be empty",
            ));
        }
        let reference = input
            .reference
            .filter(|reference| !reference.trim().is_empty())
            .map(|reference| {
                if reference.len() > 128 {
                    reference.chars().take(128).collect()
                } else {
                    reference
                }
            });
        let charge = state
            .fee_charges
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Fee charge not found"))?;
        require_record_writable_in_company_scope(&charge.company_id, &_scope)?;
        // Managers may pay any charge; anyone else may only pay a
        // charge they can view (their own, or their child's).
        if !fee_is_manager(&ctx, &caller) {
            fee_require_charge_access(&ctx, &state, &caller, &charge)?;
        }
        if charge.status == "paid" {
            return Err(UnifiedError::conflict("Fee charge is already paid"));
        }
        if charge.status == "void" {
            return Err(UnifiedError::unprocessable(
                "A void fee charge cannot be paid",
            ));
        }
        let remaining = charge.total_minor - charge.amount_paid_minor;
        if input.amount_minor > remaining {
            return Err(UnifiedError::unprocessable(format!(
                "Payment of {} exceeds the outstanding balance of {}",
                minor_to_decimal_string(input.amount_minor, &charge.currency),
                minor_to_decimal_string(remaining, &charge.currency),
            )));
        }
        record_fee_invoice_payment(&state, &charge, input.amount_minor, &method, reference)?;
        // The invoice-event sync handlers updated the charge above;
        // re-read it for the response.
        state
            .fee_charges
            .get(path.as_str())
            .map(|entry| view_fee_charge(entry.value()))
            .ok_or_else(|| UnifiedError::not_found("Fee charge not found"))
    })();
    json::respond(result, ctx.is_debug())
}

pub(crate) async fn void_fee_charge(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<BillingState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<FeeCharge> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = require_company_for_write(&_tenant, &req)?;
        let caller = fee_caller(&req)?;
        fee_require_manage(&ctx, &caller)?;
        let charge = state
            .fee_charges
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Fee charge not found"))?;
        require_record_writable_in_company_scope(&charge.company_id, &_scope)?;
        if charge.status == "paid" {
            return Err(UnifiedError::unprocessable(
                "A paid fee charge cannot be voided",
            ));
        }
        if charge.status == "void" {
            return Err(UnifiedError::conflict("Fee charge is already void"));
        }
        match &charge.invoice_id {
            Some(invoice_id) => {
                let mut invoice = state
                    .invoices
                    .get_mut(invoice_id)
                    .ok_or_else(|| UnifiedError::not_found("Linked invoice not found"))?;
                invoice.status = "void".to_string();
                invoice.updated_at = now();
                let invoice_value = serde_json::to_value(&*invoice).unwrap_or(Value::Null);
                drop(invoice);
                // The invoice.voided sync handler mirrors the void
                // onto the charge and emits school_fees.charge.voided.
                state.emit("invoice.voided", invoice_value);
            }
            None => {
                if let Some(mut stored) = state.fee_charges.get_mut(path.as_str()) {
                    stored.status = "void".to_string();
                    stored.updated_at = now();
                    let value = serde_json::to_value(&*stored).unwrap_or(Value::Null);
                    drop(stored);
                    state.emit("school_fees.charge.voided", value);
                }
            }
        }
        state
            .fee_charges
            .get(path.as_str())
            .map(|entry| view_fee_charge(entry.value()))
            .ok_or_else(|| UnifiedError::not_found("Fee charge not found"))
    })();
    json::respond(result, ctx.is_debug())
}
