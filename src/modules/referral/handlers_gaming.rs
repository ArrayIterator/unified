//! HTTP handlers for anti-gaming rules: the data-driven
//! checks (self-referral, same-candidate cooldown,
//! per-period cap, active-employee) and their enforcement.

use actix_web::{HttpRequest, HttpResponse, web};
use chrono::Utc;
use serde_json::{Value, json};
use std::sync::Arc;
use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::pagination::PaginationParams;
use uuid::Uuid;

use super::helpers::{
    caller, caller_company, created, err, load_gaming_rule, ok, paginate, record_audit,
    require_manage, require_tenant, require_text, require_view, resolve_tenant,
    validate_gaming_rule_input,
};
use super::state::ReferralState;
use super::types::{
    AntiGamingKind, AntiGamingRule, CreateAntiGamingRuleRequest, Enforcement, MAX_NAME_LEN,
    UpdateAntiGamingRuleRequest,
};

/// List anti-gaming rules (paginated, filter by kind,
/// enabled, enforcement).
pub(crate) async fn list_anti_gaming_rules(
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
        let kind_filter = params
            .filters
            .get("rule_kind")
            .and_then(|raw| AntiGamingKind::from_str(raw));
        let enabled_filter = params
            .filters
            .get("enabled")
            .and_then(|raw| match raw.as_str() {
                "true" => Some(true),
                "false" => Some(false),
                _ => None,
            });
        let enforcement_filter = params
            .filters
            .get("enforcement")
            .and_then(|raw| Enforcement::from_str(raw));
        let mut items = state.anti_gaming_rules_of(&tenant_id);
        items.retain(|rule| {
            if let Some(kind) = kind_filter {
                if rule.rule_kind != kind {
                    return false;
                }
            }
            if let Some(enabled) = enabled_filter {
                if rule.enabled != enabled {
                    return false;
                }
            }
            if let Some(enforcement) = enforcement_filter {
                if rule.enforcement != enforcement {
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

/// Create an anti-gaming rule (manager).
pub(crate) async fn create_anti_gaming_rule(
    state: web::Data<ReferralState>,
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    body: web::Json<CreateAntiGamingRuleRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let tenant_id = resolve_tenant(&req)?;
        let caller = caller(&req)?;
        require_manage(&ctx, &caller.role)?;
        let input = body.into_inner();
        let (name, kind, enforcement) = validate_gaming_rule_input(&input)?;
        let now = Utc::now();
        let rule = AntiGamingRule {
            id: Uuid::new_v4(),
            tenant_id: tenant_id.clone(),
            name,
            rule_kind: kind,
            enabled: input.enabled,
            limit_count: input.limit_count,
            window_days: input.window_days,
            cooldown_days: input.cooldown_days,
            enforcement,
            created_by: caller.actor.clone(),
            created_at: now,
            updated_at: now,
        };
        state.anti_gaming_rules.insert(rule.id, rule.clone());
        record_audit(
            &state,
            &tenant_id,
            "anti_gaming_rule",
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

/// Get one anti-gaming rule.
pub(crate) async fn get_anti_gaming_rule(
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
        let rule = load_gaming_rule(&state, &tenant_id, &path.into_inner())?;
        Ok(serde_json::to_value(&rule).unwrap_or(Value::Null))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

/// Update an anti-gaming rule (manager). Changing the kind
/// re-validates the thresholds for the new kind.
pub(crate) async fn update_anti_gaming_rule(
    state: web::Data<ReferralState>,
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    path: web::Path<String>,
    body: web::Json<UpdateAntiGamingRuleRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let tenant_id = resolve_tenant(&req)?;
        let caller = caller(&req)?;
        require_manage(&ctx, &caller.role)?;
        let input = body.into_inner();
        let mut rule = load_gaming_rule(&state, &tenant_id, &path.into_inner())?;
        let before = serde_json::to_value(&rule).ok();
        if let Some(name) = &input.name {
            rule.name = require_text(name, "name", MAX_NAME_LEN)?;
        }
        if let Some(kind_raw) = &input.rule_kind {
            rule.rule_kind = AntiGamingKind::from_str(kind_raw.trim()).ok_or_else(|| {
                UnifiedError::bad_request(
                    "rule_kind must be block_self_referral, same_candidate_cooldown, max_referrals_per_period, or require_active_employee"
                        .to_string(),
                )
            })?;
        }
        if let Some(limit) = input.limit_count {
            rule.limit_count = limit;
        }
        if let Some(window) = input.window_days {
            rule.window_days = window;
        }
        if let Some(cooldown) = input.cooldown_days {
            rule.cooldown_days = cooldown;
        }
        if let Some(enforcement_raw) = &input.enforcement {
            rule.enforcement = Enforcement::from_str(enforcement_raw.trim()).ok_or_else(|| {
                UnifiedError::bad_request("enforcement must be block or flag".to_string())
            })?;
        }
        if let Some(enabled) = input.enabled {
            rule.enabled = enabled;
        }
        // Re-validate the thresholds for the (possibly new)
        // kind, using the merged values.
        let merged = CreateAntiGamingRuleRequest {
            name: rule.name.clone(),
            rule_kind: rule.rule_kind.as_str().to_string(),
            enabled: rule.enabled,
            limit_count: rule.limit_count,
            window_days: rule.window_days,
            cooldown_days: rule.cooldown_days,
            enforcement: rule.enforcement.as_str().to_string(),
        };
        validate_gaming_rule_input(&merged)?;
        rule.updated_at = Utc::now();
        state.anti_gaming_rules.insert(rule.id, rule.clone());
        record_audit(
            &state,
            &tenant_id,
            "anti_gaming_rule",
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

/// Delete an anti-gaming rule (manager).
pub(crate) async fn delete_anti_gaming_rule(
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
        let rule = load_gaming_rule(&state, &tenant_id, &path.into_inner())?;
        let before = serde_json::to_value(&rule).ok();
        state.anti_gaming_rules.remove(&rule.id);
        record_audit(
            &state,
            &tenant_id,
            "anti_gaming_rule",
            &rule.id.to_string(),
            "deleted",
            &caller.actor,
            before,
            None,
        );
        Ok(json!({ "deleted": true, "anti_gaming_rule_id": rule.id.to_string() }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}
