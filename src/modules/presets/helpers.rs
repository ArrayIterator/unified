//! Presets + Setup Wizard module. — Helpers.
//!
//! Split from `src/modules/presets.rs`; behaviour unchanged.

use super::apply::*;
use super::events::*;
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
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::pagination::{PageResult, PaginationParams};
use crate::modules::licensing::has_module_entitlement;
use chrono::{DateTime, Utc};
use serde::Serialize;
use serde_json::{Map, Value};
use std::collections::HashSet;
use uuid::Uuid;

pub(super) fn now() -> DateTime<Utc> {
    Utc::now()
}

pub(super) fn paginate<T: Serialize + Clone>(
    items: Vec<T>,
    params: &PaginationParams,
) -> PageResult<T> {
    let pagination = params.normalized();
    let total = items.len() as u64;
    let start = ((pagination.page - 1) * pagination.per_page) as usize;
    let page_items: Vec<T> = items
        .into_iter()
        .skip(start)
        .take(pagination.per_page as usize)
        .collect();
    PageResult::new(page_items, total, &pagination)
}

pub(super) fn matches_search(haystack: &str, params: &PaginationParams) -> bool {
    match &params.search {
        Some(needle) if !needle.trim().is_empty() => haystack
            .to_lowercase()
            .contains(&needle.trim().to_lowercase()),
        _ => true,
    }
}

/// Emit one audit-shaped event on the hook bus: the audit module
/// records events by name, so the payload carries the actor, the
/// action, and the target in a fixed shape.
pub(super) fn emit_audit(
    ctx: &AppContext,
    event: &str,
    actor_id: &str,
    action: &str,
    target_type: &str,
    target_id: &str,
    company_id: Option<Uuid>,
    details: Value,
) {
    let mut payload = serde_json::json!({
        "actor_id": actor_id,
        "action": action,
        "target_type": target_type,
        "target_id": target_id,
        "details": details,
        "recorded_at": Utc::now(),
    });
    if let Some(company_id) = company_id {
        payload["company_id"] = serde_json::json!(company_id);
    }
    ctx.hooks.emit(event, payload);
}

/// Turn a name into a URL-safe slug: lowercase, runs of other
/// characters become one hyphen, leading/trailing hyphens dropped.
pub(super) fn slugify(name: &str) -> String {
    let mut slug = String::new();
    let mut last_hyphen = true;
    for ch in name.to_lowercase().chars() {
        if ch.is_ascii_alphanumeric() {
            slug.push(ch);
            last_hyphen = false;
        } else if !last_hyphen {
            slug.push('-');
            last_hyphen = true;
        }
    }
    while slug.ends_with('-') {
        slug.pop();
    }
    slug
}

pub(super) fn valid_slug(slug: &str) -> bool {
    !slug.is_empty()
        && slug.len() <= 100
        && !slug.starts_with('-')
        && !slug.ends_with('-')
        && !slug.contains("--")
        && slug
            .chars()
            .all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '-')
}

pub(super) fn valid_business_type(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 100
        && value
            .chars()
            .all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '_' || ch == '-')
}

/// Validate a wizard step list from data: non-empty, at most ten
/// steps, every name from the closed step vocabulary, no repeats.
pub(super) fn validate_steps(steps: &[String]) -> UnifiedResult<()> {
    if steps.is_empty() {
        return Err(UnifiedError::bad_request(
            "A wizard step list must contain at least one step",
        ));
    }
    if steps.len() > 10 {
        return Err(UnifiedError::bad_request(
            "A wizard step list must contain at most ten steps",
        ));
    }
    let mut seen: HashSet<&str> = HashSet::new();
    for step in steps {
        if !WIZARD_STEP_NAMES.contains(&step.as_str()) {
            return Err(UnifiedError::bad_request(format!(
                "Unknown wizard step \"{step}\" (expected one of: {})",
                WIZARD_STEP_NAMES.join(", ")
            )));
        }
        if !seen.insert(step.as_str()) {
            return Err(UnifiedError::bad_request(format!(
                "Duplicate wizard step \"{step}\""
            )));
        }
    }
    Ok(())
}

/// Validate a preset module list from data against the live
/// registry: at least one entry, every module id registered, no
/// duplicate module id. Entries are returned sorted by sort order.
pub(super) fn validate_preset_modules(
    ctx: &AppContext,
    entries: &[PresetModuleEntryInput],
) -> UnifiedResult<Vec<PresetModuleEntry>> {
    if entries.is_empty() {
        return Err(UnifiedError::bad_request(
            "A preset must list at least one module",
        ));
    }
    let mut seen: HashSet<String> = HashSet::new();
    let mut validated: Vec<PresetModuleEntry> = Vec::new();
    for (index, entry) in entries.iter().enumerate() {
        let module_id = entry.module_id.trim().to_string();
        if module_id.is_empty() {
            return Err(UnifiedError::bad_request(
                "A preset module entry must name a module id",
            ));
        }
        if ctx.modules.get(&module_id).is_none() {
            return Err(UnifiedError::bad_request(format!(
                "Unknown module \"{module_id}\": no module with that id is registered"
            ))
            .with_metadata("module_id", serde_json::json!(module_id)));
        }
        if !seen.insert(module_id.clone()) {
            return Err(UnifiedError::bad_request(format!(
                "Duplicate module \"{module_id}\" in the preset module list"
            )));
        }
        validated.push(PresetModuleEntry {
            module_id,
            required: entry.required,
            sort_order: entry.sort_order.unwrap_or(index as i32),
        });
    }
    validated.sort_by_key(|entry| entry.sort_order);
    Ok(validated)
}

/// Validate the optional-modules toggle list for one preset:
/// every toggled-off module must be an optional module of the
/// preset; required modules are locked on and rejecting them is a
/// 400, not a silent skip.
pub(super) fn validate_disabled_optional(
    preset: &Preset,
    disabled: &[String],
) -> UnifiedResult<Vec<String>> {
    let mut cleaned: Vec<String> = Vec::new();
    for module_id in disabled {
        let module_id = module_id.trim().to_string();
        match preset.modules.iter().find(|entry| entry.module_id == module_id) {
            Some(entry) if entry.required => {
                return Err(UnifiedError::bad_request(format!(
                    "Required module \"{module_id}\" cannot be toggled off"
                )));
            }
            Some(_) => {
                if !cleaned.contains(&module_id) {
                    cleaned.push(module_id);
                }
            }
            None => {
                return Err(UnifiedError::bad_request(format!(
                    "Module \"{module_id}\" is not part of preset \"{}\"",
                    preset.slug
                )));
            }
        }
    }
    Ok(cleaned)
}

/// Read-only entitlement answer for preview purposes: mirrors the
/// rule in `check_module_entitlement` without emitting its granted
/// / denied events (a preview must not leave an audit trail of a
/// decision that never happened). Non-sellable modules and sellable
/// core modules need no entitlement; sellable non-core modules need
/// an active license for the company.
pub(super) fn entitlement_available(
    ctx: &AppContext,
    company_id: Option<Uuid>,
    module_id: &str,
) -> bool {
    let Some(module) = ctx.modules.get(module_id) else {
        return false;
    };
    if !module.available_as_product() || module.is_core() {
        return true;
    }
    match company_id {
        Some(company_id) => has_module_entitlement(ctx, &company_id.to_string(), module_id),
        None => false,
    }
}

/// Resolve the activation order for one module: its transitive
/// requirements first (deepest first), then the module itself,
/// de-duplicated with order preserved. Fails with the registry's
/// error (missing dependency or cycle) when this module's own graph
/// is broken, and only for this module: callers that resolve each
/// module separately can skip just the broken ones.
pub(super) fn resolve_single_module_order(
    ctx: &AppContext,
    module_id: &str,
) -> UnifiedResult<Vec<String>> {
    let mut ordered = ctx.modules.transitive_requires_ordered(module_id)?;
    if !ordered.iter().any(|id| id == module_id) {
        ordered.push(module_id.to_string());
    }
    Ok(ordered)
}

/// Resolve the full activation order for a set of module ids:
/// every module's transitive requirements first (deepest first),
/// then the module itself, de-duplicated with order preserved.
/// Fails closed with the registry's error (missing dependency or
/// cycle) when any module's graph is broken. Prefer
/// `resolve_single_module_order` per module when one broken module
/// must not poison the rest of the set.
pub(super) fn resolve_apply_order(
    ctx: &AppContext,
    module_ids: &[String],
) -> UnifiedResult<Vec<String>> {
    let mut ordered: Vec<String> = Vec::new();
    for module_id in module_ids {
        for required in resolve_single_module_order(ctx, module_id)? {
            if !ordered.contains(&required) {
                ordered.push(required);
            }
        }
    }
    Ok(ordered)
}

/// Human-readable reason for a dependency-resolution failure: the
/// missing module ids when the registry named them, otherwise the
/// registry's own message (for example a dependency cycle).
pub(super) fn dependency_failure_reason(error: &UnifiedError) -> String {
    let missing: Vec<String> = error
        .metadata
        .get("missing_required")
        .and_then(|value| value.as_array())
        .map(|values| {
            values
                .iter()
                .filter_map(|value| value.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    if missing.is_empty() {
        error.message.clone()
    } else {
        format!("Missing dependency module(s): {}", missing.join(", "))
    }
}

/// Merge an override settings object over a base settings object
/// (top-level keys only; the override wins per key). Both sides
/// must be objects; a non-object override is rejected by the
/// caller before this runs.
pub(super) fn merge_settings(base: &Value, override_value: &Value) -> Value {
    let mut merged = match base {
        Value::Object(map) => map.clone(),
        _ => Map::new(),
    };
    if let Value::Object(overrides) = override_value {
        for (key, value) in overrides {
            merged.insert(key.clone(), value.clone());
        }
    }
    Value::Object(merged)
}

#[cfg(test)]
mod tests {
    use super::*;
    use unified_core::config::AppConfig;
    use unified_core::hooks::HookBus;
    use unified_core::modules::{Module, ModuleRegistry};
    use serde_json::json;
    use std::sync::Arc;

    /// A minimal registry module for order-resolution tests: `id`
    /// with static `requires`.
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

    fn registry_with(modules: Vec<FakeModule>) -> Arc<ModuleRegistry> {
        let registry = Arc::new(ModuleRegistry::new());
        for module in modules {
            registry.register(Arc::new(module));
        }
        registry
    }

    fn test_context(registry: Arc<ModuleRegistry>) -> AppContext {
        AppContext::new(
            AppConfig::default(),
            Arc::new(HookBus::new()),
            registry,
        )
    }

    #[test]
    fn resolve_single_module_order_lists_requirements_first() {
        let ctx = test_context(registry_with(vec![
            FakeModule { id: "base", requires: &[] },
            FakeModule { id: "mid", requires: &["base"] },
            FakeModule { id: "top", requires: &["mid"] },
        ]));
        assert_eq!(
            resolve_single_module_order(&ctx, "top").unwrap(),
            vec!["base".to_string(), "mid".to_string(), "top".to_string()]
        );
        assert_eq!(
            resolve_single_module_order(&ctx, "base").unwrap(),
            vec!["base".to_string()]
        );
    }

    #[test]
    fn resolve_single_module_order_fails_only_for_the_broken_module() {
        let ctx = test_context(registry_with(vec![
            FakeModule { id: "good", requires: &[] },
            FakeModule { id: "broken", requires: &["gone"] },
        ]));
        assert!(resolve_single_module_order(&ctx, "good").is_ok());
        let error = resolve_single_module_order(&ctx, "broken").expect_err("missing dep");
        assert_eq!(
            dependency_failure_reason(&error),
            "Missing dependency module(s): gone"
        );
        assert!(resolve_single_module_order(&ctx, "unregistered").is_err());
    }

    #[test]
    fn resolve_apply_order_still_fails_closed_for_the_whole_set() {
        // The whole-set resolver (used by preview) keeps its old
        // fail-closed shape; per-module callers partition instead.
        let ctx = test_context(registry_with(vec![
            FakeModule { id: "good", requires: &[] },
            FakeModule { id: "broken", requires: &["gone"] },
        ]));
        assert!(resolve_apply_order(
            &ctx,
            &["good".to_string(), "broken".to_string()]
        )
        .is_err());
        assert_eq!(
            resolve_apply_order(&ctx, &["good".to_string()]).unwrap(),
            vec!["good".to_string()]
        );
    }

    #[test]
    fn dependency_failure_reason_falls_back_to_the_message() {
        let cycle = UnifiedError::conflict("Circular dependency detected: a -> a")
            .with_metadata("cycle", json!(["a", "a"]));
        assert_eq!(
            dependency_failure_reason(&cycle),
            "Circular dependency detected: a -> a"
        );
        let plain = UnifiedError::not_found("Module \"ghost\" not found");
        assert_eq!(
            dependency_failure_reason(&plain),
            "Module \"ghost\" not found"
        );
    }

    #[test]
    fn slugify_collapses_runs_and_trims() {
        assert_eq!(slugify("Property Seller"), "property-seller");
        assert_eq!(slugify("Hello!!World"), "hello-world");
        assert_eq!(slugify("  spaced   out  "), "spaced-out");
        assert_eq!(slugify(""), "");
        assert_eq!(slugify("!!!"), "");
        assert_eq!(slugify("a-b_c"), "a-b-c");
    }

    #[test]
    fn valid_slug_rules() {
        assert!(valid_slug("property-seller"));
        assert!(valid_slug("a"));
        assert!(!valid_slug(""));
        assert!(!valid_slug("UPPER"));
        assert!(!valid_slug("-leading"));
        assert!(!valid_slug("trailing-"));
        assert!(!valid_slug("double--dash"));
        assert!(!valid_slug("under_score"));
        assert!(!valid_slug(&"a".repeat(101)));
        assert!(valid_slug(&"a".repeat(100)));
    }

    #[test]
    fn valid_business_type_rules() {
        assert!(valid_business_type("property_seller"));
        assert!(valid_business_type("saas-seller"));
        assert!(!valid_business_type(""));
        assert!(!valid_business_type("Property"));
        assert!(!valid_business_type("has space"));
        assert!(!valid_business_type(&"a".repeat(101)));
    }

    #[test]
    fn validate_steps_accepts_default_and_rejects_bad_lists() {
        let default: Vec<String> = DEFAULT_WIZARD_STEPS
            .iter()
            .map(|step| step.to_string())
            .collect();
        assert!(validate_steps(&default).is_ok());
        assert!(validate_steps(&[]).is_err());
        assert!(validate_steps(&["nope".to_string()]).is_err());
        assert!(
            validate_steps(&["business_type".to_string(), "business_type".to_string()]).is_err()
        );
        let eleven = vec!["business_type".to_string(); 11];
        assert!(validate_steps(&eleven).is_err());
    }

    #[test]
    fn merge_settings_overrides_shallowly() {
        let merged = merge_settings(&json!({"a": 1, "b": 2}), &json!({"b": 3, "c": 4}));
        assert_eq!(merged["a"], 1);
        assert_eq!(merged["b"], 3);
        assert_eq!(merged["c"], 4);
        let from_scalar = merge_settings(&json!(42), &json!({"x": true}));
        assert_eq!(from_scalar, json!({"x": true}));
        let kept = merge_settings(&json!({"a": 1}), &json!("not-an-object"));
        assert_eq!(kept, json!({"a": 1}));
    }
}
