//! HTTP handlers for referral links: one unique shareable
//! link per (employee, job), token-based with the raw token
//! returned once and only its hash stored, plus the public
//! token-gated click endpoint (counts only — no IP address,
//! user agent, or fingerprint is stored).

use actix_web::{HttpRequest, HttpResponse, web};
use chrono::Utc;
use serde_json::{Value, json};
use std::sync::Arc;
use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::pagination::PaginationParams;

use super::helpers::{
    caller, caller_company, created, err, load_link, mint_link, ok, paginate, parse_uuid,
    record_audit, require_manage, require_tenant, require_view, resolve_tenant,
};
use super::state::ReferralState;
use super::types::{CreateLinkRequest, EVENT_LINK_CREATED, LinkCreated, hash_value};

/// Create a referral link for the caller and one job
/// (manager or any authenticated employee with the view
/// capability — the caller is the referrer). One active link
/// per (employee, job): a second create returns 409 and the
/// existing link must be rotated for a fresh token.
pub(crate) async fn create_link(
    state: web::Data<ReferralState>,
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    body: web::Json<CreateLinkRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let tenant_id = resolve_tenant(&req)?;
        let caller = caller(&req)?;
        require_view(&ctx, &caller.role)?;
        let input = body.into_inner();
        let job_id = parse_uuid(&input.job_id, "job")?;
        if let Some(existing) = state.link_for_employee_job(&tenant_id, &caller.actor, job_id) {
            return Err(UnifiedError::conflict(format!(
                "An active referral link already exists for this job: {}; rotate it for a fresh token",
                existing.id
            )));
        }
        let created_link: LinkCreated =
            mint_link(&state, &tenant_id, job_id, &caller.actor, &caller.actor);
        state.emit(
            EVENT_LINK_CREATED,
            json!({
                "tenant_id": tenant_id,
                "link_id": created_link.link.id.to_string(),
                "job_id": job_id.to_string(),
                "referrer_user_id": caller.actor,
            }),
        );
        Ok(serde_json::to_value(&created_link).unwrap_or(Value::Null))
    })();
    match result {
        Ok(data) => created(data),
        Err(error) => err(error, debug),
    }
}

/// List referral links (paginated, filter by job,
/// referrer, active).
pub(crate) async fn list_links(
    state: web::Data<ReferralState>,
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let tenant_id = resolve_tenant(&req)?;
        let caller = caller(&req)?;
        require_view(&ctx, &caller.role)?;
        let params = query.into_inner();
        let job_filter = params.filters.get("job_id").cloned();
        let referrer_filter = params.filters.get("referrer_user_id").cloned();
        let active_filter = params
            .filters
            .get("active")
            .and_then(|raw| match raw.as_str() {
                "true" => Some(true),
                "false" => Some(false),
                _ => None,
            });
        let mut items = state.links_of(&tenant_id);
        items.retain(|link| {
            if let Some(job_id) = &job_filter {
                if link.job_id.to_string() != *job_id {
                    return false;
                }
            }
            if let Some(referrer) = &referrer_filter {
                if link.referrer_user_id != *referrer {
                    return false;
                }
            }
            if let Some(active) = active_filter {
                if link.active != active {
                    return false;
                }
            }
            true
        });
        let views: Vec<Value> = items
            .iter()
            .map(|item| serde_json::to_value(item).unwrap_or(Value::Null))
            .collect();
        Ok(paginate(views, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

/// Get one referral link (token hash never exposed).
pub(crate) async fn get_link(
    state: web::Data<ReferralState>,
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let tenant_id = resolve_tenant(&req)?;
        let caller = caller(&req)?;
        require_view(&ctx, &caller.role)?;
        let link = load_link(&state, &tenant_id, &path.into_inner())?;
        Ok(serde_json::to_value(&link).unwrap_or(Value::Null))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

/// Rotate a referral link (manager, or the link owner):
/// the old token stops working, a fresh token is issued
/// (raw token returned once), the click count is kept.
pub(crate) async fn rotate_link(
    state: web::Data<ReferralState>,
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let tenant_id = resolve_tenant(&req)?;
        let caller = caller(&req)?;
        require_manage(&ctx, &caller.role)?;
        let mut link = load_link(&state, &tenant_id, &path.into_inner())?;
        if !link.active {
            return Err(UnifiedError::bad_request(
                "A revoked referral link cannot be rotated".to_string(),
            ));
        }
        let before = serde_json::to_value(&link).ok();
        let token = super::types::generate_link_token();
        link.token_hash = hash_value(&token);
        link.updated_at = Utc::now();
        state.links.insert(link.id, link.clone());
        record_audit(
            &state,
            &tenant_id,
            "referral_link",
            &link.id.to_string(),
            "rotated",
            &caller.actor,
            before,
            serde_json::to_value(&link).ok(),
        );
        let created_link = LinkCreated { link, token };
        Ok(serde_json::to_value(&created_link).unwrap_or(Value::Null))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

/// Revoke a referral link (manager): the token stops
/// working, the row and its click count are kept for
/// reporting.
pub(crate) async fn revoke_link(
    state: web::Data<ReferralState>,
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let tenant_id = resolve_tenant(&req)?;
        let caller = caller(&req)?;
        require_manage(&ctx, &caller.role)?;
        let mut link = load_link(&state, &tenant_id, &path.into_inner())?;
        if !link.active {
            return Err(UnifiedError::bad_request(
                "Referral link is already revoked".to_string(),
            ));
        }
        let before = serde_json::to_value(&link).ok();
        link.active = false;
        link.updated_at = Utc::now();
        state.links.insert(link.id, link.clone());
        record_audit(
            &state,
            &tenant_id,
            "referral_link",
            &link.id.to_string(),
            "revoked",
            &caller.actor,
            before,
            serde_json::to_value(&link).ok(),
        );
        Ok(json!({ "revoked": true, "link_id": link.id.to_string() }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

/// PUBLIC (token-gated): follow a referral link. The token
/// is the credential — unknown, revoked, and expired tokens
/// all answer 404 with the same message, so tokens cannot
/// be probed apart. A successful click increments the click
/// count and stamps the last click; no IP address, user
/// agent, or fingerprint is stored. The response names only
/// the job the link points at — never the referrer or the
/// token hash.
///
/// Public-access gate: like the recruitment and sign
/// modules, this route relies on the API Manager's
/// `ApiAccessRule` for `GET /api/v1/referral/public/links/*`
/// (default deny without a rule).
pub(crate) async fn click_link(
    state: web::Data<ReferralState>,
    path: web::Path<String>,
) -> HttpResponse {
    let token = path.into_inner();
    let token_hash = hash_value(token.trim());
    let Some(mut link) = state.link_for_token_hash(&token_hash) else {
        return err(
            UnifiedError::not_found("Referral link not found".to_string()),
            false,
        );
    };
    if !link.active {
        return err(
            UnifiedError::not_found("Referral link not found".to_string()),
            false,
        );
    }
    link.click_count += 1;
    link.last_clicked_at = Some(Utc::now());
    link.updated_at = Utc::now();
    state.links.insert(link.id, link.clone());
    ok(json!({
        "job_id": link.job_id.to_string(),
        "click_count": link.click_count,
    }))
}
