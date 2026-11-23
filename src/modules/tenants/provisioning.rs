//! tenants - provisioning.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::json;
use unified_core::tenancy::{schema_name};
use crate::modules::licensing::{
    check_module_entitlement, has_module_entitlement, has_subscription_entitlement,
    subscription_entitlement_id,
};
use chrono::{DateTime, Utc};
use serde_json::{json, Value};
use uuid::Uuid;
use super::{constants::*, vocabulary::*, events::*, settings::*, records::*, state::*, auth::*, helpers::*, destroy::*, handlers::*, module::*, tenancy::*};



// ---------------------------------------------------------------------------
// Provisioning pipeline (idempotent, resumable, audit-logged).
// ---------------------------------------------------------------------------
/// Record a step row for (tenant, step) if none exists yet, and
/// return it. Re-running a provision never duplicates step rows:
/// the (tenant, step) key is unique in the store and in the
/// migration table. The insert is atomic (`entry().or_insert_with`),
/// so concurrent pipeline runs cannot both create the row.
pub(crate) fn record_step(state: &TenantsState, tenant_id: Uuid, step: &str) -> ProvisioningStep {
    let key = TenantsState::step_key(tenant_id, step);
    state
        .steps
        .entry(key)
        .or_insert_with(|| {
            let timestamp = now();
            ProvisioningStep {
                id: Uuid::now_v7(),
                tenant_id,
                step: step.to_string(),
                status: STEP_PENDING.to_string(),
                attempt: 0,
                error: None,
                detail: json!({}),
                started_at: None,
                completed_at: None,
                created_at: timestamp,
                updated_at: timestamp,
            }
        })
        .value()
        .clone()
}


/// Mark a step running (attempt counter included, so a retried
/// step shows how many times it was attempted).
pub(crate) fn start_step(state: &TenantsState, step: &mut ProvisioningStep) {
    step.status = STEP_RUNNING.to_string();
    step.attempt += 1;
    step.error = None;
    step.started_at = Some(now());
    step.updated_at = now();
    state.steps.insert(
        TenantsState::step_key(step.tenant_id, &step.step),
        step.clone(),
    );
}


/// Mark a step completed with a detail payload.
pub(crate) fn complete_step(state: &TenantsState, step: &mut ProvisioningStep, detail: Value) {
    step.status = STEP_COMPLETED.to_string();
    step.detail = detail;
    step.completed_at = Some(now());
    step.updated_at = now();
    state.steps.insert(
        TenantsState::step_key(step.tenant_id, &step.step),
        step.clone(),
    );
}


/// Mark a step failed with the error. A failed step stops the
/// pipeline and the tenant goes to `provision_failed`.
pub(crate) fn fail_step(state: &TenantsState, step: &mut ProvisioningStep, error: String) {
    step.status = STEP_FAILED.to_string();
    step.error = Some(error);
    step.completed_at = Some(now());
    step.updated_at = now();
    state.steps.insert(
        TenantsState::step_key(step.tenant_id, &step.step),
        step.clone(),
    );
}


/// Mark a step skipped with the reason. Skipped is a recorded
/// outcome, not a failure: the pipeline continues.
pub(crate) fn skip_step(state: &TenantsState, step: &mut ProvisioningStep, reason: String) {
    step.status = STEP_SKIPPED.to_string();
    step.detail = json!({ "reason": reason });
    step.completed_at = Some(now());
    step.updated_at = now();
    state.steps.insert(
        TenantsState::step_key(step.tenant_id, &step.step),
        step.clone(),
    );
}


/// Resolve the module ids a tenant should be granted, in priority
/// order:
///   1. explicit `module_ids` on the create request;
///   2. the package's module list, asked through the
///      `saas_packages.package.modules_requested` event;
///   3. the configured `default_module_ids` from the tenant
///      settings.
/// A preset id alone resolves no modules: the presets module
/// exposes no `modules_requested` event today, so its module list
/// is resolved by the presets apply flow, not here (see the module
/// header and the report). An unanswered package event resolves
/// to an empty list, never to an error: the grant step records
/// that outcome on the step detail.
pub(crate) fn resolve_module_ids(
    ctx: &AppContext,
    state: &TenantsState,
    tenant: &Tenant,
    explicit: &Option<Vec<String>>,
) -> (Vec<String>, Vec<SkippedModule>) {
    let mut skipped: Vec<SkippedModule> = Vec::new();
    if let Some(ids) = explicit {
        let mut cleaned: Vec<String> = Vec::new();
        for id in ids {
            let trimmed = id.trim().to_string();
            if !trimmed.is_empty() && !cleaned.contains(&trimmed) {
                cleaned.push(trimmed);
            }
        }
        return (cleaned, skipped);
    }
    if let Some(package_id) = tenant.package_id {
        let answer = ctx.hooks.emit(
            EVENT_PACKAGE_MODULES_REQUESTED,
            json!({ "package_id": package_id }),
        );
        for result in &answer.results {
            if let Some(ids) = result.get("module_ids").and_then(Value::as_array) {
                let mut cleaned: Vec<String> = Vec::new();
                for id in ids {
                    if let Some(value) = id.as_str() {
                        let trimmed = value.trim().to_string();
                        if !trimmed.is_empty() && !cleaned.contains(&trimmed) {
                            cleaned.push(trimmed);
                        }
                    }
                }
                return (cleaned, skipped);
            }
        }
        skipped.push(SkippedModule {
            module_id: String::new(),
            reason: format!(
                "Package \"{package_id}\" answered no module list; no package modules granted"
            ),
        });
        return (Vec::new(), skipped);
    }
    if tenant.preset_id.is_some() {
        skipped.push(SkippedModule {
            module_id: String::new(),
            reason: "Preset module lists are resolved by the presets apply flow; \
                a preset id alone grants no modules here"
                .to_string(),
        });
    }
    (state.settings().default_module_ids, skipped)
}


/// Decide how a granted module is recorded (`granted_via`), from
/// the purchase vehicle first and the module registry second:
/// package / preset when one was named, `builtin` for modules
/// that are not sellable, `owner_entitlement` for sellable core
/// modules, and for sellable non-core modules the live source the
/// entitlement check just passed on: `license` when a license
/// names the module, `subscription` when an active subscription
/// covers it instead. The subscription leg must be named here:
/// the entitlements mirror turns it into a stored subscription
/// row whose `source_ref` is the subscription id, which is what
/// a later subscription-cancel lapses the row through.
pub(crate) fn granted_via_for(ctx: &AppContext, tenant: &Tenant, module_id: &str) -> String {
    if tenant.package_id.is_some() {
        return "package".to_string();
    }
    if tenant.preset_id.is_some() {
        return "preset".to_string();
    }
    match ctx.modules.get(module_id) {
        None => "builtin".to_string(),
        Some(module) => {
            if !module.available_as_product() {
                "builtin".to_string()
            } else if module.is_core() {
                "owner_entitlement".to_string()
            } else if has_module_entitlement(ctx, &tenant.id.to_string(), module_id) {
                "license".to_string()
            } else if has_subscription_entitlement(ctx, &tenant.id.to_string(), module_id) {
                "subscription".to_string()
            } else {
                // The gate already passed, so a license or a
                // subscription must hold; if neither re-check
                // agrees (a race with a revocation between the
                // gate and this call), keep the old label.
                "license".to_string()
            }
        }
    }
}


/// Grant one module to one tenant after the entitlement check.
/// Returns the grant on success, or the skip reason on any
/// failure. The entitlement check is the gate: a module the
/// tenant is not entitled to is never recorded as granted, and an
/// unknown module id is never granted either (fail closed). An
/// expiry in the past is rejected outright: a grant recorded with
/// an already-past `expires_at` would look live while being dead.
pub(crate) fn grant_one_module(
    ctx: &AppContext,
    state: &TenantsState,
    tenant: &Tenant,
    module_id: &str,
    actor: &str,
    expires_at: Option<DateTime<Utc>>,
) -> Result<ModuleGrant, String> {
    if let Some(expires_at) = expires_at {
        if expires_at <= now() {
            return Err(format!(
                "Module \"{module_id}\" cannot be granted with an expiry in the past ({expires_at})"
            ));
        }
    }
    if state.live_grant(tenant.id, module_id).is_some() {
        return Err(format!("Module \"{module_id}\" is already granted"));
    }
    if ctx.modules.get(module_id).is_none() {
        return Err(format!(
            "Module \"{module_id}\" is not registered; it cannot be granted until its module registers"
        ));
    }
    let company_id = tenant.id.to_string();
    // The entitlement check is the gate (see the doc above): a
    // module the tenant is not entitled to is never recorded as
    // granted. Grace is not an entitlement here either — the gate
    // consults only live sources, so a grace-state module is
    // refused exactly as `enable_module` refuses it (single grace
    // policy, see `EntitlementsState::grace_applies`).
    if let Err(error) = check_module_entitlement(ctx, &company_id, module_id) {
        return Err(format!(
            "Module \"{module_id}\" is not entitled for this tenant: {}",
            error.message
        ));
    }
    let granted_via = granted_via_for(ctx, tenant, module_id);
    // The purchase id behind a subscription grant: the store's
    // subscription id is a string, so it only fits
    // `granted_via_id` when it parses as a UUID. The raw id
    // always rides the grant event as `source_ref`, which is what
    // the entitlements mirror stores on the row — and what a
    // later subscription-cancel lapses the row through.
    let subscription_id = if granted_via == "subscription" {
        subscription_entitlement_id(ctx, &company_id, module_id)
    } else {
        None
    };
    let granted_via_id = match granted_via.as_str() {
        "package" => tenant.package_id,
        "preset" => tenant.preset_id,
        "subscription" => subscription_id
            .as_deref()
            .and_then(|id| Uuid::parse_str(id).ok()),
        _ => None,
    };
    let grant = ModuleGrant {
        id: Uuid::now_v7(),
        tenant_id: tenant.id,
        module_id: module_id.to_string(),
        granted_via,
        granted_via_id,
        granted_at: now(),
        expires_at,
        granted_by: Some(actor.to_string()),
        revoked_at: None,
        revoke_reason: None,
    };
    state.grants.insert(grant.id, grant.clone());
    ctx.hooks.emit(
        EVENT_TENANT_MODULE_GRANTED,
        json!({
            "tenant_id": tenant.id,
            "company_id": tenant.id,
            "module_id": module_id,
            "granted_via": grant.granted_via,
            "granted_via_id": grant.granted_via_id,
            "source_ref": subscription_id,
            "actor_id": actor,
        }),
    );
    emit_audit(
        ctx,
        EVENT_TENANT_MODULE_GRANTED,
        actor,
        "tenant.module.granted",
        "tenant",
        &tenant.id.to_string(),
        Some(tenant.id),
        json!({ "module_id": module_id, "granted_via": grant.granted_via }),
    );
    Ok(grant)
}


/// Ask the domains module to claim the tenant's subdomain by
/// emitting `site.provision_requested`. The domains module owns
/// allocation: this function never touches its stores. The result
/// is `true` when a handler claims the label, `false` with the
/// recorded reason otherwise. A declined claim is a skip, not a
/// pipeline failure: the tenant keeps `subdomain_pending: true`
/// and the claim is retried on the next provisioning run.
///
/// Known convergence gap at write time: the domains handler only
/// trusts source `saas_packages` and only claims product
/// subdomains, so a tenant-level claim from this module is
/// declined today. The decline is recorded verbatim on the step
/// so the integrator can see exactly what to fix.
pub(crate) fn register_subdomain_step(
    ctx: &AppContext,
    state: &TenantsState,
    tenant: &mut Tenant,
) -> (bool, String) {
    let mut payload = json!({
        "tenant_id": tenant.id,
        "company_id": tenant.id,
        "subdomain": tenant.slug,
        "source": "tenants",
    });
    if let Some(package_id) = tenant.package_id {
        payload["package_id"] = json!(package_id);
    }
    let answer = ctx.hooks.emit(EVENT_SITE_PROVISION_REQUESTED, payload);
    for result in &answer.results {
        let claimed = result
            .get("claimed")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let has_site = result.get("site_id").is_some() || result.get("domain").is_some();
        if claimed || has_site {
            tenant.subdomain = Some(tenant.slug.clone());
            tenant.subdomain_pending = false;
            tenant.updated_at = now();
            state.tenants.insert(tenant.id, tenant.clone());
            return (true, String::new());
        }
        if let Some(reason) = result.get("reason").and_then(Value::as_str) {
            tenant.subdomain_pending = true;
            tenant.updated_at = now();
            state.tenants.insert(tenant.id, tenant.clone());
            return (false, reason.to_string());
        }
    }
    tenant.subdomain_pending = true;
    tenant.updated_at = now();
    state.tenants.insert(tenant.id, tenant.clone());
    (
        false,
        "No domains handler claimed the subdomain request".to_string(),
    )
}


/// Run the provisioning pipeline for one tenant, resuming at the
/// first incomplete step. Returns `true` when the tenant reached
/// `active`. Every step is audit-logged through its event plus an
/// audit record; a failed step marks the tenant
/// `provision_failed` and stops the pipeline.
pub(crate) fn run_provisioning_pipeline(
    ctx: &AppContext,
    state: &TenantsState,
    tenant_id: Uuid,
    actor: &str,
    explicit_module_ids: &Option<Vec<String>>,
) -> bool {
    let Ok(mut tenant) = get_tenant_by_id(state, tenant_id) else {
        return false;
    };
    for step_name in PROVISION_STEPS {
        let mut step = record_step(state, tenant_id, step_name);
        if step.status == STEP_COMPLETED {
            continue;
        }
        start_step(state, &mut step);
        match *step_name {
            STEP_CREATE_RECORD => {
                complete_step(
                    state,
                    &mut step,
                    json!({ "tenant_id": tenant.id, "slug": tenant.slug }),
                );
            }
            STEP_CREATE_SCHEMA => {
                let tenancy = tenant_context_for(tenant.id);
                let schema = tenant_context_schema(&tenancy).to_string();
                match db_pool(ctx) {
                    None => {
                        fail_step(
                            state,
                            &mut step,
                            "Database pool is not attached; the tenant schema cannot be created"
                                .to_string(),
                        );
                        return mark_provision_failed(ctx, state, &mut tenant, actor);
                    }
                    Some(pool) => match ensure_tenant_schema(&pool, tenant.id) {
                        Ok(()) => complete_step(
                            state,
                            &mut step,
                            json!({ "schema": schema }),
                        ),
                        Err(error) => {
                            fail_step(
                                state,
                                &mut step,
                                format!("Tenant schema creation failed: {}", error.message),
                            );
                            return mark_provision_failed(ctx, state, &mut tenant, actor);
                        }
                    },
                }
            }
            STEP_RUN_MIGRATIONS => {
                let tenancy = tenant_context_for(tenant.id);
                let schema = tenant_context_schema(&tenancy).to_string();
                match db_pool(ctx) {
                    None => {
                        fail_step(
                            state,
                            &mut step,
                            "Database pool is not attached; tenant migrations cannot run"
                                .to_string(),
                        );
                        return mark_provision_failed(ctx, state, &mut tenant, actor);
                    }
                    Some(pool) => match migrate_tenant_schema(&pool, tenant.id) {
                        Ok(()) => complete_step(
                            state,
                            &mut step,
                            json!({ "schema": schema }),
                        ),
                        Err(error) => {
                            fail_step(
                                state,
                                &mut step,
                                format!("Tenant migrations failed: {}", error.message),
                            );
                            return mark_provision_failed(ctx, state, &mut tenant, actor);
                        }
                    },
                }
            }
            STEP_REGISTER_SUBDOMAIN => {
                let (claimed, reason) = register_subdomain_step(ctx, state, &mut tenant);
                if claimed {
                    complete_step(
                        state,
                        &mut step,
                        json!({ "subdomain": tenant.slug, "claimed": true }),
                    );
                } else {
                    skip_step(state, &mut step, reason);
                }
            }
            STEP_GRANT_MODULES => {
                let (module_ids, mut skipped) =
                    resolve_module_ids(ctx, state, &tenant, explicit_module_ids);
                let (grants, more_skipped) =
                    grant_modules_for_tenant(ctx, state, &tenant, &module_ids, actor, None);
                let granted: Vec<String> =
                    grants.iter().map(|grant| grant.module_id.clone()).collect();
                skipped.extend(more_skipped);
                complete_step(
                    state,
                    &mut step,
                    json!({ "granted": granted, "skipped": skipped }),
                );
            }
            STEP_ACTIVATE => {
                match transition_tenant(state, &mut tenant, STATUS_ACTIVE) {
                    Ok(()) => {
                        complete_step(
                            state,
                            &mut step,
                            json!({ "status": STATUS_ACTIVE }),
                        );
                    }
                    Err(error) => {
                        fail_step(state, &mut step, error.message.clone());
                        return mark_provision_failed(ctx, state, &mut tenant, actor);
                    }
                }
            }
            _ => {
                skip_step(
                    state,
                    &mut step,
                    format!("Unknown provisioning step \"{step_name}\""),
                );
            }
        }
    }
    ctx.hooks.emit(
        EVENT_TENANT_PROVISIONED,
        json!({
            "tenant_id": tenant.id,
            "company_id": tenant.id,
            "slug": tenant.slug,
            "schema": tenant.schema_name,
            "subdomain_pending": tenant.subdomain_pending,
            "actor_id": actor,
        }),
    );
    emit_audit(
        ctx,
        EVENT_TENANT_PROVISIONED,
        actor,
        "tenant.provisioned",
        "tenant",
        &tenant.id.to_string(),
        Some(tenant.id),
        json!({ "slug": tenant.slug, "schema": tenant.schema_name }),
    );
    true
}


/// Load a tenant by id only (the pipeline's internal lookup; the
/// public lookup also accepts slugs — see `get_tenant`).
pub(crate) fn get_tenant_by_id(state: &TenantsState, tenant_id: Uuid) -> UnifiedResult<Tenant> {
    state
        .tenants
        .get(&tenant_id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Tenant \"{tenant_id}\" not found")))
}


/// Mark a tenant `provision_failed` after a failed step, emit the
/// failure event plus an audit record, and report that the
/// pipeline did not complete. The tenant stays retryable.
///
/// INTENTIONAL state-machine bypass (by design, not an oversight):
/// the tenant lifecycle graph has no edge into `provision_failed`
/// from statuses other than `provisioning` (see `vocabulary.rs`),
/// so when a step fails while the tenant is in another status (for
/// example `active` during a re-run) the transition is rejected.
/// Routing it through the machine would require adding an edge
/// from every status to `provision_failed`, which would let any
/// failure silently demote a live tenant through the normal
/// transition path. Instead the failure is logged and the status
/// is set explicitly here — and the event + audit record the
/// machine transition would have emitted are still emitted below,
/// so observers see exactly the same failure signal.
pub(crate) fn mark_provision_failed(
    ctx: &AppContext,
    state: &TenantsState,
    tenant: &mut Tenant,
    actor: &str,
) -> bool {
    if let Err(error) = transition_tenant(state, tenant, STATUS_PROVISION_FAILED) {
        log::warn!(
            "Tenant {} could not transition to {} ({}); setting the status explicitly",
            tenant.id,
            STATUS_PROVISION_FAILED,
            error.message
        );
        tenant.status = STATUS_PROVISION_FAILED.to_string();
        tenant.updated_at = now();
        state.tenants.insert(tenant.id, tenant.clone());
    }
    ctx.hooks.emit(
        EVENT_TENANT_PROVISION_FAILED,
        json!({
            "tenant_id": tenant.id,
            "company_id": tenant.id,
            "slug": tenant.slug,
            "actor_id": actor,
        }),
    );
    emit_audit(
        ctx,
        EVENT_TENANT_PROVISION_FAILED,
        actor,
        "tenant.provision_failed",
        "tenant",
        &tenant.id.to_string(),
        Some(tenant.id),
        json!({ "slug": tenant.slug, "status": STATUS_PROVISION_FAILED }),
    );
    false
}



// ---------------------------------------------------------------------------
// Shared grant helper (used by the provisioning step and by the
// grant endpoint, so the entitlement gate lives in one place).
// ---------------------------------------------------------------------------
/// Grant several modules to one tenant, entitlement-checking each
/// one. Returns the grants that succeeded and the skips (recorded
/// with reasons, never silent). A failed entitlement never blocks
/// the other modules.
pub(crate) fn grant_modules_for_tenant(
    ctx: &AppContext,
    state: &TenantsState,
    tenant: &Tenant,
    module_ids: &[String],
    actor: &str,
    expires_at: Option<DateTime<Utc>>,
) -> (Vec<ModuleGrant>, Vec<SkippedModule>) {
    let mut granted: Vec<ModuleGrant> = Vec::new();
    let mut skipped: Vec<SkippedModule> = Vec::new();
    for module_id in module_ids {
        match grant_one_module(ctx, state, tenant, module_id, actor, expires_at) {
            Ok(grant) => granted.push(grant),
            Err(reason) => {
                skipped.push(SkippedModule {
                    module_id: module_id.clone(),
                    reason: reason.clone(),
                });
                ctx.hooks.emit(
                    EVENT_TENANT_MODULE_SKIPPED,
                    json!({
                        "tenant_id": tenant.id,
                        "company_id": tenant.id,
                        "module_id": module_id,
                        "reason": reason,
                        "actor_id": actor,
                    }),
                );
                emit_audit(
                    ctx,
                    EVENT_TENANT_MODULE_SKIPPED,
                    actor,
                    "tenant.module.skipped",
                    "tenant",
                    &tenant.id.to_string(),
                    Some(tenant.id),
                    json!({ "module_id": module_id, "reason": reason }),
                );
            }
        }
    }
    (granted, skipped)
}

#[cfg(test)]
mod tests {
    use super::*;
    use unified_core::config::AppConfig;
    use unified_core::hooks::HookBus;
    use unified_core::modules::ModuleRegistry;
    use chrono::Duration;
    use std::sync::Arc;

    fn test_context() -> AppContext {
        AppContext::new(
            AppConfig::default(),
            Arc::new(HookBus::new()),
            Arc::new(ModuleRegistry::new()),
        )
    }

    fn tenant_with_status(status: &str) -> Tenant {
        Tenant {
            id: Uuid::now_v7(),
            slug: "acme".to_string(),
            display_name: "Acme".to_string(),
            status: status.to_string(),
            owner_user_id: Uuid::now_v7(),
            package_id: None,
            preset_id: None,
            plan_id: None,
            subdomain: None,
            subdomain_pending: false,
            schema_name: "tenant_acme".to_string(),
            suspended_at: None,
            suspended_reason: None,
            destroyed_at: None,
            export_reference: None,
            created_at: now(),
            updated_at: now(),
        }
    }

    #[test]
    fn record_step_creates_only_one_row_under_concurrency() {
        let state = TenantsState::default();
        let tenant_id = Uuid::now_v7();
        let ids: Vec<Uuid> = std::thread::scope(|scope| {
            let handles: Vec<_> = (0..8)
                .map(|_| {
                    let state = state.clone();
                    scope.spawn(move || {
                        record_step(&state, tenant_id, STEP_CREATE_RECORD).id
                    })
                })
                .collect();
            handles
                .into_iter()
                .map(|handle| handle.join().expect("record thread panicked"))
                .collect()
        });
        assert!(ids.iter().all(|id| id == &ids[0]));
        assert_eq!(state.steps.len(), 1);
    }

    #[test]
    fn mark_provision_failed_forces_failed_status_when_no_edge_exists() {
        let ctx = test_context();
        let state = TenantsState::default();
        // `active` has no edge to `provision_failed`: the tenant must
        // not stay active while a step has failed.
        let mut active = tenant_with_status("active");
        state.tenants.insert(active.id, active.clone());
        assert!(!mark_provision_failed(&ctx, &state, &mut active, "tester"));
        assert_eq!(active.status.as_str(), STATUS_PROVISION_FAILED);
        assert_eq!(
            state.tenants.get(&active.id).unwrap().status.as_str(),
            STATUS_PROVISION_FAILED
        );
        // The normal path still goes through the state machine.
        let mut provisioning = tenant_with_status("provisioning");
        state.tenants.insert(provisioning.id, provisioning.clone());
        assert!(!mark_provision_failed(
            &ctx,
            &state,
            &mut provisioning,
            "tester"
        ));
        assert_eq!(provisioning.status.as_str(), STATUS_PROVISION_FAILED);
    }

    #[test]
    fn grant_one_module_rejects_past_expiry() {
        let ctx = test_context();
        let state = TenantsState::default();
        let tenant = tenant_with_status("active");
        let past = now() - Duration::hours(1);
        let error =
            grant_one_module(&ctx, &state, &tenant, "printing", "tester", Some(past))
                .expect_err("a past expiry must be rejected");
        assert!(error.contains("past"), "unexpected error: {error}");
        // A future expiry passes the expiry gate (the unregistered
        // module still fails closed at the next gate), as does none.
        let future = now() + Duration::hours(1);
        let error =
            grant_one_module(&ctx, &state, &tenant, "printing", "tester", Some(future))
                .expect_err("an unregistered module must still fail closed");
        assert!(
            error.contains("not registered"),
            "unexpected error: {error}"
        );
        assert!(grant_one_module(&ctx, &state, &tenant, "printing", "tester", None)
            .is_err());
    }
}
