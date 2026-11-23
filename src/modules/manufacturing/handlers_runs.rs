//! manufacturing - handlers runs.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::pagination::PaginationParams;
use actix_web::{web, HttpRequest, HttpResponse};
use serde_json::Value;
use std::sync::Arc;
use super::{constants::*, types::*, inputs::*, helpers::*, store::*, validation::*, stock::*, module::*, hooks::*, handlers_boms::*, handlers_work_centers::*, handlers_work_orders::*, handlers_scrap::*, handlers_units::*, handlers_settings::*, reports::*};



// ---------------------------------------------------------------------------
// Production run and reservation handlers
// ---------------------------------------------------------------------------
pub(crate) async fn list_production_runs(
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
        let order_filter = params.filters.get("work_order_id").cloned();
        let status_filter = params.filters.get("status").cloned();
        let mut items: Vec<ProductionRun> = state
            .production_runs
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|run| {
                if let Some(order_id) = &order_filter {
                    if run.work_order_id != *order_id {
                        return false;
                    }
                }
                if let Some(status) = &status_filter {
                    if run.status != *status {
                        return false;
                    }
                }
                true
            })
            .collect();
        items.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        Ok(paginate(items, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}


pub(crate) async fn get_production_run(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ManufacturingState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<ProductionRun> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_MFG_VIEW)?;
        state
            .production_runs
            .get(&path.into_inner())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Production run not found"))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}


pub(crate) async fn list_reservations(
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
        let order_filter = params.filters.get("work_order_id").cloned();
        let component_filter = params.filters.get("component_product_id").cloned();
        let status_filter = params.filters.get("status").cloned();
        let mut items: Vec<MaterialReservation> = state
            .reservations
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|reservation| {
                if let Some(order_id) = &order_filter {
                    if reservation.work_order_id != *order_id {
                        return false;
                    }
                }
                if let Some(component) = &component_filter {
                    if reservation.component_product_id != *component {
                        return false;
                    }
                }
                if let Some(status) = &status_filter {
                    if reservation.status != *status {
                        return false;
                    }
                }
                true
            })
            .collect();
        items.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        Ok(paginate(items, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}
