//! HTTP handlers for the per-tenant rate-limit settings
//! and the audit trail.

use actix_web::{HttpRequest, HttpResponse, web};
use chrono::Utc;
use serde_json::{Value, json};
use std::sync::Arc;
use unified_core::context::AppContext;
use unified_core::error::UnifiedResult;
use unified_core::pagination::PaginationParams;
use uuid::Uuid;

use super::helpers::{
    caller, caller_company, err, ok, paginate, record_audit, require_caller_company,
    require_manage, require_tenant, require_view, validate_sends_per_minute,
};
use super::state::SmsMarketingState;
use super::types::{DEFAULT_TENANT_SENDS_PER_MINUTE, SmsRateLimit, UpdateRateLimitRequest};

/// Get the tenant's rate-limit settings: sends per minute
/// for the whole tenant, enforced at enqueue time alongside
/// the per-provider limit. A tenant with no row reports the
/// default.
pub(crate) async fn get_rate_limit(
    state: web::Data<SmsMarketingState>,
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let tenant = require_tenant(&req)?;
        let tenant_id = tenant.tenant_id.to_string();
        let _company = caller_company(&tenant, &req)?;
        let caller = caller(&req)?;
        require_view(&ctx, &caller.role)?;
        let (sends_per_minute, configured) = match state.rate_limit_of(&tenant_id) {
            Some(row) => (row.sends_per_minute, true),
            None => (DEFAULT_TENANT_SENDS_PER_MINUTE, false),
        };
        Ok(json!({
            "tenant_id": tenant_id,
            "sends_per_minute": sends_per_minute,
            "configured": configured,
            "window_seconds": super::types::RATE_LIMIT_WINDOW_SECS,
        }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

/// Set the tenant's rate-limit settings (manager). One row
/// per tenant; setting it again replaces it. Zero blocks
/// every send for the tenant (a deliberate off switch).
pub(crate) async fn update_rate_limit(
    state: web::Data<SmsMarketingState>,
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    body: web::Json<UpdateRateLimitRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let tenant = require_tenant(&req)?;
        let tenant_id = tenant.tenant_id.to_string();
        let _company = require_caller_company(&tenant, &req)?;
        let caller = caller(&req)?;
        require_manage(&ctx, &caller.role)?;
        let sends_per_minute = validate_sends_per_minute(body.into_inner().sends_per_minute)?;
        let now = Utc::now();
        let row = match state.rate_limit_of(&tenant_id) {
            Some(mut existing) => {
                let before = serde_json::to_value(&existing).unwrap_or(Value::Null);
                existing.sends_per_minute = sends_per_minute;
                existing.updated_by = caller.actor.clone();
                existing.updated_at = now;
                state.rate_limits.insert(existing.id, existing.clone());
                record_audit(
                    &state,
                    &tenant_id,
                    "rate_limit",
                    &existing.id.to_string(),
                    "update",
                    &caller.actor,
                    Some(before),
                    Some(serde_json::to_value(&existing).unwrap_or(Value::Null)),
                );
                existing
            }
            None => {
                let row = SmsRateLimit {
                    id: Uuid::new_v4(),
                    tenant_id: tenant_id.clone(),
                    sends_per_minute,
                    updated_by: caller.actor.clone(),
                    created_at: now,
                    updated_at: now,
                };
                state.rate_limits.insert(row.id, row.clone());
                record_audit(
                    &state,
                    &tenant_id,
                    "rate_limit",
                    &row.id.to_string(),
                    "create",
                    &caller.actor,
                    None,
                    Some(serde_json::to_value(&row).unwrap_or(Value::Null)),
                );
                row
            }
        };
        Ok(serde_json::to_value(&row).unwrap_or(Value::Null))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

/// List the audit trail (paginated, filter by entity type,
/// entity, action, actor; newest first). Audit rows are
/// never updated or deleted through the API.
pub(crate) async fn list_audit(
    state: web::Data<SmsMarketingState>,
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let tenant = require_tenant(&req)?;
        let tenant_id = tenant.tenant_id.to_string();
        let _company = caller_company(&tenant, &req)?;
        let caller = caller(&req)?;
        require_view(&ctx, &caller.role)?;
        let params = query.into_inner();
        let entity_filter = params.filters.get("entity_type").cloned();
        let entity_id_filter = params.filters.get("entity_id").cloned();
        let action_filter = params.filters.get("action").cloned();
        let actor_filter = params.filters.get("actor").cloned();
        let mut items = state.audit_of(&tenant_id);
        items.retain(|row| {
            if let Some(entity_type) = &entity_filter
                && row.entity_type != *entity_type
            {
                return false;
            }
            if let Some(entity_id) = &entity_id_filter
                && row.entity_id != *entity_id
            {
                return false;
            }
            if let Some(action) = &action_filter
                && row.action != *action
            {
                return false;
            }
            if let Some(actor) = &actor_filter
                && row.actor != *actor
            {
                return false;
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
