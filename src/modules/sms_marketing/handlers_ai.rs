//! HTTP handler for the SMS marketing AI surface: one
//! registered capability at a time through the AI module's
//! gated runner (`run_ai_capability`, capability allowlist
//! gate first), exactly like the ESG module's
//! `ai_capability` handler. The AI never creates, edits, or
//! sends anything on its own: the draft capability returns
//! text inline only.

use actix_web::{HttpRequest, HttpResponse, web};
use std::sync::Arc;
use unified_core::context::AppContext;
use unified_core::error::UnifiedResult;

use super::helpers::{caller, err, ok, require_caller_company, require_tenant};
use super::state::SmsMarketingState;
use super::types::PERM_SMS_VIEW;
use crate::modules::ai;

/// Run one registered SMS marketing AI capability through
/// the AI module's gated runner.
pub(crate) async fn ai_capability(
    state: web::Data<SmsMarketingState>,
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
    if let Err(error) = super::helpers::require_permission(&ctx, &caller.role, PERM_SMS_VIEW) {
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
        "sms_marketing",
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
/// its send counts, so the AI drafts from stored facts
/// instead of inventing campaign history. Recipient phone
/// numbers are never included.
pub(crate) fn grounding_facts(
    state: &SmsMarketingState,
    tenant_id: &str,
    campaign_id: &str,
) -> UnifiedResult<serde_json::Value> {
    let campaign = super::helpers::load_campaign(state, tenant_id, campaign_id)?;
    let template =
        super::helpers::load_template(state, tenant_id, &campaign.template_id.to_string())?;
    Ok(serde_json::json!({
        "campaign_id": campaign.id.to_string(),
        "campaign_name": campaign.name,
        "status": campaign.status.as_str(),
        "template_name": template.name,
        "template_body": template.body,
        "template_variables": template.variables,
        "send_at": campaign.send_at.map(|when| when.to_rfc3339()),
        "sent_at": campaign.sent_at.map(|when| when.to_rfc3339()),
        "stats": campaign.stats,
        "recipient_count": super::helpers::resolve_recipients(state, &campaign).len(),
    }))
}
