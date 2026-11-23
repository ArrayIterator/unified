// Gamification — Badge / award handlers.

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

// ---------------------------------------------------------------------------
// Badge / award handlers
// ---------------------------------------------------------------------------

pub(crate) async fn list_badges(
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
        let action_filter = params.filters.get("criteria_action").cloned();
        let search = params.search.clone().unwrap_or_default().to_lowercase();
        let mut items: Vec<GamificationBadge> = state
            .badges
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|badge| {
                if let Some(action) = &action_filter {
                    if &badge.criteria_action != action {
                        return false;
                    }
                }
                if !search.is_empty()
                    && !format!("{} {}", badge.name, badge.description)
                        .to_lowercase()
                        .contains(&search)
                {
                    return false;
                }
                true
            })
            .collect();
        items.sort_by_key(|badge| badge.name.clone());
        Ok(paginate(items, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn create_badge(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<GamificationState>,
    req: HttpRequest,
    body: web::Json<CreateBadgeRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<GamificationBadge> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = require_company_for_write(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_GAMIFICATION_MANAGE)?;
        let input = body.into_inner();
        let name = require_text(&input.name, "name", MAX_NAME_LEN)?;
        let description = require_text(&input.description, "description", MAX_DESCRIPTION_LEN)?;
        let criteria_action = require_text(&input.criteria_action, "criteria_action", MAX_ACTION_LEN)?;
        if !valid_action(&criteria_action) {
            return Err(UnifiedError::bad_request(format!(
                "Unknown action \"{criteria_action}\"; known actions: {}",
                KNOWN_ACTIONS.join(", ")
            )));
        }
        if input.criteria_threshold < 1 || input.criteria_threshold > 1_000_000 {
            return Err(UnifiedError::bad_request(
                "criteria_threshold must be between 1 and 1000000",
            ));
        }
        let icon = match &input.icon {
            Some(raw) => Some(require_text(raw, "icon", MAX_NAME_LEN)?),
            None => None,
        };
        let now = Utc::now();
        let badge = GamificationBadge {
            id: Uuid::now_v7(),
            name,
            description,
            criteria_action,
            criteria_threshold: input.criteria_threshold,
            icon,
            created_at: now,
            updated_at: now,
        };
        state.badges.insert(badge.id, badge.clone());
        state.emit(
            "gamification.badge.created",
            json!({
                "badge_id": badge.id.to_string(),
                "name": badge.name,
                "criteria_action": badge.criteria_action,
                "criteria_threshold": badge.criteria_threshold,
            }),
        );
        Ok(badge)
    })();
    match result {
        Ok(badge) => created(badge),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn get_badge(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<GamificationState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<GamificationBadge> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_GAMIFICATION_VIEW)?;
        load_badge(&state, &path.into_inner())
    })();
    match result {
        Ok(badge) => ok(badge),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn update_badge(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<GamificationState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdateBadgeRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<GamificationBadge> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = require_company_for_write(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_GAMIFICATION_MANAGE)?;
        let mut badge = load_badge(&state, &path.into_inner())?;
        let input = body.into_inner();
        if let Some(name) = &input.name {
            badge.name = require_text(name, "name", MAX_NAME_LEN)?;
        }
        if let Some(description) = &input.description {
            badge.description = require_text(description, "description", MAX_DESCRIPTION_LEN)?;
        }
        if let Some(action) = &input.criteria_action {
            let checked = require_text(action, "criteria_action", MAX_ACTION_LEN)?;
            if !valid_action(&checked) {
                return Err(UnifiedError::bad_request(format!(
                    "Unknown action \"{checked}\"; known actions: {}",
                    KNOWN_ACTIONS.join(", ")
                )));
            }
            badge.criteria_action = checked;
        }
        if let Some(threshold) = input.criteria_threshold {
            if threshold < 1 || threshold > 1_000_000 {
                return Err(UnifiedError::bad_request(
                    "criteria_threshold must be between 1 and 1000000",
                ));
            }
            badge.criteria_threshold = threshold;
        }
        if let Some(icon) = &input.icon {
            badge.icon = match icon {
                Some(raw) => Some(require_text(raw, "icon", MAX_NAME_LEN)?),
                None => None,
            };
        }
        badge.updated_at = Utc::now();
        state.badges.insert(badge.id, badge.clone());
        Ok(badge)
    })();
    match result {
        Ok(badge) => ok(badge),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn delete_badge(
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
        let badge = load_badge(&state, &path.into_inner())?;
        if state
            .awards
            .iter()
            .any(|entry| entry.value().badge_id == badge.id)
        {
            return Err(UnifiedError::conflict(
                "A badge with awards cannot be deleted; its award records must stay auditable",
            ));
        }
        let id = badge.id;
        state.badges.remove(&id);
        Ok(json!({ "deleted": true, "badge_id": id.to_string() }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn list_awards(
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
        let is_manager = can_manage(&ctx, &caller.role);
        let params = query.into_inner();
        let user_filter = params.filters.get("user_id").cloned();
        let badge_filter = params
            .filters
            .get("badge_id")
            .and_then(|raw| Uuid::parse_str(raw).ok());
        let mut items: Vec<Value> = state
            .awards
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|award| {
                if !is_manager && award.user_id != caller.actor {
                    return false;
                }
                if let Some(user) = &user_filter {
                    if &award.user_id != user {
                        return false;
                    }
                }
                if let Some(badge) = badge_filter {
                    if award.badge_id != badge {
                        return false;
                    }
                }
                true
            })
            .map(|award| {
                let badge_name = state
                    .badges
                    .get(&award.badge_id)
                    .map(|entry| entry.value().name.clone());
                let mut value = serde_json::to_value(&award).unwrap_or(Value::Null);
                if let Value::Object(map) = &mut value {
                    map.insert(
                        "badge_name".to_string(),
                        badge_name.map(Value::String).unwrap_or(Value::Null),
                    );
                }
                value
            })
            .collect();
        items.sort_by(|a, b| {
            b.get("awarded_at")
                .and_then(Value::as_str)
                .cmp(&a.get("awarded_at").and_then(Value::as_str))
        });
        Ok(paginate(items, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn get_award(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<GamificationState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<GamificationAward> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_GAMIFICATION_VIEW)?;
        let id = parse_uuid(&path.into_inner(), "award")?;
        let award = state
            .awards
            .get(&id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found(format!("Award not found: {id}")))?;
        if award.user_id != caller.actor && !can_manage(&ctx, &caller.role) {
            return Err(UnifiedError::not_found(format!("Award not found: {id}")));
        }
        Ok(award)
    })();
    match result {
        Ok(award) => ok(award),
        Err(error) => err(error, debug),
    }
}
