//! HTTP handlers for version history: list versions, read
//! one version, and restore a version (the sheet's cells are
//! replaced with the snapshot and recalculated).

use actix_web::{web, HttpRequest, HttpResponse};
use chrono::Utc;
use serde_json::{json, Value};
use std::sync::Arc;
use unified_core::context::AppContext;
use unified_core::error::UnifiedResult;
use unified_core::pagination::PaginationParams;

use super::handlers_workbooks::restore_cells_from_snapshot;
use super::helpers::{
    caller, caller_company, err, load_sheet, load_workbook_readable, load_workbook_writable, ok,
    paginate, parse_uuid, require_caller_company, require_manage, require_tenant, require_view,
    snapshot_version,
};
use super::state::SpreadsheetsState;
use super::types::{SheetVersion, EVENT_VERSION_RESTORED};

pub(crate) async fn list_versions(
    state: web::Data<SpreadsheetsState>,
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    path: web::Path<(String, String)>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let tenant = require_tenant(&req)?;
        let tenant_id = tenant.tenant_id.to_string();
        let _company = caller_company(&tenant, &req)?;
        let caller = caller(&req)?;
        require_view(&ctx, &caller.role)?;
        let (workbook_id, sheet_id) = path.into_inner();
        let workbook = load_workbook_readable(&state, &tenant_id, &workbook_id, &caller.actor)?;
        let sheet = load_sheet(&state, &tenant_id, &workbook, &sheet_id)?;
        let params = query.into_inner();
        let versions = state.versions_of(sheet.id);
        let views: Vec<Value> = versions
            .iter()
            .map(|version| serde_json::to_value(version).unwrap_or(Value::Null))
            .collect();
        Ok(serde_json::to_value(paginate(views, &params)).unwrap_or(Value::Null))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn get_version(
    state: web::Data<SpreadsheetsState>,
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    path: web::Path<(String, String, String)>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let tenant = require_tenant(&req)?;
        let tenant_id = tenant.tenant_id.to_string();
        let _company = caller_company(&tenant, &req)?;
        let caller = caller(&req)?;
        require_view(&ctx, &caller.role)?;
        let (workbook_id, sheet_id, version_id) = path.into_inner();
        let workbook = load_workbook_readable(&state, &tenant_id, &workbook_id, &caller.actor)?;
        let sheet = load_sheet(&state, &tenant_id, &workbook, &sheet_id)?;
        let id = parse_uuid(&version_id, "version")?;
        let version: SheetVersion = state
            .versions
            .get(&id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| {
                unified_core::error::UnifiedError::not_found(format!("Version not found: {id}"))
            })?;
        if version.tenant_id != tenant_id || version.sheet_id != sheet.id {
            return Err(unified_core::error::UnifiedError::not_found(format!(
                "Version not found: {id}"
            )));
        }
        Ok(serde_json::to_value(version).unwrap_or(Value::Null))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn restore_version(
    state: web::Data<SpreadsheetsState>,
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    path: web::Path<(String, String, String)>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let tenant = require_tenant(&req)?;
        let tenant_id = tenant.tenant_id.to_string();
        let _company = require_caller_company(&tenant, &req)?;
        let caller = caller(&req)?;
        require_manage(&ctx, &caller.role)?;
        let (workbook_id, sheet_id, version_id) = path.into_inner();
        let workbook = load_workbook_writable(&state, &tenant_id, &workbook_id, &caller.actor)?;
        let sheet = load_sheet(&state, &tenant_id, &workbook, &sheet_id)?;
        let id = parse_uuid(&version_id, "version")?;
        let version: SheetVersion = state
            .versions
            .get(&id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| {
                unified_core::error::UnifiedError::not_found(format!("Version not found: {id}"))
            })?;
        if version.tenant_id != tenant_id || version.sheet_id != sheet.id {
            return Err(unified_core::error::UnifiedError::not_found(format!(
                "Version not found: {id}"
            )));
        }
        restore_cells_from_snapshot(
            &state,
            &tenant_id,
            &workbook,
            &sheet,
            &version.cells_snapshot,
        );
        // The restore itself is a new version, so it can be
        // undone by restoring the version it replaced.
        snapshot_version(
            &state,
            &tenant_id,
            &workbook,
            &sheet,
            &caller.actor,
            &[format!(
                "Restored version {} ({} cells)",
                version.version_number,
                version.cells_snapshot.len()
            )],
        );
        let mut workbook = workbook;
        workbook.updated_at = Utc::now();
        state.workbooks.insert(workbook.id, workbook.clone());
        state.emit(
            EVENT_VERSION_RESTORED,
            json!({
                "tenant_id": tenant_id,
                "workbook_id": workbook.id.to_string(),
                "sheet_id": sheet.id.to_string(),
                "version_id": version.id.to_string(),
                "version_number": version.version_number,
                "actor": caller.actor,
            }),
        );
        Ok(json!({
            "restored_version": version.version_number,
            "sheet_id": sheet.id.to_string(),
            "cell_count": version.cells_snapshot.len(),
        }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}
