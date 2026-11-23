//! HTTP handler for the ESG AI surface: one registered
//! capability at a time through the AI module's gated runner
//! (`run_ai_capability`, capability allowlist gate first),
//! exactly like the spreadsheets module's `ai_capability`
//! handler. The AI never creates, edits, or closes anything
//! on its own: the summary capability returns text inline
//! only.

use actix_web::{HttpRequest, HttpResponse, web};
use std::sync::Arc;
use unified_core::context::AppContext;
use unified_core::error::UnifiedResult;

use super::helpers::{caller, err, ok, require_tenant};
use super::state::EsgState;
use super::types::PERM_ESG_VIEW;
use crate::modules::ai;

/// Run one registered ESG AI capability through the AI
/// module's gated runner.
pub(crate) async fn ai_capability(
    state: web::Data<EsgState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<ai::CapabilityCallInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let capability = path.into_inner();
    let input = body.into_inner().input;
    let _ = state;
    let _tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return err(error, debug),
    };
    let caller = match caller(&req) {
        Ok(caller) => caller,
        Err(error) => return err(error, debug),
    };
    if let Err(error) = super::helpers::require_permission(&ctx, &caller.role, PERM_ESG_VIEW) {
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
    match ai::run_ai_capability(&ctx, &ai_caller, company_id, "esg", &capability, input).await {
        Ok(output) => ok(output),
        Err(error) => err(error, debug),
    }
}

/// The grounding facts for one reporting period, used by the
/// AI grounding hooks in `hooks.rs`: the stored period plus
/// its summary, so the AI summarizes stored facts instead of
/// inventing a sustainability report.
pub(crate) fn grounding_facts(
    state: &EsgState,
    tenant_id: &str,
    period_id: &str,
) -> UnifiedResult<serde_json::Value> {
    let period = super::helpers::load_period(state, tenant_id, period_id)?;
    let summary = super::handlers_reports::build_period_summary(state, tenant_id, &period);
    Ok(serde_json::json!({
        "period_id": period.id.to_string(),
        "period_code": period.code,
        "status": period.status.as_str(),
        "starts_on": period.starts_on.format("%Y-%m-%d").to_string(),
        "ends_on": period.ends_on.format("%Y-%m-%d").to_string(),
        "metric_count": summary.metric_count,
        "reported_count": summary.reported_count,
        "completion_bps": summary.completion_bps,
        "targets_set": summary.targets_set,
        "targets_achieved": summary.targets_achieved,
        "average_attainment_bps": summary.average_attainment_bps,
        "by_category": summary.by_category,
        "metrics": summary.metrics,
    }))
}
