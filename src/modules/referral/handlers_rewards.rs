//! HTTP handlers for rewards: what is owed to whom, when
//! it becomes payable, and recording that the payout
//! happened elsewhere (this module never moves money).

use actix_web::{HttpRequest, HttpResponse, web};
use chrono::Utc;
use serde_json::{Value, json};
use std::sync::Arc;
use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::pagination::PaginationParams;

use super::helpers::{
    caller, caller_company, err, load_reward, ok, paginate, record_audit,
    refresh_reward_eligibility, require_manage, require_tenant, require_text, require_view,
    resolve_tenant,
};
use super::state::ReferralState;
use super::types::{
    EVENT_REWARD_PAID, EVENT_REWARD_VOIDED, MAX_REASON_LEN, MarkPaidRequest, RewardStatus,
    VoidRewardRequest,
};

/// List rewards (paginated, filter by referral, job,
/// referrer, rule, status).
pub(crate) async fn list_rewards(
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
        let referral_filter = params.filters.get("referral_id").cloned();
        let job_filter = params.filters.get("job_id").cloned();
        let referrer_filter = params.filters.get("referrer_user_id").cloned();
        let rule_filter = params.filters.get("rule_id").cloned();
        let status_filter = params
            .filters
            .get("status")
            .and_then(|raw| RewardStatus::from_str(raw));
        let mut items = state.rewards_of(&tenant_id);
        items.retain(|reward| {
            if let Some(referral_id) = &referral_filter {
                if reward.referral_id.to_string() != *referral_id {
                    return false;
                }
            }
            if let Some(job_id) = &job_filter {
                if reward.job_id.to_string() != *job_id {
                    return false;
                }
            }
            if let Some(referrer) = &referrer_filter {
                if reward.referrer_user_id != *referrer {
                    return false;
                }
            }
            if let Some(rule_id) = &rule_filter {
                if reward.rule_id.to_string() != *rule_id {
                    return false;
                }
            }
            if let Some(status) = status_filter {
                if reward.status != status {
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

/// Get one reward.
pub(crate) async fn get_reward(
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
        let reward = load_reward(&state, &tenant_id, &path.into_inner())?;
        Ok(serde_json::to_value(&reward).unwrap_or(Value::Null))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

/// Mark a reward paid (manager): records that billing / HR
/// paid the reward out elsewhere — this endpoint moves no
/// money, it only records the payout with who marked it and
/// when. A pending reward whose retention period has elapsed
/// is promoted to eligible first, so a payout is never
/// blocked on a stale status row.
pub(crate) async fn mark_reward_paid(
    state: web::Data<ReferralState>,
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    path: web::Path<String>,
    body: web::Json<MarkPaidRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let tenant_id = resolve_tenant(&req)?;
        let caller = caller(&req)?;
        require_manage(&ctx, &caller.role)?;
        let input = body.into_inner();
        let _note = match &input.note {
            Some(text) => Some(require_text(text, "note", MAX_REASON_LEN)?),
            None => None,
        };
        // `Path::into_inner` moves `path`, so take the id out once
        // and reuse it for the pre- and post-refresh loads.
        let reward_id = path.into_inner();
        let reward = load_reward(&state, &tenant_id, &reward_id)?;
        let now = Utc::now();
        refresh_reward_eligibility(&state, &tenant_id, &reward, now);
        let reward = load_reward(&state, &tenant_id, &reward_id)?;
        if reward.status != RewardStatus::Eligible {
            return Err(UnifiedError::conflict(format!(
                "Reward is {} and cannot be marked paid; only an eligible reward can be paid",
                reward.status.as_str()
            )));
        }
        let before = serde_json::to_value(&reward).ok();
        let mut updated = reward.clone();
        updated.status = RewardStatus::Paid;
        updated.paid_at = Some(now);
        updated.paid_by = Some(caller.actor.clone());
        updated.updated_at = now;
        state.rewards.insert(updated.id, updated.clone());
        record_audit(
            &state,
            &tenant_id,
            "reward",
            &reward.id.to_string(),
            "paid",
            &caller.actor,
            before,
            serde_json::to_value(&updated).ok(),
        );
        state.emit(
            EVENT_REWARD_PAID,
            json!({
                "tenant_id": tenant_id,
                "reward_id": reward.id.to_string(),
                "referral_id": reward.referral_id.to_string(),
                "job_id": reward.job_id.to_string(),
                "referrer_user_id": reward.referrer_user_id,
                "amount_minor": reward.amount_minor,
                "currency": reward.currency,
                "paid_by": caller.actor,
            }),
        );
        Ok(serde_json::to_value(&updated).unwrap_or(Value::Null))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

/// Void a reward (manager): only a pending or eligible
/// reward can be voided; a paid reward stays on record.
pub(crate) async fn void_reward(
    state: web::Data<ReferralState>,
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    path: web::Path<String>,
    body: web::Json<VoidRewardRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let tenant_id = resolve_tenant(&req)?;
        let caller = caller(&req)?;
        require_manage(&ctx, &caller.role)?;
        let input = body.into_inner();
        let reason = match &input.reason {
            Some(text) => Some(require_text(text, "reason", MAX_REASON_LEN)?),
            None => None,
        };
        let reward = load_reward(&state, &tenant_id, &path.into_inner())?;
        if !matches!(
            reward.status,
            RewardStatus::Pending | RewardStatus::Eligible
        ) {
            return Err(UnifiedError::conflict(format!(
                "Reward is {} and cannot be voided; only a pending or eligible reward can be voided",
                reward.status.as_str()
            )));
        }
        let before = serde_json::to_value(&reward).ok();
        let mut updated = reward.clone();
        updated.status = RewardStatus::Voided;
        updated.void_reason = reason.clone();
        updated.updated_at = Utc::now();
        state.rewards.insert(updated.id, updated.clone());
        record_audit(
            &state,
            &tenant_id,
            "reward",
            &reward.id.to_string(),
            "voided",
            &caller.actor,
            before,
            serde_json::to_value(&updated).ok(),
        );
        state.emit(
            EVENT_REWARD_VOIDED,
            json!({
                "tenant_id": tenant_id,
                "reward_id": reward.id.to_string(),
                "referral_id": reward.referral_id.to_string(),
                "job_id": reward.job_id.to_string(),
                "referrer_user_id": reward.referrer_user_id,
                "reason": reason,
            }),
        );
        Ok(serde_json::to_value(&updated).unwrap_or(Value::Null))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}
