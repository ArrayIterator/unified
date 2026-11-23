//! Compliance Center module — GDPR / ISO support: data export — Export handlers.
//!
//! Split from `src/modules/compliance.rs`; behaviour unchanged.

use super::breaches::*;
use super::checklists::*;
use super::consents::*;
use super::constants::*;
use super::deletion_listeners::*;
use super::deletions::*;
use super::helpers::*;
use super::module::*;
use super::records::*;
use super::requests::*;
use super::retention::*;
use super::state::*;
use super::types::*;

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::json;
use unified_core::pagination::PaginationParams;
use actix_web::{web, HttpRequest, HttpResponse};
use chrono::Duration;
use chrono::Utc;
use serde_json::{json, Value};
use std::sync::Arc;
use uuid::Uuid;

pub(super) async fn list_exports(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ComplianceState>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_COMPLIANCE_VIEW)?;
        let is_manager = can_manage(&ctx, &caller.role);
        let params = query.into_inner();
        let status_filter = params.filters.get("status").cloned();
        let requester_filter = params.filters.get("requester_id").cloned();
        let now = Utc::now();
        let mut items: Vec<Value> = state
            .exports
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|export| {
                if !is_manager && export.requester_id != caller.actor {
                    return false;
                }
                if let Some(status) = &status_filter {
                    let effective = match export.status {
                        ExportStatus::Ready => match export.download_expires_at {
                            Some(expires_at) if now >= expires_at => "expired",
                            _ => "ready",
                        },
                        other => other.as_str(),
                    };
                    if effective != status {
                        return false;
                    }
                }
                if let Some(requester) = &requester_filter {
                    if &export.requester_id != requester {
                        return false;
                    }
                }
                true
            })
            .map(|export| export_view(&export, now))
            .collect();
        items.sort_by(|a, b| {
            b.get("created_at")
                .and_then(Value::as_str)
                .cmp(&a.get("created_at").and_then(Value::as_str))
        });
        Ok(paginate(items, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(super) async fn create_export(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ComplianceState>,
    req: HttpRequest,
    body: web::Json<CreateExportRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<ExportRequest> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = require_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_COMPLIANCE_VIEW)?;
        let input = body.into_inner();
        let mut scope: Vec<String> = Vec::new();
        for raw in &input.scope {
            scope.push(require_text(raw, "scope entry", MAX_SCOPE_LEN)?);
        }
        // One open export per requester at a time: a second request
        // while one is still requested / processing is a conflict,
        // so owning modules never assemble the same export twice.
        if state.exports.iter().any(|entry| {
            entry.value().requester_id == caller.actor
                && matches!(
                    entry.value().status,
                    ExportStatus::Requested | ExportStatus::Processing
                )
        }) {
            return Err(UnifiedError::conflict(
                "You already have an open export request; wait for it to finish first",
            ));
        }
        let now = Utc::now();
        let export = ExportRequest {
            id: Uuid::now_v7(),
            requester_id: caller.actor.clone(),
            scope,
            status: ExportStatus::Requested,
            storage_target: None,
            storage_key: None,
            download_expires_at: None,
            failure_reason: None,
            created_at: now,
            updated_at: now,
        };
        state.exports.insert(export.id, export.clone());
        state.emit(
            "compliance.export.requested",
            json!({
                "export_id": export.id.to_string(),
                "requester_id": export.requester_id,
                "scope": export.scope,
            }),
        );
        Ok(export)
    })();
    match result {
        Ok(export) => created(export),
        Err(error) => err(error, debug),
    }
}

pub(super) async fn get_export(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ComplianceState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_COMPLIANCE_VIEW)?;
        let export = load_export(&state, &path.into_inner())?;
        check_own_or_manage(&ctx, &caller.role, &export.requester_id, &caller.actor)?;
        Ok(export_view(&export, Utc::now()))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(super) async fn start_export(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ComplianceState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<ExportRequest> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = require_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_COMPLIANCE_MANAGE)?;
        let mut export = load_export(&state, &path.into_inner())?;
        if export.status != ExportStatus::Requested {
            return Err(UnifiedError::conflict(format!(
                "Only a requested export can start processing (status is \"{}\")",
                export.status.as_str()
            )));
        }
        export.status = ExportStatus::Processing;
        export.updated_at = Utc::now();
        state.exports.insert(export.id, export.clone());
        state.emit(
            "compliance.export.started",
            json!({
                "export_id": export.id.to_string(),
                "requester_id": export.requester_id,
            }),
        );
        Ok(export)
    })();
    match result {
        Ok(export) => ok(export),
        Err(error) => err(error, debug),
    }
}

pub(super) async fn complete_export(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ComplianceState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<CompleteExportRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = require_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_COMPLIANCE_MANAGE)?;
        let mut export = load_export(&state, &path.into_inner())?;
        if export.status != ExportStatus::Processing {
            return Err(UnifiedError::conflict(format!(
                "Only a processing export can be completed (status is \"{}\")",
                export.status.as_str()
            )));
        }
        let input = body.into_inner();
        let storage_target = require_text(&input.storage_target, "storage_target", MAX_STORAGE_KEY_LEN)?;
        let storage_key = require_text(&input.storage_key, "storage_key", MAX_STORAGE_KEY_LEN)?;
        let hours = input.expires_in_hours.unwrap_or(DEFAULT_DOWNLOAD_LINK_HOURS);
        if hours < 1 || hours > 24 * 30 {
            return Err(UnifiedError::bad_request(
                "expires_in_hours must be between 1 and 720",
            ));
        }
        let now = Utc::now();
        export.status = ExportStatus::Ready;
        export.storage_target = Some(storage_target);
        export.storage_key = Some(storage_key);
        export.download_expires_at = Some(now + Duration::hours(hours));
        export.updated_at = now;
        state.exports.insert(export.id, export.clone());
        state.emit(
            "compliance.export.completed",
            json!({
                "export_id": export.id.to_string(),
                "requester_id": export.requester_id,
                "storage_target": export.storage_target,
                "storage_key": export.storage_key,
                "download_expires_at": export.download_expires_at.map(|value| value.to_rfc3339()),
            }),
        );
        state.notify(
            "compliance.export.completed",
            &export.requester_id,
            "Your data export is ready",
            "Your requested data export is ready to download. The link expires soon.",
            Some(format!("/api/v1/compliance/exports/{}/download", export.id)),
        );
        Ok(export_view(&export, now))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(super) async fn fail_export(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ComplianceState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<FailExportRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<ExportRequest> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = require_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_COMPLIANCE_MANAGE)?;
        let mut export = load_export(&state, &path.into_inner())?;
        if !matches!(
            export.status,
            ExportStatus::Requested | ExportStatus::Processing
        ) {
            return Err(UnifiedError::conflict(format!(
                "Only a requested or processing export can fail (status is \"{}\")",
                export.status.as_str()
            )));
        }
        let reason = match &body.into_inner().reason {
            Some(raw) => Some(require_text(raw, "reason", MAX_REASON_LEN)?),
            None => None,
        };
        export.status = ExportStatus::Failed;
        export.failure_reason = reason;
        export.updated_at = Utc::now();
        state.exports.insert(export.id, export.clone());
        state.emit(
            "compliance.export.failed",
            json!({
                "export_id": export.id.to_string(),
                "requester_id": export.requester_id,
                "failure_reason": export.failure_reason,
            }),
        );
        Ok(export)
    })();
    match result {
        Ok(export) => ok(export),
        Err(error) => err(error, debug),
    }
}

pub(super) async fn download_export(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ComplianceState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_COMPLIANCE_VIEW)?;
        let export = load_export(&state, &path.into_inner())?;
        check_own_or_manage(&ctx, &caller.role, &export.requester_id, &caller.actor)?;
        if export.status != ExportStatus::Ready {
            return Err(UnifiedError::conflict(format!(
                "This export is not ready for download (status is \"{}\")",
                export.status.as_str()
            )));
        }
        let now = Utc::now();
        let expires_at = export.download_expires_at.ok_or_else(|| {
            UnifiedError::conflict("This export has no download link attached")
        })?;
        if now >= expires_at {
            return Err(UnifiedError::conflict(
                "The download link for this export has expired; request a new export",
            ));
        }
        let (target, key) = match (&export.storage_target, &export.storage_key) {
            (Some(target), Some(key)) => (target.clone(), key.clone()),
            _ => {
                return Err(UnifiedError::conflict(
                    "This export has no download link attached",
                ))
            }
        };
        Ok(json!({
            "export_id": export.id.to_string(),
            "storage_target": target,
            "storage_key": key,
            "download_expires_at": expires_at.to_rfc3339(),
            "note": "Download the file through the storage module using this target and key before the link expires.",
        }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}
