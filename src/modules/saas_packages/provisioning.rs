//! Saas Packages - Provisioning pipeline: steps, runner, and per-step executors.

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
use super::{constants::*, types::*, state::*, seed::*, auth::*, helpers::*, handlers_packages::*, handlers_purchases::*, handlers_ai::*, events::*, module::*};

// ---------------------------------------------------------------------------
// Provisioning.
//
// The pipeline runs six steps in order. Two execution contexts
// exist: the HTTP path (a real `AppContext` is available, so the
// licensing module is consulted directly) and the hook path
// (`order.paid` and friends, where only the stored registry and
// hook bus exist, so entitlement is queried through the
// `saas_packages.entitlement.check_requested` hook). Both paths
// share this engine; the only difference is how entitlement is
// answered. A module the tenant is not entitled to BLOCKS the run
// with a 403-style error naming the module -- it is never silently
// skipped. A step that cannot be decided on the hook path is
// recorded as `blocked` (not failed) and the purchase stays in
// `provisioning` until an admin retries through the HTTP endpoint.
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ProvisionMode {
    Full,
    RetryFailed,
}

/// Emit an event and return the handler context, from either the
/// HTTP context or the stored hook bus.
pub(crate) fn emit_event(
    state: &SaasPackagesState,
    http_ctx: Option<&AppContext>,
    event: &str,
    data: Value,
) -> Option<HookContext> {
    match http_ctx {
        Some(ctx) => Some(ctx.hooks.emit(event, data)),
        None => state.emit_with_results(event, data),
    }
}

pub(crate) fn emit_event_fire(
    state: &SaasPackagesState,
    http_ctx: Option<&AppContext>,
    event: &str,
    data: Value,
) {
    let _ = emit_event(state, http_ctx, event, data);
}

/// What one provisioning step decided.
pub(crate) struct StepDecision {
    pub(crate) status: &'static str,
    pub(crate) outcome: Option<String>,
    pub(crate) detail: Option<String>,
}

pub(crate) fn step_completed(outcome: &str, detail: String) -> StepDecision {
    StepDecision {
        status: STEP_COMPLETED,
        outcome: Some(outcome.to_string()),
        detail: Some(detail),
    }
}

pub(crate) fn step_failed(detail: String) -> StepDecision {
    StepDecision {
        status: STEP_FAILED,
        outcome: Some(OUTCOME_FAILED.to_string()),
        detail: Some(detail),
    }
}

pub(crate) fn step_blocked(detail: String) -> StepDecision {
    StepDecision {
        status: STEP_BLOCKED,
        outcome: None,
        detail: Some(detail),
    }
}

/// Check one module's entitlement in either execution context.
/// Returns Ok(true) entitled, Ok(false) not entitled, Err(unknown
/// module). `Ok(None)` from the hook query (nobody answered) is
/// reported through the `blocked` out-parameter instead of an
/// error, so the caller can record a blocked step.
pub(crate) fn check_one_entitlement(
    state: &SaasPackagesState,
    http_ctx: Option<&AppContext>,
    registry: &ModuleRegistry,
    company_id: &Uuid,
    module_id: &str,
) -> UnifiedResult<Option<bool>> {
    match http_ctx {
        Some(ctx) => entitlement_available_http(ctx, company_id, module_id).map(Some),
        None => entitlement_available_hook(state, registry, company_id, module_id),
    }
}

/// Parse the stored enable-modules outcomes of the latest run.
/// No stored enable-modules step, or one with no detail, means no
/// outcomes yet. A malformed stored detail is a corrupt record,
/// not an empty outcome list: it is propagated as an error so the
/// run refuses to start instead of silently dropping the outcomes
/// and re-deriving them on retry.
pub(crate) fn parse_module_outcomes(
    state: &SaasPackagesState,
    purchase_id: Uuid,
) -> UnifiedResult<Vec<ModuleProvisionOutcome>> {
    let Some(step) = latest_step(state, purchase_id, STEP_ENABLE_MODULES) else {
        return Ok(Vec::new());
    };
    let Some(detail) = step.detail else {
        return Ok(Vec::new());
    };
    let step_id = step.id;
    serde_json::from_str::<Vec<ModuleProvisionOutcome>>(&detail).map_err(|error| {
        UnifiedError::internal(format!(
            "Stored enable-modules outcomes for purchase \"{purchase_id}\" could not be parsed (step {step_id}): {error}; an administrator must inspect the stored step before provisioning can run again"
        ))
    })
}

pub(crate) fn build_summary(state: &SaasPackagesState, purchase: &PackagePurchase) -> ProvisionSummary {
    let steps = steps_for_purchase(state, purchase.id);
    let latest_attempt = steps.iter().map(|step| step.attempt).max().unwrap_or(0);
    let current: Vec<ProvisionStep> = steps
        .iter()
        .filter(|step| step.attempt == latest_attempt)
        .cloned()
        .collect();
    // Read-only view: a malformed stored detail shows as no
    // outcomes here. The run path (parse_module_outcomes, called
    // by run_provisioning before any mutation) refuses to start on
    // corrupt data, so this fallback can never reset outcomes that
    // a retry would re-derive.
    let module_outcomes = parse_module_outcomes(state, purchase.id).unwrap_or_default();
    let blocked_reason = current
        .iter()
        .find(|step| step.status == STEP_BLOCKED)
        .and_then(|step| step.detail.clone());
    let status = if purchase.status == PURCHASE_ACTIVE {
        PURCHASE_ACTIVE.to_string()
    } else if current.iter().any(|step| step.status == STEP_FAILED) {
        PURCHASE_FAILED.to_string()
    } else if current.iter().any(|step| step.status == STEP_BLOCKED) {
        PURCHASE_PROVISIONING.to_string()
    } else {
        purchase.status.clone()
    };
    ProvisionSummary {
        purchase_id: purchase.id,
        package_id: purchase.package_id,
        package_version: purchase.package_version,
        company_id: purchase.company_id,
        tenant_schema: tenant_schema(&purchase.company_id),
        status,
        attempt: latest_attempt,
        steps: current,
        module_outcomes,
        subdomain_pending: purchase.subdomain_pending,
        blocked_reason,
        updated_at: purchase.updated_at,
    }
}

/// Run the provisioning pipeline for one purchase. `http_ctx` is
/// Some on the HTTP path and None on the hook path. Returns the
/// fresh summary; a failed or blocked run is a summary, not an
/// error -- only a missing purchase, a missing registry, an
/// exhausted attempt budget, or corrupt stored enable-modules
/// outcomes return Err.
pub(crate) fn run_provisioning(
    state: &SaasPackagesState,
    purchase_id: Uuid,
    actor: &str,
    mode: ProvisionMode,
    http_ctx: Option<&AppContext>,
) -> UnifiedResult<ProvisionSummary> {
    let settings = state.settings();
    let mut purchase = state
        .purchases
        .get(&purchase_id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Purchase \"{purchase_id}\" not found")))?;
    let version = purchase_version(state, &purchase)?;
    let registry = match http_ctx {
        Some(ctx) => ctx.modules.clone(),
        None => state
            .registry()
            .ok_or_else(|| UnifiedError::conflict("Module registry is not available yet"))?,
    };
    // The attempt cap applies to every mode and every step state:
    // a retry that finds no failed or blocked step (for example a
    // run whose steps are all skipped) must not slip past the cap
    // and consume another attempt.
    if purchase.provision_attempts >= settings.provisioning_max_attempts {
        return Err(UnifiedError::conflict(format!(
            "Provisioning attempt limit reached for purchase \"{purchase_id}\" (maximum {} attempts); an administrator must reset the purchase",
            settings.provisioning_max_attempts
        )));
    }
    // Parse the carried-over outcomes before mutating anything: a
    // malformed stored detail refuses the run (no attempt is
    // consumed, the purchase status is untouched) instead of
    // silently resetting the outcomes to empty.
    let mut module_outcomes = parse_module_outcomes(state, purchase_id)?;
    let attempt = purchase.provision_attempts + 1;
    purchase.provision_attempts = attempt;
    purchase.status = PURCHASE_PROVISIONING.to_string();
    purchase.updated_at = now();
    state.purchases.insert(purchase.id, purchase.clone());
    emit_event_fire(
        state,
        http_ctx,
        EVENT_PROVISION_STARTED,
        json!({
            "purchase_id": purchase.id,
            "package_id": purchase.package_id,
            "package_version": purchase.package_version,
            "company_id": purchase.company_id,
            "tenant_schema": tenant_schema(&purchase.company_id),
            "attempt": attempt,
            "actor_id": actor,
        }),
    );
    state.record_history(
        purchase.id,
        "provision.started",
        actor,
        json!({ "attempt": attempt, "mode": if mode == ProvisionMode::Full { "full" } else { "retry_failed" } }),
    );

    // Steps to run this attempt: everything on a full run; only
    // failed, blocked, or never-reached (skipped) steps on retry.
    let rerun: Vec<&str> = match mode {
        ProvisionMode::Full => PROVISION_STEPS.to_vec(),
        ProvisionMode::RetryFailed => PROVISION_STEPS
            .iter()
            .copied()
            .filter(|name| {
                matches!(
                    latest_step(state, purchase_id, name).map(|step| step.status.as_str()),
                    Some(STEP_FAILED) | Some(STEP_BLOCKED) | Some(STEP_SKIPPED) | None
                )
            })
            .collect(),
    };

    let mut failed: Option<String> = None;
    let mut blocked: Option<String> = None;

    for step_name in PROVISION_STEPS {
        if failed.is_some() || blocked.is_some() {
            // Never reached: record as skipped so the attempt shows
            // the whole pipeline, and retry can pick it up later.
            if rerun.contains(step_name) || mode == ProvisionMode::Full {
                record_step(state, purchase_id, step_name, attempt, StepDecision {
                    status: STEP_SKIPPED,
                    outcome: Some("skipped".to_string()),
                    detail: Some("Not reached: an earlier step failed or was blocked".to_string()),
                });
            }
            continue;
        }
        if !rerun.contains(step_name) {
            continue;
        }
        let mut step = ProvisionStep {
            id: Uuid::now_v7(),
            purchase_id,
            step: step_name.to_string(),
            status: STEP_RUNNING.to_string(),
            outcome: None,
            detail: None,
            attempt,
            started_at: now(),
            completed_at: None,
        };
        state.steps.insert(step.id, step.clone());
        let decision = execute_step(
            state,
            http_ctx,
            &registry,
            &mut purchase,
            &version,
            actor,
            step_name,
            &mut module_outcomes,
        );
        step.status = decision.status.to_string();
        step.outcome = decision.outcome;
        step.detail = decision.detail.clone();
        step.completed_at = Some(now());
        state.steps.insert(step.id, step.clone());
        emit_event_fire(
            state,
            http_ctx,
            EVENT_PROVISION_STEP_COMPLETED,
            json!({
                "purchase_id": purchase.id,
                "step": step_name,
                "status": step.status,
                "outcome": step.outcome,
                "attempt": attempt,
            }),
        );
        match decision.status {
            STEP_FAILED => failed = decision.detail,
            STEP_BLOCKED => blocked = decision.detail,
            _ => {}
        }
    }

    // Refresh the purchase row (steps may have changed it: subdomain
    // pending flag, enabled modules).
    purchase = state
        .purchases
        .get(&purchase_id)
        .map(|entry| entry.value().clone())
        .unwrap_or(purchase);
    if let Some(reason) = &failed {
        purchase.status = PURCHASE_FAILED.to_string();
        purchase.updated_at = now();
        state.purchases.insert(purchase.id, purchase.clone());
        emit_event_fire(
            state,
            http_ctx,
            EVENT_PROVISION_FAILED,
            json!({
                "purchase_id": purchase.id,
                "package_id": purchase.package_id,
                "company_id": purchase.company_id,
                "attempt": attempt,
                "reason": reason,
            }),
        );
        state.record_history(
            purchase.id,
            "provision.failed",
            actor,
            json!({ "attempt": attempt, "reason": reason }),
        );
    } else if let Some(reason) = &blocked {
        purchase.status = PURCHASE_PROVISIONING.to_string();
        purchase.updated_at = now();
        state.purchases.insert(purchase.id, purchase.clone());
        state.record_history(
            purchase.id,
            "provision.blocked",
            actor,
            json!({ "attempt": attempt, "reason": reason }),
        );
    } else {
        purchase.status = PURCHASE_ACTIVE.to_string();
        purchase.updated_at = now();
        state.purchases.insert(purchase.id, purchase.clone());
        emit_event_fire(
            state,
            http_ctx,
            EVENT_PROVISION_COMPLETED,
            json!({
                "purchase_id": purchase.id,
                "package_id": purchase.package_id,
                "package_version": purchase.package_version,
                "company_id": purchase.company_id,
                "tenant_schema": tenant_schema(&purchase.company_id),
                "attempt": attempt,
                "enabled_modules": purchase.enabled_modules,
                "subdomain_pending": purchase.subdomain_pending,
            }),
        );
        emit_event_fire(
            state,
            http_ctx,
            EVENT_PURCHASE_COMPLETED,
            json!({
                "purchase_id": purchase.id,
                "package_id": purchase.package_id,
                "company_id": purchase.company_id,
            }),
        );
        state.record_history(
            purchase.id,
            "provision.completed",
            actor,
            json!({ "attempt": attempt, "enabled_modules": purchase.enabled_modules }),
        );
    }
    let summary = build_summary(state, &purchase);
    Ok(summary)
}

pub(crate) fn record_step(
    state: &SaasPackagesState,
    purchase_id: Uuid,
    step_name: &str,
    attempt: i32,
    decision: StepDecision,
) {
    let step = ProvisionStep {
        id: Uuid::now_v7(),
        purchase_id,
        step: step_name.to_string(),
        status: decision.status.to_string(),
        outcome: decision.outcome,
        detail: decision.detail,
        attempt,
        started_at: now(),
        completed_at: Some(now()),
    };
    state.steps.insert(step.id, step);
}

/// Execute one pipeline step. Returns the step decision; a failed
/// or blocked decision stops the pipeline (the runner handles it).
#[allow(clippy::too_many_arguments)]
pub(crate) fn execute_step(
    state: &SaasPackagesState,
    http_ctx: Option<&AppContext>,
    registry: &ModuleRegistry,
    purchase: &mut PackagePurchase,
    version: &PackageVersion,
    actor: &str,
    step_name: &str,
    module_outcomes: &mut Vec<ModuleProvisionOutcome>,
) -> StepDecision {
    match step_name {
        STEP_VALIDATE_ENTITLEMENTS => validate_entitlements_step(state, http_ctx, registry, purchase, version),
        STEP_PROVISION_SUBDOMAIN => provision_subdomain_step(state, http_ctx, purchase, version),
        STEP_ENABLE_MODULES => enable_modules_step(state, http_ctx, registry, purchase, version, module_outcomes),
        STEP_APPLY_PRESET_SETTINGS => apply_settings_step(state, http_ctx, purchase, version, actor),
        STEP_RUN_SETUP_WIZARD => run_wizard_step(state, http_ctx, purchase, version, actor),
        STEP_SEND_WELCOME => send_welcome_step(state, http_ctx, purchase, version),
        _ => step_failed(format!("Unknown provisioning step \"{step_name}\"")),
    }
}

pub(crate) fn validate_entitlements_step(
    state: &SaasPackagesState,
    http_ctx: Option<&AppContext>,
    registry: &ModuleRegistry,
    purchase: &PackagePurchase,
    version: &PackageVersion,
) -> StepDecision {
    for entry in &version.modules {
        if registry.get(&entry.module_id).is_none() {
            return step_failed(format!(
                "Unknown module \"{}\": no module with that id is registered",
                entry.module_id
            ));
        }
        match check_one_entitlement(state, http_ctx, registry, &purchase.company_id, &entry.module_id) {
            Ok(Some(true)) => {}
            Ok(Some(false)) => {
                return step_failed(format!(
                    "Tenant \"{}\" is not entitled to module \"{}\": provisioning blocked (the module requires an active license entitlement)",
                    purchase.company_id, entry.module_id
                ));
            }
            Ok(None) => {
                return step_blocked(format!(
                    "No entitlement handler answered for module \"{}\": provisioning is paused until the entitlement can be checked; retry provisioning to re-check",
                    entry.module_id
                ));
            }
            Err(error) => return step_failed(error.message),
        }
    }
    step_completed(
        OUTCOME_ENABLED,
        format!(
            "All {} package modules are registered and entitled for tenant \"{}\"",
            version.modules.len(),
            purchase.company_id
        ),
    )
}

pub(crate) fn provision_subdomain_step(
    state: &SaasPackagesState,
    http_ctx: Option<&AppContext>,
    purchase: &mut PackagePurchase,
    _version: &PackageVersion,
) -> StepDecision {
    let settings = state.settings();
    let label = match &purchase.subdomain {
        Some(label) => label.clone(),
        None => format!("pkg-{}", purchase.id.to_string().replace('-', "")[..8].to_string()),
    };
    let domain = full_subdomain(&settings, &label);
    let answered = emit_event(
        state,
        http_ctx,
        EVENT_SITE_PROVISION_REQUESTED,
        json!({
            "purchase_id": purchase.id,
            "company_id": purchase.company_id,
            "tenant_schema": tenant_schema(&purchase.company_id),
            "subdomain": label,
            "domain": domain,
            "package_id": purchase.package_id,
            "source": "saas_packages",
        }),
    );
    let claimed = answered
        .as_ref()
        .map(|context| {
            context.results.iter().any(|result| {
                result.get("claimed").and_then(|value| value.as_bool()).unwrap_or(false)
                    || result.get("site_id").is_some()
                    || result.get("domain").is_some()
            })
        })
        .unwrap_or(false);
    if claimed {
        purchase.subdomain = Some(label);
        purchase.subdomain_pending = false;
        purchase.updated_at = now();
        state.purchases.insert(purchase.id, purchase.clone());
        step_completed(
            "provisioned",
            format!("Tenant subdomain \"{domain}\" was claimed by the domains flow"),
        )
    } else {
        purchase.subdomain = Some(label);
        purchase.subdomain_pending = true;
        purchase.updated_at = now();
        state.purchases.insert(purchase.id, purchase.clone());
        step_completed(
            OUTCOME_REQUESTED,
            format!(
                "Subdomain \"{domain}\" requested from the domains flow; no handler has claimed it yet, so the subdomain stays pending until the domains flow provisions it"
            ),
        )
    }
}

pub(crate) fn enable_modules_step(
    state: &SaasPackagesState,
    http_ctx: Option<&AppContext>,
    registry: &ModuleRegistry,
    purchase: &mut PackagePurchase,
    version: &PackageVersion,
    module_outcomes: &mut Vec<ModuleProvisionOutcome>,
) -> StepDecision {
    let module_ids: Vec<String> = version
        .modules
        .iter()
        .map(|entry| entry.module_id.clone())
        .collect();
    let ordered = match resolve_apply_order(registry, &module_ids) {
        Ok(ordered) => ordered,
        Err(error) => {
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
            for entry in &version.modules {
                module_outcomes.push(ModuleProvisionOutcome {
                    module_id: entry.module_id.clone(),
                    status: OUTCOME_SKIPPED_DEPENDENCY_MISSING.to_string(),
                    reason: Some(if missing.is_empty() {
                        error.message.clone()
                    } else {
                        format!("Missing dependency module(s): {}", missing.join(", "))
                    }),
                    auto_activated: Vec::new(),
                });
            }
            return step_failed(format!(
                "Cannot resolve the module dependency order for package \"{}\": {}",
                version.package_id, error.message
            ));
        }
    };
    let in_package: HashSet<&str> = module_ids.iter().map(|id| id.as_str()).collect();
    let mut enabled_before: HashSet<String> = ordered
        .iter()
        .filter(|id| state.tenant_has_module(&purchase.company_id, id))
        .cloned()
        .collect();
    for module_id in &ordered {
        if state.tenant_has_module(&purchase.company_id, module_id) {
            if in_package.contains(module_id.as_str()) {
                module_outcomes.push(ModuleProvisionOutcome {
                    module_id: module_id.clone(),
                    status: OUTCOME_ALREADY_ENABLED.to_string(),
                    reason: None,
                    auto_activated: Vec::new(),
                });
            }
            continue;
        }
        // Entitlement gate, re-checked per module (defense in depth):
        // a module the tenant is not entitled to BLOCKS the run with
        // a 403-style error naming the module -- never silently skipped.
        match check_one_entitlement(state, http_ctx, registry, &purchase.company_id, module_id) {
            Ok(Some(true)) => {}
            Ok(Some(false)) => {
                return step_failed(format!(
                    "Tenant \"{}\" is not entitled to module \"{}\": provisioning blocked (the module requires an active license entitlement)",
                    purchase.company_id, module_id
                ));
            }
            Ok(None) => {
                return step_blocked(format!(
                    "No entitlement handler answered for module \"{module_id}\": provisioning is paused until the entitlement can be checked; retry provisioning to re-check"
                ));
            }
            Err(error) => return step_failed(error.message),
        }
        let auto_activated: Vec<String> = registry
            .transitive_requires_ordered(module_id)
            .unwrap_or_default()
            .into_iter()
            .filter(|required| !enabled_before.contains(required))
            .collect();
        for required in &auto_activated {
            state.mark_tenant_module(&purchase.company_id, required, purchase.id);
            emit_event_fire(
                state,
                http_ctx,
                events::MODULE_ENABLED,
                json!({
                    "id": required,
                    "company_id": purchase.company_id,
                    "package_id": purchase.package_id,
                    "purchase_id": purchase.id,
                    "auto_activated": true,
                    "auto_activated_by": module_id,
                }),
            );
        }
        state.mark_tenant_module(&purchase.company_id, module_id, purchase.id);
        enabled_before.insert(module_id.clone());
        emit_event_fire(
            state,
            http_ctx,
            events::MODULE_ENABLED,
            json!({
                "id": module_id,
                "company_id": purchase.company_id,
                "package_id": purchase.package_id,
                "purchase_id": purchase.id,
                "actor_id": purchase.buyer_id,
            }),
        );
        emit_event_fire(
            state,
            http_ctx,
            EVENT_MODULE_ENABLED,
            json!({
                "package_id": purchase.package_id,
                "purchase_id": purchase.id,
                "company_id": purchase.company_id,
                "module_id": module_id,
                "actor_id": purchase.buyer_id,
                "auto_activated": auto_activated,
            }),
        );
        if in_package.contains(module_id.as_str()) {
            module_outcomes.push(ModuleProvisionOutcome {
                module_id: module_id.clone(),
                status: OUTCOME_ENABLED.to_string(),
                reason: None,
                auto_activated,
            });
        }
    }
    // Keep outcomes in package order (dependencies report through
    // their dependent's `auto_activated` list).
    module_outcomes.sort_by_key(|outcome| {
        version
            .modules
            .iter()
            .position(|entry| entry.module_id == outcome.module_id)
            .unwrap_or(usize::MAX)
    });
    let enabled: Vec<String> = module_outcomes
        .iter()
        .filter(|outcome| outcome.status == OUTCOME_ENABLED || outcome.status == OUTCOME_ALREADY_ENABLED)
        .map(|outcome| outcome.module_id.clone())
        .collect();
    purchase.enabled_modules = enabled;
    purchase.updated_at = now();
    state.purchases.insert(purchase.id, purchase.clone());
    // The outcomes travel in the step detail so the GET provision
    // endpoints can rebuild them without re-running anything.
    let detail = serde_json::to_string(&module_outcomes)
        .unwrap_or_else(|_| "[]".to_string());
    StepDecision {
        status: STEP_COMPLETED,
        outcome: Some(OUTCOME_ENABLED.to_string()),
        detail: Some(detail),
    }
}

pub(crate) fn apply_settings_step(
    state: &SaasPackagesState,
    http_ctx: Option<&AppContext>,
    purchase: &PackagePurchase,
    version: &PackageVersion,
    actor: &str,
) -> StepDecision {
    // Settings handoff only: the owning modules apply the package
    // settings. Nothing is applied here, and the settings payload
    // is never interpreted by this module.
    emit_event_fire(
        state,
        http_ctx,
        EVENT_SETTINGS_APPLIED,
        json!({
            "preset_id": version.package_id,
            "package_id": version.package_id,
            "application_id": purchase.id,
            "purchase_id": purchase.id,
            "company_id": purchase.company_id,
            "actor_id": actor,
            "settings": version.preset_settings,
        }),
    );
    step_completed(
        OUTCOME_REQUESTED,
        "Package settings handed off to the owning modules through the settings event".to_string(),
    )
}

pub(crate) fn run_wizard_step(
    state: &SaasPackagesState,
    http_ctx: Option<&AppContext>,
    purchase: &PackagePurchase,
    version: &PackageVersion,
    actor: &str,
) -> StepDecision {
    emit_event_fire(
        state,
        http_ctx,
        EVENT_WIZARD_PREFILL_REQUESTED,
        json!({
            "purchase_id": purchase.id,
            "company_id": purchase.company_id,
            "package_id": version.package_id,
            "package_version": version.version,
            "actor_id": actor,
            "steps": version.wizard_steps,
            "settings": version.preset_settings,
        }),
    );
    step_completed(
        OUTCOME_REQUESTED,
        "Setup wizard pre-fill requested from the presets flow with the package steps and settings".to_string(),
    )
}

pub(crate) fn send_welcome_step(
    state: &SaasPackagesState,
    http_ctx: Option<&AppContext>,
    purchase: &PackagePurchase,
    version: &PackageVersion,
) -> StepDecision {
    let settings = state.settings();
    let link = purchase.subdomain.as_ref().map(|label| {
        format!("https://{}", full_subdomain(&settings, label))
    });
    // No credential is ever carried in the payload: credentials are
    // issued by the auth/mail flow and referenced only by the
    // tenant link. Delivery itself belongs to notifications.
    let mut payload = json!({
        "kind": "saas_package_welcome",
        "user_id": purchase.buyer_id,
        "title": format!("Your \"{}\" workspace is ready", version.name),
        "body": format!("Your purchase of \"{}\" is provisioned. Sign in at your tenant workspace to finish setup with the guided wizard.", version.name),
        "module": "saas_packages",
        "category": "saas_packages",
        "priority": "normal",
        "company_id": purchase.company_id,
        "purchase_id": purchase.id,
        "package_id": purchase.package_id,
    });
    if let Some(link) = link {
        payload["link"] = json!(link);
    }
    emit_event_fire(state, http_ctx, "notification.requested", payload);
    step_completed(
        OUTCOME_REQUESTED,
        "Welcome message requested from the notifications flow".to_string(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state_with_registry() -> SaasPackagesState {
        let state = SaasPackagesState::new();
        *state
            .modules
            .write()
            .expect("modules lock poisoned") = Some(Arc::new(ModuleRegistry::default()));
        state
    }

    fn seed_package(state: &SaasPackagesState, modules: Vec<&str>) -> SaasPackage {
        let now = Utc::now();
        let package = SaasPackage {
            id: Uuid::now_v7(),
            slug: "test-package".to_string(),
            name: "Test Package".to_string(),
            description: String::new(),
            status: STATUS_PUBLISHED.to_string(),
            current_version: 1,
            product_id: None,
            preset_settings: json!({}),
            wizard_steps: Vec::new(),
            modules: modules
                .into_iter()
                .enumerate()
                .map(|(index, module_id)| PackageModuleEntry {
                    module_id: module_id.to_string(),
                    required: true,
                    sort_order: index as i32,
                })
                .collect(),
            created_by: "test".to_string(),
            vendor_id: None,
            created_at: now,
            updated_at: now,
        };
        state.store_version(&package);
        state.packages.insert(package.id, package.clone());
        package
    }

    fn seed_purchase(
        state: &SaasPackagesState,
        package: &SaasPackage,
        attempts: i32,
    ) -> PackagePurchase {
        let now = Utc::now();
        let purchase = PackagePurchase {
            id: Uuid::now_v7(),
            package_id: package.id,
            package_version: package.current_version,
            buyer_id: "buyer-1".to_string(),
            company_id: Uuid::now_v7(),
            status: PURCHASE_FAILED.to_string(),
            subdomain: None,
            subdomain_pending: false,
            order_id: Some("order-1".to_string()),
            subscription_id: None,
            product_id: None,
            enabled_modules: Vec::new(),
            grace_ends_at: None,
            cancelled_at: None,
            cancel_reason: None,
            provision_attempts: attempts,
            created_at: now,
            updated_at: now,
        };
        state.purchases.insert(purchase.id, purchase.clone());
        purchase
    }

    fn stored_purchase(state: &SaasPackagesState, id: Uuid) -> PackagePurchase {
        state
            .purchases
            .get(&id)
            .map(|entry| entry.value().clone())
            .expect("purchase must still be stored")
    }

    #[test]
    fn step_decision_constructors() {
        let done = step_completed("enabled", "done".to_string());
        assert_eq!(done.status, STEP_COMPLETED);
        assert_eq!(done.outcome.as_deref(), Some("enabled"));
        assert_eq!(done.detail.as_deref(), Some("done"));
        let failed = step_failed("boom".to_string());
        assert_eq!(failed.status, STEP_FAILED);
        assert_eq!(failed.outcome.as_deref(), Some(OUTCOME_FAILED));
        assert_eq!(failed.detail.as_deref(), Some("boom"));
        let blocked = step_blocked("waiting".to_string());
        assert_eq!(blocked.status, STEP_BLOCKED);
        assert!(blocked.outcome.is_none());
        assert_eq!(blocked.detail.as_deref(), Some("waiting"));
    }

    #[test]
    fn retry_failed_with_all_steps_skipped_still_hits_attempt_limit() {
        // Defect 2: the cap used to be skipped on RetryFailed when
        // no step was failed or blocked, so retrying a purchase
        // whose steps were all skipped consumed another attempt.
        let state = state_with_registry();
        let package = seed_package(&state, vec!["missing-module"]);
        let purchase = seed_purchase(&state, &package, 3);
        record_step(
            &state,
            purchase.id,
            STEP_VALIDATE_ENTITLEMENTS,
            3,
            StepDecision {
                status: STEP_SKIPPED,
                outcome: Some("skipped".to_string()),
                detail: Some("Not reached: an earlier step failed or was blocked".to_string()),
            },
        );
        let error = run_provisioning(
            &state,
            purchase.id,
            "test",
            ProvisionMode::RetryFailed,
            None,
        )
        .expect_err("retry at the attempt limit must be rejected");
        assert_eq!(error.code, 409);
        assert!(error.message.contains("attempt limit"), "{}", error.message);
        assert_eq!(stored_purchase(&state, purchase.id).provision_attempts, 3);
    }

    #[test]
    fn full_run_at_attempt_limit_is_rejected() {
        let state = state_with_registry();
        let package = seed_package(&state, vec![]);
        let purchase = seed_purchase(&state, &package, 3);
        let error = run_provisioning(&state, purchase.id, "test", ProvisionMode::Full, None)
            .expect_err("full run at the attempt limit must be rejected");
        assert_eq!(error.code, 409);
        assert!(error.message.contains("attempt limit"), "{}", error.message);
        assert_eq!(stored_purchase(&state, purchase.id).provision_attempts, 3);
    }

    #[test]
    fn retry_failed_below_limit_runs_even_with_only_skipped_steps() {
        let state = state_with_registry();
        let package = seed_package(&state, vec!["missing-module"]);
        let purchase = seed_purchase(&state, &package, 2);
        record_step(
            &state,
            purchase.id,
            STEP_VALIDATE_ENTITLEMENTS,
            2,
            StepDecision {
                status: STEP_SKIPPED,
                outcome: Some("skipped".to_string()),
                detail: Some("Not reached: an earlier step failed or was blocked".to_string()),
            },
        );
        // Below the cap the retry runs (and fails again on the
        // unknown module), consuming exactly one more attempt.
        let summary = run_provisioning(&state, purchase.id, "test", ProvisionMode::RetryFailed, None)
            .expect("retry below the limit must run");
        assert_eq!(summary.attempt, 3);
        assert_eq!(stored_purchase(&state, purchase.id).provision_attempts, 3);
        assert_eq!(summary.status, PURCHASE_FAILED);
    }

    #[test]
    fn parse_module_outcomes_returns_empty_without_a_step() {
        let state = SaasPackagesState::new();
        let id = Uuid::now_v7();
        assert!(parse_module_outcomes(&state, id).expect("no step").is_empty());
    }

    #[test]
    fn parse_module_outcomes_parses_valid_detail() {
        let state = SaasPackagesState::new();
        let purchase_id = Uuid::now_v7();
        record_step(
            &state,
            purchase_id,
            STEP_ENABLE_MODULES,
            1,
            StepDecision {
                status: STEP_COMPLETED,
                outcome: Some(OUTCOME_ENABLED.to_string()),
                detail: Some(
                    r#"[{"module_id":"m1","status":"enabled","auto_activated":[]}]"#.to_string(),
                ),
            },
        );
        let outcomes = parse_module_outcomes(&state, purchase_id).expect("valid detail");
        assert_eq!(outcomes.len(), 1);
        assert_eq!(outcomes[0].module_id, "m1");
        assert_eq!(outcomes[0].status, OUTCOME_ENABLED);
    }

    #[test]
    fn parse_module_outcomes_rejects_malformed_detail() {
        let state = SaasPackagesState::new();
        let purchase_id = Uuid::now_v7();
        record_step(
            &state,
            purchase_id,
            STEP_ENABLE_MODULES,
            1,
            StepDecision {
                status: STEP_COMPLETED,
                outcome: Some(OUTCOME_ENABLED.to_string()),
                detail: Some("not-json{{{".to_string()),
            },
        );
        let error = parse_module_outcomes(&state, purchase_id)
            .expect_err("malformed detail must not default to empty");
        assert_eq!(error.code, 500);
        assert!(error.message.contains("could not be parsed"), "{}", error.message);
    }

    #[test]
    fn malformed_stored_outcomes_refuse_the_run_without_consuming_an_attempt() {
        // Defect 3: the run used to swallow a malformed stored
        // enable-modules detail and restart with empty outcomes.
        let state = state_with_registry();
        let package = seed_package(&state, vec![]);
        let purchase = seed_purchase(&state, &package, 0);
        record_step(
            &state,
            purchase.id,
            STEP_ENABLE_MODULES,
            1,
            StepDecision {
                status: STEP_COMPLETED,
                outcome: Some(OUTCOME_ENABLED.to_string()),
                detail: Some("not-json{{{".to_string()),
            },
        );
        let error = run_provisioning(&state, purchase.id, "test", ProvisionMode::Full, None)
            .expect_err("malformed stored outcomes must refuse the run");
        assert_eq!(error.code, 500);
        assert!(error.message.contains("could not be parsed"), "{}", error.message);
        // Nothing was mutated: no attempt consumed, status untouched.
        let stored = stored_purchase(&state, purchase.id);
        assert_eq!(stored.provision_attempts, 0);
        assert_eq!(stored.status, PURCHASE_FAILED);
    }
}
