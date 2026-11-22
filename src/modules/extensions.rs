// Extensions module — the Modules / Extensions Manager.
//
// Lists every registered module with its enabled state and dependency
// information, enables and disables modules, validates the module
// dependency graph, and lists the hook events registered on the bus.
// Disabling a core module is refused (409). The enabled state is kept in
// memory; it is advisory — it gates the state reported by this API and
// the `module.enabled` / `module.disabled` events, but it does not
// unregister already-mounted routes. Full dynamic unload (route rebuild
// without a restart) is a follow-up; see the note in `disable_with_check`.
//
// Dependency behaviour (see `crate::core::modules`):
//   - Enabling a module auto-activates its required modules first,
//     transitively, in dependency order. Each auto-activated module
//     gets a `module.enabled` event plus a `module.auto_activated`
//     event, and the enable response lists them.
//   - Disabling a module that other enabled modules depend on is
//     refused with a 409 that lists the dependents, unless the caller
//     passes `?force=true`, in which case the dependents are disabled
//     too (dependents first) and listed in the response.
//   - `GET /extensions/{id}/requirements` reports the full requirement
//     check for one module before it is enabled, and
//     `GET /extensions/dependencies` reports the whole-graph validation.

use crate::core::context::AppContext;
use crate::core::error::UnifiedError;
use crate::core::hooks::events;
use crate::core::json::{error_response, success_response};
use crate::core::modules::{
    module_requires, DependencyReport, ExternalLibraryRequirement, Module, ModuleRegistry,
    RouteInfo, MODULE_AUTO_ACTIVATED_EVENT,
};
use crate::core::pagination::{PageResult, PaginationParams};
use crate::modules::service_monitor::{
    aggregate_external_requirements, latest_library_results, library_notice_message,
    run_library_checks, LibraryCheckResult, LibraryRecheckRequest,
};
use actix_web::{web, HttpResponse};
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::sync::Arc;

pub struct ExtensionsModule {
    /// Explicit enabled state per module id. Absent means enabled.
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

fn is_enabled(state: &EnabledState, id: &str) -> bool {
    state.get(id).map(|entry| *entry.value()).unwrap_or(true)
}

impl Default for ExtensionsModule {
    fn default() -> Self {
        Self::new()
    }
}

pub fn register(registry: &ModuleRegistry) {
    registry.register(Arc::new(ExtensionsModule::new()));
}

/// One required module and its current state, as reported by the
/// module-list and requirement-check endpoints.
#[derive(Debug, Clone, Serialize)]
pub struct RequiredModuleInfo {
    pub id: String,
    /// Display name when the required module is registered.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// `"enabled"`, `"disabled"`, or `"missing"`.
    pub status: String,
    /// False when no module with this id is registered.
    pub exists: bool,
}

/// One admin-UI warning about a module's requirements. The `code` and
/// the `required_modules` / `will_auto_activate` lists are shaped so
/// the admin UI can render a notice or a popup before enabling, and
/// can tell the operator exactly what enabling would auto-activate.
#[derive(Debug, Clone, Serialize)]
pub struct DependencyWarning {
    /// `"missing_dependency"` or `"disabled_dependency"`.
    pub code: String,
    pub message: String,
    pub required_modules: Vec<String>,
    pub will_auto_activate: Vec<String>,
}

/// One admin-UI notice about a required external library, built from
/// the Service Monitor's latest check results. Only failing results
/// produce a notice; a library that is installed and new enough, or
/// one that has never been checked, produces none. The `message` is
/// the human-readable form, for example
/// `"ImageMagick 7+ required, 6.9.11 found"`.
#[derive(Debug, Clone, Serialize)]
pub struct LibraryNotice {
    pub library: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub required_version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub found_version: Option<String>,
    /// `"missing"` or `"outdated"`.
    pub status: String,
    pub message: String,
}

/// Notices for one module from the latest local library check
/// results. The `results` argument is the shared cache read, so
/// every caller sees the same data as the Service Monitor endpoints.
fn library_notices_for(
    module_id: &str,
    requirements: &[ExternalLibraryRequirement],
    results: &[LibraryCheckResult],
) -> Vec<LibraryNotice> {
    let mut notices: Vec<LibraryNotice> = Vec::new();
    for requirement in requirements {
        let Some(result) = results
            .iter()
            .find(|result| result.library.eq_ignore_ascii_case(requirement.name))
            .filter(|result| result.required_by.iter().any(|id| id == module_id))
        else {
            continue;
        };
        if let Some(message) = library_notice_message(result) {
            notices.push(LibraryNotice {
                library: result.library.clone(),
                required_version: result.required_version.clone(),
                found_version: result.found_version.clone(),
                status: result.status.clone(),
                message,
            });
        }
    }
    notices.sort_by(|a, b| a.library.cmp(&b.library));
    notices
}

/// Module summary plus its enabled state and dependency information.
#[derive(Debug, Clone, Serialize)]
pub struct ExtensionInfo {
    pub id: String,
    pub name: String,
    pub version: String,
    pub description: String,
    pub is_core: bool,
    /// Merged required module ids (`requires` plus legacy
    /// `dependencies`).
    pub dependencies: Vec<String>,
    /// Each required module with its current status.
    pub requires: Vec<RequiredModuleInfo>,
    pub requires_libraries: Vec<String>,
    /// Declared external system libraries / binaries.
    pub requires_external_libraries: Vec<ExternalLibraryRequirement>,
    /// Admin-UI notices for required external libraries that the
    /// latest check found missing or outdated (empty when everything
    /// required is installed and new enough, or not yet checked).
    pub library_notices: Vec<LibraryNotice>,
    /// Required module ids that are not registered at all.
    pub missing_required: Vec<String>,
    /// Admin-UI warnings for this module's requirements.
    pub warnings: Vec<DependencyWarning>,
    pub enabled: bool,
}

/// Response of a successful enable: the module plus every module
/// that was auto-activated as one of its requirements.
#[derive(Debug, Clone, Serialize)]
pub struct EnableResponse {
    pub module: ExtensionInfo,
    pub auto_activated: Vec<String>,
}

/// Response of a successful disable: the module plus every dependent
/// module that was disabled along with it (`force=true` only).
#[derive(Debug, Clone, Serialize)]
pub struct DisableResponse {
    pub module: ExtensionInfo,
    pub disabled_dependents: Vec<String>,
}

/// Full requirement check for one module, before enabling it.
#[derive(Debug, Clone, Serialize)]
pub struct RequirementCheck {
    pub id: String,
    pub name: String,
    /// `"ok"` when every requirement is registered and enabled,
    /// `"missing"` when at least one requirement is not registered,
    /// `"disabled"` when requirements exist but at least one is
    /// currently disabled.
    pub status: String,
    pub requires: Vec<RequiredModuleInfo>,
    pub requires_libraries: Vec<String>,
    /// Declared external system libraries / binaries.
    pub requires_external_libraries: Vec<ExternalLibraryRequirement>,
    /// Admin-UI notices for required external libraries that the
    /// latest check found missing or outdated.
    pub library_notices: Vec<LibraryNotice>,
    pub missing_required: Vec<String>,
    pub disabled_required: Vec<String>,
    /// Modules enabling this module would auto-activate, in
    /// activation order (deepest dependency first).
    pub will_auto_activate: Vec<String>,
    pub warnings: Vec<DependencyWarning>,
}

/// One hook event and the modules that handle it.
#[derive(Debug, Clone, Serialize)]
pub struct HookEventInfo {
    pub event: String,
    pub owners: Vec<String>,
    pub handler_count: usize,
}

fn required_info(
    registry: &ModuleRegistry,
    enabled_state: &EnabledState,
    required_id: &str,
) -> RequiredModuleInfo {
    match registry.get(required_id) {
        Some(module) => RequiredModuleInfo {
            id: required_id.to_string(),
            name: Some(module.name().to_string()),
            status: if is_enabled(enabled_state, required_id) {
                "enabled".to_string()
            } else {
                "disabled".to_string()
            },
            exists: true,
        },
        None => RequiredModuleInfo {
            id: required_id.to_string(),
            name: None,
            status: "missing".to_string(),
            exists: false,
        },
    }
}

fn dependency_warnings(
    requires: &[RequiredModuleInfo],
    will_auto_activate: &[String],
) -> Vec<DependencyWarning> {
    let missing: Vec<String> = requires
        .iter()
        .filter(|entry| !entry.exists)
        .map(|entry| entry.id.clone())
        .collect();
    let disabled: Vec<String> = requires
        .iter()
        .filter(|entry| entry.exists && entry.status == "disabled")
        .map(|entry| entry.id.clone())
        .collect();
    let mut warnings = Vec::new();
    if !missing.is_empty() {
        warnings.push(DependencyWarning {
            code: "missing_dependency".to_string(),
            message: format!(
                "Required module(s) not registered: {}. The module cannot be enabled until they are installed.",
                missing.join(", ")
            ),
            required_modules: missing,
            will_auto_activate: Vec::new(),
        });
    }
    if !disabled.is_empty() {
        warnings.push(DependencyWarning {
            code: "disabled_dependency".to_string(),
            message: format!(
                "Required module(s) currently disabled: {}. Enabling this module will auto-activate them.",
                disabled.join(", ")
            ),
            required_modules: disabled,
            will_auto_activate: will_auto_activate.to_vec(),
        });
    }
    warnings
}

fn build_extension_info(
    registry: &ModuleRegistry,
    enabled_state: &EnabledState,
    module: &Arc<dyn Module>,
) -> ExtensionInfo {
    let requires: Vec<RequiredModuleInfo> = module_requires(module.as_ref())
        .iter()
        .map(|required| required_info(registry, enabled_state, required))
        .collect();
    let missing_required: Vec<String> = requires
        .iter()
        .filter(|entry| !entry.exists)
        .map(|entry| entry.id.clone())
        .collect();
    // Modules that enabling this module would auto-activate: the
    // transitive requirements that are currently disabled, in
    // activation order. A broken graph (missing module or cycle)
    // yields an empty list here; the dedicated requirements endpoint
    // reports the exact problem instead.
    let will_auto_activate: Vec<String> = registry
        .transitive_requires_ordered(module.id())
        .unwrap_or_default()
        .into_iter()
        .filter(|id| !is_enabled(enabled_state, id))
        .collect();
    let warnings = dependency_warnings(&requires, &will_auto_activate);
    let requires_external_libraries: Vec<ExternalLibraryRequirement> =
        module.requires_external_libraries().to_vec();
    let library_notices = library_notices_for(
        module.id(),
        &requires_external_libraries,
        &latest_library_results(),
    );
    ExtensionInfo {
        id: module.id().to_string(),
        name: module.name().to_string(),
        version: module.version().to_string(),
        description: module.description().to_string(),
        is_core: module.is_core(),
        dependencies: module_requires(module.as_ref())
            .iter()
            .map(|entry| entry.to_string())
            .collect(),
        requires,
        requires_libraries: module
            .requires_libraries()
            .iter()
            .map(|entry| entry.to_string())
            .collect(),
        requires_external_libraries,
        library_notices,
        missing_required,
        warnings,
        enabled: is_enabled(enabled_state, module.id()),
    }
}

/// Enable `id` together with its transitive requirements.
/// Requirements are enabled first, in dependency order; every
/// auto-activated module gets a `module.enabled` event plus a
/// `module.auto_activated` event carrying `auto_activated_by`. The
/// target module itself only gets `module.enabled` when its state
/// actually changes from disabled to enabled.
fn enable_with_dependencies(
    ctx: &AppContext,
    enabled_state: &EnabledState,
    id: &str,
) -> Result<EnableResponse, UnifiedError> {
    let found = ctx
        .modules
        .get(id)
        .ok_or_else(|| UnifiedError::not_found(format!("Module \"{id}\" not found")))?;
    // Fails with 409 when a requirement is missing or the graph has a
    // cycle; the metadata carries the missing ids or the cycle.
    let ordered = ctx.modules.transitive_requires_ordered(id)?;
    let mut auto_activated: Vec<String> = Vec::new();
    for required in &ordered {
        if !is_enabled(enabled_state, required) {
            enabled_state.insert(required.clone(), true);
            auto_activated.push(required.clone());
            ctx.hooks.emit(
                events::MODULE_ENABLED,
                serde_json::json!({
                    "id": required,
                    "auto_activated": true,
                    "auto_activated_by": id,
                }),
            );
            ctx.hooks.emit(
                MODULE_AUTO_ACTIVATED_EVENT,
                serde_json::json!({
                    "id": required,
                    "auto_activated_by": id,
                }),
            );
        }
    }
    let was_enabled = is_enabled(enabled_state, id);
    enabled_state.insert(id.to_string(), true);
    if !was_enabled {
        ctx.hooks.emit(
            events::MODULE_ENABLED,
            serde_json::json!({ "id": id }),
        );
    }
    Ok(EnableResponse {
        module: build_extension_info(&ctx.modules, enabled_state, &found),
        auto_activated,
    })
}

/// Disable `id`. When other enabled modules depend on it the disable
/// is refused with a 409 listing the dependents, unless `force` is
/// true: then the dependents are disabled first (dependents before
/// their own dependents, transitively) and listed in the response.
/// Disabling only flips the reported state and emits the events;
/// routes mounted at startup stay mounted until the process restarts.
/// Full dynamic unload needs a route rebuild (server restart or a
/// reloadable route table) and is a follow-up.
fn disable_with_check(
    ctx: &AppContext,
    enabled_state: &EnabledState,
    id: &str,
    force: bool,
) -> Result<DisableResponse, UnifiedError> {
    let found = ctx
        .modules
        .get(id)
        .ok_or_else(|| UnifiedError::not_found(format!("Module \"{id}\" not found")))?;
    if found.is_core() {
        return Err(UnifiedError::conflict(format!(
            "Core module \"{id}\" cannot be disabled"
        )));
    }
    // Enabled direct dependents block the disable; transitive
    // dependents are covered because each of them blocks its own
    // dependency in turn when force-disabling.
    let blocking: Vec<String> = ctx
        .modules
        .dependents_of(id)
        .into_iter()
        .filter(|dependent| is_enabled(enabled_state, dependent))
        .collect();
    if !blocking.is_empty() && !force {
        return Err(UnifiedError::conflict(format!(
            "Module \"{id}\" cannot be disabled: enabled module(s) depend on it: {}",
            blocking.join(", ")
        ))
        .with_metadata("dependents", serde_json::json!(blocking))
        .with_metadata(
            "hint",
            serde_json::json!("Retry with ?force=true to disable the dependent modules too."),
        ));
    }
    let mut disabled_dependents: Vec<String> = Vec::new();
    if force {
        // Repeatedly disable any still-enabled dependent until none
        // remain. Each pass disables the current dependents first, so
        // a dependent is always disabled before the modules it
        // depends on. Bounded by the module count plus one, so a
        // broken graph cannot loop forever.
        let limit = ctx.modules.ids().len() + 1;
        for _ in 0..limit {
            let remaining: Vec<String> = ctx
                .modules
                .dependents_of(id)
                .into_iter()
                .filter(|dependent| {
                    is_enabled(enabled_state, dependent)
                        && !disabled_dependents.contains(dependent)
                })
                .collect();
            if remaining.is_empty() {
                break;
            }
            // Also pick up transitive dependents: any enabled module
            // depending on a module disabled in this round.
            let mut round: Vec<String> = remaining;
            let mut index = 0;
            while index < round.len() {
                let current = round[index].clone();
                index += 1;
                for transitive in ctx.modules.dependents_of(&current) {
                    if is_enabled(enabled_state, &transitive)
                        && !round.contains(&transitive)
                        && !disabled_dependents.contains(&transitive)
                    {
                        round.push(transitive);
                    }
                }
            }
            for dependent in round {
                enabled_state.insert(dependent.clone(), false);
                disabled_dependents.push(dependent.clone());
                ctx.hooks.emit(
                    events::MODULE_DISABLED,
                    serde_json::json!({
                        "id": dependent,
                        "disabled_by_force": true,
                        "disabled_for": id,
                    }),
                );
            }
        }
    }
    enabled_state.insert(id.to_string(), false);
    ctx.hooks.emit(
        events::MODULE_DISABLED,
        serde_json::json!({ "id": id }),
    );
    Ok(DisableResponse {
        module: build_extension_info(&ctx.modules, enabled_state, &found),
        disabled_dependents,
    })
}

fn build_requirement_check(
    registry: &ModuleRegistry,
    enabled_state: &EnabledState,
    module: &Arc<dyn Module>,
) -> RequirementCheck {
    let requires: Vec<RequiredModuleInfo> = module_requires(module.as_ref())
        .iter()
        .map(|required| required_info(registry, enabled_state, required))
        .collect();
    let missing_required: Vec<String> = requires
        .iter()
        .filter(|entry| !entry.exists)
        .map(|entry| entry.id.clone())
        .collect();
    let disabled_required: Vec<String> = requires
        .iter()
        .filter(|entry| entry.exists && entry.status == "disabled")
        .map(|entry| entry.id.clone())
        .collect();
    let will_auto_activate: Vec<String> = registry
        .transitive_requires_ordered(module.id())
        .unwrap_or_default()
        .into_iter()
        .filter(|id| !is_enabled(enabled_state, id))
        .collect();
    let status = if !missing_required.is_empty() {
        "missing".to_string()
    } else if !disabled_required.is_empty() {
        "disabled".to_string()
    } else {
        "ok".to_string()
    };
    let warnings = dependency_warnings(&requires, &will_auto_activate);
    let requires_external_libraries: Vec<ExternalLibraryRequirement> =
        module.requires_external_libraries().to_vec();
    let library_notices = library_notices_for(
        module.id(),
        &requires_external_libraries,
        &latest_library_results(),
    );
    RequirementCheck {
        id: module.id().to_string(),
        name: module.name().to_string(),
        status,
        requires,
        requires_libraries: module
            .requires_libraries()
            .iter()
            .map(|entry| entry.to_string())
            .collect(),
        requires_external_libraries,
        library_notices,
        missing_required,
        disabled_required,
        will_auto_activate,
        warnings,
    }
}

fn list_extensions(
    ctx: web::Data<Arc<AppContext>>,
    enabled_state: web::Data<EnabledState>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let pagination = query.normalized();
    let search = pagination.search.clone().map(|s| s.to_lowercase());
    let enabled_filter = pagination
        .filters
        .get("enabled")
        .and_then(|value| value.parse::<bool>().ok());
    let core_filter = pagination
        .filters
        .get("is_core")
        .and_then(|value| value.parse::<bool>().ok());
    let warnings_filter = pagination
        .filters
        .get("has_warnings")
        .and_then(|value| value.parse::<bool>().ok());
    let library_warnings_filter = pagination
        .filters
        .get("has_library_warnings")
        .and_then(|value| value.parse::<bool>().ok());

    let mut items: Vec<ExtensionInfo> = ctx
        .modules
        .list()
        .iter()
        .map(|module| build_extension_info(&ctx.modules, &enabled_state, module))
        .filter(|info| {
            if let Some(search) = &search {
                let haystack = format!("{} {} {}", info.id, info.name, info.description)
                    .to_lowercase();
                if !haystack.contains(search.as_str()) {
                    return false;
                }
            }
            if let Some(enabled) = enabled_filter {
                if info.enabled != enabled {
                    return false;
                }
            }
            if let Some(is_core) = core_filter {
                if info.is_core != is_core {
                    return false;
                }
            }
            if let Some(has_warnings) = warnings_filter {
                if info.warnings.is_empty() == has_warnings {
                    return false;
                }
            }
            if let Some(has_library_warnings) = library_warnings_filter {
                if info.library_notices.is_empty() == has_library_warnings {
                    return false;
                }
            }
            true
        })
        .collect();
    items.sort_by(|a, b| a.id.cmp(b.id));
    let total = items.len() as u64;
    let start = ((pagination.page - 1) * pagination.per_page) as usize;
    let end = (start + pagination.per_page as usize).min(items.len());
    let page_items = if start < items.len() {
        items[start..end].to_vec()
    } else {
        Vec::new()
    };
    success_response(PageResult::new(page_items, total, &pagination).to_json(), 200)
}

fn get_extension(
    ctx: web::Data<Arc<AppContext>>,
    enabled_state: web::Data<EnabledState>,
    id: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let key = id.into_inner();
    match ctx.modules.get(&key) {
        Some(found) => success_response(
            build_extension_info(&ctx.modules, &enabled_state, &found),
            200,
        ),
        None => error_response(
            &UnifiedError::not_found(format!("Module \"{key}\" not found")),
            debug,
        ),
    }
}

fn get_requirements(
    ctx: web::Data<Arc<AppContext>>,
    enabled_state: web::Data<EnabledState>,
    id: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let key = id.into_inner();
    match ctx.modules.get(&key) {
        Some(found) => success_response(
            build_requirement_check(&ctx.modules, &enabled_state, &found),
            200,
        ),
        None => error_response(
            &UnifiedError::not_found(format!("Module \"{key}\" not found")),
            debug,
        ),
    }
}

fn list_dependencies(ctx: web::Data<Arc<AppContext>>) -> HttpResponse {
    let report: DependencyReport = ctx.modules.validate_dependencies();
    success_response(report, 200)
}

/// One external library requirement across all modules, with the
/// latest check result for it. `status` is `"ok"`, `"missing"`,
/// `"outdated"`, or `"unchecked"` (no check has run yet).
#[derive(Debug, Clone, Serialize)]
pub struct LibraryRequirementInfo {
    pub library: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub required_version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub found_version: Option<String>,
    pub status: String,
    pub required_by: Vec<String>,
    pub reasons: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub checked_at: Option<chrono::DateTime<chrono::Utc>>,
}

/// List every external library requirement declared by any module,
/// with the latest local check result per library (paginated, with
/// `?search=` and `?status=` filters). This is the module-facing
/// view of the same check results the Service Monitor owns; the
/// results themselves live in the Service Monitor's cache and are
/// read, not copied, so the two endpoints can never disagree.
fn list_library_requirements(
    ctx: web::Data<Arc<AppContext>>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let pagination = query.normalized();
    let search = pagination.search.clone().map(|s| s.to_lowercase());
    let status_filter = pagination.filters.get("status").cloned();
    let results = latest_library_results();
    let mut items: Vec<LibraryRequirementInfo> =
        aggregate_external_requirements(&ctx.modules)
            .into_iter()
            .map(|requirement| {
                let found = results.iter().find(|result| {
                    result.library.eq_ignore_ascii_case(&requirement.name)
                });
                let (status, found_version, message, checked_at) = match found {
                    Some(result) => (
                        result.status.clone(),
                        result.found_version.clone(),
                        library_notice_message(result),
                        Some(result.checked_at),
                    ),
                    None => ("unchecked".to_string(), None, None, None),
                };
                LibraryRequirementInfo {
                    library: requirement.name,
                    required_version: requirement.minimum_version,
                    found_version,
                    status,
                    required_by: requirement.required_by,
                    reasons: requirement.reasons,
                    message,
                    checked_at,
                }
            })
            .filter(|info| {
                if let Some(search) = &search {
                    let haystack = format!(
                        "{} {} {}",
                        info.library,
                        info.required_by.join(" "),
                        info.reasons.join(" ")
                    )
                    .to_lowercase();
                    if !haystack.contains(search.as_str()) {
                        return false;
                    }
                }
                if let Some(status) = &status_filter {
                    if &info.status != status {
                        return false;
                    }
                }
                true
            })
            .collect();
    match pagination.sort.as_deref().unwrap_or("library") {
        "status" => items.sort_by(|a, b| {
            a.status
                .cmp(&b.status)
                .then_with(|| a.library.to_lowercase().cmp(&b.library.to_lowercase()))
        }),
        _ => items.sort_by(|a, b| a.library.to_lowercase().cmp(&b.library.to_lowercase())),
    }
    if pagination.order == "desc" {
        items.reverse();
    }
    let total = items.len() as u64;
    let start = ((pagination.page - 1) * pagination.per_page) as usize;
    let end = (start + pagination.per_page as usize).min(items.len());
    let page_items = if start < items.len() {
        items[start..end].to_vec()
    } else {
        Vec::new()
    };
    success_response(PageResult::new(page_items, total, &pagination).to_json(), 200)
}

/// Re-check action for the module list: runs the Service Monitor's
/// library checks (same function, same report shape as
/// `POST /api/v1/service-monitor/libraries/check`) so the module
/// list's notices refresh without a second implementation.
async fn recheck_library_requirements(
    ctx: web::Data<Arc<AppContext>>,
    body: web::Json<LibraryRecheckRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let request = body.into_inner();
    let ctx_clone = Arc::clone(&ctx);
    let checked = web::block(move || run_library_checks(&ctx_clone, &request)).await;
    match checked {
        Ok(Ok(reports)) => success_response(reports, 200),
        Ok(Err(error)) => error_response(&error, debug),
        Err(error) => error_response(
            &UnifiedError::internal(format!("Library check failed: {error}")),
            debug,
        ),
    }
}

fn enable_extension(
    ctx: web::Data<Arc<AppContext>>,
    enabled_state: web::Data<EnabledState>,
    id: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let key = id.into_inner();
    match enable_with_dependencies(&ctx, &enabled_state, &key) {
        Ok(response) => success_response(response, 200),
        Err(error) => error_response(&error, debug),
    }
}

#[derive(Debug, Deserialize)]
struct ForceQuery {
    #[serde(default)]
    force: Option<bool>,
}

fn disable_extension(
    ctx: web::Data<Arc<AppContext>>,
    enabled_state: web::Data<EnabledState>,
    id: web::Path<String>,
    query: web::Query<ForceQuery>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let key = id.into_inner();
    let force = query.force.unwrap_or(false);
    match disable_with_check(&ctx, &enabled_state, &key, force) {
        Ok(response) => success_response(response, 200),
        Err(error) => error_response(&error, debug),
    }
}

fn list_hooks(
    ctx: web::Data<Arc<AppContext>>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let pagination = query.normalized();
    let search = pagination.search.clone().map(|s| s.to_lowercase());

    let mut by_event: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for (event, owner) in ctx.hooks.registrations() {
        by_event.entry(event).or_default().push(owner);
    }
    let mut events: Vec<HookEventInfo> = by_event
        .into_iter()
        .map(|(event, owners)| {
            let handler_count = owners.len();
            let mut owners = owners;
            owners.sort();
            owners.dedup();
            HookEventInfo {
                event,
                owners,
                handler_count,
            }
        })
        .filter(|info| {
            if let Some(search) = &search {
                info.event.to_lowercase().contains(search.as_str())
            } else {
                true
            }
        })
        .collect();
    events.sort_by(|a, b| a.event.cmp(&b.event));
    let total = events.len() as u64;
    let start = ((pagination.page - 1) * pagination.per_page) as usize;
    let end = (start + pagination.per_page as usize).min(events.len());
    let items = if start < events.len() {
        events[start..end].to_vec()
    } else {
        Vec::new()
    };
    success_response(PageResult::new(items, total, &pagination).to_json(), 200)
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

    fn init(&self, ctx: &AppContext) -> crate::core::error::UnifiedResult<()> {
        // Startup dependency-graph validation: log every problem so a
        // misconfigured registration is visible in the server log even
        // before anyone calls GET /extensions/dependencies. Startup
        // itself is not failed here — a missing optional module must
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
            RouteInfo { method: "GET", path: "/api/v1/extensions/hooks", owner: "extensions", description: "List hooks" },
            RouteInfo { method: "GET", path: "/api/v1/extensions/dependencies", owner: "extensions", description: "List dependencies" },
            RouteInfo { method: "GET", path: "/api/v1/extensions", owner: "extensions", description: "List extensions" },
            RouteInfo { method: "GET", path: "/api/v1/extensions/{id}", owner: "extensions", description: "Get extension" },
            RouteInfo { method: "GET", path: "/api/v1/extensions/libraries", owner: "extensions", description: "List external library requirements with the latest check result per library (paginated, filter by status)." },
            RouteInfo { method: "POST", path: "/api/v1/extensions/libraries/check", owner: "extensions", description: "Re-check external libraries; same reports as POST /api/v1/service-monitor/libraries/check." },
            RouteInfo { method: "GET", path: "/api/v1/extensions/{id}/requirements", owner: "extensions", description: "Get requirements" },
            RouteInfo { method: "POST", path: "/api/v1/extensions/{id}/enable", owner: "extensions", description: "Enable extension" },
            RouteInfo { method: "POST", path: "/api/v1/extensions/{id}/disable", owner: "extensions", description: "Disable extension" },
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