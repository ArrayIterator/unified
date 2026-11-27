// Gamification - Rule handlers.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::{HookBus, HookContext};
use unified_core::json;
use unified_core::modules::{Module, ModuleRegistry, RouteInfo, RouteVersionSupport};
use unified_core::pagination::{PageResult, PaginationParams};
use actix_web::{web, HttpRequest, HttpResponse};
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::sync::{Arc, RwLock};
use uuid::Uuid;

use super::*;

// Rule handlers

pub(crate) async fn list_rules(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<GamificationState>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_GAMIFICATION_VIEW)?;
        let params = query.into_inner();
        let action_filter = params.filters.get("action").cloned();
        let company_filter = params.filters.get("company_id").cloned();
        let active_filter = params
            .filters
            .get("active")
            .and_then(|raw| raw.parse::<bool>().ok());
        let mut items: Vec<GamificationRule> = state
            .rules
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|rule| in_company_scope(&rule.company_id, &_scope))
            .filter(|rule| {
                if let Some(action) = &action_filter {
                    if &rule.action != action {
                        return false;
                    }
                }
                if let Some(company) = &company_filter {
                    if rule.company_id.as_deref() != Some(company.as_str()) {
                        return false;
                    }
                }
                if let Some(active) = active_filter {
                    if rule.active != active {
                        return false;
                    }
                }
                true
            })
            .collect();
        items.sort_by_key(|rule| (rule.action.clone(), rule.name.clone()));
        Ok(paginate(items, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

/// Validate a rule's daily cap. Zero is rejected: a cap of zero
/// would silently award nothing, so "no cap" must be spelled as
/// null (`None`), never as 0.
pub(crate) fn validate_daily_cap(daily_cap: Option<i64>) -> UnifiedResult<()> {
    match daily_cap {
        Some(0) => Err(UnifiedError::bad_request(
            "daily_cap must not be 0; use null for unlimited",
        )),
        Some(cap) if cap < 0 || cap > 1_000_000 => Err(UnifiedError::bad_request(
            "daily_cap must be between 0 and 1000000",
        )),
        _ => Ok(()),
    }
}

pub(crate) async fn create_rule(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<GamificationState>,
    req: HttpRequest,
    body: web::Json<CreateRuleRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<GamificationRule> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = require_company_for_write(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_GAMIFICATION_MANAGE)?;
        let input = body.into_inner();
        let name = require_text(&input.name, "name", MAX_NAME_LEN)?;
        let action = require_text(&input.action, "action", MAX_ACTION_LEN)?;
        if !valid_action(&action) {
            return Err(UnifiedError::bad_request(format!(
                "Unknown action \"{action}\"; known actions: {}",
                KNOWN_ACTIONS.join(", ")
            )));
        }
        if input.points < 0 || input.points > 1_000_000 {
            return Err(UnifiedError::bad_request(
                "points must be between 0 and 1000000",
            ));
        }
        validate_daily_cap(input.daily_cap)?;
        let company_id = require_company_match(
            &_tenant,
            &req,
            &match &input.company_id {
                Some(raw) => Some(require_text(raw, "company_id", MAX_NAME_LEN)?),
                None => None,
            },
        )?;
        let now = Utc::now();
        let rule = GamificationRule {
            id: Uuid::now_v7(),
            name,
            action,
            points: input.points,
            company_id,
            daily_cap: input.daily_cap,
            active: input.active.unwrap_or(true),
            created_at: now,
            updated_at: now,
        };
        state.rules.insert(rule.id, rule.clone());
        state.emit(
            "gamification.rule.created",
            json!({
                "rule_id": rule.id.to_string(),
                "name": rule.name,
                "action": rule.action,
                "points": rule.points,
            }),
        );
        Ok(rule)
    })();
    match result {
        Ok(rule) => created(rule),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn get_rule(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<GamificationState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<GamificationRule> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_GAMIFICATION_VIEW)?;
        let rule = load_rule(&state, &path.into_inner())?;
        require_record_in_company_scope(&rule.company_id, &_scope)?;
        require_record_in_company_scope(&rule.company_id, &_scope)?;
        Ok(rule)
    })();
    match result {
        Ok(rule) => ok(rule),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn update_rule(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<GamificationState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdateRuleRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<GamificationRule> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = require_company_for_write(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_GAMIFICATION_MANAGE)?;
        let mut rule = load_rule(&state, &path.into_inner())?;
        require_record_writable_in_company_scope(&rule.company_id, &_scope)?;
        let input = body.into_inner();
        if let Some(name) = &input.name {
            rule.name = require_text(name, "name", MAX_NAME_LEN)?;
        }
        if let Some(action) = &input.action {
            let checked = require_text(action, "action", MAX_ACTION_LEN)?;
            if !valid_action(&checked) {
                return Err(UnifiedError::bad_request(format!(
                    "Unknown action \"{checked}\"; known actions: {}",
                    KNOWN_ACTIONS.join(", ")
                )));
            }
            rule.action = checked;
        }
        if let Some(points) = input.points {
            if points < 0 || points > 1_000_000 {
                return Err(UnifiedError::bad_request(
                    "points must be between 0 and 1000000",
                ));
            }
            rule.points = points;
        }
        if let Some(company_id) = &input.company_id {
            rule.company_id = match company_id {
                Some(raw) => Some(require_text(raw, "company_id", MAX_NAME_LEN)?),
                None => None,
            };
        }
        if let Some(daily_cap) = &input.daily_cap {
            validate_daily_cap(*daily_cap)?;
            rule.daily_cap = *daily_cap;
        }
        if let Some(active) = input.active {
            rule.active = active;
        }
        rule.updated_at = Utc::now();
        state.rules.insert(rule.id, rule.clone());
        state.emit(
            "gamification.rule.updated",
            json!({ "rule_id": rule.id.to_string(), "action": rule.action, "points": rule.points }),
        );
        Ok(rule)
    })();
    match result {
        Ok(rule) => ok(rule),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn delete_rule(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<GamificationState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = require_company_for_write(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_GAMIFICATION_MANAGE)?;
        let rule = load_rule(&state, &path.into_inner())?;
        require_record_writable_in_company_scope(&rule.company_id, &_scope)?;
        let id = rule.id;
        state.rules.remove(&id);
        state.emit(
            "gamification.rule.deleted",
            json!({ "rule_id": id.to_string(), "name": rule.name }),
        );
        Ok(json!({ "deleted": true, "rule_id": id.to_string() }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn daily_cap_zero_is_rejected_with_400() {
        let error = validate_daily_cap(Some(0)).expect_err("daily_cap 0 must be rejected");
        assert_eq!(error.code, 400);
        assert!(
            error.message.contains("use null for unlimited"),
            "message must tell the caller to use null: {}",
            error.message
        );
    }

    #[test]
    fn daily_cap_null_and_positive_are_accepted() {
        assert!(validate_daily_cap(None).is_ok());
        assert!(validate_daily_cap(Some(1)).is_ok());
        assert!(validate_daily_cap(Some(1_000_000)).is_ok());
    }

    #[test]
    fn daily_cap_negative_and_over_max_are_rejected() {
        assert!(validate_daily_cap(Some(-1)).is_err());
        assert!(validate_daily_cap(Some(1_000_001)).is_err());
    }
}
