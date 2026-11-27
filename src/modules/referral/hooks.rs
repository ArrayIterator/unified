//! Hook handlers for user deletion, HR employee creation,
//! and recruitment applicant moves / hires.

use super::helpers::{on_referral_hired, record_stage_move, void_open_rewards};
use super::state::ReferralState;
use super::types::{
    DELETED_CANDIDATE_EMAIL, DELETED_CANDIDATE_NAME, DELETED_USER_ID, KnownEmployee,
};
use chrono::Utc;
use serde_json::json;
use unified_core::hooks::HookContext;

/// `user.deleted` carries `user_id` (falling back to `id`,
/// the same shape the sign and ESG modules read). The
/// tenant's referral rows keep their history but lose the
/// identity: the referrer and every actor become
/// `deleted-user`. Candidate rows submitted by the deleted
/// user are anonymized too (name and email replaced with
/// markers) when the payload names the candidate email;
/// otherwise the candidate data is untouched - the deleted
/// user is the referrer, not necessarily the candidate.
/// Rows are never deleted: a referral history must stay on
/// record. Known-employee registry entries for the user are
/// removed, since the employee no longer exists.
pub(crate) fn on_user_deleted(state: ReferralState, ctx: &mut HookContext) {
    let user_id = ctx
        .get("user_id")
        .and_then(|value| value.as_str())
        .map(str::to_string)
        .or_else(|| {
            ctx.get("id")
                .and_then(|value| value.as_str())
                .map(str::to_string)
        });
    let Some(user_id) = user_id else {
        ctx.add_result(
            json!({ "referral": "skipped", "reason": "user.deleted carries no user_id" }),
        );
        return;
    };
    let candidate_email = ctx
        .get("candidate_email")
        .and_then(|value| value.as_str())
        .map(str::to_string);
    let now = Utc::now();
    let mut anonymized_links = 0i64;
    for mut entry in state.links.iter_mut() {
        if entry.value().referrer_user_id == user_id {
            entry.value_mut().referrer_user_id = DELETED_USER_ID.to_string();
            entry.value_mut().updated_at = now;
            anonymized_links += 1;
        }
        if entry.value().created_by == user_id {
            entry.value_mut().created_by = DELETED_USER_ID.to_string();
            entry.value_mut().updated_at = now;
        }
    }
    let mut anonymized_referrals = 0i64;
    for mut entry in state.referrals.iter_mut() {
        let mut changed = false;
        if entry.value().referrer_user_id == user_id {
            entry.value_mut().referrer_user_id = DELETED_USER_ID.to_string();
            changed = true;
        }
        if entry.value().created_by == user_id {
            entry.value_mut().created_by = DELETED_USER_ID.to_string();
            changed = true;
        }
        if let Some(email) = &candidate_email {
            if entry.value().candidate_email == *email {
                entry.value_mut().candidate_name = DELETED_CANDIDATE_NAME.to_string();
                entry.value_mut().candidate_email = DELETED_CANDIDATE_EMAIL.to_string();
                changed = true;
            }
        }
        if changed {
            entry.value_mut().updated_at = now;
            anonymized_referrals += 1;
        }
    }
    let mut anonymized_history = 0i64;
    for mut entry in state.stage_history.iter_mut() {
        if entry.value().moved_by == user_id {
            entry.value_mut().moved_by = DELETED_USER_ID.to_string();
            anonymized_history += 1;
        }
    }
    let mut anonymized_rewards = 0i64;
    for mut entry in state.rewards.iter_mut() {
        let mut changed = false;
        if entry.value().referrer_user_id == user_id {
            entry.value_mut().referrer_user_id = DELETED_USER_ID.to_string();
            changed = true;
        }
        if entry.value().created_by == user_id {
            entry.value_mut().created_by = DELETED_USER_ID.to_string();
            changed = true;
        }
        if entry.value().paid_by.as_deref() == Some(user_id.as_str()) {
            entry.value_mut().paid_by = Some(DELETED_USER_ID.to_string());
            changed = true;
        }
        if changed {
            entry.value_mut().updated_at = now;
            anonymized_rewards += 1;
        }
    }
    let mut anonymized_eligibility = 0i64;
    for mut entry in state.eligibility.iter_mut() {
        if entry.value().created_by == user_id {
            entry.value_mut().created_by = DELETED_USER_ID.to_string();
            entry.value_mut().updated_at = now;
            anonymized_eligibility += 1;
        }
    }
    let mut anonymized_stages = 0i64;
    for mut entry in state.stages.iter_mut() {
        if entry.value().created_by == user_id {
            entry.value_mut().created_by = DELETED_USER_ID.to_string();
            entry.value_mut().updated_at = now;
            anonymized_stages += 1;
        }
    }
    let mut anonymized_rules = 0i64;
    for mut entry in state.reward_rules.iter_mut() {
        if entry.value().created_by == user_id {
            entry.value_mut().created_by = DELETED_USER_ID.to_string();
            entry.value_mut().updated_at = now;
            anonymized_rules += 1;
        }
    }
    let mut anonymized_gaming = 0i64;
    for mut entry in state.anti_gaming_rules.iter_mut() {
        if entry.value().created_by == user_id {
            entry.value_mut().created_by = DELETED_USER_ID.to_string();
            entry.value_mut().updated_at = now;
            anonymized_gaming += 1;
        }
    }
    let mut anonymized_audit = 0i64;
    for mut entry in state.audit.iter_mut() {
        if entry.value().actor == user_id {
            entry.value_mut().actor = DELETED_USER_ID.to_string();
            anonymized_audit += 1;
        }
    }
    let removed_employees = if state.known_employees.remove(&user_id).is_some() {
        1i64
    } else {
        0i64
    };
    ctx.add_result(json!({
        "referral": "user_deleted",
        "user_id": user_id,
        "anonymized_links": anonymized_links,
        "anonymized_referrals": anonymized_referrals,
        "anonymized_history": anonymized_history,
        "anonymized_rewards": anonymized_rewards,
        "anonymized_eligibility": anonymized_eligibility,
        "anonymized_stages": anonymized_stages,
        "anonymized_rules": anonymized_rules,
        "anonymized_gaming_rules": anonymized_gaming,
        "anonymized_audit": anonymized_audit,
        "removed_known_employees": removed_employees,
    }));
}

/// `hr.employee_created` carries `{employee_id, user_id,
/// department_id}` (verified from the HR module's source)
/// and no tenant, so the registry is keyed by user id and
/// `tenant_id` is left empty. Later referrals check this
/// registry for the active-employee rule; absence means
/// unknown, never "not an employee".
pub(crate) fn on_hr_employee_created(state: ReferralState, ctx: &mut HookContext) {
    let user_id = ctx
        .get("user_id")
        .and_then(|value| value.as_str())
        .map(str::to_string);
    let employee_id = ctx
        .get("employee_id")
        .and_then(|value| value.as_str())
        .map(str::to_string);
    let (Some(user_id), Some(employee_id)) = (user_id, employee_id) else {
        ctx.add_result(json!({
            "referral": "skipped",
            "reason": "hr.employee_created carries no user_id or employee_id",
        }));
        return;
    };
    let department_id = ctx
        .get("department_id")
        .and_then(|value| value.as_str())
        .map(str::to_string);
    let tenant_id = ctx
        .get("tenant_id")
        .and_then(|value| value.as_str())
        .unwrap_or("")
        .to_string();
    state.known_employees.insert(
        user_id.clone(),
        KnownEmployee {
            employee_id,
            user_id: user_id.clone(),
            tenant_id,
            department_id,
            created_at: Utc::now(),
        },
    );
    ctx.add_result(json!({
        "referral": "employee_recorded",
        "user_id": user_id,
    }));
}

/// `recruitment.applicant.moved` carries `{applicant_id,
/// job_id, from_stage, to_stage, actor}` (verified from the
/// recruitment module's source) - it carries no referral
/// id, so there is nothing to sync unless a future
/// recruitment version adds one. When the payload does carry
/// `referral_id`, the referral is moved to the stage with
/// the matching slug, with the reward / void side effects.
/// Otherwise the event is acknowledged and skipped.
pub(crate) fn on_recruitment_applicant_moved(state: ReferralState, ctx: &mut HookContext) {
    let referral_id = ctx
        .get("referral_id")
        .and_then(|value| value.as_str())
        .map(str::to_string);
    let Some(referral_id) = referral_id else {
        ctx.add_result(json!({
            "referral": "skipped",
            "reason": "recruitment.applicant.moved carries no referral_id; recruitment applicants are not linked to referrals",
        }));
        return;
    };
    let tenant_id = ctx
        .get("tenant_id")
        .and_then(|value| value.as_str())
        .unwrap_or("")
        .to_string();
    let to_stage_slug = ctx
        .get("to_stage")
        .and_then(|value| value.as_str())
        .map(str::to_string);
    let Some(to_stage_slug) = to_stage_slug else {
        ctx.add_result(json!({
            "referral": "skipped",
            "reason": "recruitment.applicant.moved carries no to_stage",
        }));
        return;
    };
    sync_referral_stage(state, ctx, &tenant_id, &referral_id, &to_stage_slug);
}

/// `recruitment.hired` carries `{applicant_id, job_id,
/// offer_id, actor}` (verified from the recruitment
/// module's source) - it carries no referral id either, so
/// the same rule applies: sync only when `referral_id` is
/// present, moving the referral to the tenant's hired stage.
pub(crate) fn on_recruitment_hired(state: ReferralState, ctx: &mut HookContext) {
    let referral_id = ctx
        .get("referral_id")
        .and_then(|value| value.as_str())
        .map(str::to_string);
    let Some(referral_id) = referral_id else {
        ctx.add_result(json!({
            "referral": "skipped",
            "reason": "recruitment.hired carries no referral_id; recruitment hires are not linked to referrals",
        }));
        return;
    };
    let tenant_id = ctx
        .get("tenant_id")
        .and_then(|value| value.as_str())
        .unwrap_or("")
        .to_string();
    let stages = state.stages_of(&tenant_id);
    let Some(hired) = stages
        .iter()
        .find(|stage| stage.is_hired_stage && stage.active)
        .cloned()
    else {
        ctx.add_result(json!({
            "referral": "skipped",
            "reason": "tenant has no hired pipeline stage",
        }));
        return;
    };
    sync_referral_stage(state, ctx, &tenant_id, &referral_id, &hired.slug);
}

/// Shared sync: move one referral to the named stage when
/// the transition is allowed, with the hired / rejected
/// side effects. A disallowed transition is reported, not
/// forced - the recruitment pipeline and the referral
/// pipeline are separate data, and their stage names may
/// legitimately differ.
fn sync_referral_stage(
    state: ReferralState,
    ctx: &mut HookContext,
    tenant_id: &str,
    referral_id: &str,
    to_stage_slug: &str,
) {
    let referral_id_uuid = match uuid::Uuid::parse_str(referral_id) {
        Ok(id) => id,
        Err(_) => {
            ctx.add_result(json!({
                "referral": "skipped",
                "reason": "referral_id is not a uuid",
            }));
            return;
        }
    };
    let Some(referral) = state
        .referrals
        .get(&referral_id_uuid)
        .map(|entry| entry.value().clone())
        .filter(|referral| referral.tenant_id == tenant_id)
    else {
        ctx.add_result(json!({
            "referral": "skipped",
            "reason": "referral not found for this tenant",
        }));
        return;
    };
    let Some(to_stage) = state
        .stages_of(tenant_id)
        .into_iter()
        .find(|stage| stage.slug == to_stage_slug)
    else {
        ctx.add_result(json!({
            "referral": "skipped",
            "reason": "no pipeline stage matches the recruitment stage slug",
        }));
        return;
    };
    let actor = ctx
        .get("actor")
        .and_then(|value| value.as_str())
        .unwrap_or("recruitment")
        .to_string();
    let reason = Some("synced from recruitment".to_string());
    match record_stage_move(&state, tenant_id, &referral, &to_stage, &actor, &reason) {
        Ok(updated) => {
            let now = Utc::now();
            if to_stage.is_hired_stage {
                on_referral_hired(&state, tenant_id, &updated, now, &actor);
            }
            if to_stage.is_rejected_stage {
                void_open_rewards(&state, tenant_id, &updated, "referral rejected", &actor);
            }
            ctx.add_result(json!({
                "referral": "stage_synced",
                "referral_id": referral_id,
                "to_stage": to_stage.slug,
            }));
        }
        Err(error) => {
            ctx.add_result(json!({
                "referral": "skipped",
                "reason": format!("stage transition not allowed: {error}"),
            }));
        }
    }
}
