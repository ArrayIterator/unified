// Inbox - Canned-reply handlers.

use unified_core::context::AppContext;
use unified_core::error::UnifiedResult;
use unified_core::json;
use unified_core::pagination::PaginationParams;
use actix_web::{web, HttpRequest, HttpResponse};
use chrono::Utc;
use serde_json::{json, Value};
use std::sync::Arc;
use uuid::Uuid;

use super::auth::{require_tenant, tenant_visible};
use super::{CreateCannedReplyRequest, InboxCannedReply, InboxState, MAX_BODY_LEN, MAX_ID_LEN,
    MAX_TITLE_LEN, PERM_INBOX_MANAGE, PERM_INBOX_VIEW, UpdateCannedReplyRequest, caller, created,
    err, load_canned_reply, ok, optional_text, paginate, require_permission, require_text};

// Canned reply handlers

pub(crate) async fn list_canned_replies(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<InboxState>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let scope = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_INBOX_VIEW)?;
        let params = query.into_inner();
        let department_filter = params.filters.get("department_id").cloned();
        let search = params.search.clone().unwrap_or_default().to_lowercase();
        let mut items: Vec<InboxCannedReply> = state
            .canned_replies
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|reply| {
                if !tenant_visible(reply.tenant_id, reply.company_id.as_deref(), &scope) {
                    return false;
                }
                if let Some(department) = &department_filter {
                    if reply.department_id.as_deref() != Some(department.as_str()) {
                        return false;
                    }
                }
                if !search.is_empty() {
                    let haystack = format!("{} {}", reply.title, reply.body).to_lowercase();
                    if !haystack.contains(&search) {
                        return false;
                    }
                }
                true
            })
            .collect();
        let sort = params.allowed_sort(&["title", "created_at"], "created_at");
        let descending = params.normalized().order == "desc";
        items.sort_by(|a, b| {
            let ordering = match sort.as_str() {
                "title" => a.title.cmp(&b.title),
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

pub(crate) async fn create_canned_reply(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<InboxState>,
    req: HttpRequest,
    body: web::Json<CreateCannedReplyRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<InboxCannedReply> = (|| {
        let scope = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_INBOX_MANAGE)?;
        let input = body.into_inner();
        let title = require_text(&input.title, "title", MAX_TITLE_LEN)?;
        let body_text = require_text(&input.body, "body", MAX_BODY_LEN)?;
        let department_id = optional_text(&input.department_id, "department_id", MAX_ID_LEN)?;
        let now = Utc::now();
        let reply = InboxCannedReply {
            id: Uuid::now_v7(),
            tenant_id: scope.tenant_id,
            company_id: scope.company_id.clone(),
            department_id,
            title,
            body: body_text,
            created_by: caller.actor,
            created_at: now,
            updated_at: now,
        };
        state.canned_replies.insert(reply.id, reply.clone());
        Ok(reply)
    })();
    match result {
        Ok(data) => created(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn get_canned_reply(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<InboxState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<InboxCannedReply> = (|| {
        let scope = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_INBOX_VIEW)?;
        load_canned_reply(&state, &path, &scope)
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn update_canned_reply(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<InboxState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdateCannedReplyRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<InboxCannedReply> = (|| {
        let scope = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_INBOX_MANAGE)?;
        let input = body.into_inner();
        let mut reply = load_canned_reply(&state, &path, &scope)?;
        if let Some(department) = &input.department_id {
            reply.department_id = match department {
                Some(raw) => Some(require_text(raw, "department_id", MAX_ID_LEN)?),
                None => None,
            };
        }
        if let Some(title) = &input.title {
            reply.title = require_text(title, "title", MAX_TITLE_LEN)?;
        }
        if let Some(body_text) = &input.body {
            reply.body = require_text(body_text, "body", MAX_BODY_LEN)?;
        }
        reply.updated_at = Utc::now();
        state.canned_replies.insert(reply.id, reply.clone());
        Ok(reply)
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn delete_canned_reply(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<InboxState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let scope = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_INBOX_MANAGE)?;
        let reply = load_canned_reply(&state, &path, &scope)?;
        state.canned_replies.remove(&reply.id);
        Ok(json!({ "deleted": true, "id": reply.id.to_string() }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}
