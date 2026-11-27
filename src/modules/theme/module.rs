// Theme Management module.
//
// Discovers themes from the `themes/` directory plus an embedded default
// theme, validates each theme strictly (manifest, semver version, layout
// template, no path traversal, no files escaping the theme directory),
// and activates one theme at a time. The active theme id is persisted
// to `data/theme.json`.

// Module registration: struct, Module impl and register().

use unified_core::context::AppContext;
use unified_core::error::UnifiedResult;
use unified_core::hooks::{HookBus, HookContext};
use unified_core::modules::{Module, ModuleRegistry, RouteInfo};
use actix_web::web;
use serde_json::json;
use std::sync::Arc;

use crate::themes::engine::THEME_ACTIVATED_EVENT;

use super::*;

// Module

pub struct ThemeModule {
    pub(crate) state: ThemeState,
}

impl ThemeModule {
    pub fn new() -> Self {
        Self {
            state: ThemeState::default(),
        }
    }
}

impl Default for ThemeModule {
    fn default() -> Self {
        Self::new()
    }
}

impl Module for ThemeModule {
    fn id(&self) -> &'static str {
        "theme"
    }

    fn name(&self) -> &'static str {
        "Theme Management"
    }

    fn description(&self) -> &'static str {
        "Theme discovery, strict validation, and activation."
    }

    fn init(&self, ctx: &AppContext) -> UnifiedResult<()> {
        self.state.load_active();
        // Sync the rendering engines with the persisted active theme.
        // Hook subscriptions are registered before any module init
        // runs (`register_all_hooks` precedes `init_all` at startup),
        // so the dashboards and frontend engines are already
        // listening: without this emit a restart with a non-default
        // theme persisted would render the default theme until the
        // next activation. The engines validate the id themselves,
        // so a persisted id whose theme has since been removed is
        // ignored and they keep rendering the default theme.
        ctx.hooks.emit(
            THEME_ACTIVATED_EVENT,
            json!({ "theme_id": self.state.active_id(), "active": true }),
        );
        Ok(())
    }

    fn register_hooks(&self, bus: &HookBus) {
        let state = self.state.clone();
        bus.register(
            "theme.exists",
            "theme",
            Arc::new(move |ctx: &mut HookContext| on_theme_exists(&state, ctx)),
        );
        bus.register(
            "theme.render_block",
            "theme",
            Arc::new(on_theme_render_block),
        );
        let state = self.state.clone();
        bus.register(
            "theme.activate_requested",
            "theme",
            Arc::new(move |ctx: &mut HookContext| {
                on_theme_activate_requested(&state, ctx)
            }),
        );
    }

    fn configure_routes(&self, cfg: &mut web::ServiceConfig) {
        let state = web::Data::new(self.state.clone());
        cfg.service(
            web::scope("/api/v1/theme")
                .app_data(state)
                .route("/themes", web::get().to(list_themes))
                .route("/themes/{id}", web::get().to(get_theme))
                .route("/themes/{id}/activate", web::post().to(activate_theme))
                .route("/active", web::get().to(get_active_theme)),
        );
    }

    fn route_info(&self) -> Vec<RouteInfo> {
        vec![
            RouteInfo {
                method: "GET",
                path: "/api/v1/theme/themes",
                owner: "theme",
                description: "List all discovered themes",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/theme/themes/{id}",
                owner: "theme",
                description: "Get one theme by id",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/theme/themes/{id}/activate",
                owner: "theme",
                description: "Activate a theme (requires theme:manage)",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/theme/active",
                owner: "theme",
                description: "Get the active theme",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
        ]
    }
}

pub fn register(registry: &ModuleRegistry) {
    registry.register(Arc::new(ThemeModule::new()));
}
