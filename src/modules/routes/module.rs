// routes module — Module definition.
//
// Split from the former single-file module `src/modules/routes.rs`.
// The module root (`routes.rs`) re-exports the public items, so the
// public API is unchanged.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::json;
use unified_core::modules::{Module, ModuleRegistry, RouteInfo};
use unified_core::pagination::{PageResult, PaginationParams};
use unified_core::routes::{
    OverrideSource, RedirectEntry, RouteChange, RouteEntry, RouteRegistry,
    ROUTE_CHANGED_EVENT,
};
use actix_web::{web, HttpResponse};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::sync::{Arc, RwLock};

use super::handlers::{clear_route_override, get_route, list_redirects, list_routes, set_route_override};

// ---------------------------------------------------------------------------
// Module definition.
// ---------------------------------------------------------------------------

pub struct RoutesModule {
    /// The registry this module serves. A lock (not a plain field)
    /// so `init` can replace the construction-time registry with the
    /// single shared instance from `AppContext`: the module is built
    /// with a fresh registry by `register`, and the shared instance
    /// only becomes available in `init` through the context.
    pub(crate) registry: RwLock<Arc<RouteRegistry>>,
}

impl RoutesModule {
    pub fn new() -> Self {
        Self {
            registry: RwLock::new(Arc::new(RouteRegistry::new())),
        }
    }

    /// Build the module around a registry the integrator created (for
    /// example the shared registry stored on `AppContext`). `init`
    /// replaces it with `ctx.routes` anyway, so passing anything else
    /// here only matters before `init` runs.
    pub fn with_registry(registry: Arc<RouteRegistry>) -> Self {
        Self {
            registry: RwLock::new(registry),
        }
    }

    /// The registry this module serves, so the server wiring and the
    /// theme engine can share the same instance. After `init` this is
    /// the shared `AppContext` registry.
    pub fn registry(&self) -> Arc<RouteRegistry> {
        self.registry
            .read()
            .expect("route registry lock poisoned")
            .clone()
    }
}

impl Default for RoutesModule {
    fn default() -> Self {
        Self::new()
    }
}

impl Module for RoutesModule {
    fn id(&self) -> &'static str {
        "routes"
    }

    fn name(&self) -> &'static str {
        "Routes"
    }

    fn description(&self) -> &'static str {
        "Customizable page routes: named auth and dashboard paths with config and settings overrides, validation, and permanent redirects for renamed paths."
    }

    fn requires(&self) -> &[&'static str] {
        &["settings", "audit"]
    }

    fn init(&self, ctx: &AppContext) -> UnifiedResult<()> {
        // Unify on the single shared registry from the context: the
        // same instance `AppContext::new` seeded from the `routes:`
        // config section, the server redirect handler resolves
        // through, and the theme engine is attached to. The registry
        // this module was constructed with held no overrides yet —
        // nothing writes to it before `init` — so replacing it loses
        // nothing, and `routes:` overrides now reach this module's
        // admin API as well as the server and the theme engine.
        // The legacy `app.dashboard_path` fallback was already
        // applied to the shared registry by `AppContext::new`, so
        // there is nothing left to seed here.
        *self.registry.write().expect("route registry lock poisoned") =
            Arc::clone(&ctx.routes);
        Ok(())
    }

    fn configure_routes(&self, cfg: &mut web::ServiceConfig) {
        cfg.service(
            web::scope("/api/v1/routes")
                .app_data(web::Data::new(self.registry()))
                // Static segment first: "/redirects" must be registered
                // before "/{name}" or it would match as a route name.
                .route("/redirects", web::get().to(list_redirects))
                .route("", web::get().to(list_routes))
                .route("/{name}", web::get().to(get_route))
                .route("/{name}", web::put().to(set_route_override))
                .route("/{name}", web::delete().to(clear_route_override)),
        );
    }

    fn route_info(&self) -> Vec<RouteInfo> {
        vec![
            RouteInfo {
                method: "GET",
                path: "/api/v1/routes",
                owner: "routes",
                description: "List named routes with current path, default path, override source, and last change (paginated).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/routes/{name}",
                owner: "routes",
                description: "Get one named route.",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "PUT",
                path: "/api/v1/routes/{name}",
                owner: "routes",
                description: "Set a settings override for a named route (validated; audit-logged; emits route.changed).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "DELETE",
                path: "/api/v1/routes/{name}",
                owner: "routes",
                description: "Clear an override for a named route (falls back to config or default).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/routes/redirects",
                owner: "routes",
                description: "List active old-path redirects (paginated).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
        ]
    }
}

pub fn register(registry: &ModuleRegistry) {
    registry.register(Arc::new(RoutesModule::new()));
}
