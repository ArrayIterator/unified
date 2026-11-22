// Module system.
//
// A module is a self-contained unit of functionality that plugs into the
// core engine. Every core module is registered in the binary, and every
// module — core or extension — is fully hookable: it can register hook
// handlers, CLI commands, middleware, and HTTP routes, and core behaviour
// can be changed by code through the hook bus.
//
// Dynamic modules and extensions are listed and managed through the
// Modules / Extensions Manager module (`crate::modules::extensions`).

use crate::core::context::AppContext;
use crate::core::error::UnifiedResult;
use crate::core::hooks::HookBus;
use crate::core::middleware::MiddlewareManager;
use actix_web::web::ServiceConfig;
use clap::{ArgMatches, Command};
use serde::Serialize;
use std::sync::{Arc, RwLock};

/// Metadata describing one HTTP route a module exposes. The registry
/// aggregates these into a route catalog (see
/// `ModuleRegistry::route_catalog`) so the admin UI and the monitor
/// can list every route without introspecting the Actix router.
/// Filling it in is optional; modules that do not describe their
/// routes simply contribute nothing to the catalog.
#[derive(Debug, Clone, Serialize)]
pub struct RouteInfo {
    /// HTTP method, for example `GET`. Use `*` for any method.
    pub method: &'static str,
    /// Route path, for example `/api/v1/theme/themes/{id}`.
    pub path: &'static str,
    /// Id of the module that owns the route.
    pub owner: &'static str,
    /// Short description of what the route does.
    pub description: &'static str,
}

/// The contract every module implements.
///
/// Route handlers are registered through `configure_routes`, hook handlers
/// through `register_hooks`, and extra CLI commands through `register_cli`.
/// `init` runs once at startup, after the configuration is loaded.
pub trait Module: Send + Sync {
    fn id(&self) -> &'static str;
    fn name(&self) -> &'static str;
    fn version(&self) -> &'static str {
        "1.0.0"
    }
    fn description(&self) -> &'static str;
    fn is_core(&self) -> bool {
        true
    }
    fn dependencies(&self) -> Vec<&'static str> {
        Vec::new()
    }
    fn register_hooks(&self, _bus: &HookBus) {}
    fn register_cli(&self, _registry: &mut CliRegistry) {}
    fn configure_routes(&self, _cfg: &mut ServiceConfig) {}
    /// Metadata for the routes this module registers, for the route
    /// catalog. Optional; the default is no entries. The paths and
    /// methods should match what `configure_routes` registers.
    fn route_info(&self) -> Vec<RouteInfo> {
        Vec::new()
    }
    /// Register middleware contributed by this module. The middleware
    /// runs for every request, ordered by priority (highest first)
    /// across all modules. The default is no middleware.
    fn register_middleware(&self, _registry: &mut MiddlewareManager) {}
    fn init(&self, _ctx: &AppContext) -> UnifiedResult<()> {
        Ok(())
    }
}

/// Public summary of a module, as returned by the Modules API.
#[derive(Debug, Clone, Serialize)]
pub struct ModuleInfo {
    pub id: &'static str,
    pub name: &'static str,
    pub version: &'static str,
    pub description: &'static str,
    pub is_core: bool,
    pub dependencies: Vec<&'static str>,
}

/// One dynamically injected CLI command, contributed by a module.
pub struct CliCommandDef {
    /// Subcommand name, for example `ssl-renew`.
    pub name: &'static str,
    pub about: &'static str,
    /// The clap command definition (arguments and flags).
    pub command: Command,
    /// The handler run when the subcommand is invoked.
    pub handler: Arc<dyn Fn(&ArgMatches) -> UnifiedResult<()> + Send + Sync>,
}

/// Registry for CLI commands injected by modules.
pub struct CliRegistry {
    commands: RwLock<Vec<CliCommandDef>>,
}

impl CliRegistry {
    pub fn new() -> Self {
        Self {
            commands: RwLock::new(Vec::new()),
        }
    }

    pub fn register(&self, def: CliCommandDef) {
        self.commands
            .write()
            .expect("cli registry lock poisoned")
            .push(def);
    }

    /// All registered command definitions, as clap commands.
    pub fn build_commands(&self) -> Vec<Command> {
        self.commands
            .read()
            .expect("cli registry lock poisoned")
            .iter()
            .map(|def| def.command.clone())
            .collect()
    }

    /// Dispatch a subcommand by name. Returns `None` when no module
    /// registered a command with that name.
    pub fn dispatch(&self, name: &str, matches: &ArgMatches) -> Option<UnifiedResult<()>> {
        let commands = self.commands.read().expect("cli registry lock poisoned");
        commands
            .iter()
            .find(|def| def.name == name)
            .map(|def| (def.handler)(matches))
    }

    pub fn names(&self) -> Vec<String> {
        self.commands
            .read()
            .expect("cli registry lock poisoned")
            .iter()
            .map(|def| def.name.to_string())
            .collect()
    }
}

impl Default for CliRegistry {
    fn default() -> Self {
        Self::new()
    }
}

/// Registry holding every module known to the application.
pub struct ModuleRegistry {
    modules: RwLock<Vec<Arc<dyn Module>>>,
}

impl ModuleRegistry {
    pub fn new() -> Self {
        Self {
            modules: RwLock::new(Vec::new()),
        }
    }

    pub fn register(&self, module: Arc<dyn Module>) {
        self.modules
            .write()
            .expect("module registry lock poisoned")
            .push(module);
    }

    pub fn get(&self, id: &str) -> Option<Arc<dyn Module>> {
        self.modules
            .read()
            .expect("module registry lock poisoned")
            .iter()
            .find(|module| module.id() == id)
            .cloned()
    }

    pub fn list(&self) -> Vec<Arc<dyn Module>> {
        self.modules
            .read()
            .expect("module registry lock poisoned")
            .clone()
    }

    pub fn ids(&self) -> Vec<String> {
        self.list()
            .iter()
            .map(|module| module.id().to_string())
            .collect()
    }

    pub fn summary(&self) -> Vec<ModuleInfo> {
        self.list()
            .iter()
            .map(|module| ModuleInfo {
                id: module.id(),
                name: module.name(),
                version: module.version(),
                description: module.description(),
                is_core: module.is_core(),
                dependencies: module.dependencies(),
            })
            .collect()
    }

    /// Run `init` on every registered module, in registration order.
    pub fn init_all(&self, ctx: &AppContext) -> UnifiedResult<()> {
        for module in self.list() {
            module.init(ctx)?;
        }
        Ok(())
    }

    /// Let every module register its hook handlers.
    pub fn register_all_hooks(&self, bus: &HookBus) {
        for module in self.list() {
            module.register_hooks(bus);
        }
    }

    /// Let every module inject its CLI commands.
    pub fn register_all_cli(&self, registry: &CliRegistry) {
        for module in self.list() {
            let mut owned = CliRegistry::new();
            module.register_cli(&mut owned);
            for def in owned.commands.into_inner().expect("cli registry lock poisoned") {
                registry.register(def);
            }
        }
    }

    /// Let every module register its HTTP routes.
    pub fn configure_all_routes(&self, cfg: &mut ServiceConfig) {
        for module in self.list() {
            module.configure_routes(cfg);
        }
    }

    /// Aggregate the route metadata of every module into one catalog,
    /// sorted by path and then method. Only modules that implement
    /// `route_info` contribute entries.
    pub fn route_catalog(&self) -> Vec<RouteInfo> {
        let mut catalog: Vec<RouteInfo> = Vec::new();
        for module in self.list() {
            catalog.extend(module.route_info());
        }
        catalog.sort_by(|a, b| a.path.cmp(b.path).then(a.method.cmp(b.method)));
        catalog
    }

    /// Let every module register its middleware, in registration order.
    /// The dispatcher sorts the collected middleware by priority when
    /// the application is built.
    pub fn register_all_middleware(&self, registry: &mut MiddlewareManager) {
        for module in self.list() {
            module.register_middleware(registry);
        }
    }
}

impl Default for ModuleRegistry {
    fn default() -> Self {
        Self::new()
    }
}
