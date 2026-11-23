// Gamification — Module.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::{HookBus, HookContext};
use unified_core::json;
use unified_core::modules::{Module, ModuleRegistry, RouteInfo, RouteVersionSupport};
use unified_core::pagination::{PageResult, PaginationParams};
use crate::modules::ai;
use actix_web::{web, HttpRequest, HttpResponse};
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::sync::{Arc, RwLock};
use uuid::Uuid;

use super::*;

// ---------------------------------------------------------------------------
// Module
// ---------------------------------------------------------------------------

pub struct GamificationModule {
    state: GamificationState,
}

impl GamificationModule {
    pub fn new() -> Self {
        Self {
            state: GamificationState::default(),
        }
    }
}

impl Default for GamificationModule {
    fn default() -> Self {
        Self::new()
    }
}

impl Module for GamificationModule {
    fn id(&self) -> &'static str {
        "gamification"
    }

    fn name(&self) -> &'static str {
        "Gamification"
    }

    fn description(&self) -> &'static str {
        "Staff recognition gamification: a configurable rules engine (action -> points, per company, daily caps), a points ledger that is the only source of truth for balances, badges awarded automatically on action thresholds with audited award records, time-boxed seasons with one active at a time per company, team leaderboards computed from the ledger, personal progress, and staff reward redemption with an explicit approve-then-deduct flow. Staff points are a separate ledger from customer loyalty points and are never mixed."
    }

    fn is_core(&self) -> bool {
        false
    }

    fn requires(&self) -> &[&'static str] {
        &["users", "roles", "notifications"]
    }

    fn dependencies(&self) -> Vec<&'static str> {
        vec!["users", "roles", "notifications"]
    }

    fn available_as_product(&self) -> bool {
        true
    }

    fn product_category(&self) -> Option<&'static str> {
        Some("communication")
    }

    fn init(&self, ctx: &AppContext) -> UnifiedResult<()> {

        ai::register_ai_capabilities("gamification", AI_CAPABILITIES);
        *self
            .state
            .hooks
            .write()
            .expect("hooks lock poisoned") = Some(Arc::clone(&ctx.hooks));
        Ok(())
    }

    fn register_hooks(&self, bus: &HookBus) {
        let state = self.state.clone();
        bus.register(
            TICKET_UPDATED,
            "gamification",
            Arc::new(move |ctx: &mut HookContext| {
                on_ticket_updated(state.clone(), ctx)
            }),
        );
        let state = self.state.clone();
        bus.register(
            STORE_ORDER_COMPLETED,
            "gamification",
            Arc::new(move |ctx: &mut HookContext| {
                on_order_completed(state.clone(), ctx)
            }),
        );
        let state = self.state.clone();
        bus.register(
            ORDER_PAID,
            "gamification",
            Arc::new(move |ctx: &mut HookContext| {
                on_order_completed(state.clone(), ctx)
            }),
        );
        let state = self.state.clone();
        bus.register(
            SURVEY_RESPONSE_RECEIVED,
            "gamification",
            Arc::new(move |ctx: &mut HookContext| {
                on_survey_response_received(state.clone(), ctx)
            }),
        );
        let state = self.state.clone();
        bus.register(
            COMMUNITY_POST_REPLIED,
            "gamification",
            Arc::new(move |ctx: &mut HookContext| {
                on_community_post_replied(state.clone(), ctx)
            }),
        );
        let state = self.state.clone();
        bus.register(
            COMMUNITY_TOPIC_CREATED,
            "gamification",
            Arc::new(move |ctx: &mut HookContext| {
                on_community_topic_created(state.clone(), ctx)
            }),
        );
    }

    fn configure_routes(&self, cfg: &mut web::ServiceConfig) {
        let state = web::Data::new(self.state.clone());
        cfg.service(
            web::scope("/api/v1/gamification")
                .app_data(state)
                .route("/rules", web::get().to(list_rules))
                .route("/rules", web::post().to(create_rule))
                .route("/rules/{id}", web::get().to(get_rule))
                .route("/rules/{id}", web::patch().to(update_rule))
                .route("/rules/{id}", web::delete().to(delete_rule))
                .route("/ledger", web::get().to(list_ledger))
                .route("/ledger/{id}", web::get().to(get_ledger_entry))
                .route("/balance", web::get().to(get_own_balance))
                .route("/balance/{user_id}", web::get().to(get_user_balance))
                .route("/points/award", web::post().to(award_points))
                .route("/points/adjust", web::post().to(adjust_points))
                .route("/badges", web::get().to(list_badges))
                .route("/badges", web::post().to(create_badge))
                .route("/badges/{id}", web::get().to(get_badge))
                .route("/badges/{id}", web::patch().to(update_badge))
                .route("/badges/{id}", web::delete().to(delete_badge))
                .route("/awards", web::get().to(list_awards))
                .route("/awards/{id}", web::get().to(get_award))
                .route("/seasons", web::get().to(list_seasons))
                .route("/seasons", web::post().to(create_season))
                .route("/seasons/{id}", web::get().to(get_season))
                .route("/seasons/{id}", web::patch().to(update_season))
                .route("/seasons/{id}/start", web::post().to(start_season))
                .route("/seasons/{id}/end", web::post().to(end_season))
                .route("/leaderboard", web::get().to(get_leaderboard))
                .route("/progress", web::get().to(get_own_progress))
                .route("/progress/{user_id}", web::get().to(get_user_progress))
                .route("/rewards", web::get().to(list_rewards))
                .route("/rewards", web::post().to(create_reward))
                .route("/rewards/{id}", web::get().to(get_reward))
                .route("/rewards/{id}", web::patch().to(update_reward))
                .route("/rewards/{id}", web::delete().to(delete_reward))
                .route("/rewards/{id}/request", web::post().to(request_reward))
                .route("/reward-requests", web::get().to(list_reward_requests))
                .route("/reward-requests/{id}", web::get().to(get_reward_request))
                .route(
                    "/reward-requests/{id}/approve",
                    web::post().to(approve_reward_request),
                )
                .route(
                    "/reward-requests/{id}/reject",
                    web::post().to(reject_reward_request),
                )
                .route(
                    "/reward-requests/{id}/fulfill",
                    web::post().to(fulfill_reward_request),
                ),
        );
    }

    fn route_info(&self) -> Vec<RouteInfo> {
        vec![
            RouteInfo { method: "GET", path: "/api/v1/gamification/rules", owner: "gamification", description: "List points rules (paginated, filter by action, company_id, active).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/gamification/rules", owner: "gamification", description: "Create a points rule: action, points, optional company, optional daily cap (manager).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/gamification/rules/{id}", owner: "gamification", description: "Get one points rule.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "PATCH", path: "/api/v1/gamification/rules/{id}", owner: "gamification", description: "Update a points rule (manager).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "DELETE", path: "/api/v1/gamification/rules/{id}", owner: "gamification", description: "Delete a points rule (manager).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/gamification/ledger", owner: "gamification", description: "List ledger entries (own only unless manager; filter by user_id, action, company_id, season_id, department_id).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/gamification/ledger/{id}", owner: "gamification", description: "Get one ledger entry (own only unless manager).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/gamification/balance", owner: "gamification", description: "The caller's points balances (total and per season).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/gamification/balance/{user_id}", owner: "gamification", description: "One user's points balances (own only unless manager).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/gamification/points/award", owner: "gamification", description: "Run the rules engine for one action for one user (manager).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/gamification/points/adjust", owner: "gamification", description: "Direct ledger adjustment, positive or negative (manager; balance may never go below zero).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/gamification/badges", owner: "gamification", description: "List badge definitions (paginated, filter by criteria_action).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/gamification/badges", owner: "gamification", description: "Define a badge with an action-count threshold (manager).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/gamification/badges/{id}", owner: "gamification", description: "Get one badge definition.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "PATCH", path: "/api/v1/gamification/badges/{id}", owner: "gamification", description: "Update a badge definition (manager).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "DELETE", path: "/api/v1/gamification/badges/{id}", owner: "gamification", description: "Delete a badge definition with no awards (manager).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/gamification/awards", owner: "gamification", description: "List badge awards (own only unless manager; filter by user_id, badge_id).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/gamification/awards/{id}", owner: "gamification", description: "Get one badge award (own only unless manager).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/gamification/seasons", owner: "gamification", description: "List seasons (paginated, filter by status, company_id).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/gamification/seasons", owner: "gamification", description: "Create a season (manager).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/gamification/seasons/{id}", owner: "gamification", description: "Get one season.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "PATCH", path: "/api/v1/gamification/seasons/{id}", owner: "gamification", description: "Update an upcoming or active season (manager).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/gamification/seasons/{id}/start", owner: "gamification", description: "Start a season; ends any other active season of the same company (manager).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/gamification/seasons/{id}/end", owner: "gamification", description: "End an active season (manager).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/gamification/leaderboard", owner: "gamification", description: "Ranked points per user, optionally per season, company, or department (computed from the ledger).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/gamification/progress", owner: "gamification", description: "The caller's points, badges, and progress toward the next badges.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/gamification/progress/{user_id}", owner: "gamification", description: "One user's progress (own only unless manager).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/gamification/rewards", owner: "gamification", description: "List staff rewards (paginated, filter by active).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/gamification/rewards", owner: "gamification", description: "Define a staff reward with a points cost (manager).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/gamification/rewards/{id}", owner: "gamification", description: "Get one staff reward.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "PATCH", path: "/api/v1/gamification/rewards/{id}", owner: "gamification", description: "Update a staff reward (manager).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "DELETE", path: "/api/v1/gamification/rewards/{id}", owner: "gamification", description: "Delete a staff reward with no open requests (manager).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/gamification/rewards/{id}/request", owner: "gamification", description: "Request a reward (balance checked; no points deducted yet).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/gamification/reward-requests", owner: "gamification", description: "List reward requests (own only unless manager; filter by status, user_id, reward_id).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/gamification/reward-requests/{id}", owner: "gamification", description: "Get one reward request (own only unless manager).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/gamification/reward-requests/{id}/approve", owner: "gamification", description: "Approve a reward request and deduct its points once (manager).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/gamification/reward-requests/{id}/reject", owner: "gamification", description: "Reject a reward request; no points are deducted (manager).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/gamification/reward-requests/{id}/fulfill", owner: "gamification", description: "Mark an approved reward request fulfilled (manager).", auth: unified_core::auth::RouteAuth::Authenticated},
        ]
    }

    fn route_version_support(&self) -> RouteVersionSupport {
        RouteVersionSupport::All
    }
}

pub fn register(registry: &ModuleRegistry) {
    registry.register(Arc::new(GamificationModule::new()));
}
