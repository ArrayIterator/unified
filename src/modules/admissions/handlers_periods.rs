// admissions module - Period handlers.
//
// Split from the former single-file module `src/modules/admissions.rs`.
// The module root (`admissions.rs`) re-exports the public items, so the
// public API is unchanged.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::{HookBus, HookContext, HookHandler};
use unified_core::json;
use unified_core::modules::{Module, ModuleRegistry, RouteInfo, RouteVersionSupport};
use unified_core::pagination::{PageResult, PaginationParams};
use actix_web::{web, HttpRequest, HttpResponse};
use chrono::{DateTime, NaiveDate, Utc};
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::sync::{Arc, RwLock};
use uuid::Uuid;

use super::constants::{KEY_FEE_CURRENCY, MAX_SHORT, PERM_ADMISSIONS_MANAGE, PERM_ADMISSIONS_READ, SEED_FEE_CURRENCY};
use super::types::{AdmissionPeriod, CreatePeriod, UpdatePeriod};
use super::state::AdmissionsState;
use super::helpers::{
    caller, caller_company, matches_search, new_id, now, paginate, parse_date, parse_money,
    require_id, require_permission, require_tenant, require_text, today, valid_currency,
};

// Handlers: periods

pub(crate) async fn list_periods(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<AdmissionsState>>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PageResult<AdmissionPeriod>> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ADMISSIONS_READ)?;
        let params = query.into_inner();
        let mut items: Vec<AdmissionPeriod> = state
            .periods
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|period| {
                matches_search(
                    &format!("{} {} {}", period.name, period.academic_year, period.term),
                    &params,
                ) && params
                    .filters
                    .get("status")
                    .is_none_or(|status| period.status == *status)
                    && params
                        .filters
                        .get("academic_year")
                        .is_none_or(|year| period.academic_year == *year)
                    && params
                        .filters
                        .get("term")
                        .is_none_or(|term| period.term == *term)
            })
            .collect();
        items.sort_by(|a, b| {
            b.open_date
                .cmp(&a.open_date)
                .then_with(|| a.name.cmp(&b.name))
        });
        if params.normalized().order == "asc" {
            items.reverse();
        }
        Ok(paginate(items, &params))
    })();
    match result {
        Ok(page) => json::success_response(page, 200),
        Err(error) => json::error_response(&error, debug),
    }
}

pub(crate) async fn create_period(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<AdmissionsState>>,
    req: HttpRequest,
    body: web::Json<CreatePeriod>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<AdmissionPeriod> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ADMISSIONS_MANAGE)?;
        let input = body.into_inner();
        let academic_year = require_text(&input.academic_year, "academic_year", MAX_SHORT)?;
        let term = require_text(&input.term, "term", MAX_SHORT)?;
        let name = match input.name {
            Some(name) if !name.trim().is_empty() => require_text(&name, "name", MAX_SHORT)?,
            _ => format!("{academic_year} {term}"),
        };
        let open_date = parse_date(&input.open_date, "open_date")?;
        let close_date = parse_date(&input.close_date, "close_date")?;
        if close_date < open_date {
            return Err(UnifiedError::bad_request(
                "close_date must not be before open_date",
            ));
        }
        if state.periods.iter().any(|entry| {
            let period = entry.value();
            period.academic_year == academic_year && period.term == term
        }) {
            return Err(UnifiedError::conflict(format!(
                "A period already exists for academic year \"{academic_year}\" term \"{term}\""
            )));
        }
        let registration_fee_amount = match input.registration_fee_amount {
            Some(amount) => Some(parse_money(amount, "registration_fee_amount")?),
            None => None,
        };
        let fee_currency = match input.fee_currency {
            Some(currency) => valid_currency(&currency)?,
            None => state.setting(KEY_FEE_CURRENCY, SEED_FEE_CURRENCY),
        };
        let registration_form_id = match input.registration_form_id {
            Some(form_id) if !form_id.trim().is_empty() => {
                Some(require_id(&form_id, "registration_form_id")?)
            }
            _ => None,
        };
        let period = AdmissionPeriod {
            id: new_id(),
            academic_year,
            term,
            name,
            open_date,
            close_date,
            status: "draft".to_string(),
            registration_fee_amount,
            fee_currency,
            registration_form_id,
            created_at: now(),
            updated_at: now(),
        };
        state.periods.insert(period.id.clone(), period.clone());
        state.emit(
            "admissions.period_created",
            json!({
                "period_id": period.id,
                "academic_year": period.academic_year,
                "term": period.term,
            }),
        );
        Ok(period)
    })();
    match result {
        Ok(data) => json::success_response(data, 201),
        Err(error) => json::error_response(&error, debug),
    }
}

pub(crate) async fn get_period(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<AdmissionsState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<AdmissionPeriod> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ADMISSIONS_READ)?;
        state.period(path.as_str())
    })();
    json::respond(result, debug)
}

pub(crate) async fn update_period(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<AdmissionsState>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdatePeriod>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<AdmissionPeriod> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ADMISSIONS_MANAGE)?;
        let input = body.into_inner();
        let period_id = path.as_str().to_string();
        let mut period = state.period(&period_id)?;
        if period.status != "draft" && period.status != "closed" {
            return Err(UnifiedError::conflict(
                "Only draft or closed periods can be updated; open periods must be closed first",
            ));
        }
        if let Some(academic_year) = input.academic_year {
            period.academic_year = require_text(&academic_year, "academic_year", MAX_SHORT)?;
        }
        if let Some(term) = input.term {
            period.term = require_text(&term, "term", MAX_SHORT)?;
        }
        if let Some(name) = input.name {
            period.name = require_text(&name, "name", MAX_SHORT)?;
        }
        let mut open_date = period.open_date;
        let mut close_date = period.close_date;
        if let Some(raw) = input.open_date {
            open_date = parse_date(&raw, "open_date")?;
        }
        if let Some(raw) = input.close_date {
            close_date = parse_date(&raw, "close_date")?;
        }
        if close_date < open_date {
            return Err(UnifiedError::bad_request(
                "close_date must not be before open_date",
            ));
        }
        period.open_date = open_date;
        period.close_date = close_date;
        if let Some(amount) = input.registration_fee_amount {
            period.registration_fee_amount = match amount {
                Some(value) => Some(parse_money(value, "registration_fee_amount")?),
                None => None,
            };
        }
        if let Some(currency) = input.fee_currency {
            period.fee_currency = valid_currency(&currency)?;
        }
        if let Some(form_id) = input.registration_form_id {
            period.registration_form_id = match form_id {
                Some(value) if !value.trim().is_empty() => {
                    Some(require_id(&value, "registration_form_id")?)
                }
                _ => None,
            };
        }
        if let Some(status) = input.status {
            if status != "archived" {
                return Err(UnifiedError::bad_request(
                    "status can only be changed to archived here; use the open and close endpoints for the other transitions",
                ));
            }
            if period.status != "closed" {
                return Err(UnifiedError::conflict(
                    "Only a closed period can be archived",
                ));
            }
            period.status = status;
        }
        if state.periods.iter().any(|entry| {
            let other = entry.value();
            other.id != period.id
                && other.academic_year == period.academic_year
                && other.term == period.term
        }) {
            return Err(UnifiedError::conflict(format!(
                "A period already exists for academic year \"{}\" term \"{}\"",
                period.academic_year, period.term
            )));
        }
        period.updated_at = now();
        state.periods.insert(period.id.clone(), period.clone());
        Ok(period)
    })();
    json::respond(result, debug)
}

pub(crate) async fn delete_period(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<AdmissionsState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ADMISSIONS_MANAGE)?;
        let period_id = path.as_str().to_string();
        let period = state.period(&period_id)?;
        if period.status != "draft" {
            return Err(UnifiedError::conflict(
                "Only a draft period can be deleted",
            ));
        }
        if state
            .applications
            .iter()
            .any(|entry| entry.value().period_id == period_id)
        {
            return Err(UnifiedError::conflict(
                "Period still has applications; archive it instead",
            ));
        }
        state
            .quotas
            .retain(|_, quota| quota.period_id != period_id);
        state
            .stages
            .retain(|_, stage| stage.period_id != period_id);
        state
            .required_documents
            .retain(|_, document| document.period_id != period_id);
        state
            .periods
            .remove(period_id.as_str())
            .map(|_| json!({ "deleted": true, "id": period_id }))
            .ok_or_else(|| UnifiedError::not_found("Admission period not found"))
    })();
    json::respond(result, debug)
}

pub(crate) async fn open_period(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<AdmissionsState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<AdmissionPeriod> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ADMISSIONS_MANAGE)?;
        let period_id = path.as_str().to_string();
        let mut period = state.period(&period_id)?;
        if period.status != "draft" {
            return Err(UnifiedError::conflict(
                "Only a draft period can be opened",
            ));
        }
        if period.close_date < today() {
            return Err(UnifiedError::conflict(
                "close_date is already in the past; extend it before opening",
            ));
        }
        period.status = "open".to_string();
        period.updated_at = now();
        state.periods.insert(period.id.clone(), period.clone());
        state.emit(
            "admissions.period_opened",
            json!({ "period_id": period.id }),
        );
        Ok(period)
    })();
    json::respond(result, debug)
}

pub(crate) async fn close_period(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<AdmissionsState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<AdmissionPeriod> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ADMISSIONS_MANAGE)?;
        let period_id = path.as_str().to_string();
        let mut period = state.period(&period_id)?;
        if period.status != "open" {
            return Err(UnifiedError::conflict(
                "Only an open period can be closed",
            ));
        }
        period.status = "closed".to_string();
        period.updated_at = now();
        state.periods.insert(period.id.clone(), period.clone());
        state.emit(
            "admissions.period_closed",
            json!({ "period_id": period.id }),
        );
        Ok(period)
    })();
    json::respond(result, debug)
}
