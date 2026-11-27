//! manufacturing - handlers scrap.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::pagination::PaginationParams;
use actix_web::{web, HttpRequest, HttpResponse};
use serde_json::{json, Value};
use std::sync::Arc;
use super::{constants::*, types::*, inputs::*, helpers::*, store::*, validation::*, stock::*, module::*, hooks::*, handlers_boms::*, handlers_work_centers::*, handlers_work_orders::*, handlers_runs::*, handlers_units::*, handlers_settings::*, reports::*};



// Scrap reason handlers (admin definitions)
pub(crate) async fn list_scrap_reasons(
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
        let active_filter = params.filters.get("active").cloned();
        let search = params.search.clone().unwrap_or_default().to_lowercase();
        let mut items: Vec<ScrapReason> = state
            .scrap_reasons
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|reason| {
                if let Some(active) = &active_filter {
                    let want = active == "true";
                    if reason.active != want {
                        return false;
                    }
                }
                if !search.is_empty() {
                    let haystack = format!("{} {}", reason.code, reason.label).to_lowercase();
                    if !haystack.contains(&search) {
                        return false;
                    }
                }
                true
            })
            .collect();
        items.sort_by(|a, b| a.code.cmp(&b.code));
        Ok(paginate(items, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}


pub(crate) async fn create_scrap_reason(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ManufacturingState>,
    req: HttpRequest,
    body: web::Json<CreateScrapReasonRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<ScrapReason> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_MFG_ADMIN)?;
        let input = body.into_inner();
        let code = require_text(&input.code, "code", MAX_CODE_LEN)?.to_lowercase();
        let label = require_text(&input.label, "label", MAX_LABEL_LEN)?;
        if state
            .scrap_reasons
            .iter()
            .any(|entry| entry.value().code == code)
        {
            return Err(UnifiedError::conflict(format!(
                "Scrap reason code already exists: {code}"
            )));
        }
        let description = optional_text(&input.description, "description", MAX_NOTES_LEN)?;
        let reason = ScrapReason {
            id: new_id(),
            code,
            label,
            active: true,
            description,
            created_at: now(),
            updated_at: now(),
        };
        state
            .scrap_reasons
            .insert(reason.id.clone(), reason.clone());
        Ok(reason)
    })();
    match result {
        Ok(data) => created(data),
        Err(error) => err(error, debug),
    }
}


pub(crate) async fn update_scrap_reason(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ManufacturingState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdateScrapReasonRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<ScrapReason> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_MFG_ADMIN)?;
        let input = body.into_inner();
        let mut reason = load_scrap_reason(&state, &path.into_inner())?;
        if let Some(label) = &input.label {
            reason.label = require_text(label, "label", MAX_LABEL_LEN)?;
        }
        if let Some(active) = input.active {
            reason.active = active;
        }
        if let Some(description) = &input.description {
            reason.description =
                optional_text(&Some(description.clone()), "description", MAX_NOTES_LEN)?;
        }
        reason.updated_at = now();
        state
            .scrap_reasons
            .insert(reason.id.clone(), reason.clone());
        Ok(reason)
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}


pub(crate) async fn delete_scrap_reason(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ManufacturingState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_MFG_ADMIN)?;
        let reason = load_scrap_reason(&state, &path.into_inner())?;
        if state.production_runs.iter().any(|entry| {
            entry.value().scrap_reason_code_id.as_deref() == Some(reason.id.as_str())
        }) {
            return Err(UnifiedError::conflict(
                "Scrap reason cannot be deleted: production runs reference it",
            ));
        }
        state.scrap_reasons.remove(&reason.id);
        Ok(json!({ "deleted": true, "scrap_reason_id": reason.id }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}
