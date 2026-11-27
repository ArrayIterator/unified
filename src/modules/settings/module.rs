//! Settings - Module definition: routes, hooks, and registration.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::{events, HookBus, HookContext};
use unified_core::json::{error_response, success_response};
use unified_core::modules::{Module, ModuleRegistry, RouteInfo};
use unified_core::pagination::{PageResult, PaginationParams};
use actix_web::{web, HttpResponse};
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::sync::Arc;
use super::{types::*, handlers::*};

pub struct SettingsModule {
    pub(crate) store: SettingsStore,
}

impl SettingsModule {
    pub fn new() -> Self {
        Self {
            store: Arc::new(DashMap::new()),
        }
    }
}

impl Default for SettingsModule {
    fn default() -> Self {
        Self::new()
    }
}

pub fn register(registry: &ModuleRegistry) {
    registry.register(Arc::new(SettingsModule::new()));
}

impl Module for SettingsModule {
    fn id(&self) -> &'static str {
        "settings"
    }

    fn name(&self) -> &'static str {
        "Settings"
    }

    fn description(&self) -> &'static str {
        "Namespaced key-value settings with typed values, validated through the config.validate hook."
    }

    fn register_hooks(&self, bus: &HookBus) {
        let store = self.store.clone();
        bus.register(
            "settings.merge",
            "settings",
            Arc::new(move |ctx: &mut HookContext| {
                on_settings_merge(store.clone(), ctx)
            }),
        );
        // Validate `site/*` writes against the site
        // definitions (WordPress General Settings parity). Other
        // namespaces pass through untouched.
        bus.register(
            events::CONFIG_VALIDATE,
            "settings",
            Arc::new(move |ctx: &mut HookContext| on_config_validate(ctx)),
        );
    }

    fn route_info(&self) -> Vec<RouteInfo> {
        vec![
            RouteInfo { method: "GET", path: "/api/v1/settings", owner: "settings", description: "List settings (requires X-User-Id + X-Role and settings.view via role.check, fail-closed; no namespace is public)", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/settings/definitions", owner: "settings", description: "Setting definitions for a namespace (site: WordPress General Settings parity - title, tagline, URLs, admin email, membership, default role, language, timezone, date/time formats, week start) with effective values.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/settings/{namespace}/{key}", owner: "settings", description: "Get setting (requires X-User-Id + X-Role and settings.view via role.check, fail-closed)", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "PUT", path: "/api/v1/settings/{namespace}/{key}", owner: "settings", description: "Put setting (requires X-User-Id + X-Role and settings.manage via role.check, fail-closed)", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "DELETE", path: "/api/v1/settings/{namespace}/{key}", owner: "settings", description: "Delete setting (requires X-User-Id + X-Role and settings.manage via role.check, fail-closed)", auth: unified_core::auth::RouteAuth::Authenticated},
        ]
    }

    fn configure_routes(&self, cfg: &mut web::ServiceConfig) {
        cfg.service(
            web::scope("/api/v1/settings")
                .app_data(web::Data::new(self.store.clone()))
                .route("", web::get().to(list_settings))
                .route("/definitions", web::get().to(list_definitions))
                .route("/{namespace}/{key}", web::get().to(get_setting))
                .route("/{namespace}/{key}", web::put().to(put_setting))
                .route("/{namespace}/{key}", web::delete().to(delete_setting)),
        );
    }
}
