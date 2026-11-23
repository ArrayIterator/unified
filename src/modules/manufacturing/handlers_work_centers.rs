//! manufacturing - handlers work centers.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::pagination::PaginationParams;
use actix_web::{web, HttpRequest, HttpResponse};
use chrono::Datelike;
use serde_json::{json, Value};
use std::sync::Arc;
use super::{constants::*, types::*, inputs::*, helpers::*, store::*, validation::*, stock::*, module::*, hooks::*, handlers_boms::*, handlers_work_orders::*, handlers_runs::*, handlers_scrap::*, handlers_units::*, handlers_settings::*, reports::*};



// ---------------------------------------------------------------------------
// Work center handlers
// ---------------------------------------------------------------------------
pub(crate) async fn list_work_centers(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ManufacturingState>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_MFG_VIEW)?;
        let params = query.into_inner();
        let status_filter = params.filters.get("status").cloned();
        let search = params.search.clone().unwrap_or_default().to_lowercase();
        let mut items: Vec<WorkCenter> = state
            .work_centers
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|center| {
                if let Some(status) = &status_filter {
                    if center.status != *status {
                        return false;
                    }
                }
                if !search.is_empty() {
                    let haystack = format!("{} {}", center.name, center.code).to_lowercase();
                    if !haystack.contains(&search) {
                        return false;
                    }
                }
                true
            })
            .collect();
        let sort = params.allowed_sort(&["name", "code", "created_at"], "created_at");
        let descending = params.normalized().order == "desc";
        items.sort_by(|a, b| {
            let ordering = match sort.as_str() {
                "name" => a.name.to_lowercase().cmp(&b.name.to_lowercase()),
                "code" => a.code.to_lowercase().cmp(&b.code.to_lowercase()),
                _ => a.created_at.cmp(&b.created_at),
            };
            if descending {
                ordering.reverse()
            } else {
                ordering
            }
        });
        Ok(paginate(items, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}


pub(crate) async fn create_work_center(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ManufacturingState>,
    req: HttpRequest,
    body: web::Json<CreateWorkCenterRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<WorkCenter> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_MFG_MANAGE)?;
        let input = body.into_inner();
        let settings = state.settings();
        let name = require_text(&input.name, "name", MAX_NAME_LEN)?;
        let code = match &input.code {
            Some(raw) => require_text(raw, "code", MAX_CODE_LEN)?.to_uppercase(),
            None => format!("WC-{}", &new_id()[..8].to_uppercase()),
        };
        if state
            .work_centers
            .iter()
            .any(|entry| entry.value().code == code)
        {
            return Err(UnifiedError::conflict(format!(
                "Work center code already exists: {code}"
            )));
        }
        let capacity = match input.capacity_hours_per_day {
            Some(value) => valid_quantity(value, "capacity_hours_per_day")?,
            None => settings.default_capacity_hours_per_day,
        };
        let cost_per_hour = match input.cost_per_hour {
            Some(value) => Some(valid_non_negative(value, "cost_per_hour")?),
            None => None,
        };
        let currency = match &input.currency {
            Some(raw) => Some(valid_currency(raw)?),
            None => None,
        };
        let description = optional_text(&input.description, "description", MAX_NOTES_LEN)?;
        let center = WorkCenter {
            id: new_id(),
            name,
            code,
            capacity_hours_per_day: capacity,
            cost_per_hour,
            currency,
            status: "active".to_string(),
            description,
            created_at: now(),
            updated_at: now(),
        };
        state
            .work_centers
            .insert(center.id.clone(), center.clone());
        Ok(center)
    })();
    match result {
        Ok(data) => created(data),
        Err(error) => err(error, debug),
    }
}


pub(crate) async fn get_work_center(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ManufacturingState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<WorkCenter> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_MFG_VIEW)?;
        load_work_center(&state, &path.into_inner())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}


pub(crate) async fn update_work_center(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ManufacturingState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdateWorkCenterRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<WorkCenter> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_MFG_MANAGE)?;
        let input = body.into_inner();
        let mut center = load_work_center(&state, &path.into_inner())?;
        if let Some(name) = &input.name {
            center.name = require_text(name, "name", MAX_NAME_LEN)?;
        }
        if let Some(capacity) = input.capacity_hours_per_day {
            center.capacity_hours_per_day = valid_quantity(capacity, "capacity_hours_per_day")?;
        }
        if let Some(cost) = input.cost_per_hour {
            center.cost_per_hour = Some(valid_non_negative(cost, "cost_per_hour")?);
        }
        if let Some(currency) = &input.currency {
            center.currency = Some(valid_currency(currency)?);
        }
        if let Some(status) = &input.status {
            let status = status.trim().to_lowercase();
            if !["active", "inactive"].contains(&status.as_str()) {
                return Err(UnifiedError::bad_request(
                    "status must be active or inactive",
                ));
            }
            center.status = status;
        }
        if let Some(description) = &input.description {
            center.description =
                optional_text(&Some(description.clone()), "description", MAX_NOTES_LEN)?;
        }
        center.updated_at = now();
        state
            .work_centers
            .insert(center.id.clone(), center.clone());
        Ok(center)
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}


pub(crate) async fn delete_work_center(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ManufacturingState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_MFG_MANAGE)?;
        let center = load_work_center(&state, &path.into_inner())?;
        if state.bom_versions.iter().any(|entry| {
            entry
                .value()
                .routing
                .iter()
                .any(|step| step.work_center_id == center.id)
        }) {
            return Err(UnifiedError::conflict(
                "Work center cannot be deleted: BOM routings reference it",
            ));
        }
        state.work_centers.remove(&center.id);
        Ok(json!({ "deleted": true, "work_center_id": center.id }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}


pub(crate) async fn get_work_center_schedule(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ManufacturingState>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_MFG_VIEW)?;
        let params = query.into_inner();
        let center = load_work_center(&state, &path.into_inner())?;
        let today = now().date_naive();
        let from = params
            .filters
            .get("from")
            .and_then(|raw| parse_date_filter(raw))
            .unwrap_or(today);
        let to = params
            .filters
            .get("to")
            .and_then(|raw| parse_date_filter(raw))
            .unwrap_or(from);
        if to < from {
            return Err(UnifiedError::bad_request(
                "to must not be earlier than from",
            ));
        }
        if (to - from).num_days() > 366 {
            return Err(UnifiedError::bad_request(
                "Schedule range must be at most 366 days",
            ));
        }
        let days = work_center_schedule(&state, &center.id, from, to);
        Ok(json!({
            "work_center_id": center.id,
            "code": center.code,
            "capacity_hours_per_day": center.capacity_hours_per_day,
            "from": from.to_string(),
            "to": to.to_string(),
            "days": days,
        }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}
