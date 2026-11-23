// Example external module for the Unified server.
//
// This crate shows the smallest complete module: one HTTP route, one
// hook listener, no database, no configuration section. Copy this crate
// to start your own module; see README.md in this directory and
// docs/WORKSPACE_AND_EXTERNAL_MODULES.md (sections 3 and 4) for the
// full contract.
//
// The crate depends only on `unified-core` (plus actix-web and serde
// for its own handler). It never imports the server crate, other
// modules, the HTTP layer, the CLI, or the gateway.

use actix_web::{web, HttpResponse};
use serde_json::json;
use std::sync::Arc;
use unified_core::context::AppContext;
use unified_core::error::UnifiedResult;
use unified_core::hooks::{events, HookBus, HookContext};
use unified_core::json::success_response;
use unified_core::modules::{Module, ModuleRegistry, RouteInfo};

/// The module. One struct, no fields: all behaviour is in the trait
/// implementation below.
pub struct HelloModule;

/// Route handler for `GET /api/v1/hello`.
///
/// Returns the standard success envelope (`{"data": ...}`) through
/// `unified_core::json::success_response`. No handler returns any
/// other envelope.
async fn hello_handler() -> HttpResponse {
    success_response(
        json!({
            "message": "Hello from the example module.",
            "module": "hello",
        }),
        200,
    )
}

impl Module for HelloModule {
    fn id(&self) -> &'static str {
        "hello"
    }

    fn name(&self) -> &'static str {
        "Hello"
    }

    fn version(&self) -> &'static str {
        "0.1.0"
    }

    fn description(&self) -> &'static str {
        "Example external module: one route, one hook listener, no database."
    }

    /// External modules must return `false`. The default is `true`,
    /// and a module that leaves the default is treated as a core
    /// module: the Extensions Manager refuses to disable it.
    fn is_core(&self) -> bool {
        false
    }

    /// This module needs nothing else, so it declares no requirements.
    /// A module that needs another module's routes or hooks lists its
    /// ids here; the registry validates the graph and the Extensions
    /// Manager auto-activates them transitively.
    fn requires(&self) -> &[&'static str] {
        &[]
    }

    /// One hook listener: when the application starts, contribute a
    /// result to the `app.start` event. The handler receives a mutable
    /// `HookContext`; it may read and modify the event payload, add
    /// results, or stop further handlers with `ctx.stop()`.
    fn register_hooks(&self, bus: &HookBus) {
        bus.register(events::APP_START, "hello", Arc::new(|ctx: &mut HookContext| {
            ctx.add_result(json!({
                "module": "hello",
                "event": "started",
            }));
        }));
    }

    /// One route: `GET /api/v1/hello`. Routes live under
    /// `/api/v1/<module-id>/` unless the module documents a reason
    /// not to.
    fn configure_routes(&self, cfg: &mut web::ServiceConfig) {
        cfg.route("/api/v1/hello", web::get().to(hello_handler));
    }

    /// Metadata for the route above, so the route catalog
    /// (`ModuleRegistry::route_catalog`) lists this module. The path
    /// and method must match what `configure_routes` registers.
    fn route_info(&self) -> Vec<RouteInfo> {
        vec![RouteInfo {
            method: "GET",
            path: "/api/v1/hello",
            owner: "hello",
            description: "Example greeting from the hello module.",
            auth: unified_core::auth::RouteAuth::Authenticated,
        }]
    }

    /// Runs once at startup, after the core migrations have run and
    /// the database pool (when configured) is attached to the context.
    /// This module needs no startup work.
    fn init(&self, _ctx: &AppContext) -> UnifiedResult<()> {
        Ok(())
    }
}

/// The one function the server calls. Same shape as every built-in
/// module's `register` function.
///
/// Wiring (done by the server, not by this crate): the root
/// `Cargo.toml` gains `example-hello-module = { path =
/// "crates/example-hello-module" }`, and
/// `src/modules/mod.rs::register_all` gains one line in its
/// external-modules section:
/// `example_hello_module::register(registry);`
pub fn register(registry: &ModuleRegistry) {
    registry.register(Arc::new(HelloModule));
}

/// Convenience accessor for tests and for embedding.
pub fn module() -> Arc<dyn Module> {
    Arc::new(HelloModule)
}
