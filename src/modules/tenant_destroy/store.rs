// Tenant Destroy module — in-memory stores and destroy lookups.
// Split from the former single-file module; items keep their original names and behaviour. Private items are `pub(crate)` so sibling files in this module can use them.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::{HookBus, HookContext};
use unified_core::json;
use unified_core::modules::{Module, ModuleRegistry, RouteInfo};
use unified_core::pagination::{PageResult, PaginationParams};
use unified_core::tenancy::{
    drop_tenant_schema, is_valid_schema_name, schema_exists, schema_name,
    EVENT_TENANT_DESTROY_REQUESTED, EVENT_TENANT_DESTROYED, EVENT_TENANT_SUSPENDED,
};
use unified_core::db::DbPool;
use crate::modules::import_export::import_export_events;
use crate::modules::tenants::{
    begin_deprovision, finish_deprovision_as_deleted, set_export_reference, TenantsState,
    STATUS_DEPROVISIONING,
};
use actix_web::{web, HttpRequest, HttpResponse};
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::sync::{Arc, OnceLock, RwLock};
use uuid::Uuid;
use super::*;

// ---------------------------------------------------------------------------
// Stores.
// ---------------------------------------------------------------------------

pub(crate) static DESTROY_REQUESTS: OnceLock<DashMap<Uuid, DestroyRequest>> = OnceLock::new();

pub(crate) static HOOK_BUS: OnceLock<Arc<HookBus>> = OnceLock::new();

/// The ONE shared tenants state (see the module header). The
/// tenants module and this module must see the same rows, so the
/// integrator creates a single `TenantsState` (via
/// `tenants::new_state()`), hands it to
/// `TenantsModule::with_state`, and hands the same clone here via
/// `TenantDestroyModule::with_state` (or
/// `set_shared_tenants_state` directly). There is deliberately NO
/// fallback to a fresh `new_state()`: a fresh state would be an
/// empty store, and the destroy flow would move a tenant that does
/// not exist there.
pub(crate) static SHARED_TENANTS_STATE: OnceLock<TenantsState> = OnceLock::new();

/// Hand the shared `TenantsState` to the destroy flow. Call once
/// at startup with the same clone the tenants module was built
/// around; later calls are ignored (the first state wins).
pub fn set_shared_tenants_state(state: TenantsState) {
    let _ = SHARED_TENANTS_STATE.set(state);
}

/// The shared tenants state, or an internal error naming the
/// missing wiring when the integrator has not shared it yet.
/// Destroy steps that must move the tenant record fail closed
/// here instead of touching a second store.
pub(crate) fn tenants_state() -> UnifiedResult<TenantsState> {
    SHARED_TENANTS_STATE.get().cloned().ok_or_else(|| {
        UnifiedError::internal(
            "Shared TenantsState is not wired: hand one TenantsState clone to TenantsModule::with_state and the same clone to TenantDestroyModule::with_state (or tenant_destroy::set_shared_tenants_state); destroy refuses to proceed without it",
        )
    })
}

pub(crate) fn destroy_requests_store() -> &'static DashMap<Uuid, DestroyRequest> {
    DESTROY_REQUESTS.get_or_init(DashMap::new)
}

/// True when the tenants module's record for this tenant says it
/// is already destroyed. Answered through the `tenant.lookup`
/// hook query on the stored bus (hook handlers have no
/// AppContext), so the tenants row is the single source of truth.
/// A tenant the registry does not know is NOT destroyed: a
/// destroyed tenant always keeps its row (status `deleted`),
/// so "no record" can only mean "never existed".
/// Fails closed: when the hook bus was never wired (the module was
/// never initialised), the guard cannot be answered and an error
/// is returned instead of `false` — answering "not destroyed"
/// without a bus would let provisioning re-create a destroyed
/// tenant. Callers must treat the error as destroyed.
/// Public so provisioning (tenants module / integrator) can
/// consult the guard without going through the hook bus.
pub fn is_destroyed_tenant(tenant_id: &Uuid) -> UnifiedResult<bool> {
    let Some(bus) = HOOK_BUS.get() else {
        return Err(UnifiedError::internal(
            "Tenant-destroy hook bus is not wired: the TenantDestroyModule was never initialised, so the destroyed-tenant guard cannot be answered; provisioning refuses to proceed",
        ));
    };
    let context = bus.emit(
        QUERY_TENANT_LOOKUP,
        json!({ "tenant_id": tenant_id.to_string(), "tenant": null }),
    );
    Ok(tenant_ref_from_context(&context)
        .is_some_and(|tenant| tenant_already_destroyed(&tenant)))
}

/// Due destroy requests: in `suspended_grace` with an expired grace
/// period. Public so the scheduler arm can list them before calling
/// `process_due_destroys`.
pub fn due_destroy_requests() -> Vec<DestroyRequest> {
    let now = Utc::now();
    let mut due: Vec<DestroyRequest> = destroy_requests_store()
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|request| {
            request.status == STATUS_SUSPENDED_GRACE
                && request
                    .grace_expires_at
                    .is_some_and(|expires_at| now >= expires_at)
        })
        .collect();
    due.sort_by(|a, b| a.grace_expires_at.cmp(&b.grace_expires_at));
    due
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn destroyed_guard_fails_closed_without_a_hook_bus() {
        // No test in this binary wires the destroy hook bus, so the
        // guard must refuse to answer instead of reporting "not
        // destroyed" (fail-open) when the module was never
        // initialised.
        assert!(HOOK_BUS.get().is_none());
        assert!(is_destroyed_tenant(&Uuid::new_v4()).is_err());
    }
}
