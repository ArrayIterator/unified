//! HTTP handler for the social marketing audit trail:
//! every network, account, post, campaign, and metric
//! change, written as append-only rows.

use actix_web::{HttpRequest, HttpResponse, web};
use serde_json::Value;
use std::sync::Arc;
use unified_core::context::AppContext;
use unified_core::error::UnifiedResult;
use unified_core::pagination::PaginationParams;

use super::helpers::{caller, caller_company, err, ok, paginate, require_tenant, require_view};
use super::state::SocialMarketingState;

/// List the audit trail (paginated, filter by entity type,
/// entity, action, actor; newest first). Audit rows are
/// never updated or deleted through the API.
pub(crate) async fn list_audit(
    state: web::Data<SocialMarketingState>,
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
        let entity_type_filter = params.filters.get("entity_type").cloned();
        let entity_filter = params.filters.get("entity_id").cloned();
        let action_filter = params.filters.get("action").cloned();
        let actor_filter = params.filters.get("actor").cloned();
        let mut items = state.audit_of(&tenant_id);
        items.retain(|row| {
            if let Some(entity_type) = &entity_type_filter {
                if row.entity_type != *entity_type {
                    return false;
                }
            }
            if let Some(entity_id) = &entity_filter {
                if row.entity_id != *entity_id {
                    return false;
                }
            }
            if let Some(action) = &action_filter {
                if row.action != *action {
                    return false;
                }
            }
            if let Some(actor) = &actor_filter {
                if row.actor != *actor {
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
