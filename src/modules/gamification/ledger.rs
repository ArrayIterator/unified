// Gamification - Ledger / balance / points handlers.

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

// Ledger / balance / points handlers

pub(crate) async fn list_ledger(
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
        let action_filter = params.filters.get("action").cloned();
        let company_filter = params.filters.get("company_id").cloned();
        let season_filter = params
            .filters
            .get("season_id")
            .and_then(|raw| Uuid::parse_str(raw).ok());
        let department_filter = params.filters.get("department_id").cloned();
        let mut items: Vec<PointsLedgerEntry> = state
            .ledger
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|row| in_company_scope(&row.company_id, &_scope))
            .filter(|row| {
                // A staff member sees only their own ledger unless
                // they hold the manage capability.
                if !is_manager && row.user_id != caller.actor {
                    return false;
                }
                if let Some(user) = &user_filter {
                    if &row.user_id != user {
                        return false;
                    }
                }
                if let Some(action) = &action_filter {
                    if &row.action != action {
                        return false;
                    }
                }
                if let Some(company) = &company_filter {
                    if row.company_id.as_deref() != Some(company.as_str()) {
                        return false;
                    }
                }
                if let Some(season) = season_filter {
                    if row.season_id != Some(season) {
                        return false;
                    }
                }
                if let Some(department) = &department_filter {
                    if row.department_id.as_deref() != Some(department.as_str()) {
                        return false;
                    }
                }
                true
            })
            .collect();
        items.sort_by_key(|row| std::cmp::Reverse(row.created_at));
        Ok(paginate(items, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn get_ledger_entry(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<GamificationState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PointsLedgerEntry> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_GAMIFICATION_VIEW)?;
        let id = parse_uuid(&path.into_inner(), "ledger entry")?;
        let entry = state
            .ledger
            .get(&id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found(format!("Ledger entry not found: {id}")))?;
        require_record_in_company_scope(&entry.company_id, &_scope)?;
        if entry.user_id != caller.actor && !can_manage(&ctx, &caller.role) {
            return Err(UnifiedError::not_found(format!("Ledger entry not found: {id}")));
        }
        Ok(entry)
    })();
    match result {
        Ok(entry) => ok(entry),
        Err(error) => err(error, debug),
    }
}

pub(crate) fn balance_view(
    state: &GamificationState,
    user_id: &str,
    company_id: Option<&str>,
    season_id: Option<Uuid>,
) -> Value {
    let total = user_balance(state, user_id, company_id, None);
    let season_points = season_id.map(|season| user_balance(state, user_id, company_id, Some(season)));
    let mut seasons: Vec<Value> = Vec::new();
    for season in state.seasons.iter().map(|entry| entry.value().clone()) {
        if company_id.is_some() && season.company_id.as_deref() != company_id {
            continue;
        }
        seasons.push(json!({
            "season_id": season.id.to_string(),
            "season_name": season.name,
            "status": season.status,
            "points": user_balance(state, user_id, company_id, Some(season.id)),
        }));
    }
    json!({
        "user_id": user_id,
        "company_id": company_id,
        "total_points": total,
        "season_id": season_id.map(|id| id.to_string()),
        "season_points": season_points,
        "seasons": seasons,
    })
}

pub(crate) async fn get_own_balance(
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
        let company_id = params.filters.get("company_id").cloned();
        let season_id = params
            .filters
            .get("season_id")
            .and_then(|raw| Uuid::parse_str(raw).ok());
        Ok(balance_view(
            &state,
            &caller.actor,
            company_id.as_deref(),
            season_id,
        ))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn get_user_balance(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<GamificationState>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
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
        let params = query.into_inner();
        let company_id = params.filters.get("company_id").cloned();
        let season_id = params
            .filters
            .get("season_id")
            .and_then(|raw| Uuid::parse_str(raw).ok());
        Ok(balance_view(
            &state,
            &user_id,
            company_id.as_deref(),
            season_id,
        ))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn award_points(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<GamificationState>,
    req: HttpRequest,
    body: web::Json<AwardPointsRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = require_company_for_write(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_GAMIFICATION_MANAGE)?;
        let input = body.into_inner();
        let user_id = require_text(&input.user_id, "user_id", MAX_NAME_LEN)?;
        let action = require_text(&input.action, "action", MAX_ACTION_LEN)?;
        if !valid_action(&action) {
            return Err(UnifiedError::bad_request(format!(
                "Unknown action \"{action}\"; known actions: {}",
                KNOWN_ACTIONS.join(", ")
            )));
        }
        let company_id = require_company_match(
            &_tenant,
            &req,
            &match &input.company_id {
                Some(raw) => Some(require_text(raw, "company_id", MAX_NAME_LEN)?),
                None => None,
            },
        )?;
        let department_id = match &input.department_id {
            Some(raw) => Some(require_text(raw, "department_id", MAX_NAME_LEN)?),
            None => None,
        };
        let reason = match &input.reason {
            Some(raw) => Some(require_text(raw, "reason", MAX_REASON_LEN)?),
            None => None,
        };
        let outcome = award_points_for_action(
            &state,
            &user_id,
            &action,
            company_id,
            department_id,
            reason,
            None,
        );
        Ok(json!({
            "user_id": user_id,
            "action": action,
            "awarded": outcome.awarded,
            "points": outcome.points,
            "entry_id": outcome.entry_id,
            "rule_id": outcome.rule_id,
            "reason": outcome.reason,
            "balance_after": user_balance(&state, &user_id, None, None),
        }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn adjust_points(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<GamificationState>,
    req: HttpRequest,
    body: web::Json<AdjustPointsRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PointsLedgerEntry> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = require_company_for_write(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_GAMIFICATION_MANAGE)?;
        let input = body.into_inner();
        let user_id = require_text(&input.user_id, "user_id", MAX_NAME_LEN)?;
        if input.points == 0 || input.points < -1_000_000 || input.points > 1_000_000 {
            return Err(UnifiedError::bad_request(
                "points must be non-zero and between -1000000 and 1000000",
            ));
        }
        let company_id = match &input.company_id {
            Some(raw) => Some(require_text(raw, "company_id", MAX_NAME_LEN)?),
            None => None,
        };
        let department_id = match &input.department_id {
            Some(raw) => Some(require_text(raw, "department_id", MAX_NAME_LEN)?),
            None => None,
        };
        let reason = match &input.reason {
            Some(raw) => Some(require_text(raw, "reason", MAX_REASON_LEN)?),
            None => Some(format!("Manual adjustment by {}", caller.actor)),
        };
        // A balance may never go below zero.
        let current = user_balance(&state, &user_id, company_id.as_deref(), None);
        if current + input.points < 0 {
            return Err(UnifiedError::conflict(format!(
                "Adjustment of {} points would take the balance below zero (current balance is {current})",
                input.points
            )));
        }
        let season_id = active_season_for(&state, company_id.as_deref()).map(|season| season.id);
        let entry = append_ledger_entry(
            &state,
            &user_id,
            company_id,
            department_id,
            None,
            "manual_adjustment",
            input.points,
            season_id,
            None,
            reason,
        );
        Ok(entry)
    })();
    match result {
        Ok(entry) => created(entry),
        Err(error) => err(error, debug),
    }
}
