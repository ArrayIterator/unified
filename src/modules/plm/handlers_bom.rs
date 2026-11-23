//! HTTP handlers for versioned BOM snapshots.

use actix_web::{web, HttpRequest, HttpResponse};
use chrono::Utc;
use serde::Deserialize;
use serde_json::{json, Value};
use std::sync::Arc;
use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::pagination::PaginationParams;
use uuid::Uuid;

use super::helpers::{
    caller, caller_company, created, err, load_bom_version, load_eco, max_bom_version, ok,
    paginate, require_permission, require_tenant, require_text, resolve_tenant,
};
use super::state::PlmState;
use super::types::{
    CreateBomVersionRequest, EcoStatus, PlmBomVersion, EVENT_BOM_VERSIONED, MAX_DESCRIPTION_LEN,
    PERM_PLM_MANAGE,
};

/// Path carrying a BOM version id.
#[derive(Debug, Deserialize)]
struct BomVersionPath {
    bom_version_id: String,
}

// ---------------------------------------------------------------------------
// BOM version handlers (authenticated)
// ---------------------------------------------------------------------------

pub(crate) async fn list_bom_versions(
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
        let bom_filter = params.filters.get("bom_id").cloned();
        let eco_filter = params.filters.get("eco_id").cloned();
        let mut items: Vec<PlmBomVersion> = state
            .bom_versions
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|version| {
                if version.tenant_id != tenant_id {
                    return false;
                }
                if let Some(bom_id) = &bom_filter {
                    if version.bom_id != *bom_id {
                        return false;
                    }
                }
                if let Some(eco_id) = &eco_filter {
                    if version.eco_id.map(|id| id.to_string()) != Some(eco_id.clone()) {
                        return false;
                    }
                }
                true
            })
            .collect();
        items.sort_by(|a, b| {
            b.version_number
                .cmp(&a.version_number)
                .then(b.created_at.cmp(&a.created_at))
        });
        Ok(paginate(items, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

/// Version a BOM snapshot. The version number is strictly
/// increasing per BOM: when omitted it is the next number
/// after the current maximum (starting at 1); when supplied
/// it must be above the current maximum. A linked ECO, when
/// given, must be approved or implemented — a draft ECO may
/// not drive a BOM version. When the tenant settings require
/// an ECO for BOM versions, the link is mandatory.
pub(crate) async fn create_bom_version(
    state: web::Data<PlmState>,
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    body: web::Json<CreateBomVersionRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PlmBomVersion> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let tenant_id = resolve_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PLM_MANAGE)?;
        let input = body.into_inner();
        let bom_id = require_text(&input.bom_id, "bom_id", 200)?;
        let settings = state.current_settings();
        if settings.require_eco_for_bom && input.eco_id.is_none() {
            return Err(UnifiedError::bad_request(
                "an ECO link is required for BOM versions on this tenant",
            ));
        }
        if let Some(eco_id) = input.eco_id {
            let eco = load_eco(&state, &tenant_id, &eco_id.to_string())?;
            if !matches!(eco.status, EcoStatus::Approved | EcoStatus::Implemented) {
                return Err(UnifiedError::bad_request(format!(
                    "ECO {} is in status {}; only an approved or implemented ECO may drive a BOM version",
                    eco.code,
                    eco.status.as_str()
                )));
            }
        }
        let max_version = max_bom_version(&state, &tenant_id, &bom_id);
        let version_number = match input.version_number {
            Some(number) => {
                if number <= 0 {
                    return Err(UnifiedError::bad_request("version_number must be positive"));
                }
                if let Some(max) = max_version {
                    if number <= max {
                        return Err(UnifiedError::conflict(format!(
                            "version_number {number} is not above the current maximum {max} for this BOM"
                        )));
                    }
                }
                number
            }
            None => max_version.map(|max| max + 1).unwrap_or(1),
        };
        let notes = match &input.notes {
            Some(raw) => Some(require_text(raw, "notes", MAX_DESCRIPTION_LEN)?),
            None => None,
        };
        let now = Utc::now();
        let bom_version = PlmBomVersion {
            id: Uuid::now_v7(),
            tenant_id: tenant_id.clone(),
            bom_id: bom_id.clone(),
            version_number,
            eco_id: input.eco_id,
            snapshot: input.snapshot.clone(),
            effective_from: input.effective_from,
            notes,
            created_by: caller.actor.clone(),
            created_at: now,
        };
        state
            .bom_versions
            .insert(bom_version.id, bom_version.clone());
        state.record_audit(
            &tenant_id,
            "bom_version",
            bom_version.id,
            "versioned",
            &caller.actor,
            input.eco_id.map(|id| format!("eco {id}")),
            None,
            Some(version_number.to_string()),
        );
        state.emit(
            EVENT_BOM_VERSIONED,
            json!({
                "tenant_id": tenant_id,
                "bom_version_id": bom_version.id.to_string(),
                "bom_id": bom_version.bom_id,
                "version_number": bom_version.version_number,
                "eco_id": bom_version.eco_id.map(|id| id.to_string()),
                "created_by": caller.actor,
            }),
        );
        Ok(bom_version)
    })();
    match result {
        Ok(data) => created(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn get_bom_version(
    state: web::Data<PlmState>,
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    path: web::Path<BomVersionPath>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PlmBomVersion> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let tenant_id = resolve_tenant(&req)?;
        let _caller = caller(&req)?;
        load_bom_version(&state, &tenant_id, &path.bom_version_id)
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn delete_bom_version(
    state: web::Data<PlmState>,
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    path: web::Path<BomVersionPath>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let tenant_id = resolve_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PLM_MANAGE)?;
        let bom_version = load_bom_version(&state, &tenant_id, &path.bom_version_id)?;
        // The newest snapshot of a BOM is the live one; only
        // older snapshots may be removed.
        if let Some(max) = max_bom_version(&state, &tenant_id, &bom_version.bom_id) {
            if bom_version.version_number == max {
                return Err(UnifiedError::bad_request(
                    "the newest BOM version cannot be deleted",
                ));
            }
        }
        state.bom_versions.remove(&bom_version.id);
        state.record_audit(
            &tenant_id,
            "bom_version",
            bom_version.id,
            "deleted",
            &caller.actor,
            None,
            Some(bom_version.version_number.to_string()),
            None,
        );
        Ok(json!({ "deleted": true, "bom_version_id": bom_version.id.to_string() }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}
