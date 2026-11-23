//! HTTP handlers: positions.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::{HookBus, HookContext, HookHandler};
use unified_core::json;
use unified_core::modules::{Module, ModuleRegistry, RouteInfo};
use unified_core::pagination::{PageResult, PaginationParams};
use actix_web::{web, HttpRequest, HttpResponse};
use chrono::{DateTime, NaiveDate, Utc};
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use uuid::Uuid;

use super::*;

// ---------------------------------------------------------------------------
// Handlers: positions
// ---------------------------------------------------------------------------

pub(crate) async fn list_positions(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<HrState>>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PageResult<Value>> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_HR_READ)?;
        let params = query.into_inner();
        let mut items: Vec<Value> = state
            .positions
            .iter()
            .map(|entry| position_view(&ctx, &caller, entry.value()))
            .filter(|position| {
                let title = position
                    .get("title")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                matches_search(title, &params)
                    && params
                        .filters
                        .get("status")
                        .is_none_or(|status| {
                            position.get("status").and_then(|v| v.as_str()) == Some(status.as_str())
                        })
                    && params.filters.get("department_id").is_none_or(|department| {
                        position.get("department_id").and_then(|v| v.as_str())
                            == Some(department.as_str())
                    })
            })
            .collect();
        items.sort_by(|a, b| {
            a.get("title")
                .and_then(|v| v.as_str())
                .cmp(&b.get("title").and_then(|v| v.as_str()))
        });
        if params.normalized().order == "desc" {
            items.reverse();
        }
        Ok(paginate(items, &params))
    })();
    match result {
        Ok(page) => json::success_response(page, 200),
        Err(error) => json::error_response(&error, debug),
    }
}

pub(crate) async fn create_position(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<HrState>>,
    req: HttpRequest,
    body: web::Json<CreatePosition>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Position> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_HR_MANAGE)?;
        let input = body.into_inner();
        let title = require_text(&input.title, "title", 255)?;
        let salary_band_min = match input.salary_band_min {
            Some(value) => Some(parse_money(value, "salary_band_min")?),
            None => None,
        };
        let salary_band_max = match input.salary_band_max {
            Some(value) => Some(parse_money(value, "salary_band_max")?),
            None => None,
        };
        if let (Some(min), Some(max)) = (salary_band_min, salary_band_max) {
            if min > max {
                return Err(UnifiedError::bad_request(
                    "salary_band_min must not exceed salary_band_max",
                ));
            }
        }
        let position = Position {
            id: new_id(),
            title,
            department_id: input
                .department_id
                .map(|value| value.trim().to_string())
                .filter(|value| !value.is_empty()),
            level: optional_text(&input.level, "level", 128)?,
            salary_band_min,
            salary_band_max,
            status: "active".to_string(),
            created_at: now(),
            updated_at: now(),
        };
        state.positions.insert(position.id.clone(), position.clone());
        Ok(position)
    })();
    match result {
        Ok(data) => json::success_response(data, 201),
        Err(error) => json::error_response(&error, debug),
    }
}

pub(crate) async fn get_position(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<HrState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_HR_READ)?;
        let position = state
            .positions
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Position not found"))?;
        Ok(position_view(&ctx, &caller, &position))
    })();
    json::respond(result, debug)
}

pub(crate) async fn update_position(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<HrState>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdatePosition>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Position> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_HR_MANAGE)?;
        let input = body.into_inner();
        if let Some(status) = &input.status {
            if !matches!(status.as_str(), "active" | "inactive") {
                return Err(UnifiedError::bad_request(
                    "status must be one of: active, inactive",
                ));
            }
        }
        let salary_band_min = match input.salary_band_min {
            Some(Some(value)) => Some(Some(parse_money(value, "salary_band_min")?)),
            Some(None) => Some(None),
            None => None,
        };
        let salary_band_max = match input.salary_band_max {
            Some(Some(value)) => Some(Some(parse_money(value, "salary_band_max")?)),
            Some(None) => Some(None),
            None => None,
        };
        let mut position = state
            .positions
            .get_mut(path.as_str())
            .ok_or_else(|| UnifiedError::not_found("Position not found"))?;
        // Validate the new band before writing anything: the entry is
        // a live reference, so a failed check after assignment would
        // leave the bad values stored.
        let new_min = salary_band_min.unwrap_or(position.salary_band_min);
        let new_max = salary_band_max.unwrap_or(position.salary_band_max);
        if let (Some(min), Some(max)) = (new_min, new_max) {
            if min > max {
                return Err(UnifiedError::bad_request(
                    "salary_band_min must not exceed salary_band_max",
                ));
            }
        }
        if let Some(title) = input.title {
            position.title = require_text(&title, "title", 255)?;
        }
        if let Some(department_id) = input.department_id {
            position.department_id = if department_id.trim().is_empty() {
                None
            } else {
                Some(department_id.trim().to_string())
            };
        }
        if let Some(level) = input.level {
            position.level = match level {
                Some(value) => Some(require_text(&value, "level", 128)?),
                None => None,
            };
        }
        position.salary_band_min = new_min;
        position.salary_band_max = new_max;
        if let Some(status) = input.status {
            position.status = status;
        }
        position.updated_at = now();
        Ok(position.clone())
    })();
    json::respond(result, debug)
}

pub(crate) async fn delete_position(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<HrState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_HR_MANAGE)?;
        let position_id = path.as_str().to_string();
        if state
            .employees
            .iter()
            .any(|entry| entry.value().position_id.as_deref() == Some(position_id.as_str()))
        {
            return Err(UnifiedError::conflict(
                "Position is still assigned to employees; reassign them first",
            ));
        }
        state
            .positions
            .remove(position_id.as_str())
            .map(|_| json!({ "deleted": true, "id": position_id }))
            .ok_or_else(|| UnifiedError::not_found("Position not found"))
    })();
    json::respond(result, debug)
}
