//! tenants - handlers.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::json;
use unified_core::pagination::{PaginationParams};
use unified_core::tenancy::{schema_name};
use actix_web::{web, HttpRequest, HttpResponse};
use serde_json::{json, Value};
use std::sync::{Arc};
use uuid::Uuid;
use super::{constants::*, vocabulary::*, events::*, settings::*, records::*, state::*, auth::*, helpers::*, provisioning::*, destroy::*, module::*, tenancy::*};



// ---------------------------------------------------------------------------
// Handlers.
// ---------------------------------------------------------------------------
/// Atomically claim a slug for a tenant: under the slug-claim lock,
/// reject with 409 when another tenant already holds the slug and
/// insert the tenant otherwise. The check and the insert happen
/// under one lock hold, so two concurrent creates with the same
/// slug cannot both pass the check before either inserts. The
/// handler still runs its own pre-check first (for error
/// precedence); this is the atomic second check at insert time.
pub(crate) fn claim_tenant(state: &TenantsState, tenant: &Tenant) -> UnifiedResult<()> {
    let _guard = state
        .slug_claim_lock
        .lock()
        .expect("tenants slug-claim lock poisoned");
    if state
        .tenants
        .iter()
        .any(|entry| entry.value().slug == tenant.slug)
    {
        return Err(UnifiedError::conflict(format!(
            "Slug \"{}\" is already claimed by another tenant",
            tenant.slug
        ))
        .with_metadata("slug", json!(tenant.slug)));
    }
    state.tenants.insert(tenant.id, tenant.clone());
    Ok(())
}


/// List tenants (paginated; filter by `status`, `owner_user_id`,
/// or `package_id`; `search` matches slug or display name).
pub(crate) async fn list_tenants(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<TenantsState>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_TENANTS_VIEW)?;
        let params = query.into_inner();
        let status_filter = params.filters.get("status").cloned();
        let owner_filter = params.filters.get("owner_user_id").cloned();
        let package_filter = params.filters.get("package_id").cloned();
        let search = params.search.clone().map(|value| value.to_ascii_lowercase());
        let mut items: Vec<Tenant> = state
            .tenants
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|tenant| {
                if let Some(status) = &status_filter {
                    if tenant.status != *status {
                        return false;
                    }
                }
                if let Some(owner) = &owner_filter {
                    if tenant.owner_user_id.to_string() != *owner {
                        return false;
                    }
                }
                if let Some(package) = &package_filter {
                    if tenant.package_id.map(|id| id.to_string()) != Some(package.clone()) {
                        return false;
                    }
                }
                if let Some(needle) = &search {
                    let haystack = format!("{} {}", tenant.slug, tenant.display_name)
                        .to_ascii_lowercase();
                    if !haystack.contains(needle.as_str()) {
                        return false;
                    }
                }
                true
            })
            .collect();
        let sort_key = params.sort.clone().unwrap_or_else(|| "created_at".to_string());
        items.sort_by(|a, b| match sort_key.as_str() {
            "slug" => a.slug.cmp(&b.slug),
            "display_name" => a.display_name.cmp(&b.display_name),
            _ => a.created_at.cmp(&b.created_at),
        });
        if params.order.as_deref() == Some("desc") {
            items.reverse();
        }
        Ok(paginate(items, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}


/// Create a tenant and start provisioning. The tenant id is a
/// server-generated uuidv7 and the schema name is derived from
/// it; neither is ever accepted from the client. Returns 201 with
/// the tenant record: when the synchronous pipeline attempt
/// finishes the status is `active`, and when a step fails it is
/// `provision_failed` (retryable through the retry endpoint).
/// The status endpoint (`GET /{id}/provision`) is the polling
/// contract for asynchronous runtimes.
pub(crate) async fn create_tenant(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<TenantsState>,
    req: HttpRequest,
    body: web::Json<CreateTenantRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_TENANTS_PROVISION)?;
        let settings = state.settings();
        let request = body.into_inner();
        let slug = validate_slug(&request.slug, &settings)?;
        let display_name = request.display_name.trim().to_string();
        if display_name.is_empty() {
            return Err(UnifiedError::bad_request("display_name must not be empty"));
        }
        if display_name.chars().count() > 255 {
            return Err(UnifiedError::bad_request(
                "display_name must be at most 255 characters",
            ));
        }
        if state
            .tenants
            .iter()
            .any(|entry| entry.value().slug == slug)
        {
            return Err(UnifiedError::conflict(format!(
                "Slug \"{slug}\" is already claimed by another tenant"
            ))
            .with_metadata("slug", json!(slug)));
        }
        if settings.max_tenants_per_owner > 0 {
            let owned = state
                .tenants
                .iter()
                .filter(|entry| entry.value().owner_user_id == request.owner_user_id)
                .count() as u32;
            if owned >= settings.max_tenants_per_owner {
                return Err(UnifiedError::forbidden(format!(
                    "Owner already holds the maximum of {} tenants",
                    settings.max_tenants_per_owner
                ))
                .with_metadata("owner_user_id", json!(request.owner_user_id)));
            }
        }
        let id = Uuid::now_v7();
        let timestamp = now();
        let tenant = Tenant {
            id,
            slug: slug.clone(),
            display_name,
            status: STATUS_PROVISIONING.to_string(),
            owner_user_id: request.owner_user_id,
            package_id: request.package_id,
            preset_id: request.preset_id,
            plan_id: request.plan_id.map(|value| value.trim().to_string()),
            subdomain: None,
            subdomain_pending: false,
            schema_name: tenant_schema_name(id),
            suspended_at: None,
            suspended_reason: None,
            destroyed_at: None,
            export_reference: None,
            created_at: timestamp,
            updated_at: timestamp,
        };
        // Atomic claim: the slug check and the insert happen under
        // one lock hold (see `claim_tenant`).
        claim_tenant(&state, &tenant)?;
        for step_name in PROVISION_STEPS {
            record_step(&state, id, step_name);
        }
        ctx.hooks.emit(
            EVENT_TENANT_CREATED,
            json!({
                "tenant_id": id,
                "company_id": id,
                "slug": slug,
                "owner_user_id": tenant.owner_user_id,
                "package_id": tenant.package_id,
                "preset_id": tenant.preset_id,
                "actor_id": caller.actor,
            }),
        );
        emit_audit(
            &ctx,
            EVENT_TENANT_CREATED,
            &caller.actor,
            "tenant.created",
            "tenant",
            &id.to_string(),
            Some(id),
            json!({ "slug": slug, "schema": tenant.schema_name }),
        );
        let explicit_module_ids = request.module_ids.clone();
        run_provisioning_pipeline(&ctx, &state, id, &caller.actor, &explicit_module_ids);
        let tenant = get_tenant_by_id(&state, id)?;
        Ok(json!(tenant))
    })();
    match result {
        Ok(data) => created(data),
        Err(error) => err(error, debug),
    }
}


/// Get the admin settings for tenant defaults (slug rules, grace
/// defaults, quotas, default modules).
pub(crate) async fn get_settings(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<TenantsState>,
    req: HttpRequest,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_TENANTS_VIEW)?;
        Ok(json!(state.settings()))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}


/// Update the admin settings for tenant defaults. The new value
/// is validated before it is stored; slug rules apply to every
/// tenant created afterwards, never retroactively.
pub(crate) async fn update_settings(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<TenantsState>,
    req: HttpRequest,
    body: web::Json<TenantsSettings>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_TENANTS_MANAGE)?;
        let settings = body.into_inner().validated()?;
        {
            let mut stored = state
                .settings
                .write()
                .expect("tenants settings lock poisoned");
            *stored = settings.clone();
        }
        ctx.hooks.emit(
            EVENT_TENANT_SETTINGS_UPDATED,
            json!({ "actor_id": caller.actor }),
        );
        emit_audit(
            &ctx,
            EVENT_TENANT_SETTINGS_UPDATED,
            &caller.actor,
            "tenant.settings.updated",
            "tenants_settings",
            "tenants",
            None,
            json!({}),
        );
        Ok(json!(settings))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}


/// Get one tenant by id or slug.
pub(crate) async fn get_tenant_handler(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<TenantsState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_TENANTS_VIEW)?;
        let tenant = get_tenant(&state, &path.into_inner())?;
        Ok(json!(tenant))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}


/// Get the resolved tenant context for one tenant: the values
/// every other module must carry to resolve tenant data (tenant
/// id, schema, status, entitlement scope). Fails closed: an
/// unknown tenant is a 404, never a control-plane fallback.
pub(crate) async fn get_tenant_context(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<TenantsState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_TENANTS_VIEW)?;
        let tenant = get_tenant(&state, &path.into_inner())?;
        let tenancy = tenant_context_for(tenant.id);
        let view = TenantContextView {
            tenant_id: tenant.id,
            schema: tenant_context_schema(&tenancy).to_string(),
            status: tenant.status.clone(),
            suspended: tenant.status == STATUS_SUSPENDED,
            subdomain: tenant.subdomain.clone(),
            subdomain_pending: tenant.subdomain_pending,
            modules: state
                .live_grants_for(tenant.id)
                .iter()
                .map(|grant| grant.module_id.clone())
                .collect(),
        };
        Ok(json!(view))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}


/// Get the provisioning status for one tenant: the tenant, every
/// recorded step in pipeline order, the first step that still
/// needs work, and whether provisioning finished.
pub(crate) async fn get_provision_status(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<TenantsState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_TENANTS_VIEW)?;
        let tenant = get_tenant(&state, &path.into_inner())?;
        let steps = state.steps_for(tenant.id);
        let next_step = steps
            .iter()
            .find(|step| step.status != STEP_COMPLETED)
            .map(|step| step.step.clone());
        let completed = next_step.is_none();
        Ok(json!(ProvisionStatus {
            tenant,
            steps,
            next_step,
            completed,
        }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}


/// List the recorded provisioning steps for one tenant
/// (paginated, pipeline order).
pub(crate) async fn list_provision_steps(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<TenantsState>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_TENANTS_VIEW)?;
        let tenant = get_tenant(&state, &path.into_inner())?;
        let params = query.into_inner();
        Ok(paginate(state.steps_for(tenant.id), &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}


/// Retry provisioning: resumes a `provision_failed` (or stalled
/// `provisioning`) tenant at the first incomplete step instead of
/// duplicating anything. An `active` tenant is a 409 — there is
/// nothing to retry.
pub(crate) async fn retry_provisioning(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<TenantsState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_TENANTS_PROVISION)?;
        let mut tenant = get_tenant(&state, &path.into_inner())?;
        if tenant.status == STATUS_ACTIVE {
            return Err(UnifiedError::conflict(
                "Tenant is already active; there is nothing to retry",
            ));
        }
        if tenant.status == STATUS_DELETED || tenant.status == STATUS_DEPROVISIONING {
            return Err(UnifiedError::conflict(format!(
                "Tenant in status \"{}\" cannot be provisioned",
                tenant.status
            )));
        }
        // A stalled `provisioning` tenant is already in the target
        // status, and the state machine has no self-edges: skip the
        // no-op transition instead of failing the retry with a 409.
        if tenant.status != STATUS_PROVISIONING {
            transition_tenant(&state, &mut tenant, STATUS_PROVISIONING)?;
        }
        let explicit: Option<Vec<String>> = None;
        run_provisioning_pipeline(&ctx, &state, tenant.id, &caller.actor, &explicit);
        let tenant = get_tenant_by_id(&state, tenant.id)?;
        let steps = state.steps_for(tenant.id);
        let next_step = steps
            .iter()
            .find(|step| step.status != STEP_COMPLETED)
            .map(|step| step.step.clone());
        let completed = next_step.is_none();
        Ok(json!(ProvisionStatus {
            tenant,
            steps,
            next_step,
            completed,
        }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}


/// Suspend a tenant. Suspension stops serving through the
/// middleware contract, which already blocks suspended tenants;
/// this endpoint only records the suspension, stamps the time
/// and reason, and emits `tenant.suspended` plus an audit record.
pub(crate) async fn suspend_tenant(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<TenantsState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<SuspendTenantRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_TENANTS_SUSPEND)?;
        let mut tenant = get_tenant(&state, &path.into_inner())?;
        let reason = body.into_inner().reason.trim().to_string();
        if reason.is_empty() {
            return Err(UnifiedError::bad_request(
                "A suspension reason is required",
            ));
        }
        transition_tenant(&state, &mut tenant, STATUS_SUSPENDED)?;
        tenant.suspended_at = Some(now());
        tenant.suspended_reason = Some(reason.clone());
        tenant.updated_at = now();
        state.tenants.insert(tenant.id, tenant.clone());
        ctx.hooks.emit(
            EVENT_TENANT_SUSPENDED,
            json!({
                "tenant_id": tenant.id,
                "company_id": tenant.id,
                "slug": tenant.slug,
                "reason": reason,
                "actor_id": caller.actor,
            }),
        );
        emit_audit(
            &ctx,
            EVENT_TENANT_SUSPENDED,
            &caller.actor,
            "tenant.suspended",
            "tenant",
            &tenant.id.to_string(),
            Some(tenant.id),
            json!({ "slug": tenant.slug, "reason": reason }),
        );
        Ok(json!(tenant))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}


/// Resume a suspended tenant: `suspended` -> `active`, clearing
/// the suspension stamp and reason.
pub(crate) async fn resume_tenant(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<TenantsState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_TENANTS_MANAGE)?;
        let mut tenant = get_tenant(&state, &path.into_inner())?;
        transition_tenant(&state, &mut tenant, STATUS_ACTIVE)?;
        tenant.suspended_at = None;
        tenant.suspended_reason = None;
        tenant.updated_at = now();
        state.tenants.insert(tenant.id, tenant.clone());
        ctx.hooks.emit(
            EVENT_TENANT_RESUMED,
            json!({
                "tenant_id": tenant.id,
                "company_id": tenant.id,
                "slug": tenant.slug,
                "actor_id": caller.actor,
            }),
        );
        emit_audit(
            &ctx,
            EVENT_TENANT_RESUMED,
            &caller.actor,
            "tenant.resumed",
            "tenant",
            &tenant.id.to_string(),
            Some(tenant.id),
            json!({ "slug": tenant.slug, "status": STATUS_ACTIVE }),
        );
        Ok(json!(tenant))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}


/// List the live module grants for one tenant (paginated,
/// ordered by module id). Revoked grants are not listed here;
/// they stay in the store as history.
pub(crate) async fn list_tenant_modules(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<TenantsState>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_TENANTS_VIEW)?;
        let tenant = get_tenant(&state, &path.into_inner())?;
        let params = query.into_inner();
        Ok(paginate(state.live_grants_for(tenant.id), &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}


/// Grant additional modules to an `active` tenant. Every module
/// id is entitlement-checked through the same gate as the
/// provisioning step: a non-entitled module is recorded as
/// skipped with a reason and is never granted.
pub(crate) async fn grant_tenant_modules(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<TenantsState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<GrantModulesRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_TENANTS_MANAGE)?;
        let tenant = get_tenant(&state, &path.into_inner())?;
        if tenant.status != STATUS_ACTIVE {
            return Err(UnifiedError::conflict(format!(
                "Modules can only be granted to an active tenant (current status: \"{}\")",
                tenant.status
            )));
        }
        let request = body.into_inner();
        if request.module_ids.is_empty() {
            return Err(UnifiedError::bad_request(
                "module_ids must not be empty",
            ));
        }
        let (granted, skipped) = grant_modules_for_tenant(
            &ctx,
            &state,
            &tenant,
            &request.module_ids,
            &caller.actor,
            request.expires_at,
        );
        Ok(json!({ "granted": granted, "skipped": skipped }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}


/// Revoke one live module grant (soft delete: `revoked_at` is
/// stamped and the row is kept as history, so the tenant can be
/// re-granted later). Revoking a grant never deletes tenant data.
pub(crate) async fn revoke_tenant_module(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<TenantsState>,
    req: HttpRequest,
    path: web::Path<(String, String)>,
    body: web::Json<RevokeGrantRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_TENANTS_MANAGE)?;
        let (raw_id, module_id) = path.into_inner();
        let tenant = get_tenant(&state, &raw_id)?;
        let reason = body.into_inner().reason.trim().to_string();
        if reason.is_empty() {
            return Err(UnifiedError::bad_request("A revoke reason is required"));
        }
        let mut grant = state.live_grant(tenant.id, &module_id).ok_or_else(|| {
            UnifiedError::not_found(format!(
                "Tenant has no live grant for module \"{module_id}\""
            ))
        })?;
        grant.revoked_at = Some(now());
        grant.revoke_reason = Some(reason.clone());
        state.grants.insert(grant.id, grant.clone());
        ctx.hooks.emit(
            EVENT_TENANT_MODULE_REVOKED,
            json!({
                "tenant_id": tenant.id,
                "company_id": tenant.id,
                "module_id": module_id,
                "reason": reason,
                "actor_id": caller.actor,
            }),
        );
        emit_audit(
            &ctx,
            EVENT_TENANT_MODULE_REVOKED,
            &caller.actor,
            "tenant.module.revoked",
            "tenant",
            &tenant.id.to_string(),
            Some(tenant.id),
            json!({ "module_id": module_id, "reason": reason }),
        );
        Ok(json!(grant))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tenant_with_slug(id: Uuid, slug: &str) -> Tenant {
        Tenant {
            id,
            slug: slug.to_string(),
            display_name: "Acme".to_string(),
            status: STATUS_PROVISIONING.to_string(),
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
    fn claim_tenant_admits_only_one_concurrent_claim_per_slug() {
        let state = TenantsState::default();
        let outcomes: Vec<bool> = std::thread::scope(|scope| {
            let handles: Vec<_> = (0..8)
                .map(|_| {
                    let state = state.clone();
                    scope.spawn(move || {
                        let tenant = tenant_with_slug(Uuid::now_v7(), "acme");
                        claim_tenant(&state, &tenant).is_ok()
                    })
                })
                .collect();
            handles
                .into_iter()
                .map(|handle| handle.join().expect("claim thread panicked"))
                .collect()
        });
        assert_eq!(outcomes.iter().filter(|ok| **ok).count(), 1);
        assert_eq!(state.tenants.len(), 1);
    }
}
