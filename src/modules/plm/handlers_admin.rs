//! HTTP handlers for per-tenant PLM settings and the audit
//! trail.

use actix_web::{web, HttpRequest, HttpResponse};
use serde_json::Value;
use std::sync::Arc;
use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::pagination::PaginationParams;

use super::helpers::{
    caller, caller_company, err, ok, paginate, require_permission, require_slug, require_tenant,
    resolve_tenant,
};
use super::state::PlmState;
use super::types::{
    PlmAuditEntry, PlmSettings, UpdateSettingsRequest, MAX_ECO_TYPES, PERM_PLM_MANAGE,
};

// ---------------------------------------------------------------------------
// Settings handlers (authenticated)
// ---------------------------------------------------------------------------

pub(crate) async fn get_settings(
    state: web::Data<PlmState>,
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PlmSettings> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let _tenant_id = resolve_tenant(&req)?;
        let _caller = caller(&req)?;
        Ok(state.current_settings())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn update_settings(
    state: web::Data<PlmState>,
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    body: web::Json<UpdateSettingsRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PlmSettings> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let _tenant_id = resolve_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PLM_MANAGE)?;
        let input = body.into_inner();
        let mut settings = state.current_settings();
        if let Some(eco_types) = &input.eco_types {
            if eco_types.is_empty() || eco_types.len() > MAX_ECO_TYPES {
                return Err(UnifiedError::bad_request(format!(
                    "eco_types must have between 1 and {MAX_ECO_TYPES} entries"
                )));
            }
            let mut validated = Vec::with_capacity(eco_types.len());
            for raw in eco_types {
                validated.push(require_slug(raw, "eco_types entry")?);
            }
            validated.sort();
            validated.dedup();
            settings.eco_types = validated;
        }
        if let Some(require_eco) = input.require_eco_for_bom {
            settings.require_eco_for_bom = require_eco;
        }
        state.update_settings(settings.clone());
        Ok(settings)
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

// ---------------------------------------------------------------------------
// Audit handlers (authenticated)
// ---------------------------------------------------------------------------

pub(crate) async fn list_audit(
    state: web::Data<PlmState>,
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let tenant_id = resolve_tenant(&req)?;
        let _caller = caller(&req)?;
        let params = query.into_inner();
        let entity_type_filter = params.filters.get("entity_type").cloned();
        let entity_id_filter = params.filters.get("entity_id").cloned();
        let action_filter = params.filters.get("action").cloned();
        let search = params.search.clone().unwrap_or_default().to_lowercase();
        let mut items: Vec<PlmAuditEntry> = state
            .audit
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|entry| {
                if entry.tenant_id != tenant_id {
                    return false;
                }
                if let Some(entity_type) = &entity_type_filter {
                    if entry.entity_type != *entity_type {
                        return false;
                    }
                }
                if let Some(entity_id) = &entity_id_filter {
                    if entry.entity_id.to_string() != *entity_id {
                        return false;
                    }
                }
                if let Some(action) = &action_filter {
                    if entry.action != *action {
                        return false;
                    }
                }
                if !search.is_empty() {
                    let haystack = format!(
                        "{} {} {}",
                        entry.action,
                        entry.actor_id,
                        entry.comment.clone().unwrap_or_default()
                    )
                    .to_lowercase();
                    if !haystack.contains(&search) {
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
