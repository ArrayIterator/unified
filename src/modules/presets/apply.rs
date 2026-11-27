//! Presets + Setup Wizard module. - Applying a preset. Only ever enables and configures: nothing here disables a module, and modules the tenant already uses are reported as `already_enabled`, never touched. A module the tenant is not entitled to is reported as `skipped_not_entitled` and never enabled; a module whose dependency graph is broken is reported as `skipped_dependency_missing` with the missing ids in `reason`.
//!
//! Split from `src/modules/presets.rs`; behaviour unchanged.

use super::events::*;
use super::helpers::*;
use super::identity::*;
use super::lookups::*;
use super::preset_handlers::*;
use super::records::*;
use super::requests::*;
use super::seeds::*;
use super::stores::*;
use super::wizard_handlers::*;
use super::wizard_steps::*;

use unified_core::context::AppContext;
use unified_core::error::UnifiedResult;
use unified_core::hooks::events;
use crate::modules::licensing::check_module_entitlement;
use chrono::{DateTime, Utc};
use serde_json::Value;
use std::collections::HashSet;
use uuid::Uuid;

/// Apply `preset` to `company_id`. `disabled_optional` lists the
/// optional modules the caller toggled off (already validated);
/// `settings_override` is merged over the preset's settings.
/// Returns the recorded application.
pub(super) fn apply_preset(
    ctx: &AppContext,
    state: &PresetsState,
    preset: &Preset,
    company_id: Uuid,
    actor_id: &str,
    disabled_optional: &[String],
    settings_override: Option<&Value>,
) -> UnifiedResult<PresetApplication> {
    let now = Utc::now();
    let settings_applied = match settings_override {
        Some(overrides) => merge_settings(&preset.settings, overrides),
        None => preset.settings.clone(),
    };
    // Effective module list, in preset order: required modules plus
    // optional modules the caller left on. Optional modules toggled
    // off are recorded as skipped, not silently dropped.
    let mut effective: Vec<PresetModuleEntry> = Vec::new();
    let mut outcomes: Vec<ModuleApplyOutcome> = Vec::new();
    for entry in &preset.modules {
        if !entry.required && disabled_optional.contains(&entry.module_id) {
            outcomes.push(ModuleApplyOutcome {
                module_id: entry.module_id.clone(),
                status: OUTCOME_SKIPPED_OPTIONAL_OFF.to_string(),
                reason: Some("Optional module toggled off in the review step".to_string()),
                auto_activated: Vec::new(),
            });
            continue;
        }
        effective.push(entry.clone());
    }
    let effective_ids: Vec<String> = effective
        .iter()
        .map(|entry| entry.module_id.clone())
        .collect();
    // Dependency order, resolved per module. A broken graph fails
    // closed for the affected module only: each module whose own
    // requirements cannot be resolved (an unregistered module id,
    // a missing dependency, or a cycle) is skipped with the reason,
    // while the others still apply. A module that depends on a
    // broken module fails its own resolution too, so it is skipped
    // as well; independent modules are never poisoned by it.
    let mut ordered: Vec<String> = Vec::new();
    for entry in &effective {
        match resolve_single_module_order(ctx, &entry.module_id) {
            Ok(order) => {
                for module_id in order {
                    if !ordered.contains(&module_id) {
                        ordered.push(module_id);
                    }
                }
            }
            Err(error) => {
                outcomes.push(ModuleApplyOutcome {
                    module_id: entry.module_id.clone(),
                    status: OUTCOME_SKIPPED_DEPENDENCY_MISSING.to_string(),
                    reason: Some(dependency_failure_reason(&error)),
                    auto_activated: Vec::new(),
                });
            }
        }
    };
    // Modules in `ordered` that are not in the preset are transitive
    // dependencies pulled in by a preset module; they are enabled
    // with it and listed as auto-activated on that module's outcome.
    let preset_ids: HashSet<&str> = effective_ids.iter().map(|id| id.as_str()).collect();
    let mut enabled_before: HashSet<String> = ordered
        .iter()
        .filter(|id| state.tenant_has_module(&company_id, id))
        .cloned()
        .collect();
    for module_id in &ordered {
        let in_preset = preset_ids.contains(module_id.as_str());
        if state.tenant_has_module(&company_id, module_id) {
            if in_preset {
                outcomes.push(ModuleApplyOutcome {
                    module_id: module_id.clone(),
                    status: OUTCOME_ALREADY_ENABLED.to_string(),
                    reason: None,
                    auto_activated: Vec::new(),
                });
            }
            continue;
        }
        // Entitlement gate: a sellable module without an active
        // entitlement for this tenant is never enabled. The check
        // itself emits the granted / denied events.
        if let Err(error) = check_module_entitlement(ctx, &company_id.to_string(), module_id) {
            if error.code == 404 {
                outcomes.push(ModuleApplyOutcome {
                    module_id: module_id.clone(),
                    status: OUTCOME_FAILED.to_string(),
                    reason: Some(error.message.clone()),
                    auto_activated: Vec::new(),
                });
            } else {
                outcomes.push(ModuleApplyOutcome {
                    module_id: module_id.clone(),
                    status: OUTCOME_SKIPPED_NOT_ENTITLED.to_string(),
                    reason: Some(
                        "No active entitlement for this module for this tenant".to_string(),
                    ),
                    auto_activated: Vec::new(),
                });
                ctx.hooks.emit(
                    PRESET_MODULE_SKIPPED,
                    serde_json::json!({
                        "preset_id": preset.id,
                        "company_id": company_id,
                        "module_id": module_id,
                        "actor_id": actor_id,
                        "reason": "no_active_entitlement",
                    }),
                );
            }
            continue;
        }
        // Dependencies enabled in this run for this module: its
        // transitive requirements that were not enabled before.
        let auto_activated: Vec<String> = ctx
            .modules
            .transitive_requires_ordered(module_id)
            .unwrap_or_default()
            .into_iter()
            .filter(|required| !enabled_before.contains(required))
            .collect();
        for required in &auto_activated {
            state.mark_tenant_module(&company_id, required);
            ctx.hooks.emit(
                events::MODULE_ENABLED,
                serde_json::json!({
                    "id": required,
                    "company_id": company_id,
                    "preset_id": preset.id,
                    "auto_activated": true,
                    "auto_activated_by": module_id,
                }),
            );
        }
        state.mark_tenant_module(&company_id, module_id);
        enabled_before.insert(module_id.clone());
        ctx.hooks.emit(
            events::MODULE_ENABLED,
            serde_json::json!({
                "id": module_id,
                "company_id": company_id,
                "preset_id": preset.id,
                "actor_id": actor_id,
            }),
        );
        ctx.hooks.emit(
            PRESET_MODULE_ENABLED,
            serde_json::json!({
                "preset_id": preset.id,
                "company_id": company_id,
                "module_id": module_id,
                "actor_id": actor_id,
                "auto_activated": auto_activated,
            }),
        );
        if in_preset {
            outcomes.push(ModuleApplyOutcome {
                module_id: module_id.clone(),
                status: OUTCOME_ENABLED.to_string(),
                reason: None,
                auto_activated,
            });
        }
    }
    // Keep outcomes in preset order (dependencies report through
    // their dependent's `auto_activated` list).
    outcomes.sort_by_key(|outcome| {
        preset
            .modules
            .iter()
            .position(|entry| entry.module_id == outcome.module_id)
            .unwrap_or(usize::MAX)
    });
    finish_application(
        ctx,
        state,
        preset,
        company_id,
        actor_id,
        outcomes,
        settings_applied,
        now,
    )
}

/// Record one application, apply its settings handoff event, and
/// emit the audit-shaped `preset.applied` event.
#[allow(clippy::too_many_arguments)]
pub(super) fn finish_application(
    ctx: &AppContext,
    state: &PresetsState,
    preset: &Preset,
    company_id: Uuid,
    actor_id: &str,
    outcomes: Vec<ModuleApplyOutcome>,
    settings_applied: Value,
    started_at: DateTime<Utc>,
) -> UnifiedResult<PresetApplication> {
    let enabled_count = outcomes
        .iter()
        .filter(|outcome| outcome.status == OUTCOME_ENABLED)
        .count();
    let failed_count = outcomes
        .iter()
        .filter(|outcome| outcome.status == OUTCOME_FAILED)
        .count();
    let blocked_count = outcomes
        .iter()
        .filter(|outcome| {
            outcome.status == OUTCOME_SKIPPED_DEPENDENCY_MISSING
                || outcome.status == OUTCOME_SKIPPED_NOT_ENTITLED
        })
        .count();
    let status = if failed_count == outcomes.len() && !outcomes.is_empty() {
        "failed".to_string()
    } else if failed_count > 0 || blocked_count > 0 {
        "partial".to_string()
    } else {
        "completed".to_string()
    };
    let application = PresetApplication {
        id: Uuid::now_v7(),
        preset_id: preset.id,
        preset_slug: preset.slug.clone(),
        company_id,
        actor_id: actor_id.to_string(),
        status: status.clone(),
        outcomes,
        settings_applied: settings_applied.clone(),
        created_at: started_at,
        completed_at: Some(Utc::now()),
    };
    state
        .applications
        .insert(application.id, application.clone());
    // Settings handoff: the preset's settings (merged with the
    // caller's overrides) go out as an event for the owning modules
    // instead of being applied here.
    ctx.hooks.emit(
        PRESET_SETTINGS_APPLIED,
        serde_json::json!({
            "preset_id": preset.id,
            "application_id": application.id,
            "company_id": company_id,
            "actor_id": actor_id,
            "settings": settings_applied,
        }),
    );
    // Module-list handoff: the set this application enabled (or
    // found already enabled), in preset order, including the
    // transitive dependencies recorded on each outcome's
    // `auto_activated` list. Emitted on the dedicated
    // `presets.preset.modules_applied` event - never on the
    // `presets.preset.modules_requested` query, which stays a
    // query only (the tenants provisioning flow asks it for a
    // preset's stored module list) - so no consumer can mistake a
    // query answer for the applied set, or vice versa. Skipped and
    // failed modules are never listed here.
    let mut module_ids: Vec<String> = Vec::new();
    for outcome in &outcomes {
        if outcome.status == OUTCOME_ENABLED || outcome.status == OUTCOME_ALREADY_ENABLED {
            if !module_ids.contains(&outcome.module_id) {
                module_ids.push(outcome.module_id.clone());
            }
            for dependency in &outcome.auto_activated {
                if !module_ids.contains(dependency) {
                    module_ids.push(dependency.clone());
                }
            }
        }
    }
    ctx.hooks.emit(
        PRESET_MODULES_APPLIED,
        serde_json::json!({
            "preset_id": preset.id,
            "preset_slug": preset.slug,
            "application_id": application.id,
            "company_id": company_id,
            "actor_id": actor_id,
            "module_ids": module_ids,
        }),
    );
    emit_audit(
        ctx,
        PRESET_APPLIED,
        actor_id,
        "preset.applied",
        "preset",
        &preset.id.to_string(),
        Some(company_id),
        serde_json::json!({
            "preset_slug": preset.slug,
            "application_id": application.id,
            "status": status,
            "enabled": enabled_count,
            "failed": failed_count,
            "blocked": blocked_count,
        }),
    );
    Ok(application)
}

/// Build the done summary for a wizard from its application.
pub(super) fn summary_for(application: &PresetApplication, wizard: &WizardState) -> WizardSummary {
    let pick = |status: &str| {
        application
            .outcomes
            .iter()
            .filter(|outcome| outcome.status == status)
            .map(|outcome| outcome.module_id.clone())
            .collect()
    };
    let needs_attention: Vec<ModuleApplyOutcome> = application
        .outcomes
        .iter()
        .filter(|outcome| {
            outcome.status == OUTCOME_SKIPPED_NOT_ENTITLED
                || outcome.status == OUTCOME_SKIPPED_DEPENDENCY_MISSING
                || outcome.status == OUTCOME_FAILED
        })
        .cloned()
        .collect();
    WizardSummary {
        wizard_id: wizard.id,
        company_id: wizard.company_id,
        preset_id: wizard.preset_id,
        application_id: Some(application.id),
        enabled: pick(OUTCOME_ENABLED),
        already_enabled: pick(OUTCOME_ALREADY_ENABLED),
        skipped_not_entitled: pick(OUTCOME_SKIPPED_NOT_ENTITLED),
        skipped_dependency_missing: pick(OUTCOME_SKIPPED_DEPENDENCY_MISSING),
        skipped_optional_off: pick(OUTCOME_SKIPPED_OPTIONAL_OFF),
        failed: pick(OUTCOME_FAILED),
        needs_attention,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use unified_core::config::AppConfig;
    use unified_core::hooks::{HookBus, HookContext};
    use unified_core::modules::{Module, ModuleRegistry};
    use serde_json::json;
    use std::sync::{Arc, Mutex};

    /// A minimal registry module for apply tests.
    struct FakeModule {
        id: &'static str,
        requires: &'static [&'static str],
    }

    impl Module for FakeModule {
        fn id(&self) -> &'static str {
            self.id
        }

        fn name(&self) -> &'static str {
            self.id
        }

        fn description(&self) -> &'static str {
            "test-only module"
        }

        fn requires(&self) -> &[&'static str] {
            self.requires
        }
    }

    fn test_preset(module_ids: &[&str]) -> Preset {
        let now = Utc::now();
        Preset {
            id: Uuid::now_v7(),
            slug: "test-preset".to_string(),
            name: "Test Preset".to_string(),
            description: String::new(),
            business_type: "test".to_string(),
            built_in: false,
            archived: false,
            modules: module_ids
                .iter()
                .enumerate()
                .map(|(index, module_id)| PresetModuleEntry {
                    module_id: (*module_id).to_string(),
                    required: true,
                    sort_order: index as i32,
                })
                .collect(),
            settings: json!({}),
            steps: Vec::new(),
            created_by: None,
            created_at: now,
            updated_at: now,
        }
    }

    /// Context over a registry of fake modules, recording which of
    /// the two module-list events `finish_application` emits.
    fn test_context(
        modules: Vec<FakeModule>,
    ) -> (AppContext, Arc<Mutex<Vec<&'static str>>>) {
        let registry = Arc::new(ModuleRegistry::new());
        for module in modules {
            registry.register(Arc::new(module));
        }
        let hooks = Arc::new(HookBus::new());
        let seen: Arc<Mutex<Vec<&'static str>>> = Arc::new(Mutex::new(Vec::new()));
        for (event, name) in [
            (PRESET_MODULES_APPLIED, "applied"),
            (PRESET_MODULES_REQUESTED, "requested"),
        ] {
            let seen = Arc::clone(&seen);
            hooks.register(
                event,
                "test",
                Arc::new(move |_context: &mut HookContext| {
                    seen.lock().expect("seen lock").push(name);
                }),
            );
        }
        let ctx = AppContext::new(AppConfig::default(), hooks, registry);
        (ctx, seen)
    }

    fn outcome<'a>(application: &'a PresetApplication, module_id: &str) -> &'a ModuleApplyOutcome {
        application
            .outcomes
            .iter()
            .find(|outcome| outcome.module_id == module_id)
            .unwrap_or_else(|| panic!("no outcome for {module_id}"))
    }

    #[test]
    fn apply_skips_only_the_module_whose_graph_is_broken() {
        let (ctx, seen) = test_context(vec![
            FakeModule { id: "base", requires: &[] },
            FakeModule { id: "good-a", requires: &["base"] },
            FakeModule { id: "broken", requires: &["gone"] },
            FakeModule { id: "good-c", requires: &[] },
        ]);
        let state = PresetsState::new();
        let preset = test_preset(&["good-a", "broken", "good-c"]);
        let company_id = Uuid::now_v7();
        let application = apply_preset(&ctx, &state, &preset, company_id, "actor-1", &[], None)
            .expect("apply");
        assert_eq!(application.status, "partial");
        assert_eq!(outcome(&application, "good-a").status, OUTCOME_ENABLED);
        assert_eq!(outcome(&application, "good-c").status, OUTCOME_ENABLED);
        let broken = outcome(&application, "broken");
        assert_eq!(broken.status, OUTCOME_SKIPPED_DEPENDENCY_MISSING);
        assert!(
            broken.reason.as_deref().unwrap_or("").contains("gone"),
            "reason names the missing module: {:?}",
            broken.reason
        );
        assert!(state.tenant_has_module(&company_id, "base"));
        assert!(state.tenant_has_module(&company_id, "good-a"));
        assert!(state.tenant_has_module(&company_id, "good-c"));
        assert!(!state.tenant_has_module(&company_id, "broken"));
        // The handoff goes out on the dedicated applied event, and
        // the query-only event is never emitted by an application.
        let seen = seen.lock().expect("seen lock");
        assert!(seen.contains(&"applied"), "applied event emitted: {seen:?}");
        assert!(
            !seen.contains(&"requested"),
            "query event must not be emitted by an application: {seen:?}"
        );
    }

    #[test]
    fn apply_skips_only_the_module_on_a_cycle() {
        let (ctx, _) = test_context(vec![
            FakeModule { id: "loopy", requires: &["loopy"] },
            FakeModule { id: "fine", requires: &[] },
        ]);
        let state = PresetsState::new();
        let preset = test_preset(&["loopy", "fine"]);
        let company_id = Uuid::now_v7();
        let application = apply_preset(&ctx, &state, &preset, company_id, "actor-1", &[], None)
            .expect("apply");
        assert_eq!(application.status, "partial");
        let loopy = outcome(&application, "loopy");
        assert_eq!(loopy.status, OUTCOME_SKIPPED_DEPENDENCY_MISSING);
        assert!(
            loopy.reason.as_deref().unwrap_or("").contains("Circular"),
            "reason names the cycle: {:?}",
            loopy.reason
        );
        assert_eq!(outcome(&application, "fine").status, OUTCOME_ENABLED);
    }

    #[test]
    fn apply_skips_an_unregistered_module_without_poisoning_the_rest() {
        let (ctx, _) = test_context(vec![FakeModule { id: "fine", requires: &[] }]);
        let state = PresetsState::new();
        let preset = test_preset(&["ghost", "fine"]);
        let company_id = Uuid::now_v7();
        let application = apply_preset(&ctx, &state, &preset, company_id, "actor-1", &[], None)
            .expect("apply");
        assert_eq!(application.status, "partial");
        assert_eq!(
            outcome(&application, "ghost").status,
            OUTCOME_SKIPPED_DEPENDENCY_MISSING
        );
        assert_eq!(outcome(&application, "fine").status, OUTCOME_ENABLED);
    }
}
