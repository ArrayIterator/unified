//! HTTP handler for the social marketing AI surface: one
//! registered capability at a time through the AI module's
//! gated runner (`run_ai_capability`, capability allowlist
//! gate first), exactly like the SMS module's
//! `ai_capability` handler. The AI never creates, edits, or
//! publishes anything on its own: the draft capability
//! returns text inline only.

use actix_web::{HttpRequest, HttpResponse, web};
use std::sync::Arc;
use unified_core::context::AppContext;
use unified_core::error::UnifiedResult;

use super::helpers::{caller, err, ok, require_caller_company, require_tenant};
use super::state::SocialMarketingState;
use super::types::PERM_SOCIAL_VIEW;
use crate::modules::ai;

/// Run one registered social marketing AI capability through
/// the AI module's gated runner.
pub(crate) async fn ai_capability(
    state: web::Data<SocialMarketingState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<ai::CapabilityCallInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let capability = path.into_inner();
    let input = body.into_inner().input;
    let _ = state;
    let tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return err(error, debug),
    };
    if let Err(error) = require_caller_company(&tenant, &req) {
        return err(error, debug);
    }
    let caller = match caller(&req) {
        Ok(caller) => caller,
        Err(error) => return err(error, debug),
    };
    if let Err(error) = super::helpers::require_permission(&ctx, &caller.role, PERM_SOCIAL_VIEW) {
        return err(error, debug);
    }
    let ai_caller = match ai::caller(&req) {
        Ok(caller) => caller,
        Err(error) => return err(error, debug),
    };
    if let Err(error) = ai::require_permission(&ctx, &ai_caller.role, ai::PERM_AI_USE) {
        return err(error, debug);
    }
    let company_id = ai::resolve_capability_company(&ctx, &req, &ai_caller);
    match ai::run_ai_capability(
        &ctx,
        &ai_caller,
        company_id,
        "social_marketing",
        &capability,
        input,
    )
    .await
    {
        Ok(output) => ok(output),
        Err(error) => err(error, debug),
    }
}

/// The grounding facts for one campaign, used by the AI
/// grounding hooks in `hooks.rs`: the stored campaign plus
/// its live post counts and summed metrics, so the AI
/// drafts from stored facts instead of inventing campaign
/// history. Account handles and credentials are never
/// included.
pub(crate) fn grounding_facts(
    state: &SocialMarketingState,
    tenant_id: &str,
    campaign_id: &str,
) -> UnifiedResult<serde_json::Value> {
    let campaign = super::helpers::load_campaign(state, tenant_id, campaign_id)?;
    let counts = state.post_counts_of_campaign(tenant_id, campaign.id);
    let totals = state.rollup_of_campaign(tenant_id, campaign.id);
    Ok(serde_json::json!({
        "campaign_id": campaign.id.to_string(),
        "campaign_name": campaign.name,
        "status": campaign.status.as_str(),
        "goal": campaign.goal,
        "goal_metric": campaign.goal_metric,
        "goal_target": campaign.goal_target,
        "starts_at": campaign.starts_at.map(|when| when.to_rfc3339()),
        "ends_at": campaign.ends_at.map(|when| when.to_rfc3339()),
        "post_counts": counts,
        "metrics": totals,
        "engagement_bps": super::types::engagement_bps(&totals),
    }))
}
