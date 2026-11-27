// Audit module - HTTP handlers.
//
// Split from `src/modules/audit.rs`; the module root re-exports the public API of this module.

use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::json;
use unified_core::pagination::PaginationParams;
use actix_web::{web, HttpRequest, HttpResponse};
use std::sync::Arc;

use super::helpers::{matches_search, paginate};
use super::state::{AuditState};
use super::types::{AuditEntry, ChainVerification};

// Handlers (read-only: entries are immutable)

pub(crate) async fn list_entries(
    state: web::Data<Arc<AuditState>>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    // C2: the full audit log is admin-only; no verified caller, no read.
    if let Err(error) = unified_core::caller::require_caller(&req) {
        return json::error_response(&error, false);
    }
    let params = query.into_inner();
    let mut items: Vec<AuditEntry> = state
        .entries
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|entry| {
            matches_search(
                &format!("{} {} {}", entry.action, entry.actor, entry.resource),
                &params,
            ) && params
                .filters
                .get("actor")
                .is_none_or(|actor| entry.actor == *actor)
                && params
                    .filters
                    .get("action")
                    .is_none_or(|action| entry.action == *action)
                && params
                    .filters
                    .get("resource")
                    .is_none_or(|resource| entry.resource == *resource)
        })
        .collect();
    // Newest first by insertion sequence.
    items.sort_by(|a, b| b.seq.cmp(&a.seq));
    if params.normalized().order == "asc" {
        items.reverse();
    }
    let page = paginate(items, &params);
    json::success_response(page, 200)
}

pub(crate) async fn get_entry(
    state: web::Data<Arc<AuditState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    // C2: audit entries are admin-only; no verified caller, no read.
    if let Err(error) = unified_core::caller::require_caller(&req) {
        return json::error_response(&error, false);
    }
    let result: UnifiedResult<AuditEntry> = state
        .entries
        .get(path.as_str())
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found("Audit entry not found"));
    json::respond(result, false)
}

/// Recompute the hash chain over the entries still held in memory
/// and report whether any entry was tampered with, deleted, or
/// reordered. Read-only: verification never mutates the log.
pub(crate) async fn verify_chain(
    state: web::Data<Arc<AuditState>>,
    req: HttpRequest,
) -> HttpResponse {
    // C2: chain verification is admin-only; no verified caller, no read.
    if let Err(error) = unified_core::caller::require_caller(&req) {
        return json::error_response(&error, false);
    }
    let result: ChainVerification = state.verify_chain();
    json::success_response(result, 200)
}
