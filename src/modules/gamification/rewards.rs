// Gamification — Reward handlers.

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
// Reward handlers
// ---------------------------------------------------------------------------

pub(crate) async fn list_rewards(
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
        let active_filter = params
            .filters
            .get("active")
            .and_then(|raw| raw.parse::<bool>().ok());
        let search = params.search.clone().unwrap_or_default().to_lowercase();
        let mut items: Vec<GamificationReward> = state
            .rewards
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|reward| {
                if let Some(active) = active_filter {
                    if reward.active != active {
                        return false;
                    }
                }
                if !search.is_empty()
                    && !format!("{} {}", reward.name, reward.description)
                        .to_lowercase()
                        .contains(&search)
                {
                    return false;
                }
                true
            })
            .collect();
        items.sort_by_key(|reward| reward.points_cost);
        Ok(paginate(items, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn create_reward(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<GamificationState>,
    req: HttpRequest,
    body: web::Json<CreateRewardRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<GamificationReward> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = require_company_for_write(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_GAMIFICATION_MANAGE)?;
        let input = body.into_inner();
        let name = require_text(&input.name, "name", MAX_NAME_LEN)?;
        let description = require_text(&input.description, "description", MAX_DESCRIPTION_LEN)?;
        if input.points_cost < 1 || input.points_cost > 1_000_000 {
            return Err(UnifiedError::bad_request(
                "points_cost must be between 1 and 1000000",
            ));
        }
        let now = Utc::now();
        let reward = GamificationReward {
            id: Uuid::now_v7(),
            name,
            description,
            points_cost: input.points_cost,
            active: input.active.unwrap_or(true),
            created_at: now,
            updated_at: now,
        };
        state.rewards.insert(reward.id, reward.clone());
        state.emit(
            "gamification.reward.created",
            json!({
                "reward_id": reward.id.to_string(),
                "name": reward.name,
                "points_cost": reward.points_cost,
            }),
        );
        Ok(reward)
    })();
    match result {
        Ok(reward) => created(reward),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn get_reward(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<GamificationState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<GamificationReward> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_GAMIFICATION_VIEW)?;
        load_reward(&state, &path.into_inner())
    })();
    match result {
        Ok(reward) => ok(reward),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn update_reward(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<GamificationState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdateRewardRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<GamificationReward> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = require_company_for_write(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_GAMIFICATION_MANAGE)?;
        let mut reward = load_reward(&state, &path.into_inner())?;
        let input = body.into_inner();
        if let Some(name) = &input.name {
            reward.name = require_text(name, "name", MAX_NAME_LEN)?;
        }
        if let Some(description) = &input.description {
            reward.description = require_text(description, "description", MAX_DESCRIPTION_LEN)?;
        }
        if let Some(cost) = input.points_cost {
            if cost < 1 || cost > 1_000_000 {
                return Err(UnifiedError::bad_request(
                    "points_cost must be between 1 and 1000000",
                ));
            }
            reward.points_cost = cost;
        }
        if let Some(active) = input.active {
            reward.active = active;
        }
        reward.updated_at = Utc::now();
        state.rewards.insert(reward.id, reward.clone());
        Ok(reward)
    })();
    match result {
        Ok(reward) => ok(reward),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn delete_reward(
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
        let reward = load_reward(&state, &path.into_inner())?;
        if state.reward_requests.iter().any(|entry| {
            entry.value().reward_id == reward.id && entry.value().status == "requested"
        }) {
            return Err(UnifiedError::conflict(
                "A reward with open requests cannot be deleted; decide the requests first",
            ));
        }
        let id = reward.id;
        state.rewards.remove(&id);
        Ok(json!({ "deleted": true, "reward_id": id.to_string() }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

/// Resolve the scope a reward request spends from: an explicit
/// season wins; otherwise the active season of the company (or
/// the global active season when no company is given); when no
/// season is active the scope is unseasoned (`None`).
pub(crate) fn resolve_reward_scope(
    state: &GamificationState,
    company_id: Option<String>,
    season_id: Option<Uuid>,
) -> (Option<String>, Option<Uuid>) {
    let season_id = season_id
        .or_else(|| active_season_for(state, company_id.as_deref()).map(|season| season.id));
    (company_id, season_id)
}

pub(crate) async fn request_reward(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<GamificationState>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<RequestRewardScope>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<GamificationRewardRequest> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = require_company_for_write(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_GAMIFICATION_VIEW)?;
        let reward = load_reward(&state, &path.into_inner())?;
        if !reward.active {
            return Err(UnifiedError::conflict("This reward is not active"));
        }
        // The scope the points are spent from is resolved here and
        // snapshotted onto the request: the balance is checked
        // within that (company, season) scope, and approval deducts
        // within the same scope — never against the unscoped total,
        // which could spend another company's or season's points.
        let scope = query.into_inner();
        let company_id = scope
            .company_id
            .map(|raw| raw.trim().to_string())
            .filter(|raw| !raw.is_empty());
        let (company_id, season_id) =
            resolve_reward_scope(&state, company_id, scope.season_id);
        // No points are deducted on request — only on approval.
        // The request is refused up front when the balance in scope
        // cannot cover the cost, so a request can never be approved
        // into a negative balance in that scope.
        let balance = user_balance(&state, &caller.actor, company_id.as_deref(), season_id);
        if balance < reward.points_cost {
            return Err(UnifiedError::conflict(format!(
                "Not enough points: this reward costs {} points and your balance is {balance}",
                reward.points_cost
            )));
        }
        if state.reward_requests.iter().any(|entry| {
            entry.value().reward_id == reward.id
                && entry.value().user_id == caller.actor
                && entry.value().status == "requested"
        }) {
            return Err(UnifiedError::conflict(
                "You already have an open request for this reward",
            ));
        }
        let now = Utc::now();
        let request = GamificationRewardRequest {
            id: Uuid::now_v7(),
            reward_id: reward.id,
            user_id: caller.actor.clone(),
            status: "requested".to_string(),
            points_spent: reward.points_cost,
            company_id: company_id.clone(),
            season_id,
            decided_by: None,
            decided_at: None,
            created_at: now,
            updated_at: now,
        };
        state.reward_requests.insert(request.id, request.clone());
        state.emit(
            "gamification.reward.requested",
            json!({
                "request_id": request.id.to_string(),
                "reward_id": reward.id.to_string(),
                "reward_name": reward.name,
                "user_id": request.user_id,
                "points_cost": reward.points_cost,
                "company_id": request.company_id,
                "season_id": request.season_id.map(|id| id.to_string()),
            }),
        );
        Ok(request)
    })();
    match result {
        Ok(request) => created(request),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn list_reward_requests(
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
        let status_filter = params.filters.get("status").cloned();
        let user_filter = params.filters.get("user_id").cloned();
        let reward_filter = params
            .filters
            .get("reward_id")
            .and_then(|raw| Uuid::parse_str(raw).ok());
        let mut items: Vec<Value> = state
            .reward_requests
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|request| in_company_scope(&request.company_id, &_scope))
            .filter(|request| {
                if !is_manager && request.user_id != caller.actor {
                    return false;
                }
                if let Some(status) = &status_filter {
                    if &request.status != status {
                        return false;
                    }
                }
                if let Some(user) = &user_filter {
                    if &request.user_id != user {
                        return false;
                    }
                }
                if let Some(reward) = reward_filter {
                    if request.reward_id != reward {
                        return false;
                    }
                }
                true
            })
            .map(|request| {
                let reward_name = state
                    .rewards
                    .get(&request.reward_id)
                    .map(|entry| entry.value().name.clone());
                let mut value = serde_json::to_value(&request).unwrap_or(Value::Null);
                if let Value::Object(map) = &mut value {
                    map.insert(
                        "reward_name".to_string(),
                        reward_name.map(Value::String).unwrap_or(Value::Null),
                    );
                }
                value
            })
            .collect();
        items.sort_by(|a, b| {
            b.get("created_at")
                .and_then(Value::as_str)
                .cmp(&a.get("created_at").and_then(Value::as_str))
        });
        Ok(paginate(items, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn get_reward_request(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<GamificationState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<GamificationRewardRequest> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_GAMIFICATION_VIEW)?;
        let request = load_reward_request(&state, &path.into_inner())?;
        require_record_in_company_scope(&request.company_id, &_scope)?;
        if request.user_id != caller.actor && !can_manage(&ctx, &caller.role) {
            return Err(UnifiedError::not_found(format!(
                "Reward request not found: {}",
                request.id
            )));
        }
        Ok(request)
    })();
    match result {
        Ok(request) => ok(request),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn approve_reward_request(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<GamificationState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<GamificationRewardRequest> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = require_company_for_write(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_GAMIFICATION_MANAGE)?;
        let mut request = load_reward_request(&state, &path.into_inner())?;
        require_record_writable_in_company_scope(&request.company_id, &_scope)?;
        if request.status != "requested" {
            return Err(UnifiedError::conflict(format!(
                "Only a requested reward can be approved (status is \"{}\")",
                request.status
            )));
        }
        // Deduct the points exactly once, on approval. The balance
        // is re-checked here because it may have changed since the
        // request was made. Both the check and the deduction use
        // the scope snapshotted onto the request, so approval can
        // never spend another company's or season's points.
        let balance = user_balance(
            &state,
            &request.user_id,
            request.company_id.as_deref(),
            request.season_id,
        );
        if balance < request.points_spent {
            return Err(UnifiedError::conflict(format!(
                "Not enough points: the reward costs {} points and the user's balance is now {balance}",
                request.points_spent
            )));
        }
        let now = Utc::now();
        append_ledger_entry(
            &state,
            &request.user_id,
            request.company_id.clone(),
            None,
            None,
            "reward_redemption",
            -request.points_spent,
            request.season_id,
            None,
            Some(format!("Reward redemption: {}", request.reward_id)),
        );
        request.status = "approved".to_string();
        request.decided_by = Some(caller.actor.clone());
        request.decided_at = Some(now);
        request.updated_at = now;
        state
            .reward_requests
            .insert(request.id, request.clone());
        state.emit(
            "gamification.reward.approved",
            json!({
                "request_id": request.id.to_string(),
                "reward_id": request.reward_id.to_string(),
                "user_id": request.user_id,
                "points_spent": request.points_spent,
                "company_id": request.company_id,
                "season_id": request.season_id.map(|id| id.to_string()),
                "approved_by": caller.actor,
            }),
        );
        Ok(request)
    })();
    match result {
        Ok(request) => ok(request),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn reject_reward_request(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<GamificationState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<GamificationRewardRequest> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = require_company_for_write(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_GAMIFICATION_MANAGE)?;
        let mut request = load_reward_request(&state, &path.into_inner())?;
        require_record_writable_in_company_scope(&request.company_id, &_scope)?;
        if request.status != "requested" {
            return Err(UnifiedError::conflict(format!(
                "Only a requested reward can be rejected (status is \"{}\")",
                request.status
            )));
        }
        // Rejection deducts nothing: points were never spent.
        let now = Utc::now();
        request.status = "rejected".to_string();
        request.decided_by = Some(caller.actor.clone());
        request.decided_at = Some(now);
        request.updated_at = now;
        state
            .reward_requests
            .insert(request.id, request.clone());
        state.emit(
            "gamification.reward.rejected",
            json!({
                "request_id": request.id.to_string(),
                "reward_id": request.reward_id.to_string(),
                "user_id": request.user_id,
                "rejected_by": caller.actor,
            }),
        );
        Ok(request)
    })();
    match result {
        Ok(request) => ok(request),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn fulfill_reward_request(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<GamificationState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<GamificationRewardRequest> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = require_company_for_write(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_GAMIFICATION_MANAGE)?;
        let mut request = load_reward_request(&state, &path.into_inner())?;
        require_record_writable_in_company_scope(&request.company_id, &_scope)?;
        if request.status != "approved" {
            return Err(UnifiedError::conflict(format!(
                "Only an approved reward can be fulfilled (status is \"{}\")",
                request.status
            )));
        }
        request.status = "fulfilled".to_string();
        request.updated_at = Utc::now();
        state
            .reward_requests
            .insert(request.id, request.clone());
        state.emit(
            "gamification.reward.fulfilled",
            json!({
                "request_id": request.id.to_string(),
                "reward_id": request.reward_id.to_string(),
                "user_id": request.user_id,
            }),
        );
        Ok(request)
    })();
    match result {
        Ok(request) => ok(request),
        Err(error) => err(error, debug),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn active_season(state: &GamificationState, company_id: Option<&str>) -> GamificationSeason {
        let now = Utc::now();
        let season = GamificationSeason {
            id: Uuid::now_v7(),
            name: "Season".to_string(),
            company_id: company_id.map(str::to_string),
            starts_at: now - chrono::Duration::days(1),
            ends_at: now + chrono::Duration::days(1),
            status: "active".to_string(),
            created_at: now,
            updated_at: now,
        };
        state.seasons.insert(season.id, season.clone());
        season
    }

    #[test]
    fn resolve_scope_defaults_to_the_active_season_of_the_company() {
        let state = GamificationState::default();
        let season = active_season(&state, Some("c1"));
        let (company, season_id) = resolve_reward_scope(&state, Some("c1".to_string()), None);
        assert_eq!(company.as_deref(), Some("c1"));
        assert_eq!(season_id, Some(season.id));
    }

    #[test]
    fn resolve_scope_does_not_borrow_another_companys_season() {
        let state = GamificationState::default();
        active_season(&state, Some("c1"));
        let (company, season_id) = resolve_reward_scope(&state, Some("c2".to_string()), None);
        assert_eq!(company.as_deref(), Some("c2"));
        assert_eq!(season_id, None);
    }

    #[test]
    fn resolve_scope_prefers_an_explicit_season() {
        let state = GamificationState::default();
        let active = active_season(&state, Some("c1"));
        let explicit = Uuid::now_v7();
        let (_, season_id) =
            resolve_reward_scope(&state, Some("c1".to_string()), Some(explicit));
        assert_eq!(season_id, Some(explicit));
        assert_ne!(season_id, Some(active.id));
    }

    #[test]
    fn resolve_scope_without_an_active_season_is_unseasoned() {
        let state = GamificationState::default();
        let (company, season_id) = resolve_reward_scope(&state, None, None);
        assert_eq!(company, None);
        assert_eq!(season_id, None);
    }

    #[test]
    fn scoped_balance_check_uses_the_request_scope_not_the_total() {
        // The bug: a request was approved against the unscoped
        // total. Here the user holds 100 points in company c2 but
        // only 5 in c1, so a 10-point reward scoped to c1 must fail
        // the balance check while the unscoped total would pass.
        let state = GamificationState::default();
        let mut rich = PointsLedgerEntry {
            id: Uuid::now_v7(),
            user_id: "u1".to_string(),
            company_id: Some("c2".to_string()),
            department_id: None,
            rule_id: None,
            action: "sale_completed".to_string(),
            points: 100,
            balance_after: 100,
            season_id: None,
            source_ref: None,
            reason: None,
            created_at: Utc::now(),
        };
        state.ledger.insert(rich.id, rich.clone());
        rich.id = Uuid::now_v7();
        rich.company_id = Some("c1".to_string());
        rich.points = 5;
        rich.balance_after = 5;
        state.ledger.insert(rich.id, rich);
        assert_eq!(user_balance(&state, "u1", None, None), 105);
        assert_eq!(user_balance(&state, "u1", Some("c1"), None), 5);
        assert!(user_balance(&state, "u1", Some("c1"), None) < 10);
    }
}
