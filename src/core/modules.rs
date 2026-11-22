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
//
// Dependency system: every module declares the other modules it needs
// through `Module::requires` (and the libraries/crates it needs through
// `Module::requires_libraries`). A module may also declare the external
// system libraries / binaries it needs through
// `Module::requires_external_libraries` (see `ExternalLibraryRequirement`);
// the Service Monitor checks those against the host and reports
// missing or outdated ones. The registry validates the resulting
// graph — missing dependencies and cycles — with
// `ModuleRegistry::validate_dependencies`, and the Extensions Manager
// auto-activates required modules when a module is enabled (emitting
// `module.auto_activated` per auto-activated module) and refuses to
// disable a module that other enabled modules still depend on unless
// the caller passes `force=true`.
//
// Product declarations: a module may declare itself sellable as a
// product through `Module::available_as_product` together with a
// `Module::product_category` from the closed `ProductCategory` set.
// `ModuleRegistry::validate_product_declarations` reports modules
// whose declaration is missing or invalid, `sellable_modules` and
// `product_offerings` expose the sellable modules to Product
// Management, and the licensing module gates enabling a sellable
// module on a module entitlement (see
// `crate::modules::licensing::check_module_entitlement`).

use crate::core::context::AppContext;
use crate::core::error::{UnifiedError, UnifiedResult};
use crate::core::hooks::HookBus;
use crate::core::middleware::MiddlewareManager;
use actix_web::web::ServiceConfig;
use clap::{ArgMatches, Command};
use serde::Serialize;
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, RwLock};

/// Event emitted once per module that was auto-activated as a dependency
/// of another module being enabled. Payload: `{"id", "auto_activated_by"}`.
/// The hook bus has no dedicated constant for it (it lives outside the
/// well-known core events in `crate::core::hooks::events`), so the name
/// is defined here next to the dependency system that emits it.
pub const MODULE_AUTO_ACTIVATED_EVENT: &str = "module.auto_activated";

/// Event emitted when enabling a sellable module is allowed by its
/// entitlement check. Payload: `{"id", "company_id", "reason"}`,
/// where `reason` is `"license"` for a matching active license or
/// `"owner_entitlement"` for the implicit owner entitlement of a
/// core sellable module. Emitted by
/// `crate::modules::licensing::check_module_entitlement`.
pub const MODULE_ENTITLEMENT_GRANTED_EVENT: &str = "module.entitlement.granted";

/// Event emitted when enabling a sellable module is refused because
/// no entitlement covers it. Payload:
/// `{"id", "company_id", "reason"}`, where `reason` is
/// `"no_active_entitlement"`. Emitted by
/// `crate::modules::licensing::check_module_entitlement`.
pub const MODULE_ENTITLEMENT_DENIED_EVENT: &str = "module.entitlement.denied";

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

/// Which public API versions expose a module's routes.
///
/// A module declares this through `Module::route_version_support`;
/// the API Manager reads it when it collects the built-in endpoint
/// registry, so a module's routes land in exactly the versions the
/// module supports instead of every version. The default is `All`,
/// so modules that say nothing about versions keep the old
/// behaviour. The variants mirror the API Manager's
/// `VersionSupport` shape (all/min/max/range/list) with `&'static`
/// bounds, so the declaration can live in a `const`-friendly trait
/// method without depending on the API Manager module.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RouteVersionSupport {
    /// Available in every registered version.
    All,
    /// Available in `min_version` and every later version.
    Min { min_version: &'static str },
    /// Available in `max_version` and every earlier version.
    Max { max_version: &'static str },
    /// Available in the inclusive range
    /// `min_version..=max_version`.
    Range {
        min_version: &'static str,
        max_version: &'static str,
    },
    /// Available only in the listed versions.
    List { versions: &'static [&'static str] },
}

impl Default for RouteVersionSupport {
    fn default() -> Self {
        RouteVersionSupport::All
    }
}

/// Closed set of product categories a sellable module may declare
/// through `Module::product_category`. The set is closed on purpose:
/// Product Management groups module offerings by these categories,
/// so a free-form string would silently create a category nothing
/// else understands. `from_str` rejects anything outside the set.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum ProductCategory {
    #[serde(rename = "productivity")]
    Productivity,
    #[serde(rename = "communication")]
    Communication,
    #[serde(rename = "commerce")]
    Commerce,
    #[serde(rename = "infrastructure")]
    Infrastructure,
    #[serde(rename = "ai")]
    Ai,
}

impl ProductCategory {
    /// Every valid category string, in canonical order.
    pub const ALL: [&'static str; 5] = [
        "productivity",
        "communication",
        "commerce",
        "infrastructure",
        "ai",
    ];

    /// Canonical string of this category.
    pub fn as_str(&self) -> &'static str {
        match self {
            ProductCategory::Productivity => "productivity",
            ProductCategory::Communication => "communication",
            ProductCategory::Commerce => "commerce",
            ProductCategory::Infrastructure => "infrastructure",
            ProductCategory::Ai => "ai",
        }
    }

    /// Parse a category string. Returns `None` for any string
    /// outside the closed set, so callers can reject unknown
    /// categories instead of storing them.
    pub fn from_str(value: &str) -> Option<Self> {
        match value {
            "productivity" => Some(ProductCategory::Productivity),
            "communication" => Some(ProductCategory::Communication),
            "commerce" => Some(ProductCategory::Commerce),
            "infrastructure" => Some(ProductCategory::Infrastructure),
            "ai" => Some(ProductCategory::Ai),
            _ => None,
        }
    }

    /// True when `value` is one of the valid category strings.
    pub fn is_valid(value: &str) -> bool {
        Self::from_str(value).is_some()
    }
}

/// One external tool the Service Monitor is allowed to probe with a
/// version flag. This is a closed allowlist on purpose: the checker
/// never runs an arbitrary command from configuration or from a module
/// declaration — it only ever runs the fixed binary and arguments
/// mapped to one of these variants (see
/// `crate::modules::service_monitor::probe_commands`). A module that
/// needs a tool not listed here declares the requirement with
/// `probe: None` and the checker reports it as unverifiable until a
/// probe for it is added to the allowlist.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum ExternalLibraryProbe {
    ImageMagick,
    Ffmpeg,
    Ghostscript,
    ExifTool,
    Poppler,
    LibreOffice,
    Python3,
    Node,
    Git,
    Curl,
    Wget,
    Rsync,
    Tar,
    Unzip,
    Zip,
    Jpegoptim,
    Pngquant,
    Optipng,
    Cwebp,
    Gifsicle,
    Sqlite3,
    PostgresClient,
    MysqlClient,
    RedisCli,
    Wkhtmltopdf,
    RsvgConvert,
    Vips,
    Inkscape,
}

/// One external system library or binary a module needs on the host
/// (for example ImageMagick for image processing). This is separate
/// from `requires_libraries`, which lists Rust crates: those are
/// resolved at compile time, while these are checked at runtime by
/// the Service Monitor against the machine the application runs on
/// (and, on request, against remote servers over SSH).
///
/// `minimum_version` is a loose version such as `"7"`, `"7.1"` or
/// `"7.1.0"`; a missing minimum means "any installed version is
/// enough, presence is what matters". `reason` explains why the
/// module needs the library and is shown in admin notices.
/// `probe` selects the version probe from the fixed allowlist
/// (`ExternalLibraryProbe`); when it is `None` the checker falls
/// back to a name lookup in the same allowlist, and reports the
/// library as unverifiable when no probe matches.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct ExternalLibraryRequirement {
    /// Library name, for example `"ImageMagick"`.
    pub name: &'static str,
    /// Minimum acceptable version, or `None` when any version works.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub minimum_version: Option<&'static str>,
    /// Why the module needs this library.
    pub reason: &'static str,
    /// Version probe from the fixed allowlist, or `None` for a
    /// name-based lookup.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub probe: Option<ExternalLibraryProbe>,
}

impl ExternalLibraryRequirement {
    /// A requirement with no explicit probe (name-based lookup).
    pub const fn new(
        name: &'static str,
        minimum_version: Option<&'static str>,
        reason: &'static str,
    ) -> Self {
        Self {
            name,
            minimum_version,
            reason,
            probe: None,
        }
    }

    /// A requirement with an explicit probe from the allowlist.
    pub const fn with_probe(
        name: &'static str,
        minimum_version: Option<&'static str>,
        reason: &'static str,
        probe: ExternalLibraryProbe,
    ) -> Self {
        Self {
            name,
            minimum_version,
            reason,
            probe: Some(probe),
        }
    }
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
    /// Legacy spelling of the module requirements. New modules should
    /// implement `requires` instead; both are merged by
    /// `module_requires`, so existing modules keep working unchanged.
    fn dependencies(&self) -> Vec<&'static str> {
        Vec::new()
    }
    /// Ids of the modules this module needs in order to work. The
    /// Extensions Manager auto-activates these when this module is
    /// enabled, and refuses to disable a module its dependents still
    /// need. Defaults to an empty list, so existing modules compile
    /// without edits.
    fn requires(&self) -> &[&'static str] {
        &[]
    }
    /// Library / crate names this module relies on (for example
    /// `"diesel"` or `"serde"`). Informational: it documents the
    /// dependency for the admin UI and for operators, and is reported
    /// by the module-list and requirement-check endpoints. Defaults
    /// to an empty list.
    fn requires_libraries(&self) -> &[&'static str] {
        &[]
    }
    /// External system libraries / binaries this module needs on the
    /// host (for example ImageMagick for image processing). The
    /// Service Monitor checks each declared requirement — presence
    /// plus installed version against `minimum_version` — locally
    /// and, on request, on remote servers over SSH, and the
    /// Extensions Manager surfaces a notice per module when a
    /// required library is missing or outdated. Defaults to an empty
    /// list, so existing modules compile without edits.
    fn requires_external_libraries(&self) -> &[ExternalLibraryRequirement] {
        &[]
    }
    /// True when this module may be sold as a product: it appears in
    /// the module offerings Product Management lists and a customer
    /// can buy an entitlement for it. Defaults to `false`, so
    /// existing modules compile unchanged and stay unsellable.
    /// A module that returns `true` here MUST also return a valid
    /// `product_category` (one of `ProductCategory::ALL`); the
    /// registry reports violations through
    /// `ModuleRegistry::validate_product_declarations`.
    fn available_as_product(&self) -> bool {
        false
    }
    /// Product category of this module, from the closed
    /// `ProductCategory` set, when `available_as_product` is true.
    /// Defaults to `None`; returning `Some` for a module that is not
    /// sellable is a declaration error reported by
    /// `ModuleRegistry::validate_product_declarations`.
    fn product_category(&self) -> Option<&'static str> {
        None
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
    /// Which public API versions expose this module's routes, for
    /// the API Manager's per-version endpoint registry. Defaults to
    /// `All`, so existing modules compile without edits; override it
    /// when a module's routes only exist in some versions.
    fn route_version_support(&self) -> RouteVersionSupport {
        RouteVersionSupport::All
    }
    /// Register middleware contributed by this module. The middleware
    /// runs for every request, ordered by priority (highest first)
    /// across all modules. The default is no middleware.
    fn register_middleware(&self, _registry: &mut MiddlewareManager) {}
    fn init(&self, _ctx: &AppContext) -> UnifiedResult<()> {
        Ok(())
    }
}

/// The merged module requirements of one module: `requires()` first,
/// then any ids only declared through the legacy `dependencies()`,
/// de-duplicated with order preserved.
pub fn module_requires(module: &dyn Module) -> Vec<&'static str> {
    let mut merged: Vec<&'static str> = module.requires().to_vec();
    for dependency in module.dependencies() {
        if !merged.contains(&dependency) {
            merged.push(dependency);
        }
    }
    merged
}

/// Public summary of a module, as returned by the Modules API.
#[derive(Debug, Clone, Serialize)]
pub struct ModuleInfo {
    pub id: &'static str,
    pub name: &'static str,
    pub version: &'static str,
    pub description: &'static str,
    pub is_core: bool,
    /// Merged requirements (`requires` plus legacy `dependencies`).
    pub dependencies: Vec<&'static str>,
    pub requires_libraries: Vec<&'static str>,
    /// Declared external system libraries / binaries.
    pub requires_external_libraries: Vec<ExternalLibraryRequirement>,
    /// True when this module may be sold as a product.
    pub available_as_product: bool,
    /// Product category, from the closed `ProductCategory` set, when
    /// the module is sellable; `None` otherwise.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub product_category: Option<&'static str>,
}

/// One sellable module as a product offering, as listed by Product
/// Management (`GET /api/v1/products/module-offerings`). Derived
/// from the registry in memory; no database row backs it.
#[derive(Debug, Clone, Serialize)]
pub struct ModuleProductOffering {
    /// Module id; this is also the product reference a module
    /// entitlement license names.
    pub id: &'static str,
    pub name: &'static str,
    pub description: &'static str,
    /// Category string from the closed `ProductCategory` set.
    pub category: &'static str,
    pub version: &'static str,
}

/// The external library requirements declared by one module.
pub fn module_external_requirements(
    module: &dyn Module,
) -> Vec<ExternalLibraryRequirement> {
    module.requires_external_libraries().to_vec()
}

/// One dependency-graph problem found by `validate_dependencies`.
/// The same shape also carries product-declaration problems from
/// `validate_product_declarations` (kind
/// `"invalid_product_declaration"`).
#[derive(Debug, Clone, Serialize)]
pub struct DependencyIssue {
    /// `"missing_dependency"`, `"circular_dependency"`, or
    /// `"invalid_product_declaration"`.
    pub kind: &'static str,
    /// Id of the module the problem belongs to.
    pub module: String,
    /// The required module id that is missing (missing-dependency
    /// issues only).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dependency: Option<String>,
    /// The modules on the cycle, in order, with the repeated module
    /// closing the loop (circular-dependency issues only).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub cycle: Vec<String>,
    /// Human-readable explanation.
    pub message: String,
}

/// Result of validating the registered module dependency graph.
#[derive(Debug, Clone, Serialize)]
pub struct DependencyReport {
    /// True when no issue was found.
    pub valid: bool,
    /// Every problem found, in deterministic (registration) order.
    pub issues: Vec<DependencyIssue>,
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
                dependencies: module_requires(module.as_ref()),
                requires_libraries: module.requires_libraries().to_vec(),
                requires_external_libraries: module_external_requirements(module.as_ref()),
                available_as_product: module.available_as_product(),
                product_category: module.product_category(),
            })
            .collect()
    }

    /// Summaries of the registered modules that declare themselves
    /// sellable as products, in registration order.
    pub fn sellable_modules(&self) -> Vec<ModuleInfo> {
        self.summary()
            .into_iter()
            .filter(|info| info.available_as_product)
            .collect()
    }

    /// Product offerings for the sellable modules: id, name,
    /// description, category, and version, in registration order.
    /// Derived from the registry in memory; no database row backs
    /// an offering. Callers should run
    /// `validate_product_declarations` first: an offering whose
    /// module declares no category carries an empty category string.
    pub fn product_offerings(&self) -> Vec<ModuleProductOffering> {
        self.list()
            .iter()
            .filter(|module| module.available_as_product())
            .map(|module| ModuleProductOffering {
                id: module.id(),
                name: module.name(),
                version: module.version(),
                description: module.description(),
                category: module.product_category().unwrap_or_default(),
            })
            .collect()
    }

    /// Validate the product declarations of the registered modules:
    /// a module that declares `available_as_product` must also
    /// declare a valid `product_category` (one of
    /// `ProductCategory::ALL`), and a module that declares a
    /// category must declare itself sellable. Reports every problem
    /// with kind `"invalid_product_declaration"` and never stops at
    /// the first one, following the `validate_dependencies` pattern.
    pub fn validate_product_declarations(&self) -> DependencyReport {
        let mut issues: Vec<DependencyIssue> = Vec::new();
        for module in self.list() {
            match (module.available_as_product(), module.product_category()) {
                (true, None) => issues.push(DependencyIssue {
                    kind: "invalid_product_declaration",
                    module: module.id().to_string(),
                    dependency: None,
                    cycle: Vec::new(),
                    message: format!(
                        "Module \"{}\" is available as a product but declares no product category",
                        module.id()
                    ),
                }),
                (true, Some(category)) if !ProductCategory::is_valid(category) => {
                    issues.push(DependencyIssue {
                        kind: "invalid_product_declaration",
                        module: module.id().to_string(),
                        dependency: None,
                        cycle: Vec::new(),
                        message: format!(
                            "Module \"{}\" declares unknown product category \"{category}\" (expected one of: {})",
                            module.id(),
                            ProductCategory::ALL.join(", ")
                        ),
                    })
                }
                (false, Some(category)) => issues.push(DependencyIssue {
                    kind: "invalid_product_declaration",
                    module: module.id().to_string(),
                    dependency: None,
                    cycle: Vec::new(),
                    message: format!(
                        "Module \"{}\" declares product category \"{category}\" but is not available as a product",
                        module.id()
                    ),
                }),
                _ => {}
            }
        }
        let valid = issues.is_empty();
        DependencyReport { valid, issues }
    }

    /// Merged requirements of one registered module, or an empty list
    /// when no module with that id is registered.
    pub fn requires_of(&self, id: &str) -> Vec<&'static str> {
        match self.get(id) {
            Some(module) => module_requires(module.as_ref()),
            None => Vec::new(),
        }
    }

    /// Version support declared by the module with `id`, or `All`
    /// when no module with that id is registered. Unknown owners in
    /// the route catalog therefore default to every version, which
    /// matches the registry behaviour before this method existed.
    pub fn route_version_support_of(&self, id: &str) -> RouteVersionSupport {
        self.get(id)
            .map(|module| module.route_version_support())
            .unwrap_or_default()
    }

    /// Ids of the registered modules that (transitively or directly)
    /// require `id`, sorted. Used to refuse disabling a module that
    /// other modules still depend on.
    pub fn dependents_of(&self, id: &str) -> Vec<String> {
        let mut dependents: Vec<String> = self
            .list()
            .iter()
            .filter(|module| {
                module.id() != id
                    && module_requires(module.as_ref())
                        .iter()
                        .any(|required| *required == id)
            })
            .map(|module| module.id().to_string())
            .collect();
        dependents.sort();
        dependents
    }

    /// Validate the dependency graph of the registered modules:
    /// every required id must be registered, and the graph must be
    /// acyclic. Missing dependencies and cycles are both reported;
    /// validation never stops at the first problem so the caller sees
    /// the whole picture at once.
    pub fn validate_dependencies(&self) -> DependencyReport {
        let modules = self.list();
        let registered: HashSet<&str> = modules.iter().map(|module| module.id()).collect();
        let mut issues: Vec<DependencyIssue> = Vec::new();
        for module in &modules {
            for required in module_requires(module.as_ref()) {
                if !registered.contains(required) {
                    issues.push(DependencyIssue {
                        kind: "missing_dependency",
                        module: module.id().to_string(),
                        dependency: Some(required.to_string()),
                        cycle: Vec::new(),
                        message: format!(
                            "Module \"{}\" requires module \"{}\", which is not registered",
                            module.id(),
                            required
                        ),
                    });
                }
            }
        }
        // Cycle detection: depth-first search with visiting state
        // (0 = unvisited, 1 = on the current stack, 2 = finished).
        // The stack records the path so a cycle can be reported in
        // full instead of just naming one edge.
        let by_id: HashMap<&str, &Arc<dyn Module>> = modules
            .iter()
            .map(|module| (module.id(), module))
            .collect();
        let mut state: HashMap<&str, u8> = HashMap::new();
        let mut stack: Vec<String> = Vec::new();
        for module in &modules {
            visit_for_cycles(module.id(), &by_id, &registered, &mut state, &mut stack, &mut issues);
        }
        let valid = issues.is_empty();
        DependencyReport { valid, issues }
    }

    /// Transitive requirements of `id` in activation order
    /// (deepest dependency first, `id` itself excluded), so a caller
    /// can enable them one by one before enabling `id`. Fails with a
    /// 409 conflict when a required module is missing or when the
    /// requirements form a cycle; the error metadata carries the
    /// missing ids or the cycle so the caller can report them.
    pub fn transitive_requires_ordered(&self, id: &str) -> UnifiedResult<Vec<String>> {
        let modules = self.list();
        let by_id: HashMap<&str, &Arc<dyn Module>> = modules
            .iter()
            .map(|module| (module.id(), module))
            .collect();
        let root = by_id.get(id).ok_or_else(|| {
            UnifiedError::not_found(format!("Module \"{id}\" not found"))
        })?;
        let mut ordered: Vec<String> = Vec::new();
        let mut visiting: HashSet<String> = HashSet::new();
        let mut done: HashSet<String> = HashSet::new();
        let mut stack: Vec<String> = Vec::new();
        for required in module_requires(root.as_ref()) {
            visit_requirement(
                required,
                &by_id,
                &mut visiting,
                &mut done,
                &mut stack,
                &mut ordered,
            )?;
        }
        Ok(ordered)
    }

    /// Run `init` on every registered module, in registration order.
    ///
    /// Before any module initializes, the product declarations are
    /// validated (`validate_product_declarations`): a module that
    /// declares itself sellable must declare a valid product
    /// category, and a module that declares a category must declare
    /// itself sellable. An invalid declaration fails startup
    /// loudly instead of silently listing a broken offering in
    /// `product_offerings` / `sellable_modules`.
    pub fn init_all(&self, ctx: &AppContext) -> UnifiedResult<()> {
        let report = self.validate_product_declarations();
        if !report.valid {
            let details: Vec<String> = report
                .issues
                .iter()
                .map(|issue| issue.message.clone())
                .collect();
            return Err(UnifiedError::conflict(format!(
                "Invalid module product declarations: {}",
                details.join("; ")
            )));
        }
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

/// Depth-first cycle check for one module. `state` maps module id to
/// 0 (unvisited), 1 (on the current stack) or 2 (finished); `stack`
/// holds the current DFS path. Finding a required id that is already
/// on the stack means the stack segment from that id to the top plus
/// the id again is a cycle.
fn visit_for_cycles(
    id: &str,
    by_id: &HashMap<&str, &Arc<dyn Module>>,
    registered: &HashSet<&str>,
    state: &mut HashMap<&str, u8>,
    stack: &mut Vec<String>,
    issues: &mut Vec<DependencyIssue>,
) {
    if state.get(id).copied().unwrap_or(0) != 0 {
        return;
    }
    state.insert(id, 1);
    stack.push(id.to_string());
    if let Some(module) = by_id.get(id) {
        for required in module_requires(module.as_ref()) {
            if !registered.contains(required) {
                continue;
            }
            if state.get(required).copied().unwrap_or(0) == 1 {
                let start = stack
                    .iter()
                    .position(|entry| entry == required)
                    .unwrap_or(0);
                let mut cycle: Vec<String> = stack[start..].to_vec();
                cycle.push(required.to_string());
                issues.push(DependencyIssue {
                    kind: "circular_dependency",
                    module: id.to_string(),
                    dependency: Some(required.to_string()),
                    cycle: cycle.clone(),
                    message: format!(
                        "Circular dependency detected: {}",
                        cycle.join(" -> ")
                    ),
                });
            } else if state.get(required).copied().unwrap_or(0) == 0 {
                visit_for_cycles(required, by_id, registered, state, stack, issues);
            }
        }
    }
    stack.pop();
    state.insert(id, 2);
}

/// Depth-first visit of one requirement for
/// `transitive_requires_ordered`. Appends the requirement to `ordered`
/// only after all of its own requirements, so the result is in
/// activation order. `visiting` holds the current path (cycle
/// detection) and `done` the modules already ordered.
fn visit_requirement(
    id: &str,
    by_id: &HashMap<&str, &Arc<dyn Module>>,
    visiting: &mut HashSet<String>,
    done: &mut HashSet<String>,
    stack: &mut Vec<String>,
    ordered: &mut Vec<String>,
) -> UnifiedResult<()> {
    if done.contains(id) {
        return Ok(());
    }
    if visiting.contains(id) {
        let start = stack.iter().position(|entry| entry == id).unwrap_or(0);
        let mut cycle: Vec<String> = stack[start..].to_vec();
        cycle.push(id.to_string());
        return Err(UnifiedError::conflict(format!(
            "Circular dependency detected: {}",
            cycle.join(" -> ")
        ))
        .with_metadata("cycle", serde_json::json!(cycle)));
    }
    let Some(module) = by_id.get(id) else {
        return Err(UnifiedError::conflict(format!(
            "Cannot enable module: required module \"{id}\" is not registered"
        ))
        .with_metadata("missing_required", serde_json::json!([id])));
    };
    visiting.insert(id.to_string());
    stack.push(id.to_string());
    for required in module_requires(module.as_ref()) {
        visit_requirement(required, by_id, visiting, done, stack, ordered)?;
    }
    stack.pop();
    visiting.remove(id);
    done.insert(id.to_string());
    ordered.push(id.to_string());
    Ok(())
}
