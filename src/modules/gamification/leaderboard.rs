// Gamification - Leaderboard / progress handlers.

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

// Leaderboard / progress handlers

pub(crate) async fn get_leaderboard(
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
        let company_filter = params.filters.get("company_id").cloned();
        let department_filter = params.filters.get("department_id").cloned();
        let season_filter = params
            .filters
            .get("season_id")
            .and_then(|raw| Uuid::parse_str(raw).ok());
        // Aggregate the ledger per user; the balance is always a
        // sum of ledger entries, never a stored column.
        let mut totals: std::collections::HashMap<String, i64> = std::collections::HashMap::new();
        for entry in state.ledger.iter() {
            let row = entry.value();
            if let Some(company) = &company_filter {
                if row.company_id.as_deref() != Some(company.as_str()) {
                    continue;
                }
            }
            if let Some(department) = &department_filter {
                if row.department_id.as_deref() != Some(department.as_str()) {
                    continue;
                }
            }
            if let Some(season) = season_filter {
                if row.season_id != Some(season) {
                    continue;
                }
            }
            *totals.entry(row.user_id.clone()).or_insert(0) += row.points;
        }
        let mut rows: Vec<(String, i64)> = totals.into_iter().collect();
        rows.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
        let mut items: Vec<Value> = Vec::new();
        for (index, (user_id, points)) in rows.into_iter().enumerate() {
            let badge_count = state
                .awards
                .iter()
                .filter(|entry| entry.value().user_id == user_id)
                .count();
            items.push(json!({
                "rank": index + 1,
                "user_id": user_id,
                "points": points,
                "badge_count": badge_count,
            }));
        }
        Ok(paginate(items, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) fn progress_view(state: &GamificationState, user_id: &str) -> Value {
    let total_points = user_balance(state, user_id, None, None);
    let awards: Vec<Value> = {
        let mut rows: Vec<GamificationAward> = state
            .awards
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|award| award.user_id == user_id)
            .collect();
        rows.sort_by_key(|award| std::cmp::Reverse(award.awarded_at));
        rows.into_iter()
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
            .collect()
    };
    // Progress toward badges not yet earned: current action count
    // against the threshold.
    let earned_badge_ids: Vec<Uuid> = state
        .awards
        .iter()
        .filter(|entry| entry.value().user_id == user_id)
        .map(|entry| entry.value().badge_id)
        .collect();
    let mut next_badges: Vec<Value> = Vec::new();
    for badge in state.badges.iter().map(|entry| entry.value().clone()) {
        if earned_badge_ids.contains(&badge.id) {
            continue;
        }
        let count = action_count(state, user_id, &badge.criteria_action);
        next_badges.push(json!({
            "badge_id": badge.id.to_string(),
            "badge_name": badge.name,
            "criteria_action": badge.criteria_action,
            "criteria_threshold": badge.criteria_threshold,
            "current_count": count,
            "remaining": (badge.criteria_threshold - count).max(0),
        }));
    }
    next_badges.sort_by_key(|item| {
        item.get("remaining")
            .and_then(Value::as_i64)
            .unwrap_or(i64::MAX)
    });
    let mut season_points: Vec<Value> = Vec::new();
    for season in state.seasons.iter().map(|entry| entry.value().clone()) {
        season_points.push(json!({
            "season_id": season.id.to_string(),
            "season_name": season.name,
            "status": season.status,
            "points": user_balance(state, user_id, None, Some(season.id)),
        }));
    }
    json!({
        "user_id": user_id,
        "total_points": total_points,
        "badge_count": awards.len(),
        "awards": awards,
        "next_badges": next_badges,
        "seasons": season_points,
    })
}

pub(crate) async fn get_own_progress(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<GamificationState>,
    req: HttpRequest,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_GAMIFICATION_VIEW)?;
        Ok(progress_view(&state, &caller.actor))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn get_user_progress(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<GamificationState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_GAMIFICATION_VIEW)?;
        let user_id = path.into_inner();
        if user_id != caller.actor && !can_manage(&ctx, &caller.role) {
            return Err(UnifiedError::not_found(format!("User not found: {user_id}")));
        }
        Ok(progress_view(&state, &user_id))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}
