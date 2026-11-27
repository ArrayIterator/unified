//! Activity Timeline module. - Module.
//!
//! Split from `src/modules/timeline.rs`; behaviour unchanged.

use super::handlers::*;
use super::helpers::*;
use super::state::*;
use super::types::*;
use super::ai_capabilities::AI_CAPABILITIES;

use unified_core::hooks::HookBus;
use unified_core::hooks::HookContext;
use unified_core::hooks::HookHandler;
use unified_core::modules::{Module, ModuleRegistry, RouteInfo};
use unified_core::context::AppContext;
use unified_core::error::UnifiedResult;
use crate::modules::ai;
use actix_web::web;
use std::sync::Arc;

pub struct TimelineModule {
    pub(super) state: Arc<TimelineState>,
}

impl TimelineModule {
    pub fn new() -> Self {
        Self {
            state: Arc::new(TimelineState::new()),
        }
    }
}

impl Default for TimelineModule {
    fn default() -> Self {
        Self::new()
    }
}

impl Module for TimelineModule {
    fn init(&self, _ctx: &AppContext) -> UnifiedResult<()> {
        ai::register_ai_capabilities("timeline", AI_CAPABILITIES);
        Ok(())
    }

    fn id(&self) -> &'static str {
        "timeline"
    }

    fn name(&self) -> &'static str {
        "Activity Timeline"
    }

    fn description(&self) -> &'static str {
        "Unified newest-first activity feed combining records, audit and task events, with per-user and per-entity filters and grouping by day."
    }

    fn register_hooks(&self, bus: &HookBus) {
        for event in TIMELINE_EVENTS {
            let event_name = (*event).to_string();
            let state = self.state.clone();
            let handler: HookHandler = Arc::new(move |ctx: &mut HookContext| {
                state.record(&event_name, &ctx.data.clone());
            });
            bus.register(event, "timeline", handler);
        }
    }

    fn configure_routes(&self, cfg: &mut web::ServiceConfig) {
        cfg.app_data(web::Data::new(self.state.clone()));
        cfg.service(
            web::scope("/api/v1/timeline")
                .route("", web::get().to(list_feed))
                .route("/counts", web::get().to(counts))
                .route("/by-day", web::get().to(by_day))
                .route("/entity/{entity_type}/{entity_id}", web::get().to(entity_timeline)),
        );
    }

    fn route_info(&self) -> Vec<RouteInfo> {
        vec![
            RouteInfo { method: "GET", path: "/api/v1/timeline", owner: "timeline", description: "Unified activity feed, newest first, paginated with actor/entity/date filters.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/timeline/counts", owner: "timeline", description: "Timeline counts by action, entity type and day.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/timeline/by-day", owner: "timeline", description: "Timeline entries grouped by day, newest day first.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/timeline/entity/{entity_type}/{entity_id}", owner: "timeline", description: "One entity's timeline, newest first.", auth: unified_core::auth::RouteAuth::Authenticated},
        ]
    }
}

pub fn register(registry: &ModuleRegistry) {
    registry.register(Arc::new(TimelineModule::new()));
}
