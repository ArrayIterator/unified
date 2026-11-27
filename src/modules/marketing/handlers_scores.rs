// Marketing Automation - Lead scoring handlers.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::{HookBus, HookContext};
use unified_core::json as api_json;
use unified_core::modules::{Module, ModuleRegistry, RouteInfo, RouteVersionSupport};
use unified_core::pagination::{PageResult, PaginationParams};
use actix_web::{web, HttpRequest, HttpResponse};
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, RwLock};
use uuid::Uuid;

use super::constants::*;
use super::types::*;
use super::store::*;
use super::helpers::*;
use super::validation::*;
use super::service::*;
use super::service_sequences::*;
use super::handlers_campaigns::*;
use super::handlers_segments::*;
use super::handlers_sequences::*;
use super::handlers_audience::*;
use super::hooks::*;
use super::module::*;

// Scoring handlers.

pub(crate) async fn list_scores(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<MarketingState>>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PageResult<ContactScore>> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_MARKETING_VIEW)?;
        let params = query.into_inner();
        let mut items: Vec<ContactScore> = state
            .scores
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|record| {
                matches_search(&format!("{} {}", record.email, record.contact_id), &params)
                    && params.filters.get("hot").is_none_or(|wanted| {
                        record.hot == (wanted == "true")
                    })
                    && params.filters.get("warm").is_none_or(|wanted| {
                        record.warm == (wanted == "true")
                    })
            })
            .collect();
        // Refresh each visible score so decay is applied at read time.
        for record in &mut items {
            let total: i64 = record
                .events
                .iter()
                .map(|event| effective_event_points(&state, event))
                .sum();
            record.score = total;
        }
        items.sort_by(|a, b| b.score.cmp(&a.score));
        if params.normalized().order == "asc" {
            items.reverse();
        }
        Ok(paginate(items, &params))
    })();
    match result {
        Ok(page) => ok(page),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn get_score(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<MarketingState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<ContactScore> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_MARKETING_VIEW)?;
        let contact_id = require_text(path.as_str(), "contact_id", 200)?;
        refresh_score(&state, &contact_id);
        state
            .scores
            .get(&contact_id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("No score recorded for this contact"))
    })();
    match result {
        Ok(score) => ok(score),
        Err(error) => err(error, debug),
    }
}

#[derive(Debug, Deserialize)]
pub(crate) struct RecordScoreInput {
    action: String,
    email: Option<String>,
}

pub(crate) async fn record_score(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<MarketingState>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<RecordScoreInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_MARKETING_MANAGE)?;
        let contact_id = require_text(path.as_str(), "contact_id", 200)?;
        let input = body.into_inner();
        let action = require_text(&input.action, "action", 200)?;
        let email = match &input.email {
            Some(email) => {
                if !valid_email(email) {
                    return Err(UnifiedError::bad_request("email is not a valid email address"));
                }
                email.trim().to_string()
            }
            None => state
                .audience
                .get(&contact_id)
                .map(|entry| entry.value().email.clone())
                .or_else(|| {
                    state
                        .scores
                        .get(&contact_id)
                        .map(|entry| entry.value().email.clone())
                })
                .ok_or_else(|| {
                    UnifiedError::bad_request(
                        "email is required when the contact is not in the known audience",
                    )
                })?,
        };
        let applied = apply_score_action(&state, &contact_id, &email, &action);
        let record = state
            .scores
            .get(&contact_id)
            .map(|entry| entry.value().clone());
        Ok(json!({
            "contact_id": contact_id,
            "action": action,
            "rules_applied": applied,
            "score": record,
        }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn list_score_rules(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<MarketingState>>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PageResult<ScoreRule>> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_MARKETING_VIEW)?;
        let params = query.into_inner();
        let mut items: Vec<ScoreRule> = state
            .score_rules
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|rule| {
                matches_search(&rule.action, &params)
                    && params.filters.get("action").is_none_or(|action| rule.action == *action)
                    && params.filters.get("active").is_none_or(|wanted| {
                        rule.active == (wanted == "true")
                    })
            })
            .collect();
        items.sort_by(|a, b| a.created_at.cmp(&b.created_at));
        if params.normalized().order == "desc" {
            items.reverse();
        }
        Ok(paginate(items, &params))
    })();
    match result {
        Ok(page) => ok(page),
        Err(error) => err(error, debug),
    }
}

#[derive(Debug, Deserialize)]
pub(crate) struct ScoreRuleInput {
    action: String,
    points: i64,
    decay_days: Option<u64>,
    active: Option<bool>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct ScoreRuleUpdateInput {
    action: Option<String>,
    points: Option<i64>,
    decay_days: Option<u64>,
    active: Option<bool>,
}

pub(crate) async fn create_score_rule(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<MarketingState>>,
    req: HttpRequest,
    body: web::Json<ScoreRuleInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<ScoreRule> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_MARKETING_MANAGE)?;
        let input = body.into_inner();
        let action = require_text(&input.action, "action", 200)?;
        if input.points == 0 || input.points < -1000 || input.points > 1000 {
            return Err(UnifiedError::bad_request(
                "points must be between -1000 and 1000, excluding 0",
            ));
        }
        if let Some(days) = input.decay_days {
            if days == 0 || days > 3650 {
                return Err(UnifiedError::bad_request(
                    "decay_days must be between 1 and 3650",
                ));
            }
        }
        if state
            .score_rules
            .iter()
            .any(|entry| entry.value().action == action && entry.value().active)
            && input.active.unwrap_or(true)
        {
            return Err(UnifiedError::conflict(format!(
                "An active scoring rule already exists for action \"{action}\""
            )));
        }
        let rule = ScoreRule {
            id: new_id(),
            action,
            points: input.points,
            decay_days: input.decay_days.or_else(|| {
                state.settings_snapshot().default_score_decay_days
            }),
            active: input.active.unwrap_or(true),
            created_at: now_string(),
        };
        state.score_rules.insert(rule.id.clone(), rule.clone());
        Ok(rule)
    })();
    match result {
        Ok(rule) => created(rule),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn update_score_rule(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<MarketingState>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<ScoreRuleUpdateInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<ScoreRule> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_MARKETING_MANAGE)?;
        let id = parse_uuid(path.as_str(), "score rule")?;
        let input = body.into_inner();
        if let Some(points) = input.points {
            if points == 0 || points < -1000 || points > 1000 {
                return Err(UnifiedError::bad_request(
                    "points must be between -1000 and 1000, excluding 0",
                ));
            }
        }
        if let Some(days) = input.decay_days {
            if days == 0 || days > 3650 {
                return Err(UnifiedError::bad_request(
                    "decay_days must be between 1 and 3650",
                ));
            }
        }
        let mut rule = state
            .score_rules
            .get_mut(&id.to_string())
            .ok_or_else(|| UnifiedError::not_found("Score rule not found"))?;
        if let Some(action) = input.action {
            rule.action = require_text(&action, "action", 200)?;
        }
        if let Some(points) = input.points {
            rule.points = points;
        }
        if let Some(days) = input.decay_days {
            rule.decay_days = Some(days);
        }
        if let Some(active) = input.active {
            rule.active = active;
        }
        Ok(rule.clone())
    })();
    match result {
        Ok(rule) => ok(rule),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn delete_score_rule(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<MarketingState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_MARKETING_MANAGE)?;
        let id = parse_uuid(path.as_str(), "score rule")?;
        match state.score_rules.remove(&id.to_string()) {
            Some(_) => Ok(json!({ "deleted": true, "id": id.to_string() })),
            None => Err(UnifiedError::not_found("Score rule not found")),
        }
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn apply_decay(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<MarketingState>>,
    req: HttpRequest,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_MARKETING_MANAGE)?;
        let contact_ids: Vec<String> = state
            .scores
            .iter()
            .map(|entry| entry.key().clone())
            .collect();
        for contact_id in &contact_ids {
            refresh_score(&state, contact_id);
        }
        Ok(json!({ "recomputed": contact_ids.len() }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}
