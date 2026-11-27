//! HTTP handler for the referral AI surface: one registered
//! capability at a time through the AI module's gated runner
//! (`run_ai_capability`, capability allowlist gate first),
//! exactly like the ESG module's `ai_capability` handler.
//! The AI never creates, edits, or moves anything on its
//! own: the capabilities return text inline only.

use actix_web::{HttpRequest, HttpResponse, web};
use std::sync::Arc;
use unified_core::context::AppContext;
use unified_core::error::UnifiedResult;

use super::helpers::{caller, caller_company, err, ok, parse_uuid, require_tenant};
use super::state::ReferralState;
use super::types::{JobReferralSummary, PERM_REFERRAL_VIEW, RewardStatus, StageCount};
use crate::modules::ai;

/// Run one registered referral AI capability through the AI
/// module's gated runner.
pub(crate) async fn ai_capability(
    state: web::Data<ReferralState>,
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
    let _scope = match caller_company(&_tenant, &req) {
        Ok(scope) => scope,
        Err(error) => return err(error, debug),
    };
    let caller = match caller(&req) {
        Ok(caller) => caller,
        Err(error) => return err(error, debug),
    };
    if let Err(error) = super::helpers::require_permission(&ctx, &caller.role, PERM_REFERRAL_VIEW) {
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
    match ai::run_ai_capability(&ctx, &ai_caller, company_id, "referral", &capability, input).await
    {
        Ok(output) => ok(output),
        Err(error) => err(error, debug),
    }
}

/// The grounding facts for one job: referral counts by
/// pipeline stage plus reward totals, built from stored
/// rows only - no candidate names or emails, so the AI
/// summarizes aggregates instead of personal data.
pub(crate) fn grounding_facts(
    state: &ReferralState,
    tenant_id: &str,
    job_id: &str,
) -> UnifiedResult<serde_json::Value> {
    let job_uuid = parse_uuid(job_id, "job")?;
    let stages = state.stages_of(tenant_id);
    let referrals: Vec<_> = state
        .referrals_of(tenant_id)
        .into_iter()
        .filter(|referral| referral.job_id == job_uuid)
        .collect();
    let by_stage: Vec<StageCount> = super::types::counts_by_stage(&stages, &referrals)
        .into_iter()
        .map(|(stage_slug, count)| StageCount { stage_slug, count })
        .collect();
    let rewards: Vec<_> = state
        .rewards_of(tenant_id)
        .into_iter()
        .filter(|reward| reward.job_id == job_uuid)
        .collect();
    let count_status = |status: RewardStatus| {
        rewards
            .iter()
            .filter(|reward| reward.status == status)
            .count() as i64
    };
    let paid_minor: i64 = rewards
        .iter()
        .filter(|reward| reward.status == RewardStatus::Paid)
        .map(|reward| reward.amount_minor)
        .sum();
    let summary = JobReferralSummary {
        job_id: job_uuid.to_string(),
        referral_count: referrals.len() as i64,
        by_stage,
        rewards_pending: count_status(RewardStatus::Pending),
        rewards_eligible: count_status(RewardStatus::Eligible),
        rewards_paid: count_status(RewardStatus::Paid),
        rewards_voided: count_status(RewardStatus::Voided),
        rewards_paid_minor: paid_minor,
    };
    Ok(serde_json::to_value(&summary).unwrap_or(serde_json::Value::Null))
}
