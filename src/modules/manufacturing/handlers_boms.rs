//! manufacturing - handlers boms.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::pagination::PaginationParams;
use actix_web::{web, HttpRequest, HttpResponse};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::Arc;
use super::{constants::*, types::*, inputs::*, helpers::*, store::*, validation::*, stock::*, module::*, hooks::*, handlers_work_centers::*, handlers_work_orders::*, handlers_runs::*, handlers_scrap::*, handlers_units::*, handlers_settings::*, reports::*};



// BOM handlers
pub(crate) async fn list_boms(
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
        let product_filter = params.filters.get("product_id").cloned();
        let search = params.search.clone().unwrap_or_default().to_lowercase();
        let mut items: Vec<Bom> = state
            .boms
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|bom| {
                if let Some(status) = &status_filter {
                    if bom.status != *status {
                        return false;
                    }
                }
                if let Some(product_id) = &product_filter {
                    if bom.product_id != *product_id {
                        return false;
                    }
                }
                if !search.is_empty() {
                    let haystack = format!("{} {} {}", bom.name, bom.code, bom.product_id)
                        .to_lowercase();
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


pub(crate) async fn create_bom(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ManufacturingState>,
    req: HttpRequest,
    body: web::Json<CreateBomRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Bom> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_MFG_MANAGE)?;
        let input = body.into_inner();
        let settings = state.settings();
        let product_id = require_text(&input.product_id, "product_id", MAX_CODE_LEN)?;
        let name = require_text(&input.name, "name", MAX_NAME_LEN)?;
        let code = match &input.code {
            Some(raw) => require_text(raw, "code", MAX_CODE_LEN)?.to_uppercase(),
            None => format!("BOM-{}", &new_id()[..8].to_uppercase()),
        };
        if state.boms.iter().any(|entry| entry.value().code == code) {
            return Err(UnifiedError::conflict(format!(
                "BOM code already exists: {code}"
            )));
        }
        let description = optional_text(&input.description, "description", MAX_NOTES_LEN)?;
        let lines = validate_lines(&state, &input.lines, &settings)?;
        let routing = validate_routing(&state, &input.routing)?;
        let bom = Bom {
            id: new_id(),
            product_id,
            name,
            code,
            description,
            status: "active".to_string(),
            active_version_id: None,
            latest_version: 1,
            created_by: caller.actor.clone(),
            created_at: now(),
            updated_at: now(),
        };
        let version = create_version(&state, &bom.id, 1, lines, routing, &caller.actor);
        let mut bom = bom;
        bom.active_version_id = Some(version.id.clone());
        save_bom(&state, &bom);
        state.emit(
            EVENT_BOM_CREATED,
            json!({
                "bom_id": bom.id,
                "product_id": bom.product_id,
                "code": bom.code,
                "version_id": version.id,
                "version": 1,
                "created_by": caller.actor,
            }),
        );
        Ok(bom)
    })();
    match result {
        Ok(data) => created(data),
        Err(error) => err(error, debug),
    }
}


pub(crate) fn bom_with_version(state: &ManufacturingState, bom: Bom) -> Value {
    let version = bom
        .active_version_id
        .as_ref()
        .and_then(|id| load_bom_version(state, id).ok());
    json!({ "bom": bom, "active_version": version })
}


pub(crate) async fn get_bom(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ManufacturingState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_MFG_VIEW)?;
        let bom = load_bom(&state, &path.into_inner())?;
        Ok(bom_with_version(&state, bom))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}


pub(crate) async fn update_bom(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ManufacturingState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdateBomRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_MFG_MANAGE)?;
        let input = body.into_inner();
        let id = path.into_inner();
        let mut bom = load_bom(&state, &id)?;
        let settings = state.settings();
        if let Some(name) = &input.name {
            bom.name = require_text(name, "name", MAX_NAME_LEN)?;
        }
        if let Some(description) = &input.description {
            bom.description = optional_text(&Some(description.clone()), "description", MAX_NOTES_LEN)?;
        }
        if let Some(status) = &input.status {
            let status = status.trim().to_lowercase();
            if !["active", "draft", "archived"].contains(&status.as_str()) {
                return Err(UnifiedError::bad_request(
                    "status must be active, draft, or archived",
                ));
            }
            bom.status = status;
        }
        // New lines (or routing) always create a new version.
        if let Some(lines_input) = &input.lines {
            let lines = validate_lines(&state, lines_input, &settings)?;
            let routing_input = input.routing.clone().unwrap_or_default();
            let routing = validate_routing(&state, &routing_input)?;
            let next = bom.latest_version + 1;
            let version = create_version(&state, &bom.id, next, lines, routing, &caller.actor);
            bom.latest_version = next;
            bom.active_version_id = Some(version.id.clone());
        } else if let Some(routing_input) = &input.routing {
            let current = bom
                .active_version_id
                .as_ref()
                .and_then(|vid| load_bom_version(&state, vid).ok())
                .ok_or_else(|| {
                    UnifiedError::conflict("BOM has no active version to base routing on")
                })?;
            let routing = validate_routing(&state, routing_input)?;
            let next = bom.latest_version + 1;
            let version = create_version(
                &state,
                &bom.id,
                next,
                current.lines.clone(),
                routing,
                &caller.actor,
            );
            bom.latest_version = next;
            bom.active_version_id = Some(version.id.clone());
        }
        bom.updated_at = now();
        save_bom(&state, &bom);
        Ok(bom_with_version(&state, bom))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}


pub(crate) async fn delete_bom(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ManufacturingState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_MFG_MANAGE)?;
        let id = path.into_inner();
        let bom = load_bom(&state, &id)?;
        if state
            .work_orders
            .iter()
            .any(|entry| entry.value().bom_id == bom.id)
        {
            return Err(UnifiedError::conflict(
                "BOM cannot be deleted: work orders reference it",
            ));
        }
        state.boms.remove(&bom.id);
        state
            .bom_versions
            .retain(|_, version| version.bom_id != bom.id);
        Ok(json!({ "deleted": true, "bom_id": bom.id }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}


pub(crate) async fn list_bom_versions(
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
        let bom = load_bom(&state, &path.into_inner())?;
        let params = query.into_inner();
        let mut items: Vec<BomVersion> = state
            .bom_versions
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|version| version.bom_id == bom.id)
            .collect();
        items.sort_by_key(|version| std::cmp::Reverse(version.version));
        Ok(paginate(items, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}


pub(crate) async fn create_bom_version(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ManufacturingState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<CreateBomVersionRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<BomVersion> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_MFG_MANAGE)?;
        let input = body.into_inner();
        let mut bom = load_bom(&state, &path.into_inner())?;
        let settings = state.settings();
        let lines = validate_lines(&state, &input.lines, &settings)?;
        let routing = validate_routing(&state, &input.routing)?;
        let next = bom.latest_version + 1;
        let version = create_version(&state, &bom.id, next, lines, routing, &caller.actor);
        bom.latest_version = next;
        bom.active_version_id = Some(version.id.clone());
        bom.updated_at = now();
        save_bom(&state, &bom);
        Ok(version)
    })();
    match result {
        Ok(data) => created(data),
        Err(error) => err(error, debug),
    }
}


pub(crate) async fn get_bom_version(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ManufacturingState>,
    req: HttpRequest,
    path: web::Path<(String, String)>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<BomVersion> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_MFG_VIEW)?;
        let (bom_id, version_id) = path.into_inner();
        let version = load_bom_version(&state, &version_id)?;
        if version.bom_id != bom_id {
            return Err(UnifiedError::not_found(format!(
                "BOM version not found: {version_id}"
            )));
        }
        Ok(version)
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}


pub(crate) async fn activate_bom_version(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ManufacturingState>,
    req: HttpRequest,
    path: web::Path<(String, String)>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_MFG_MANAGE)?;
        let (bom_id, version_id) = path.into_inner();
        let mut bom = load_bom(&state, &bom_id)?;
        let version = load_bom_version(&state, &version_id)?;
        if version.bom_id != bom.id {
            return Err(UnifiedError::not_found(format!(
                "BOM version not found: {version_id}"
            )));
        }
        bom.active_version_id = Some(version.id.clone());
        bom.updated_at = now();
        save_bom(&state, &bom);
        Ok(bom_with_version(&state, bom))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}


pub(crate) async fn explode_bom(
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
        let bom = load_bom(&state, &path.into_inner())?;
        let quantity = params
            .filters
            .get("quantity")
            .and_then(|raw| raw.parse::<f64>().ok())
            .unwrap_or(1.0);
        let quantity = valid_quantity(quantity, "quantity")?;
        let version_id = params
            .filters
            .get("version_id")
            .cloned()
            .or(bom.active_version_id.clone())
            .ok_or_else(|| UnifiedError::conflict("BOM has no active version"))?;
        let version = load_bom_version(&state, &version_id)?;
        if version.bom_id != bom.id {
            return Err(UnifiedError::not_found(format!(
                "BOM version not found: {version_id}"
            )));
        }
        let settings = state.settings();
        let tree = explode_version(&state, &version, quantity, &settings)?;
        let mut totals: HashMap<String, (f64, String)> = HashMap::new();
        flatten_explosion(&tree, &mut totals);
        let mut flat: Vec<Value> = totals
            .into_iter()
            .map(|(product_id, (total_quantity, unit))| {
                json!({ "product_id": product_id, "quantity": total_quantity, "unit": unit })
            })
            .collect();
        flat.sort_by(|a, b| {
            a.get("product_id")
                .and_then(|v| v.as_str())
                .cmp(&b.get("product_id").and_then(|v| v.as_str()))
        });
        Ok(json!({
            "bom_id": bom.id,
            "product_id": bom.product_id,
            "bom_version": version.version,
            "quantity": quantity,
            "tree": tree,
            "totals": flat,
        }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}
