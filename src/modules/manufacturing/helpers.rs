//! manufacturing - helpers.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::json;
use unified_core::pagination::{PageResult, PaginationParams};
use unified_core::tenancy::TenantContext as TenancyContext;
use actix_web::{ HttpRequest, HttpResponse };
use chrono::{DateTime, Datelike, NaiveDate, Utc};
use dashmap::DashMap;
use serde::Serialize;
use serde_json::json;
use uuid::Uuid;



// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------
pub(crate) fn new_id() -> String {
    Uuid::new_v4().to_string()
}


pub(crate) fn now() -> DateTime<Utc> {
    Utc::now()
}


pub(crate) fn round_money(value: f64) -> f64 {
    (value * 100.0).round() / 100.0
}


/// Caller identity: the server-verified caller (user id plus
/// server-resolved role, both required for capability checks),
/// exactly like the assets and POS modules.
pub(crate) struct Caller {
    pub(crate) actor: String,
    pub(crate) role: String,
}


/// Resolve the tenant context for a request. THE single
/// convergence point for tenant-scoped data access in this file
/// (see `unified_core::tenancy`): this function returns the tenancy
/// `TenantContext` (tenant id as Uuid + schema name derived only
/// by `tenancy::schema_name()`), and the Diesel persistence wiring
/// - when it lands - takes the Uuid from here into
/// `tenancy::with_tenant_connection`. Fail closed:
/// `from_http_request` answers 401 when the request did not
/// resolve to a tenant and 403 when the resolved tenant id is not
/// a valid UUID; no resolved tenant means the request is refused
/// here, never served from `public`.
///
/// OPEN (not closed by this guard): the in-memory DashMap stores
/// in this module carry no `tenant_id` on their records yet, so
/// there is no per-record tenant filtering to apply once the
/// context is resolved - that needs a store-shape change (tenant
/// id on every record + a migration column) and is integrator
/// work. What this guard does close is the context requirement
/// itself: every tenant-data endpoint resolves the tenant first
/// through `caller()`, and a request with no tenant context is
/// rejected before any business data is read.
pub(crate) fn resolve_tenant(req: &HttpRequest) -> UnifiedResult<TenancyContext> {
    TenancyContext::from_http_request(req)
}


pub(crate) fn caller(req: &HttpRequest) -> UnifiedResult<Caller> {
    // Tenant first: no resolved tenant context means fail closed,
    // never fall back to the public schema. The resolved context
    // is bound for the Diesel persistence layer (see
    // `resolve_tenant`); the in-memory stores below carry no
    // tenant id yet, so per-record filtering is still open.
    let _tenant = resolve_tenant(req)?;
    let verified = unified_core::caller::require_caller(req)?;
    let actor = verified.user_id.clone();
    let role = verified.role().to_string();
    Ok(Caller { actor, role })
}


/// Ask the roles module whether `role` holds `permission` through
/// the `role.check` hook. Fails closed: no answer means not allowed.
pub(crate) fn check_capability(ctx: &AppContext, role: &str, permission: &str) -> bool {
    let context = ctx.hooks.emit(
        "role.check",
        json!({ "role": role, "permission": permission, "allowed": false }),
    );
    context
        .get("allowed")
        .and_then(|value| value.as_bool())
        .unwrap_or(false)
}


pub(crate) fn require_permission(ctx: &AppContext, role: &str, permission: &str) -> UnifiedResult<()> {
    if check_capability(ctx, role, permission) {
        Ok(())
    } else {
        Err(UnifiedError::forbidden(format!(
            "Role \"{role}\" does not hold permission \"{permission}\""
        )))
    }
}


pub(crate) fn ok<T: Serialize>(data: T) -> HttpResponse {
    json::success_response(data, 200)
}


pub(crate) fn created<T: Serialize>(data: T) -> HttpResponse {
    json::success_response(data, 201)
}


pub(crate) fn err(error: UnifiedError, debug: bool) -> HttpResponse {
    json::error_response(&error, debug)
}


pub(crate) fn paginate<T: Serialize + Clone>(items: Vec<T>, params: &PaginationParams) -> PageResult<T> {
    let pagination = params.normalized();
    let total = items.len() as u64;
    let start = ((pagination.page - 1) * pagination.per_page) as usize;
    let page_items: Vec<T> = items
        .into_iter()
        .skip(start)
        .take(pagination.per_page as usize)
        .collect();
    PageResult::new(page_items, total, &pagination)
}


pub(crate) fn require_text(value: &str, field: &str, max: usize) -> UnifiedResult<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err(UnifiedError::bad_request(format!("{field} must not be empty")));
    }
    if trimmed.len() > max {
        return Err(UnifiedError::bad_request(format!(
            "{field} must be at most {max} characters"
        )));
    }
    Ok(trimmed.to_string())
}


pub(crate) fn optional_text(value: &Option<String>, field: &str, max: usize) -> UnifiedResult<Option<String>> {
    match value {
        None => Ok(None),
        Some(text) => {
            let trimmed = text.trim();
            if trimmed.is_empty() {
                return Ok(None);
            }
            if trimmed.len() > max {
                return Err(UnifiedError::bad_request(format!(
                    "{field} must be at most {max} characters"
                )));
            }
            Ok(Some(trimmed.to_string()))
        }
    }
}


pub(crate) fn valid_quantity(value: f64, field: &str) -> UnifiedResult<f64> {
    if !value.is_finite() || value <= 0.0 {
        return Err(UnifiedError::bad_request(format!(
            "{field} must be a finite number greater than zero"
        )));
    }
    Ok(value)
}


pub(crate) fn valid_non_negative(value: f64, field: &str) -> UnifiedResult<f64> {
    if !value.is_finite() || value < 0.0 {
        return Err(UnifiedError::bad_request(format!(
            "{field} must be a finite number at least zero"
        )));
    }
    Ok(value)
}


pub(crate) fn valid_percent(value: f64, field: &str) -> UnifiedResult<f64> {
    if !value.is_finite() || value < 0.0 || value > 100.0 {
        return Err(UnifiedError::bad_request(format!(
            "{field} must be between 0 and 100"
        )));
    }
    Ok(value)
}


pub(crate) fn valid_currency(currency: &str) -> UnifiedResult<String> {
    let code = currency.trim().to_uppercase();
    if code.len() != 3 || !code.chars().all(|c| c.is_ascii_alphabetic()) {
        return Err(UnifiedError::bad_request(
            "currency must be a 3-letter ISO 4217 code",
        ));
    }
    Ok(code)
}


pub(crate) fn parse_date_filter(value: &str) -> Option<NaiveDate> {
    NaiveDate::parse_from_str(value.trim(), "%Y-%m-%d").ok()
}


pub(crate) fn in_period(ts: DateTime<Utc>, from: Option<NaiveDate>, to: Option<NaiveDate>) -> bool {
    let date = ts.date_naive();
    if let Some(from) = from {
        if date < from {
            return false;
        }
    }
    if let Some(to) = to {
        if date > to {
            return false;
        }
    }
    true
}
