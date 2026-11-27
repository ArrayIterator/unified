//! manufacturing - handlers units.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::pagination::PaginationParams;
use actix_web::{web, HttpRequest, HttpResponse};
use serde_json::{json, Value};
use std::sync::Arc;
use super::{constants::*, types::*, inputs::*, helpers::*, store::*, validation::*, stock::*, module::*, hooks::*, handlers_boms::*, handlers_work_centers::*, handlers_work_orders::*, handlers_runs::*, handlers_scrap::*, handlers_settings::*, reports::*};



// Unit handlers (admin definitions)
pub(crate) async fn list_units(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ManufacturingState>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_MFG_VIEW)?;
        state.seed_definitions();
        let params = query.into_inner();
        let mut items: Vec<UnitDefinition> = state
            .units
            .iter()
            .map(|entry| entry.value().clone())
            .collect();
        items.sort_by(|a, b| a.code.cmp(&b.code));
        Ok(paginate(items, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}


pub(crate) async fn create_unit(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ManufacturingState>,
    req: HttpRequest,
    body: web::Json<CreateUnitRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<UnitDefinition> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_MFG_ADMIN)?;
        let input = body.into_inner();
        let code = require_text(&input.code, "code", MAX_UNIT_LEN)?.to_lowercase();
        let label = require_text(&input.label, "label", MAX_LABEL_LEN)?;
        if state.units.iter().any(|entry| entry.value().code == code) {
            return Err(UnifiedError::conflict(format!(
                "Unit already exists: {code}"
            )));
        }
        let unit = UnitDefinition {
            id: new_id(),
            code: code.clone(),
            label,
            active: true,
            created_at: now(),
        };
        state.units.insert(unit.id.clone(), unit.clone());
        let mut settings = state.settings();
        if !settings.units.contains(&code) {
            settings.units.push(code);
            *state
                .settings
                .write()
                .expect("manufacturing settings lock poisoned") = settings;
        }
        Ok(unit)
    })();
    match result {
        Ok(data) => created(data),
        Err(error) => err(error, debug),
    }
}


pub(crate) async fn delete_unit(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ManufacturingState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_MFG_ADMIN)?;
        let id = path.into_inner();
        let unit = state
            .units
            .get(&id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found(format!("Unit not found: {id}")))?;
        if state.bom_versions.iter().any(|entry| {
            entry
                .value()
                .lines
                .iter()
                .any(|line| line.unit == unit.code)
        }) {
            return Err(UnifiedError::conflict(
                "Unit cannot be deleted: BOM lines reference it",
            ));
        }
        state.units.remove(&unit.id);
        let mut settings = state.settings();
        settings.units.retain(|u| u != &unit.code);
        *state
            .settings
            .write()
            .expect("manufacturing settings lock poisoned") = settings;
        Ok(json!({ "deleted": true, "unit_id": unit.id }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}
