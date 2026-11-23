// Handlers: privacy notices and audit trail.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::json;
use unified_core::pagination::{PageResult, PaginationParams};
use actix_web::{web, HttpRequest, HttpResponse};
use serde_json::json;
use std::sync::Arc;

use super::*;

// ---------------------------------------------------------------------------
// Handlers: privacy notices and audit trail
// ---------------------------------------------------------------------------

pub(crate) async fn list_notices(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<PrivacyState>>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PageResult<PrivacyNotice>> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PRIVACY_READ)?;
        let params = query.into_inner();
        let mut items: Vec<PrivacyNotice> = state
            .notices
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|record| {
                matches_search(&format!("{} {}", record.module_id, record.text), &params)
                    && params
                        .filters
                        .get("module_id")
                        .is_none_or(|module_id| record.module_id == *module_id)
            })
            .collect();
        items.sort_by(|a, b| {
            b.version
                .cmp(&a.version)
                .then(b.effective_from.cmp(&a.effective_from))
        });
        if params.normalized().order == "asc" {
            items.reverse();
        }
        Ok(paginate(items, &params))
    })();
    json::respond(result, debug)
}

pub(crate) async fn create_notice(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<PrivacyState>>,
    req: HttpRequest,
    body: web::Json<CreateNoticeInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PrivacyNotice> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PRIVACY_ADMIN)?;
        let input = body.into_inner();
        if !valid_slug(&input.module_id) {
            return Err(UnifiedError::bad_request(
                "module_id must be a lowercase slug",
            ));
        }
        let text = require_text(&input.text, "text", 20000)?;
        if input.version < 1 {
            return Err(UnifiedError::bad_request(
                "version must be at least 1",
            ));
        }
        if state.notices.iter().any(|entry| {
            entry.value().module_id == input.module_id
                && entry.value().version == input.version
        }) {
            return Err(UnifiedError::conflict(format!(
                "Notice version {} already exists for module \"{}\"",
                input.version, input.module_id
            )));
        }
        let record = PrivacyNotice {
            id: new_id(),
            module_id: input.module_id.clone(),
            text,
            version: input.version,
            effective_from: input.effective_from,
            created_by: caller.actor.clone(),
            created_at: now(),
        };
        state.notices.insert(record.id.clone(), record.clone());
        state.audit(
            EVENT_PRIVACY_NOTICE_PUBLISHED,
            &caller.actor,
            "privacy.notice.published",
            &format!("module:{}", record.module_id),
            json!({
                "module_id": record.module_id,
                "notice_id": record.id,
                "version": record.version,
                "effective_from": record.effective_from,
            }),
        );
        Ok(record)
    })();
    match result {
        Ok(record) => json::success_response(record, 201),
        Err(error) => json::error_response(&error, debug),
    }
}

pub(crate) async fn get_current_notice(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<PrivacyState>>,
    req: HttpRequest,
    query: web::Query<CurrentNoticeQuery>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<CurrentNotice> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PRIVACY_READ)?;
        let input = query.into_inner();
        if !valid_slug(&input.module_id) {
            return Err(UnifiedError::bad_request(
                "module_id must be a lowercase slug",
            ));
        }
        Ok(state.current_notice(&input.module_id))
    })();
    json::respond(result, debug)
}

pub(crate) async fn get_notice(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<PrivacyState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PrivacyNotice> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PRIVACY_READ)?;
        state
            .notices
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Privacy notice not found"))
    })();
    json::respond(result, debug)
}

pub(crate) async fn list_audit_entries(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<PrivacyState>>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PageResult<PrivacyAuditEntry>> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PRIVACY_READ)?;
        let params = query.into_inner();
        let mut items: Vec<PrivacyAuditEntry> = state
            .audit_entries
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
            })
            .collect();
        sort_desc_by_created(&mut items, |entry| entry.created_at);
        if params.normalized().order == "asc" {
            items.reverse();
        }
        Ok(paginate(items, &params))
    })();
    json::respond(result, debug)
}
