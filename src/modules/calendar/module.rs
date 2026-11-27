//! calendar module - Module registration: struct, Default, Module impl, register.

use unified_core::context::AppContext;
use unified_core::error::UnifiedResult;
use unified_core::hooks::{HookBus, HookContext};
use unified_core::modules::{Module, ModuleRegistry, RouteInfo, RouteVersionSupport};
use crate::modules::ai::register_ai_capabilities;
use actix_web::web;
use std::sync::Arc;

use super::constants::CALENDAR_AI_CAPABILITIES;
use super::constants::EVENT_AI_SUMMARIZE_DAY_REQUESTED;
use super::constants::EVENT_AI_WHATS_NEXT_REQUESTED;
use super::constants::EVENT_FEEDS_PRUNE_REQUESTED;
use super::hooks::on_ai_grounding_requested;
use super::hooks::on_feeds_prune_requested;
use super::state::CalendarState;
use super::ai_handlers::{ai_summarize_day, ai_whats_next};
use super::handlers::{create_feed, create_intent, get_calendar_view, get_feed_ics, get_settings, get_sources, list_calendar_items, list_feeds, revoke_feed, update_settings, update_sources};

// Module declaration.

pub struct CalendarModule {
    pub(crate) state: CalendarState,
}

impl CalendarModule {
    pub fn new() -> Self {
        Self {
            state: CalendarState::default(),
        }
    }
}

impl Default for CalendarModule {
    fn default() -> Self {
        Self::new()
    }
}

impl Module for CalendarModule {
    fn id(&self) -> &'static str {
        "calendar"
    }

    fn name(&self) -> &'static str {
        "Calendar"
    }

    fn description(&self) -> &'static str {
        "Integrated calendar aggregation view: month/week/day/agenda views over dated items contributed by every module through the calendar.items_requested hook, per-user module visibility toggles, HMAC-signed revocable iCalendar subscription feeds, working hours and non-working days (configured weekends plus holidays from the holidays module), create-from-calendar hand-off to the owning module, conflict hints, and AI draft day summaries and briefings. Owns no dated records and never creates, moves, or deletes an item."
    }

    fn requires(&self) -> &[&'static str] {
        &["users", "roles"]
    }

    fn dependencies(&self) -> Vec<&'static str> {
        vec!["users", "roles"]
    }

    fn available_as_product(&self) -> bool {
        true
    }

    fn product_category(&self) -> Option<&'static str> {
        Some("productivity")
    }

    fn route_version_support(&self) -> RouteVersionSupport {
        RouteVersionSupport::All
    }

    fn init(&self, ctx: &AppContext) -> UnifiedResult<()> {
        *self.state.hooks.write().expect("hooks lock poisoned") = Some(Arc::clone(&ctx.hooks));
        // Derive the feed-signing key from the configured app
        // secret, so outstanding feed tokens keep verifying after
        // a restart. With no secret configured the state's random
        // per-process key stays (tokens then do not survive a
        // restart - same fallback as the auth manager).
        {
            let config = ctx.config.read().expect("config lock poisoned");
            self.state
                .use_derived_signing_key(&config.app.secret_key, &config.app.salt_key);
        }
        register_ai_capabilities("calendar", CALENDAR_AI_CAPABILITIES);
        Ok(())
    }

    fn register_hooks(&self, bus: &HookBus) {
        let state = self.state.clone();
        bus.register(
            EVENT_FEEDS_PRUNE_REQUESTED,
            "calendar",
            Arc::new(move |ctx: &mut HookContext| on_feeds_prune_requested(state.clone(), ctx)),
        );
        let state = self.state.clone();
        bus.register(
            EVENT_AI_SUMMARIZE_DAY_REQUESTED,
            "calendar",
            Arc::new(move |ctx: &mut HookContext| {
                on_ai_grounding_requested(state.clone(), ctx, "summarize_day")
            }),
        );
        let state = self.state.clone();
        bus.register(
            EVENT_AI_WHATS_NEXT_REQUESTED,
            "calendar",
            Arc::new(move |ctx: &mut HookContext| {
                on_ai_grounding_requested(state.clone(), ctx, "whats_next")
            }),
        );
    }

    fn configure_routes(&self, cfg: &mut web::ServiceConfig) {
        let state = web::Data::new(self.state.clone());
        cfg.service(
            web::scope("/api/v1/calendar")
                .app_data(state)
                .route("", web::get().to(get_calendar_view))
                .route("/items", web::get().to(list_calendar_items))
                .route("/create-intent", web::post().to(create_intent))
                .route("/settings", web::get().to(get_settings))
                .route("/settings", web::put().to(update_settings))
                .route("/sources", web::get().to(get_sources))
                .route("/sources", web::put().to(update_sources))
                .route("/feeds", web::post().to(create_feed))
                .route("/feeds", web::get().to(list_feeds))
                .route("/feeds/{id}", web::delete().to(revoke_feed))
                .route("/feed/{token}", web::get().to(get_feed_ics))
                .route("/ai/summarize-day", web::post().to(ai_summarize_day))
                .route("/ai/whats-next", web::post().to(ai_whats_next)),
        );
    }

    fn route_info(&self) -> Vec<RouteInfo> {
        vec![
            RouteInfo {
                method: "GET",
                path: "/api/v1/calendar",
                owner: "calendar",
                description: "Month/week/day/agenda view: aggregated items with viewer-local times, non-working days, and conflict hints.",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/calendar/items",
                owner: "calendar",
                description: "Paginated, filtered aggregated items (filters: from, to, modules, timezone).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/calendar/create-intent",
                owner: "calendar",
                description: "Resolve the owning module's create deep link for a date/time; the calendar never creates a record itself.",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/calendar/settings",
                owner: "calendar",
                description: "Effective settings: company defaults merged with the caller's overrides.",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "PUT",
                path: "/api/v1/calendar/settings",
                owner: "calendar",
                description: "Save settings (scope user or company; weekend days, working hours, default view, timezone, holiday country, create-link templates).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/calendar/sources",
                owner: "calendar",
                description: "The caller's per-module visibility toggles (absent means visible).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "PUT",
                path: "/api/v1/calendar/sources",
                owner: "calendar",
                description: "Replace the caller's per-module visibility toggles.",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/calendar/feeds",
                owner: "calendar",
                description: "Issue a revocable HMAC-signed iCalendar subscription token (raw token returned once).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/calendar/feeds",
                owner: "calendar",
                description: "List the caller's own feed tokens (metadata only; paginated).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "DELETE",
                path: "/api/v1/calendar/feeds/{id}",
                owner: "calendar",
                description: "Revoke a feed token (a foreign tenant's feed answers 404).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/calendar/feed/{token}",
                owner: "calendar",
                description: "Public iCalendar subscription feed; the signed token is the credential and must belong to the resolved tenant (.ics suffix accepted).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/calendar/ai/summarize-day",
                owner: "calendar",
                description: "AI draft summary of the caller's calendar day (draft only; 503 when no AI handler answers).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/calendar/ai/whats-next",
                owner: "calendar",
                description: "AI draft briefing of what comes next on the caller's calendar (draft only; 503 when no AI handler answers).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
        ]
    }
}

pub fn register(registry: &ModuleRegistry) {
    registry.register(Arc::new(CalendarModule::new()));
}
