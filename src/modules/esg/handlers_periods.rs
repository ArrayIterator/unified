//! HTTP handlers for reporting periods: period definitions
//! with a code, a date range, and an open / closed status.
//! Entries and targets may only be written while a period is
//! open; closing freezes the period and emits
//! `esg.period.closed`.

use actix_web::{HttpRequest, HttpResponse, web};
use chrono::Utc;
use serde_json::{Value, json};
use std::sync::Arc;
use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::pagination::PaginationParams;
use uuid::Uuid;

use super::helpers::{
    caller, created, err, load_period, ok, paginate, record_audit, require_manage, require_text,
    require_view, require_tenant, resolve_tenant, validate_period_input,
};
use super::state::EsgState;
use super::types::{
    CreatePeriodRequest, EVENT_PERIOD_CLOSED, MAX_CODE_LEN, MAX_NAME_LEN, Period, PeriodStatus,
    UpdatePeriodRequest,
};

/// List periods (paginated, filter by status; search by code
/// / name).
pub(crate) async fn list_periods(
    state: web::Data<EsgState>,
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let tenant_id = resolve_tenant(&req)?;
        let caller = caller(&req)?;
        require_view(&ctx, &caller.role)?;
        let params = query.into_inner();
        let status_filter = params
            .filters
            .get("status")
            .and_then(|raw| PeriodStatus::from_str(raw));
        let search = params.search.clone().map(|needle| needle.to_lowercase());
        let mut items = state.periods_of(&tenant_id);
        items.retain(|period| {
            if let Some(status) = status_filter {
                if period.status != status {
                    return false;
                }
            }
            if let Some(needle) = &search {
                if !period.code.to_lowercase().contains(needle)
                    && !period
                        .name
                        .as_deref()
                        .unwrap_or("")
                        .to_lowercase()
                        .contains(needle)
                {
                    return false;
                }
            }
            true
        });
        let views: Vec<Value> = items
            .iter()
            .map(|item| serde_json::to_value(item).unwrap_or(Value::Null))
            .collect();
        Ok(paginate(views, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

/// Create a reporting period (manager). The code must be
/// unique per tenant.
pub(crate) async fn create_period(
    state: web::Data<EsgState>,
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    body: web::Json<CreatePeriodRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let tenant_id = resolve_tenant(&req)?;
        let caller = caller(&req)?;
        require_manage(&ctx, &caller.role)?;
        let input = body.into_inner();
        let (code, name, starts_on, ends_on) =
            validate_period_input(&input.code, &input.name, &input.starts_on, &input.ends_on)?;
        if state
            .periods_of(&tenant_id)
            .iter()
            .any(|period| period.code == code)
        {
            return Err(UnifiedError::bad_request(format!(
                "A period with code \"{code}\" already exists"
            )));
        }
        let now = Utc::now();
        let period = Period {
            id: Uuid::new_v4(),
            tenant_id: tenant_id.clone(),
            code,
            name,
            starts_on,
            ends_on,
            status: PeriodStatus::Open,
            created_by: caller.actor.clone(),
            created_at: now,
            updated_at: now,
        };
        state.periods.insert(period.id, period.clone());
        Ok(serde_json::to_value(&period).unwrap_or(Value::Null))
    })();
    match result {
        Ok(data) => created(data),
        Err(error) => err(error, debug),
    }
}

/// Get one reporting period.
pub(crate) async fn get_period(
    state: web::Data<EsgState>,
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let tenant_id = resolve_tenant(&req)?;
        let caller = caller(&req)?;
        require_view(&ctx, &caller.role)?;
        let period = load_period(&state, &tenant_id, &path.into_inner())?;
        Ok(serde_json::to_value(&period).unwrap_or(Value::Null))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

/// Update a reporting period (manager). The date range must
/// stay valid; the status is changed only through the
/// close / reopen endpoints, never here.
pub(crate) async fn update_period(
    state: web::Data<EsgState>,
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    path: web::Path<String>,
    body: web::Json<UpdatePeriodRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let tenant_id = resolve_tenant(&req)?;
        let caller = caller(&req)?;
        require_manage(&ctx, &caller.role)?;
        let input = body.into_inner();
        let mut period = load_period(&state, &tenant_id, &path.into_inner())?;
        if let Some(code) = &input.code {
            let code = require_text(code, "code", MAX_CODE_LEN)?;
            if code != period.code
                && state
                    .periods_of(&tenant_id)
                    .iter()
                    .any(|other| other.code == code)
            {
                return Err(UnifiedError::bad_request(format!(
                    "A period with code \"{code}\" already exists"
                )));
            }
            period.code = code;
        }
        if let Some(name) = &input.name {
            period.name = match name {
                Some(text) => Some(require_text(text, "name", MAX_NAME_LEN)?),
                None => None,
            };
        }
        if input.starts_on.is_some() || input.ends_on.is_some() {
            let start_text = input
                .starts_on
                .clone()
                .unwrap_or_else(|| period.starts_on.format("%Y-%m-%d").to_string());
            let end_text = input
                .ends_on
                .clone()
                .unwrap_or_else(|| period.ends_on.format("%Y-%m-%d").to_string());
            let (start, end) = super::types::validate_period_range(&start_text, &end_text)
                .map_err(UnifiedError::bad_request)?;
            period.starts_on = start;
            period.ends_on = end;
        }
        period.updated_at = Utc::now();
        state.periods.insert(period.id, period.clone());
        Ok(serde_json::to_value(&period).unwrap_or(Value::Null))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

/// Delete a reporting period (manager). Entries, history,
/// and targets of the period are kept: the reporting history
/// stays auditable after the period definition is gone.
pub(crate) async fn delete_period(
    state: web::Data<EsgState>,
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let tenant_id = resolve_tenant(&req)?;
        let caller = caller(&req)?;
        require_manage(&ctx, &caller.role)?;
        let period = load_period(&state, &tenant_id, &path.into_inner())?;
        state.periods.remove(&period.id);
        Ok(json!({ "deleted": true, "period_id": period.id.to_string() }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

/// Close a reporting period (manager): entries and targets
/// can no longer be written for it. Emits
/// `esg.period.closed` and writes an audit row.
pub(crate) async fn close_period(
    state: web::Data<EsgState>,
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let tenant_id = resolve_tenant(&req)?;
        let caller = caller(&req)?;
        require_manage(&ctx, &caller.role)?;
        let mut period = load_period(&state, &tenant_id, &path.into_inner())?;
        if period.status == PeriodStatus::Closed {
            return Err(UnifiedError::bad_request(format!(
                "Period {} is already closed",
                period.code
            )));
        }
        let before = serde_json::to_value(&period).ok();
        period.status = PeriodStatus::Closed;
        period.updated_at = Utc::now();
        state.periods.insert(period.id, period.clone());
        let after = serde_json::to_value(&period).ok();
        record_audit(
            &state,
            &tenant_id,
            "period",
            &period.id.to_string(),
            "closed",
            &caller.actor,
            before,
            after,
        );
        state.emit(
            EVENT_PERIOD_CLOSED,
            json!({
                "tenant_id": tenant_id,
                "period_id": period.id.to_string(),
                "period_code": period.code,
                "closed_by": caller.actor,
            }),
        );
        Ok(serde_json::to_value(&period).unwrap_or(Value::Null))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

/// Reopen a closed reporting period (manager): entries and
/// targets can be written again. Writes an audit row.
pub(crate) async fn reopen_period(
    state: web::Data<EsgState>,
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let tenant_id = resolve_tenant(&req)?;
        let caller = caller(&req)?;
        require_manage(&ctx, &caller.role)?;
        let mut period = load_period(&state, &tenant_id, &path.into_inner())?;
        if period.status == PeriodStatus::Open {
            return Err(UnifiedError::bad_request(format!(
                "Period {} is already open",
                period.code
            )));
        }
        let before = serde_json::to_value(&period).ok();
        period.status = PeriodStatus::Open;
        period.updated_at = Utc::now();
        state.periods.insert(period.id, period.clone());
        let after = serde_json::to_value(&period).ok();
        record_audit(
            &state,
            &tenant_id,
            "period",
            &period.id.to_string(),
            "reopened",
            &caller.actor,
            before,
            after,
        );
        Ok(serde_json::to_value(&period).unwrap_or(Value::Null))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}
