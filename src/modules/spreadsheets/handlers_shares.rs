//! HTTP handlers for sharing: share rows (viewer / editor),
//! and the token-gated public-link read. The public link is
//! off by default and only works when the tenant allows public
//! links and the workbook has its link enabled; unknown,
//! disabled, and revoked tokens all answer 404 with the same
//! message, so tokens cannot be probed apart.

use actix_web::{web, HttpRequest, HttpResponse};
use chrono::Utc;
use serde_json::{json, Value};
use std::sync::Arc;
use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::pagination::PaginationParams;
use uuid::Uuid;

use super::helpers::{
    caller, caller_company, created, err, hash_value, load_share, load_workbook,
    load_workbook_readable, ok, optional_text, paginate, require_caller_company, require_manage,
    require_tenant, require_text, require_view, validate_share_role,
};
use super::state::SpreadsheetsState;
use super::types::{CreateShareRequest, Share, UpdateShareRequest, EVENT_SHARED, MAX_EMAIL_LEN};

pub(crate) async fn list_shares(
    state: web::Data<SpreadsheetsState>,
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let tenant = require_tenant(&req)?;
        let tenant_id = tenant.tenant_id.to_string();
        let _company = caller_company(&tenant, &req)?;
        let caller = caller(&req)?;
        require_view(&ctx, &caller.role)?;
        let workbook =
            load_workbook_readable(&state, &tenant_id, &path.into_inner(), &caller.actor)?;
        let params = query.into_inner();
        let role_filter = params.filters.get("role").cloned();
        let user_filter = params.filters.get("shared_with").cloned();
        let mut items = state.shares_of(workbook.id);
        if let Some(role) = &role_filter {
            items.retain(|share| &share.role == role);
        }
        if let Some(user) = &user_filter {
            items.retain(|share| share.shared_with == *user);
        }
        let views: Vec<Value> = items
            .iter()
            .map(|item| serde_json::to_value(item).unwrap_or(Value::Null))
            .collect();
        Ok(serde_json::to_value(paginate(views, &params)).unwrap_or(Value::Null))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn create_share(
    state: web::Data<SpreadsheetsState>,
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    path: web::Path<String>,
    body: web::Json<CreateShareRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let tenant = require_tenant(&req)?;
        let tenant_id = tenant.tenant_id.to_string();
        let _company = require_caller_company(&tenant, &req)?;
        let caller = caller(&req)?;
        require_manage(&ctx, &caller.role)?;
        let workbook = load_workbook(&state, &tenant_id, &path.into_inner())?;
        // Only the owner shares a workbook: an editor sharee
        // may edit cells, never hand the document to someone else.
        if workbook.owner_id != caller.actor {
            return Err(UnifiedError::not_found(format!(
                "Workbook not found: {}",
                workbook.id
            )));
        }
        let input = body.into_inner();
        let shared_with = require_text(
            &input.shared_with,
            "shared_with",
            super::types::MAX_USER_ID_LEN,
        )?;
        if shared_with == workbook.owner_id {
            return Err(UnifiedError::bad_request(
                "The owner already has full access to the workbook",
            ));
        }
        let email = optional_text(&input.shared_with_email, "shared_with_email", MAX_EMAIL_LEN)?;
        if let Some(email) = &email {
            if !email.contains('@') || email.contains(' ') {
                return Err(UnifiedError::bad_request(
                    "shared_with_email must be a valid email address",
                ));
            }
        }
        let settings = state.settings_for(&tenant_id);
        let role = match input.role {
            Some(role) => validate_share_role(&role, &settings)?,
            None => settings.default_share_role.clone(),
        };
        if state
            .shares_of(workbook.id)
            .iter()
            .any(|share| share.shared_with == shared_with)
        {
            return Err(UnifiedError::conflict(format!(
                "Workbook is already shared with \"{shared_with}\""
            )));
        }
        let now = Utc::now();
        let share = Share {
            id: Uuid::new_v4(),
            tenant_id: tenant_id.clone(),
            workbook_id: workbook.id,
            shared_with: shared_with.clone(),
            shared_with_email: email,
            role: role.clone(),
            created_by: caller.actor.clone(),
            created_at: now,
            updated_at: now,
        };
        state.shares.insert(share.id, share.clone());
        state.emit(
            EVENT_SHARED,
            json!({
                "tenant_id": tenant_id,
                "workbook_id": workbook.id.to_string(),
                "shared_with": shared_with,
                "role": role,
            }),
        );
        state.notify(
            &shared_with,
            &format!("Workbook shared with you: {}", workbook.title),
            &format!(
                "{} shared \"{}\" with you as {}.",
                caller.actor, workbook.title, role
            ),
            Some(format!("/api/v1/spreadsheets/workbooks/{}", workbook.id)),
        );
        Ok(serde_json::to_value(share).unwrap_or(Value::Null))
    })();
    match result {
        Ok(data) => created(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn get_share(
    state: web::Data<SpreadsheetsState>,
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    path: web::Path<(String, String)>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let tenant = require_tenant(&req)?;
        let tenant_id = tenant.tenant_id.to_string();
        let _company = caller_company(&tenant, &req)?;
        let caller = caller(&req)?;
        require_view(&ctx, &caller.role)?;
        let (workbook_id, share_id) = path.into_inner();
        let workbook = load_workbook_readable(&state, &tenant_id, &workbook_id, &caller.actor)?;
        let share = load_share(&state, &tenant_id, &workbook, &share_id)?;
        Ok(serde_json::to_value(share).unwrap_or(Value::Null))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn update_share(
    state: web::Data<SpreadsheetsState>,
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    path: web::Path<(String, String)>,
    body: web::Json<UpdateShareRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let tenant = require_tenant(&req)?;
        let tenant_id = tenant.tenant_id.to_string();
        let _company = require_caller_company(&tenant, &req)?;
        let caller = caller(&req)?;
        require_manage(&ctx, &caller.role)?;
        let (workbook_id, share_id) = path.into_inner();
        let workbook = load_workbook(&state, &tenant_id, &workbook_id)?;
        if workbook.owner_id != caller.actor {
            return Err(UnifiedError::not_found(format!(
                "Workbook not found: {}",
                workbook.id
            )));
        }
        let mut share = load_share(&state, &tenant_id, &workbook, &share_id)?;
        let input = body.into_inner();
        if let Some(role) = input.role {
            let settings = state.settings_for(&tenant_id);
            share.role = validate_share_role(&role, &settings)?;
        }
        share.updated_at = Utc::now();
        state.shares.insert(share.id, share.clone());
        state.emit(
            EVENT_SHARED,
            json!({
                "tenant_id": tenant_id,
                "workbook_id": workbook.id.to_string(),
                "shared_with": share.shared_with,
                "role": share.role,
            }),
        );
        Ok(serde_json::to_value(share).unwrap_or(Value::Null))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn delete_share(
    state: web::Data<SpreadsheetsState>,
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    path: web::Path<(String, String)>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let tenant = require_tenant(&req)?;
        let tenant_id = tenant.tenant_id.to_string();
        let _company = require_caller_company(&tenant, &req)?;
        let caller = caller(&req)?;
        require_manage(&ctx, &caller.role)?;
        let (workbook_id, share_id) = path.into_inner();
        let workbook = load_workbook(&state, &tenant_id, &workbook_id)?;
        if workbook.owner_id != caller.actor {
            return Err(UnifiedError::not_found(format!(
                "Workbook not found: {}",
                workbook.id
            )));
        }
        let share = load_share(&state, &tenant_id, &workbook, &share_id)?;
        state.shares.remove(&share.id);
        Ok(json!({ "deleted": share.id.to_string() }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

/// PUBLIC (token-gated): read one workbook through its public
/// link. The token is the credential, so no tenant context is
/// needed — possession of the unguessable token is the
/// authorization. The response is read-only: sheets and cells,
/// never shares or the token hash.
pub(crate) async fn get_public_workbook(
    state: web::Data<SpreadsheetsState>,
    ctx: web::Data<Arc<AppContext>>,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let token = path.into_inner();
        let token_hash = hash_value(&token);
        let workbook = state
            .workbooks
            .iter()
            .map(|entry| entry.value().clone())
            .find(|workbook| {
                workbook.public_link_enabled && workbook.public_token_hash == token_hash
            })
            .ok_or_else(|| UnifiedError::not_found("Workbook not found"))?;
        let mut value = serde_json::to_value(&workbook).unwrap_or(Value::Null);
        if let Value::Object(map) = &mut value {
            // Read-only: sheets with their cells, never shares
            // or the token hash (the hash is skipped by serde).
            let sheets: Vec<Value> = state
                .sheets_of(workbook.id)
                .iter()
                .map(|sheet| super::helpers::sheet_view(&state, sheet))
                .collect();
            map.insert("sheets".to_string(), Value::Array(sheets));
        }
        Ok(value)
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}
