//! Saas Packages - Shared helpers.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::{events, HookBus, HookContext};
use unified_core::json;
use unified_core::middleware::TenantContext;
use unified_core::modules::{Module, ModuleRegistry, RouteInfo, RouteVersionSupport};
use unified_core::tenancy as tenancy;
use unified_core::pagination::{PageResult, PaginationParams};
use crate::modules::ai::{register_ai_capabilities, run_ai_capability, AiCapability};
use crate::modules::licensing::check_module_entitlement;
use actix_web::{web, HttpRequest, HttpResponse};
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use std::collections::HashSet;
use std::sync::{Arc, RwLock};
use uuid::Uuid;
use super::{constants::*, types::*, state::*, seed::*, auth::*, provisioning::*, handlers_packages::*, handlers_purchases::*, handlers_ai::*, events::*, module::*};

// ---------------------------------------------------------------------------
// Helpers.
// ---------------------------------------------------------------------------

pub(crate) fn now() -> DateTime<Utc> {
    Utc::now()
}

pub(crate) fn paginate<T: Serialize + Clone>(items: Vec<T>, params: &PaginationParams) -> PageResult<T> {
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

pub(crate) fn parse_uuid(raw: &str, what: &str) -> UnifiedResult<Uuid> {
    Uuid::parse_str(raw.trim())
        .map_err(|_| UnifiedError::bad_request(format!("Invalid {what} id: {raw}")))
}

pub(crate) fn require_text(value: &str, field: &str, max: usize) -> UnifiedResult<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err(UnifiedError::bad_request(format!("{field} must not be empty")));
    }
    if trimmed.len() > max {
        return Err(UnifiedError::bad_request(format!(
            "{field} must be at most {max} characters"
        )));
    }
    Ok(trimmed.to_string())
}

pub(crate) fn optional_text(value: &Option<String>, field: &str, max: usize) -> UnifiedResult<Option<String>> {
    match value {
        Some(raw) => Ok(Some(require_text(raw, field, max)?)),
        None => Ok(None),
    }
}

/// Turn a name into a URL-safe slug: lowercase, runs of other
/// characters become one hyphen, leading/trailing hyphens dropped.
pub(crate) fn slugify(name: &str) -> String {
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

pub(crate) fn valid_slug(slug: &str) -> bool {
    !slug.is_empty()
        && slug.len() <= MAX_SLUG_LEN
        && !slug.starts_with('-')
        && !slug.ends_with('-')
        && !slug.contains("--")
        && slug
            .chars()
            .all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '-')
}

pub(crate) fn valid_subdomain(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_SUBDOMAIN_LEN
        && !value.starts_with('-')
        && !value.ends_with('-')
        && value
            .chars()
            .all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '-')
}

/// Emit one audit-shaped event on the hook bus: the audit module
/// records events by name, so the payload carries the actor, the
/// action, and the target in a fixed shape.
pub(crate) fn emit_audit(
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

pub(crate) fn emit_audit_state(
    state: &SaasPackagesState,
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
    state.emit(event, payload);
}

pub(crate) fn ok<T: Serialize>(data: T) -> HttpResponse {
    json::success_response(data, 200)
}

pub(crate) fn created<T: Serialize>(data: T) -> HttpResponse {
    json::success_response(data, 201)
}

pub(crate) fn err(error: UnifiedError, debug: bool) -> HttpResponse {
    json::error_response(&error, debug)
}

/// Load a package by id or slug, or 404.
pub(crate) fn load_package(state: &SaasPackagesState, raw_id: &str) -> UnifiedResult<SaasPackage> {
    if let Ok(id) = Uuid::parse_str(raw_id.trim()) {
        if let Some(package) = state.packages.get(&id) {
            return Ok(package.value().clone());
        }
    }
    let slug = raw_id.trim();
    for entry in state.packages.iter() {
        if entry.value().slug == slug {
            return Ok(entry.value().clone());
        }
    }
    Err(UnifiedError::not_found(format!(
        "Package \"{raw_id}\" not found"
    )))
}

pub(crate) fn load_purchase(state: &SaasPackagesState, raw_id: &str) -> UnifiedResult<PackagePurchase> {
    let id = parse_uuid(raw_id, "purchase")?;
    state
        .purchases
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Purchase \"{raw_id}\" not found")))
}

pub(crate) fn load_version(state: &SaasPackagesState, package: &SaasPackage, version: i32) -> UnifiedResult<PackageVersion> {
    state
        .versions
        .get(&SaasPackagesState::version_key(&package.id, version))
        .map(|entry| entry.value().clone())
        .ok_or_else(|| {
            UnifiedError::not_found(format!(
                "Package \"{}\" has no version {version}",
                package.slug
            ))
        })
}

/// The module list of the version a purchase pinned.
pub(crate) fn purchase_version(state: &SaasPackagesState, purchase: &PackagePurchase) -> UnifiedResult<PackageVersion> {
    let package = state
        .packages
        .get(&purchase.package_id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| {
            UnifiedError::not_found(format!(
                "Package \"{}\" of purchase \"{}\" not found",
                purchase.package_id, purchase.id
            ))
        })?;
    load_version(state, &package, purchase.package_version)
}

/// Validate a package module list from data against the live
/// registry: at least one entry, at most the configured cap, every
/// module id registered, no duplicate module id. Entries are
/// returned sorted by sort order.
pub(crate) fn validate_package_modules(
    registry: &ModuleRegistry,
    settings: &SaasPackagesSettings,
    entries: &[PackageModuleInput],
) -> UnifiedResult<Vec<PackageModuleEntry>> {
    if entries.is_empty() {
        return Err(UnifiedError::bad_request(
            "A package must list at least one module",
        ));
    }
    if entries.len() > settings.max_modules_per_package {
        return Err(UnifiedError::bad_request(format!(
            "A package may list at most {} modules",
            settings.max_modules_per_package
        )));
    }
    let mut seen: HashSet<String> = HashSet::new();
    let mut validated: Vec<PackageModuleEntry> = Vec::new();
    for (index, entry) in entries.iter().enumerate() {
        let module_id = entry.module_id.trim().to_string();
        if module_id.is_empty() {
            return Err(UnifiedError::bad_request(
                "A package module entry must name a module id",
            ));
        }
        if registry.get(&module_id).is_none() {
            return Err(UnifiedError::bad_request(format!(
                "Unknown module \"{module_id}\": no module with that id is registered"
            ))
            .with_metadata("module_id", serde_json::json!(module_id)));
        }
        if !seen.insert(module_id.clone()) {
            return Err(UnifiedError::bad_request(format!(
                "Duplicate module \"{module_id}\" in the package module list"
            )));
        }
        validated.push(PackageModuleEntry {
            module_id,
            required: entry.required,
            sort_order: entry.sort_order.unwrap_or(index as i32),
        });
    }
    validated.sort_by_key(|entry| entry.sort_order);
    Ok(validated)
}

/// Validate a wizard step list from data against the live settings
/// vocabulary: non-empty, at most the configured cap, every name
/// from the vocabulary, no repeats.
pub(crate) fn validate_wizard_steps(
    settings: &SaasPackagesSettings,
    steps: &[String],
) -> UnifiedResult<Vec<String>> {
    if steps.is_empty() {
        return Err(UnifiedError::bad_request(
            "A wizard step list must contain at least one step",
        ));
    }
    if steps.len() > settings.max_wizard_steps {
        return Err(UnifiedError::bad_request(format!(
            "A wizard step list must contain at most {} steps",
            settings.max_wizard_steps
        )));
    }
    let mut seen: HashSet<String> = HashSet::new();
    let mut cleaned: Vec<String> = Vec::new();
    for step in steps {
        let step = step.trim().to_string();
        if !settings.wizard_step_names.iter().any(|name| *name == step) {
            return Err(UnifiedError::bad_request(format!(
                "Unknown wizard step \"{step}\" (expected one of: {})",
                settings.wizard_step_names.join(", ")
            )));
        }
        if !seen.insert(step.clone()) {
            return Err(UnifiedError::bad_request(format!(
                "Duplicate wizard step \"{step}\""
            )));
        }
        cleaned.push(step);
    }
    Ok(cleaned)
}

/// Resolve the full activation order for a set of module ids:
/// every module's transitive requirements first (deepest first),
/// then the module itself, de-duplicated with order preserved.
/// Fails closed with the registry's error (missing dependency or
/// cycle) when any module's graph is broken.
pub(crate) fn resolve_apply_order(
    registry: &ModuleRegistry,
    module_ids: &[String],
) -> UnifiedResult<Vec<String>> {
    let mut ordered: Vec<String> = Vec::new();
    for module_id in module_ids {
        for required in registry.transitive_requires_ordered(module_id)? {
            if !ordered.contains(&required) {
                ordered.push(required);
            }
        }
        if !ordered.contains(module_id) {
            ordered.push(module_id.clone());
        }
    }
    Ok(ordered)
}

/// Entitlement answer for one module and tenant, using the real
/// licensing check (HTTP path only -- an `AppContext` is required).
/// Returns Ok(true) when entitled, Ok(false) when the tenant holds
/// no entitlement, and Err when the module itself is unknown.
pub(crate) fn entitlement_available_http(
    ctx: &AppContext,
    company_id: &Uuid,
    module_id: &str,
) -> UnifiedResult<bool> {
    let Some(module) = ctx.modules.get(module_id) else {
        return Err(UnifiedError::not_found(format!(
            "Module \"{module_id}\" not found"
        )));
    };
    if !module.available_as_product() || module.is_core() {
        return Ok(true);
    }
    match check_module_entitlement(ctx, &company_id.to_string(), module_id) {
        Ok(()) => Ok(true),
        Err(error) if error.code == 403 => Ok(false),
        Err(error) => Err(error),
    }
}

/// Entitlement answer for the hook path, where no `AppContext`
/// exists. Non-sellable and core modules pass from the registry
/// alone. A sellable non-core module is checked through the
/// `saas_packages.entitlement.check_requested` hook query
/// (fail-closed shape, like `role.check`): an explicit `allowed:
/// true` answer passes, anything else blocks. Returns
/// `Ok(Some(true/false))` for a decided answer, `Ok(None)` when no
/// handler answered at all (the caller records the step as
/// `blocked`, never as passed and never as silently skipped).
pub(crate) fn entitlement_available_hook(
    state: &SaasPackagesState,
    registry: &ModuleRegistry,
    company_id: &Uuid,
    module_id: &str,
) -> UnifiedResult<Option<bool>> {
    let Some(module) = registry.get(module_id) else {
        return Err(UnifiedError::not_found(format!(
            "Module \"{module_id}\" not found"
        )));
    };
    if !module.available_as_product() || module.is_core() {
        return Ok(Some(true));
    }
    let answered = state.emit_with_results(
        EVENT_ENTITLEMENT_CHECK_REQUESTED,
        json!({
            "company_id": company_id.to_string(),
            "module_id": module_id,
            "allowed": false,
        }),
    );
    // No handler answered at all (empty results) means the question
    // is undecided -- the caller records a blocked step. Any answer
    // is read from the payload first (same shape as `role.check`)
    // and then from the contributed results.
    match answered {
        Some(context) if !context.results.is_empty() => Ok(Some(
            context
                .get("allowed")
                .and_then(|value| value.as_bool())
                .or_else(|| {
                    context
                        .results
                        .iter()
                        .filter_map(|value| value.get("allowed").and_then(|allowed| allowed.as_bool()))
                        .next()
                })
                .unwrap_or(false),
        )),
        _ => Ok(None),
    }
}

/// Compute the module diff between two module lists: what the
/// target adds, what it drops, and what both share.
pub(crate) fn module_diff(from: &[PackageModuleEntry], to: &[PackageModuleEntry]) -> PackageDiff {
    let from_ids: HashSet<&str> = from.iter().map(|entry| entry.module_id.as_str()).collect();
    let to_ids: HashSet<&str> = to.iter().map(|entry| entry.module_id.as_str()).collect();
    let mut to_enable: Vec<String> = to
        .iter()
        .filter(|entry| !from_ids.contains(entry.module_id.as_str()))
        .map(|entry| entry.module_id.clone())
        .collect();
    let mut to_disable: Vec<String> = from
        .iter()
        .filter(|entry| !to_ids.contains(entry.module_id.as_str()))
        .map(|entry| entry.module_id.clone())
        .collect();
    let mut shared: Vec<String> = from
        .iter()
        .filter(|entry| to_ids.contains(entry.module_id.as_str()))
        .map(|entry| entry.module_id.clone())
        .collect();
    to_enable.sort();
    to_disable.sort();
    shared.sort();
    PackageDiff {
        from_package_id: Uuid::nil(),
        to_package_id: Uuid::nil(),
        to_enable,
        to_disable,
        shared,
        kept_shared: Vec::new(),
    }
}

/// Full subdomain for a purchase: the requested label plus the
/// configured base domain, or the label alone when none is set.
pub(crate) fn full_subdomain(settings: &SaasPackagesSettings, subdomain: &str) -> String {
    let base = settings.subdomain_base_domain.trim().trim_start_matches('.');
    if base.is_empty() {
        subdomain.to_string()
    } else {
        format!("{subdomain}.{base}")
    }
}

pub(crate) fn steps_for_purchase(state: &SaasPackagesState, purchase_id: Uuid) -> Vec<ProvisionStep> {
    let mut steps: Vec<ProvisionStep> = state
        .steps
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|step| step.purchase_id == purchase_id)
        .collect();
    steps.sort_by_key(|step| (step.attempt, PROVISION_STEPS.iter().position(|name| *name == step.step).unwrap_or(usize::MAX)));
    steps
}

pub(crate) fn latest_step(state: &SaasPackagesState, purchase_id: Uuid, step_name: &str) -> Option<ProvisionStep> {
    steps_for_purchase(state, purchase_id)
        .into_iter()
        .filter(|step| step.step == step_name)
        .max_by_key(|step| step.attempt)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slugify_cases() {
        assert_eq!(slugify("Hello World!"), "hello-world");
        assert_eq!(slugify("  A  B  "), "a-b");
        assert_eq!(slugify("already-slug"), "already-slug");
        assert_eq!(slugify(""), "");
        assert_eq!(slugify("---"), "");
        assert_eq!(slugify("UPPER_case 123"), "upper-case-123");
    }

    #[test]
    fn valid_slug_cases() {
        assert!(valid_slug("hello-world"));
        assert!(valid_slug("a1"));
        assert!(!valid_slug("Hello"));
        assert!(!valid_slug("-a"));
        assert!(!valid_slug("a-"));
        assert!(!valid_slug("a--b"));
        assert!(!valid_slug(""));
        assert!(!valid_slug("a b"));
    }

    #[test]
    fn valid_subdomain_allows_double_hyphen() {
        // Unlike slugs, subdomains do not forbid "--".
        assert!(valid_subdomain("a--b"));
        assert!(valid_subdomain("shop-1"));
        assert!(!valid_subdomain("A"));
        assert!(!valid_subdomain(""));
        assert!(!valid_subdomain("-a"));
        assert!(!valid_subdomain("a-"));
    }

    #[test]
    fn require_text_cases() {
        assert_eq!(require_text("  hi ", "name", 10).expect("ok"), "hi");
        assert!(require_text("", "name", 10).is_err());
        assert!(require_text("   ", "name", 10).is_err());
        assert!(require_text("x".repeat(11).as_str(), "name", 10).is_err());
        assert!(require_text("x".repeat(10).as_str(), "name", 10).is_ok());
    }

    #[test]
    fn optional_text_cases() {
        assert_eq!(optional_text(&None, "name", 10).expect("none"), None);
        assert_eq!(
            optional_text(&Some("  x ".to_string()), "name", 10).expect("some"),
            Some("x".to_string())
        );
        assert!(optional_text(&Some("".to_string()), "name", 10).is_err());
    }

    #[test]
    fn module_diff_cases() {
        let entry = |id: &str| PackageModuleEntry {
            module_id: id.to_string(),
            required: true,
            sort_order: 0,
        };
        let from = vec![entry("a"), entry("b")];
        let to = vec![entry("b"), entry("c")];
        let diff = module_diff(&from, &to);
        assert_eq!(diff.to_enable, vec!["c".to_string()]);
        assert_eq!(diff.to_disable, vec!["a".to_string()]);
        assert_eq!(diff.shared, vec!["b".to_string()]);
        assert!(diff.kept_shared.is_empty());
        let same = module_diff(&from, &from);
        assert!(same.to_enable.is_empty());
        assert!(same.to_disable.is_empty());
        assert_eq!(same.shared, vec!["a".to_string(), "b".to_string()]);
    }

    #[test]
    fn full_subdomain_cases() {
        let mut settings = SaasPackagesSettings::default();
        assert_eq!(full_subdomain(&settings, "shop"), "shop");
        settings.subdomain_base_domain = "example.com".to_string();
        assert_eq!(full_subdomain(&settings, "shop"), "shop.example.com");
        settings.subdomain_base_domain = ".example.com".to_string();
        assert_eq!(full_subdomain(&settings, "shop"), "shop.example.com");
    }

    #[test]
    fn validate_wizard_steps_cases() {
        let settings = SaasPackagesSettings::default();
        assert_eq!(
            validate_wizard_steps(
                &settings,
                &["business_type".to_string(), "review_modules".to_string()]
            )
            .expect("known steps"),
            vec!["business_type".to_string(), "review_modules".to_string()]
        );
        assert!(validate_wizard_steps(&settings, &[]).is_err());
        assert!(validate_wizard_steps(&settings, &["bogus".to_string()]).is_err());
        assert!(validate_wizard_steps(
            &settings,
            &["apply".to_string(), "apply".to_string()]
        )
        .is_err());
        let many = vec!["apply".to_string(); 11];
        assert!(validate_wizard_steps(&settings, &many).is_err());
    }

    #[test]
    fn resolve_apply_order_unknown_module_errors() {
        let registry = ModuleRegistry::default();
        assert!(resolve_apply_order(&registry, &["nope".to_string()]).is_err());
        assert_eq!(
            resolve_apply_order(&registry, &[]).expect("empty"),
            Vec::<String>::new()
        );
    }

    #[test]
    fn steps_for_purchase_empty_state() {
        let state = SaasPackagesState::default();
        let id = uuid::Uuid::new_v4();
        assert!(steps_for_purchase(&state, id).is_empty());
        assert!(latest_step(&state, id, STEP_VALIDATE_ENTITLEMENTS).is_none());
    }
}
