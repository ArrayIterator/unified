//! extensions - module.

use unified_core::context::AppContext;
use unified_core::modules::{Module, ModuleRegistry, RouteInfo};
use actix_web::web;
use dashmap::DashMap;
use std::sync::Arc;
use super::handlers::{disable_extension, enable_extension, get_extension, get_requirements, list_dependencies, list_extensions, list_hooks, list_library_requirements, recheck_library_requirements};

pub struct ExtensionsModule {
    /// Explicit enabled state per module id. Absent means "use the
    /// registry default" (see `is_enabled`): enabled for a registered
    /// module, disabled for an id the registry does not know.
    /// This map is shared with the route handlers through `web::Data`,
    /// so the state they read and write is the same one this module owns.
    enabled: Arc<DashMap<String, bool>>,
}

impl ExtensionsModule {
    pub fn new() -> Self {
        Self {
            enabled: Arc::new(DashMap::new()),
        }
    }
}

pub type EnabledState = Arc<DashMap<String, bool>>;

/// Is `id` enabled?
///
/// Explicit state wins: an entry in `state` is honoured even for an
/// id the registry does not know. With no explicit entry, a
/// *registered* module defaults to enabled (opt-out: a module runs
/// unless explicitly disabled), while an *unregistered* id defaults
/// to disabled -- a typo'd or not-yet-registered id must never read
/// as enabled anywhere. This used to default every unknown id to
/// enabled; that behaviour changed here because the old default let
/// a misspelled id look live in the module list, requirement checks,
/// and dependency decisions.
pub(crate) fn is_enabled(state: &EnabledState, registry: &ModuleRegistry, id: &str) -> bool {
    match state.get(id) {
        Some(entry) => *entry.value(),
        None => registry.get(id).is_some(),
    }
}

impl Default for ExtensionsModule {
    fn default() -> Self {
        Self::new()
    }
}

pub fn register(registry: &ModuleRegistry) {
    registry.register(Arc::new(ExtensionsModule::new()));
}

impl Module for ExtensionsModule {
    fn id(&self) -> &'static str {
        "extensions"
    }

    fn name(&self) -> &'static str {
        "Extensions"
    }

    fn description(&self) -> &'static str {
        "Modules / Extensions Manager: lists, enables and disables modules with dependency auto-activation, validates the module dependency graph, lists registered hook events, and surfaces per-module notices for required external libraries that the Service Monitor found missing or outdated."
    }

    fn init(&self, ctx: &AppContext) -> unified_core::error::UnifiedResult<()> {
        // Startup dependency-graph validation: log every problem so a
        // misconfigured registration is visible in the server log even
        // before anyone calls GET /extensions/dependencies. Startup
        // itself is not failed here -- a missing optional module must
        // not take the whole server down; the endpoint reports the
        // same issues and enabling a broken module is refused there.
        let report = ctx.modules.validate_dependencies();
        if !report.valid {
            for issue in &report.issues {
                log::warn!("Module dependency problem: {}", issue.message);
            }
        }
        Ok(())
    }

    fn route_info(&self) -> Vec<RouteInfo> {
        vec![
            RouteInfo { method: "GET", path: "/api/v1/extensions/hooks", owner: "extensions", description: "List hooks", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/extensions/dependencies", owner: "extensions", description: "List dependencies", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/extensions", owner: "extensions", description: "List extensions", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/extensions/{id}", owner: "extensions", description: "Get extension", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/extensions/libraries", owner: "extensions", description: "List external library requirements with the latest check result per library (paginated, filter by status).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/extensions/libraries/check", owner: "extensions", description: "Re-check external libraries; same reports as POST /api/v1/service-monitor/libraries/check.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/extensions/{id}/requirements", owner: "extensions", description: "Get requirements", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/extensions/{id}/enable", owner: "extensions", description: "Enable extension", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/extensions/{id}/disable", owner: "extensions", description: "Disable extension", auth: unified_core::auth::RouteAuth::Authenticated},
        ]
    }

    fn configure_routes(&self, cfg: &mut web::ServiceConfig) {
        // The enabled-state map is shared with the handlers through
        // `web::Data`, so the state they read and write is the same map
        // this module instance owns. `/dependencies` and `/libraries`
        // are registered before `/{id}` so they are not captured as a
        // module id.
        cfg.service(
            web::scope("/api/v1/extensions")
                .app_data(web::Data::new(self.enabled.clone()))
                .route("/hooks", web::get().to(list_hooks))
                .route("/dependencies", web::get().to(list_dependencies))
                .route("/libraries", web::get().to(list_library_requirements))
                .route(
                    "/libraries/check",
                    web::post().to(recheck_library_requirements),
                )
                .route("", web::get().to(list_extensions))
                .route("/{id}", web::get().to(get_extension))
                .route("/{id}/requirements", web::get().to(get_requirements))
                .route("/{id}/enable", web::post().to(enable_extension))
                .route("/{id}/disable", web::post().to(disable_extension)),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn registry_with_extensions() -> ModuleRegistry {
        let registry = ModuleRegistry::new();
        registry.register(Arc::new(ExtensionsModule::new()));
        registry
    }

    #[test]
    fn is_enabled_defaults_to_true_for_registered_modules_with_no_explicit_entry() {
        let state: EnabledState = Arc::new(DashMap::new());
        let registry = registry_with_extensions();
        assert!(is_enabled(&state, &registry, "extensions"));
    }

    #[test]
    fn is_enabled_defaults_to_false_for_unknown_module_ids() {
        let state: EnabledState = Arc::new(DashMap::new());
        let registry = registry_with_extensions();
        // A typo'd or unregistered id must never read as enabled.
        assert!(!is_enabled(&state, &registry, "billing"));
        assert!(!is_enabled(&state, &registry, "typo-modul"));
    }

    #[test]
    fn is_enabled_respects_an_explicit_false() {
        let state: EnabledState = Arc::new(DashMap::new());
        state.insert("extensions".to_string(), false);
        let registry = registry_with_extensions();
        assert!(!is_enabled(&state, &registry, "extensions"));
    }

    #[test]
    fn is_enabled_respects_an_explicit_true() {
        let state: EnabledState = Arc::new(DashMap::new());
        state.insert("extensions".to_string(), true);
        let registry = registry_with_extensions();
        assert!(is_enabled(&state, &registry, "extensions"));
    }

    #[test]
    fn is_enabled_explicit_entry_wins_for_an_unknown_id() {
        // Explicit state is honoured even when the id is not
        // registered: an operator (or a test) that wrote a value for
        // an id gets that value back, registry or not.
        let state: EnabledState = Arc::new(DashMap::new());
        state.insert("ghost".to_string(), true);
        let registry = registry_with_extensions();
        assert!(is_enabled(&state, &registry, "ghost"));
        state.insert("ghost".to_string(), false);
        assert!(!is_enabled(&state, &registry, "ghost"));
    }
}
