//! extensions - service.

use unified_core::context::AppContext;
use unified_core::error::UnifiedError;
use unified_core::hooks::events;
use unified_core::modules::{
    module_requires,
    ExternalLibraryRequirement,
    Module,
    ModuleRegistry,
    MODULE_AUTO_ACTIVATED_EVENT,
};
use crate::modules::service_monitor::latest_library_results;
use std::sync::Arc;
use super::module::*;
use super::types::*;

pub(crate) fn required_info(
    registry: &ModuleRegistry,
    enabled_state: &EnabledState,
    required_id: &str,
) -> RequiredModuleInfo {
    match registry.get(required_id) {
        Some(module) => RequiredModuleInfo {
            id: required_id.to_string(),
            name: Some(module.name().to_string()),
            status: if is_enabled(enabled_state, registry, required_id) {
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

pub(crate) fn dependency_warnings(
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

pub(crate) fn build_extension_info(
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
        .filter(|id| !is_enabled(enabled_state, registry, id))
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
        enabled: is_enabled(enabled_state, registry, module.id()),
    }
}

/// Enable `id` together with its transitive requirements.
/// Requirements are enabled first, in dependency order; every
/// auto-activated module gets a `module.enabled` event plus a
/// `module.auto_activated` event carrying `auto_activated_by`. The
/// target module itself only gets `module.enabled` when its state
/// actually changes from disabled to enabled.
pub(crate) fn enable_with_dependencies(
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
        if !is_enabled(enabled_state, &ctx.modules, required) {
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
    let was_enabled = is_enabled(enabled_state, &ctx.modules, id);
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
///
/// KNOWN LIMITATION -- disabling is state-only, not a live unload:
/// disabling flips the reported state and emits the events, but the
/// module's routes were mounted at startup (see `configure_routes`)
/// and stay mounted until the process restarts, so a disabled module
/// keeps serving requests in-process until then. Do not rely on
/// disable alone to stop a module serving traffic: restart the
/// process after disabling (or block the module at the gateway) when
/// it must stop serving immediately. A successful disable response
/// therefore means "recorded as disabled", not "no longer serving".
///
/// Follow-up (deliberately not attempted here): a reloadable route
/// table -- rebuild the Actix scope from the registry's enabled set
/// on enable/disable, so a disable unmounts the module's routes
/// without a restart. That needs a route-table rebuild in the server
/// setup, outside this module's `configure_routes`, plus a decision
/// on in-flight requests and on state owned by the unmounted module.
pub(crate) fn disable_with_check(
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
        .filter(|dependent| is_enabled(enabled_state, &ctx.modules, dependent))
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
                    is_enabled(enabled_state, &ctx.modules, dependent)
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
                    if is_enabled(enabled_state, &ctx.modules, &transitive)
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

pub(crate) fn build_requirement_check(
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
        .filter(|id| !is_enabled(enabled_state, registry, id))
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

#[cfg(test)]
mod tests {
    use super::*;
    use unified_core::config::AppConfig;
    use unified_core::context::AppContext;
    use unified_core::hooks::HookBus;
    use unified_core::modules::{Module, ModuleRegistry};
    use dashmap::DashMap;
    use std::sync::Arc;

    struct DummyModule {
        requires: &'static [&'static str],
    }

    impl Module for DummyModule {
        fn id(&self) -> &'static str {
            "dummy"
        }

        fn name(&self) -> &'static str {
            "Dummy"
        }

        fn description(&self) -> &'static str {
            "Test dummy module"
        }

        fn requires(&self) -> &[&'static str] {
            self.requires
        }
    }

    fn fresh_state() -> EnabledState {
        Arc::new(DashMap::new())
    }

    fn registry_with(requires: &'static [&'static str]) -> (ModuleRegistry, Arc<dyn Module>) {
        let registry = ModuleRegistry::new();
        let module: Arc<dyn Module> = Arc::new(DummyModule { requires });
        registry.register(Arc::clone(&module));
        (registry, module)
    }

    fn required(id: &str, status: &str, exists: bool) -> RequiredModuleInfo {
        RequiredModuleInfo {
            id: id.to_string(),
            name: None,
            status: status.to_string(),
            exists,
        }
    }

    #[test]
    fn dependency_warnings_flags_missing_modules() {
        let warnings = dependency_warnings(&[required("ghost", "missing", false)], &[]);
        assert_eq!(warnings.len(), 1);
        assert_eq!(warnings[0].code, "missing_dependency");
        assert_eq!(warnings[0].required_modules, vec!["ghost".to_string()]);
    }

    #[test]
    fn dependency_warnings_flags_disabled_modules_and_lists_auto_activation() {
        let warnings = dependency_warnings(
            &[required("x", "disabled", true)],
            &["x".to_string()],
        );
        assert_eq!(warnings.len(), 1);
        assert_eq!(warnings[0].code, "disabled_dependency");
        assert_eq!(warnings[0].will_auto_activate, vec!["x".to_string()]);
    }

    #[test]
    fn dependency_warnings_is_empty_when_all_requirements_are_enabled() {
        let warnings = dependency_warnings(&[required("x", "enabled", true)], &[]);
        assert!(warnings.is_empty());
    }

    #[test]
    fn required_info_reports_a_registered_module_as_enabled() {
        let (registry, _) = registry_with(&[]);
        let info = required_info(&registry, &fresh_state(), "dummy");
        assert!(info.exists);
        assert_eq!(info.status, "enabled");
        assert_eq!(info.name.as_deref(), Some("Dummy"));
    }

    #[test]
    fn required_info_reports_a_disabled_module() {
        let (registry, _) = registry_with(&[]);
        let state = fresh_state();
        state.insert("dummy".to_string(), false);
        let info = required_info(&registry, &state, "dummy");
        assert!(info.exists);
        assert_eq!(info.status, "disabled");
    }

    #[test]
    fn required_info_reports_an_unknown_module() {
        let registry = ModuleRegistry::new();
        let info = required_info(&registry, &fresh_state(), "ghost");
        assert!(!info.exists);
        assert_eq!(info.status, "missing");
    }

    #[test]
    fn build_extension_info_describes_a_module_with_no_requirements() {
        let (registry, module) = registry_with(&[]);
        let info = build_extension_info(&registry, &fresh_state(), &module);
        assert_eq!(info.id, "dummy");
        assert_eq!(info.name, "Dummy");
        assert!(info.enabled);
        assert!(info.missing_required.is_empty());
        assert!(info.warnings.is_empty());
    }

    #[test]
    fn build_requirement_check_is_ok_with_no_requirements_and_missing_with_a_ghost() {
        let (registry, module) = registry_with(&[]);
        let check = build_requirement_check(&registry, &fresh_state(), &module);
        assert_eq!(check.status, "ok");
        let (registry, module) = registry_with(&["ghost"]);
        let check = build_requirement_check(&registry, &fresh_state(), &module);
        assert_eq!(check.status, "missing");
        assert_eq!(check.missing_required, vec!["ghost".to_string()]);
    }

    struct NamedModule {
        id: &'static str,
        requires: &'static [&'static str],
        core: bool,
    }

    impl Module for NamedModule {
        fn id(&self) -> &'static str {
            self.id
        }

        fn name(&self) -> &'static str {
            "Named"
        }

        fn description(&self) -> &'static str {
            "Test named module"
        }

        fn is_core(&self) -> bool {
            self.core
        }

        fn requires(&self) -> &[&'static str] {
            self.requires
        }
    }

    /// An `AppContext` whose registry holds a `base` module, a `child`
    /// module that requires it, and a core module -- the graph the
    /// enable/disable tests below exercise.
    fn app_context_with_modules() -> (AppContext, EnabledState) {
        let registry = Arc::new(ModuleRegistry::new());
        registry.register(Arc::new(NamedModule {
            id: "base",
            requires: &[],
            core: false,
        }));
        registry.register(Arc::new(NamedModule {
            id: "child",
            requires: &["base"],
            core: false,
        }));
        registry.register(Arc::new(NamedModule {
            id: "core-mod",
            requires: &[],
            core: true,
        }));
        let ctx = AppContext::new(AppConfig::default(), Arc::new(HookBus::new()), registry);
        (ctx, fresh_state())
    }

    #[test]
    fn enable_with_dependencies_auto_activates_a_disabled_requirement() {
        let (ctx, state) = app_context_with_modules();
        state.insert("base".to_string(), false);
        state.insert("child".to_string(), false);
        let response = enable_with_dependencies(&ctx, &state, "child").unwrap();
        assert_eq!(response.auto_activated, vec!["base".to_string()]);
        assert!(is_enabled(&state, &ctx.modules, "base"));
        assert!(is_enabled(&state, &ctx.modules, "child"));
    }

    #[test]
    fn enable_with_dependencies_does_not_reactivate_an_enabled_requirement() {
        // Both modules are enabled by default (registered, no explicit
        // entry), so enabling the child auto-activates nothing.
        let (ctx, state) = app_context_with_modules();
        let response = enable_with_dependencies(&ctx, &state, "child").unwrap();
        assert!(response.auto_activated.is_empty());
    }

    #[test]
    fn disable_with_check_refuses_a_core_module() {
        let (ctx, state) = app_context_with_modules();
        let error = disable_with_check(&ctx, &state, "core-mod", false).unwrap_err();
        assert_eq!(error.code, 409);
    }

    #[test]
    fn disable_with_check_refuses_when_a_dependent_is_enabled() {
        // Both modules are enabled by default (registered, no explicit
        // entry), so the child blocks disabling the base.
        let (ctx, state) = app_context_with_modules();
        let error = disable_with_check(&ctx, &state, "base", false).unwrap_err();
        assert_eq!(error.code, 409);
        assert_eq!(
            error.metadata.get("dependents"),
            Some(&serde_json::json!(["child"]))
        );
        assert!(is_enabled(&state, &ctx.modules, "base"));
    }

    #[test]
    fn disable_with_check_force_disables_dependents_first() {
        let (ctx, state) = app_context_with_modules();
        let response = disable_with_check(&ctx, &state, "base", true).unwrap();
        assert_eq!(response.disabled_dependents, vec!["child".to_string()]);
        assert!(!is_enabled(&state, &ctx.modules, "base"));
        assert!(!is_enabled(&state, &ctx.modules, "child"));
    }

    #[test]
    fn disable_with_check_returns_not_found_for_unknown_id() {
        let (ctx, state) = app_context_with_modules();
        let error = disable_with_check(&ctx, &state, "ghost", false).unwrap_err();
        assert_eq!(error.code, 404);
    }
}
