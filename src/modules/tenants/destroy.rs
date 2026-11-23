//! tenants - destroy.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedResult};
use unified_core::json;
use serde_json::{json};
use uuid::Uuid;
use super::{constants::*, vocabulary::*, events::*, settings::*, records::*, state::*, auth::*, helpers::*, provisioning::*, handlers::*, module::*, tenancy::*};



// ---------------------------------------------------------------------------
// Destroy-flow extension point (B3 — `tenant_destroy.rs`).
//
// The destroy flow is NOT in this file. It extends this module
// through these public helpers: B3 drives export, confirmation,
// and the schema removal, and calls these helpers to move the
// record through `deprovisioning` -> `deleted` with the matching
// events and audit records. No HTTP destroy route is registered
// here.
// ---------------------------------------------------------------------------
/// Begin deprovisioning: move a tenant to `deprovisioning` from
/// any live status (`provisioning`, `provision_failed`, `active`,
/// `suspended`). Emits `tenant.deprovisioning_started` plus an
/// audit record. The caller (B3) performs the export and the
/// schema removal before finishing with
/// `finish_deprovision_as_deleted`.
pub fn begin_deprovision(
    ctx: &AppContext,
    state: &TenantsState,
    tenant_id: Uuid,
    actor: &str,
    reason: &str,
) -> UnifiedResult<Tenant> {
    let mut tenant = get_tenant_by_id(state, tenant_id)?;
    transition_tenant(state, &mut tenant, STATUS_DEPROVISIONING)?;
    ctx.hooks.emit(
        EVENT_TENANT_DEPROVISIONING_STARTED,
        json!({
            "tenant_id": tenant.id,
            "company_id": tenant.id,
            "slug": tenant.slug,
            "reason": reason,
            "actor_id": actor,
        }),
    );
    emit_audit(
        ctx,
        EVENT_TENANT_DEPROVISIONING_STARTED,
        actor,
        "tenant.deprovisioning_started",
        "tenant",
        &tenant.id.to_string(),
        Some(tenant.id),
        json!({ "slug": tenant.slug, "reason": reason }),
    );
    Ok(tenant)
}


/// Record the export reference the destroy flow produced, so the
/// tenant row points at the export an administrator can restore
/// from. Called by B3 before the schema removal.
pub fn set_export_reference(
    state: &TenantsState,
    tenant_id: Uuid,
    reference: &str,
) -> UnifiedResult<Tenant> {
    let mut tenant = get_tenant_by_id(state, tenant_id)?;
    tenant.export_reference = Some(reference.trim().to_string());
    tenant.updated_at = now();
    state.tenants.insert(tenant.id, tenant.clone());
    Ok(tenant)
}


/// Finish deprovisioning: move a `deprovisioning` tenant to
/// `deleted`, stamp `destroyed_at`, and emit `tenant.deleted`
/// plus an audit record. Only B3 calls this, after the export and
/// the schema removal have completed.
pub fn finish_deprovision_as_deleted(
    ctx: &AppContext,
    state: &TenantsState,
    tenant_id: Uuid,
    actor: &str,
) -> UnifiedResult<Tenant> {
    let mut tenant = get_tenant_by_id(state, tenant_id)?;
    transition_tenant(state, &mut tenant, STATUS_DELETED)?;
    tenant.destroyed_at = Some(now());
    tenant.updated_at = now();
    state.tenants.insert(tenant.id, tenant.clone());
    ctx.hooks.emit(
        EVENT_TENANT_DELETED,
        json!({
            "tenant_id": tenant.id,
            "company_id": tenant.id,
            "slug": tenant.slug,
            "actor_id": actor,
        }),
    );
    emit_audit(
        ctx,
        EVENT_TENANT_DELETED,
        actor,
        "tenant.deleted",
        "tenant",
        &tenant.id.to_string(),
        Some(tenant.id),
        json!({ "slug": tenant.slug, "status": STATUS_DELETED }),
    );
    Ok(tenant)
}
