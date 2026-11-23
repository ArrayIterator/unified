//! HTTP handlers for reward rules: the data-driven payout
//! rules (amount in minor units, trigger stage, retention
//! days, eligibility conditions, default flag).

use actix_web::{HttpRequest, HttpResponse, web};
use chrono::Utc;
use serde_json::{Value, json};
use std::sync::Arc;
use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::pagination::PaginationParams;
use uuid::Uuid;

use super::helpers::{
    caller, caller_company, created, err, load_rule, load_stage, ok, paginate, record_audit,
    require_manage, require_tenant, require_text, require_view, resolve_tenant,
    validate_conditions, validate_currency_code, validate_reward_rule_input,
};
use super::state::ReferralState;
use super::types::{
    CreateRewardRuleRequest, MAX_DESCRIPTION_LEN, MAX_NAME_LEN, RewardRule, UpdateRewardRuleRequest,
};

/// List reward rules (paginated, filter by active,
/// default; search by name/description).
pub(crate) async fn list_reward_rules(
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
        let active_filter = params
            .filters
            .get("active")
            .and_then(|raw| match raw.as_str() {
                "true" => Some(true),
                "false" => Some(false),
                _ => None,
            });
        let default_filter = params
            .filters
            .get("is_default")
            .and_then(|raw| match raw.as_str() {
                "true" => Some(true),
                "false" => Some(false),
                _ => None,
            });
        let search = params.search.clone().map(|needle| needle.to_lowercase());
        let mut items = state.reward_rules_of(&tenant_id);
        items.retain(|rule| {
            if let Some(active) = active_filter {
                if rule.active != active {
                    return false;
                }
            }
            if let Some(is_default) = default_filter {
                if rule.is_default != is_default {
                    return false;
                }
            }
            if let Some(needle) = &search {
                if !rule.name.to_lowercase().contains(needle)
                    && !rule
                        .description
                        .as_deref()
                        .unwrap_or("")
                        .to_lowercase()
                        .contains(needle)
                {
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

/// Clear the default flag on every other rule of the
/// tenant: at most one default rule exists per tenant.
fn clear_other_defaults(state: &ReferralState, tenant_id: &str, keep_id: Uuid) {
    for mut entry in state.reward_rules.iter_mut() {
        if entry.value().tenant_id == tenant_id
            && entry.value().id != keep_id
            && entry.value().is_default
        {
            entry.value_mut().is_default = false;
            entry.value_mut().updated_at = Utc::now();
        }
    }
}

/// Create a reward rule (manager).
pub(crate) async fn create_reward_rule(
    state: web::Data<ReferralState>,
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    body: web::Json<CreateRewardRuleRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let tenant_id = resolve_tenant(&req)?;
        let caller = caller(&req)?;
        require_manage(&ctx, &caller.role)?;
        let input = body.into_inner();
        let (
            name,
            description,
            amount_minor,
            currency,
            trigger_stage_id,
            retention_days,
            conditions,
        ) = validate_reward_rule_input(&state, &tenant_id, &input)?;
        let now = Utc::now();
        let rule = RewardRule {
            id: Uuid::new_v4(),
            tenant_id: tenant_id.clone(),
            name,
            description,
            amount_minor,
            currency,
            trigger_stage_id,
            retention_days,
            eligibility_conditions: conditions,
            is_default: input.is_default,
            active: true,
            created_by: caller.actor.clone(),
            created_at: now,
            updated_at: now,
        };
        state.reward_rules.insert(rule.id, rule.clone());
        if rule.is_default {
            clear_other_defaults(&state, &tenant_id, rule.id);
        }
        record_audit(
            &state,
            &tenant_id,
            "reward_rule",
            &rule.id.to_string(),
            "created",
            &caller.actor,
            None,
            serde_json::to_value(&rule).ok(),
        );
        Ok(serde_json::to_value(&rule).unwrap_or(Value::Null))
    })();
    match result {
        Ok(data) => created(data),
        Err(error) => err(error, debug),
    }
}

/// Get one reward rule.
pub(crate) async fn get_reward_rule(
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
        let rule = load_rule(&state, &tenant_id, &path.into_inner())?;
        Ok(serde_json::to_value(&rule).unwrap_or(Value::Null))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

/// Update a reward rule (manager). Editing a rule never
/// rewrites rewards already created from it: rewards
/// snapshot the amount, currency, and rule name at
/// creation.
pub(crate) async fn update_reward_rule(
    state: web::Data<ReferralState>,
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    path: web::Path<String>,
    body: web::Json<UpdateRewardRuleRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let tenant_id = resolve_tenant(&req)?;
        let caller = caller(&req)?;
        require_manage(&ctx, &caller.role)?;
        let input = body.into_inner();
        let mut rule = load_rule(&state, &tenant_id, &path.into_inner())?;
        let before = serde_json::to_value(&rule).ok();
        if let Some(name) = &input.name {
            rule.name = require_text(name, "name", MAX_NAME_LEN)?;
        }
        if let Some(description) = &input.description {
            rule.description = match description {
                Some(text) => Some(require_text(text, "description", MAX_DESCRIPTION_LEN)?),
                None => None,
            };
        }
        if let Some(amount) = input.amount_minor {
            if amount < 0 {
                return Err(UnifiedError::bad_request(
                    "amount_minor must not be negative".to_string(),
                ));
            }
            rule.amount_minor = amount;
        }
        if let Some(currency) = &input.currency {
            rule.currency = validate_currency_code(currency)?;
        }
        if let Some(trigger) = &input.trigger_stage_id {
            rule.trigger_stage_id = match trigger {
                Some(raw) => Some(load_stage(&state, &tenant_id, raw)?.id),
                None => None,
            };
        }
        if let Some(retention) = input.retention_days {
            if retention < 0 {
                return Err(UnifiedError::bad_request(
                    "retention_days must not be negative".to_string(),
                ));
            }
            rule.retention_days = retention;
        }
        if let Some(conditions) = &input.eligibility_conditions {
            rule.eligibility_conditions = validate_conditions(conditions)?;
        }
        if let Some(is_default) = input.is_default {
            rule.is_default = is_default;
        }
        if let Some(active) = input.active {
            rule.active = active;
        }
        rule.updated_at = Utc::now();
        state.reward_rules.insert(rule.id, rule.clone());
        if rule.is_default {
            clear_other_defaults(&state, &tenant_id, rule.id);
        }
        record_audit(
            &state,
            &tenant_id,
            "reward_rule",
            &rule.id.to_string(),
            "updated",
            &caller.actor,
            before,
            serde_json::to_value(&rule).ok(),
        );
        Ok(serde_json::to_value(&rule).unwrap_or(Value::Null))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

/// Delete a reward rule (manager). Rewards already created
/// from it are kept — they snapshot the rule — and jobs
/// that pointed at it fall back to the tenant default.
pub(crate) async fn delete_reward_rule(
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
        let rule = load_rule(&state, &tenant_id, &path.into_inner())?;
        let before = serde_json::to_value(&rule).ok();
        state.reward_rules.remove(&rule.id);
        for mut entry in state.eligibility.iter_mut() {
            if entry.value().tenant_id == tenant_id && entry.value().reward_rule_id == Some(rule.id)
            {
                entry.value_mut().reward_rule_id = None;
                entry.value_mut().updated_at = Utc::now();
            }
        }
        record_audit(
            &state,
            &tenant_id,
            "reward_rule",
            &rule.id.to_string(),
            "deleted",
            &caller.actor,
            before,
            None,
        );
        Ok(json!({ "deleted": true, "reward_rule_id": rule.id.to_string() }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}
