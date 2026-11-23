//! Planner - Module definition: routes, hooks, and registration.

use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::HookBus;
use unified_core::json;
use unified_core::modules::{Module, ModuleRegistry, RouteInfo};
use unified_core::pagination::{PageResult, PaginationParams};
use unified_core::context::AppContext;
use crate::modules::ai;
use actix_web::{web, HttpResponse};
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::sync::Arc;
use uuid::Uuid;
use super::{constants::*, helpers::*, types::*, state::*, handlers_pools::*, forecast::*, handlers_plans::*, events::*};
use super::ai_capabilities::AI_CAPABILITIES;

// ---------------------------------------------------------------------------
// Module definition.
// ---------------------------------------------------------------------------

pub struct PlannerModule {
    pub(crate) state: Arc<PlannerState>,
}

impl PlannerModule {
    pub fn new() -> Self {
        Self {
            state: Arc::new(PlannerState::new()),
        }
    }
}

impl Default for PlannerModule {
    fn default() -> Self {
        Self::new()
    }
}

impl Module for PlannerModule {
    fn init(&self, _ctx: &AppContext) -> UnifiedResult<()> {
        ai::register_ai_capabilities("planner", AI_CAPABILITIES);
        Ok(())
    }

    fn id(&self) -> &'static str {
        "planner"
    }

    fn name(&self) -> &'static str {
        "Resource Planner"
    }

    fn description(&self) -> &'static str {
        "Capacity planning across servers, clusters and providers: capacity pools, utilisation samples, linear-trend forecasts, allocation plans and overcommit warnings. All routes require X-User-Id / X-Role headers and a planner capability: planner.view for reads, planner.manage for writes."
    }

    fn register_hooks(&self, bus: &HookBus) {
        let state = self.state.clone();
        bus.register(
            PROVIDER_SYNC_COMPLETED,
            "planner",
            Arc::new(move |ctx: &mut unified_core::hooks::HookContext| {
                on_provider_sync_completed(state.clone(), &ctx.data.clone());
            }),
        );
    }

    fn configure_routes(&self, cfg: &mut web::ServiceConfig) {
        cfg.app_data(web::Data::new(self.state.clone()));
        cfg.service(
            web::scope("/api/v1/planner")
                .route("/overcommit", web::get().to(list_overcommit))
                .route("/pools", web::get().to(list_pools))
                .route("/pools", web::post().to(create_pool))
                .route("/pools/{id}", web::get().to(get_pool))
                .route("/pools/{id}", web::put().to(update_pool))
                .route("/pools/{id}", web::delete().to(delete_pool))
                .route("/pools/{id}/utilisation", web::get().to(list_utilisation))
                .route("/pools/{id}/usage", web::post().to(record_usage))
                .route("/pools/{id}/forecast", web::get().to(get_forecast))
                .route("/pools/{id}/overcommit", web::get().to(get_pool_overcommit))
                .route("/plans", web::get().to(list_plans))
                .route("/plans", web::post().to(create_plan))
                .route("/plans/{id}", web::get().to(get_plan))
                .route("/plans/{id}", web::put().to(update_plan))
                .route("/plans/{id}", web::delete().to(delete_plan))
                .route("/plans/{id}/approve", web::post().to(approve_plan))
                .route("/plans/{id}/activate", web::post().to(activate_plan))
                .route("/plans/{id}/expire", web::post().to(expire_plan)),
        );
    }

    fn route_info(&self) -> Vec<RouteInfo> {
        vec![
            RouteInfo { method: "GET", path: "/api/v1/planner/pools", owner: "planner", description: "List capacity pools with live allocation totals. (requires planner.view)", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/planner/pools", owner: "planner", description: "Create a capacity pool. (requires planner.manage)", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/planner/pools/{id}", owner: "planner", description: "Get one capacity pool with allocation and headroom. (requires planner.view)", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "PUT", path: "/api/v1/planner/pools/{id}", owner: "planner", description: "Update a capacity pool. (requires planner.manage)", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "DELETE", path: "/api/v1/planner/pools/{id}", owner: "planner", description: "Delete a capacity pool (refused while live plans use it). (requires planner.manage)", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/planner/pools/{id}/utilisation", owner: "planner", description: "List recorded utilisation samples for a pool. (requires planner.view)", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/planner/pools/{id}/usage", owner: "planner", description: "Record one utilisation sample for a pool. (requires planner.manage)", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/planner/pools/{id}/forecast", owner: "planner", description: "Linear-trend capacity forecast for a pool (horizon_days 7/30/60/90/180). (requires planner.view)", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/planner/pools/{id}/overcommit", owner: "planner", description: "Overcommit report for one pool. (requires planner.view)", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/planner/overcommit", owner: "planner", description: "Overcommit report for every pool. (requires planner.view)", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/planner/plans", owner: "planner", description: "List allocation plans. (requires planner.view)", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/planner/plans", owner: "planner", description: "Create an allocation plan (draft). (requires planner.manage)", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/planner/plans/{id}", owner: "planner", description: "Get one allocation plan. (requires planner.view)", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "PUT", path: "/api/v1/planner/plans/{id}", owner: "planner", description: "Update a draft allocation plan. (requires planner.manage)", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "DELETE", path: "/api/v1/planner/plans/{id}", owner: "planner", description: "Delete a draft or expired allocation plan. (requires planner.manage)", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/planner/plans/{id}/approve", owner: "planner", description: "Approve a draft plan (overcommit warned, not blocked). (requires planner.manage)", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/planner/plans/{id}/activate", owner: "planner", description: "Activate an approved plan (overcommit warned, not blocked). (requires planner.manage)", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/planner/plans/{id}/expire", owner: "planner", description: "Expire a plan and release its capacity. (requires planner.manage)", auth: unified_core::auth::RouteAuth::Authenticated},
        ]
    }
}

pub fn register(registry: &ModuleRegistry) {
    registry.register(Arc::new(PlannerModule::new()));
}
