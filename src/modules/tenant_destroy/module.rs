// Tenant Destroy module — module definition and registration.
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
// Module definition.
// ---------------------------------------------------------------------------

/// The Tenant Destroy module.
pub struct TenantDestroyModule;

impl TenantDestroyModule {
    pub fn new() -> Self {
        Self
    }

    /// Build the module with the one shared `TenantsState` (the
    /// same clone the tenants module was built around). This is the
    /// wiring the integrator must use: the destroy flow moves the
    /// tenant record through the tenants helpers over this state,
    /// and a module built with `new()` alone refuses to destroy
    /// until `set_shared_tenants_state` is called.
    pub fn with_state(state: TenantsState) -> Self {
        set_shared_tenants_state(state);
        Self
    }
}

impl Default for TenantDestroyModule {
    fn default() -> Self {
        Self::new()
    }
}

impl Module for TenantDestroyModule {
    fn id(&self) -> &'static str {
        "tenant_destroy"
    }

    fn name(&self) -> &'static str {
        "Tenant Destroy"
    }

    fn description(&self) -> &'static str {
        "Irreversible tenant destroy flow: mandatory export, typed confirmation, second approval, suspension with grace period, verified schema drop, and tracked external cleanup."
    }

    /// Only `import_export` is listed here because it is the only
    /// dependency already registered in `src/modules/mod.rs`. The
    /// tenants module (B2, not landed yet) and saas_domains (not
    /// registered) are event-only integrations, the same pattern the
    /// SaaS packages module uses for unregistered modules.
    fn requires(&self) -> &[&'static str] {
        &["import_export"]
    }

    fn requires_libraries(&self) -> &[&'static str] {
        &["diesel", "uuid"]
    }

    fn init(&self, ctx: &AppContext) -> UnifiedResult<()> {
        let _ = HOOK_BUS.set(Arc::clone(&ctx.hooks));
        Ok(())
    }

    fn register_hooks(&self, bus: &HookBus) {
        // `export.completed` — the export proof. Moves the matching
        // request from `export_pending` to `export_ready`. Read
        // defensively: unknown export ids and malformed payloads are
        // skipped, never an error. The event carries the export's
        // `company_id`, which for a full-tenant export IS the tenant
        // id the export was scoped to: a completion for another
        // tenant never marks this request ready.
        bus.register(
            import_export_events::EXPORT_COMPLETED,
            "tenant_destroy",
            Arc::new(|context: &mut HookContext| {
                let Some(export_id) = context
                    .get("export_id")
                    .and_then(Value::as_str)
                    .and_then(|raw| Uuid::parse_str(raw).ok())
                else {
                    return;
                };
                let event_tenant = context
                    .get("company_id")
                    .and_then(Value::as_str)
                    .and_then(|raw| Uuid::parse_str(raw).ok());
                let download_url = context
                    .get("download_url")
                    .and_then(Value::as_str)
                    .map(str::to_string);
                for mut entry in destroy_requests_store().iter_mut() {
                    let request = entry.value_mut();
                    if request.export_job_id == Some(export_id)
                        && request.status == STATUS_EXPORT_PENDING
                        && event_tenant.map_or(true, |bound| bound == request.tenant_id)
                    {
                        mark_export_ready(request, download_url.clone(), "completed_event");
                        let finished = request.clone();
                        drop(entry);
                        emit_from_hook(
                            EVENT_EXPORT_READY,
                            json!({
                                "tenant_id": finished.tenant_id.to_string(),
                                "destroy_request_id": finished.id.to_string(),
                                "export_id": export_id.to_string(),
                                "verified_via": "completed_event",
                            }),
                        );
                        break;
                    }
                }
            }),
        );

        // External-cleanup confirmations from the owning modules.
        // Each handler marks one item confirmed; the last one moves
        // the request to `destroyed`.
        for (event, kind) in [
            (EVENT_STORAGE_CLEANUP_COMPLETED, CLEANUP_STORAGE),
            (EVENT_BACKUP_CLEANUP_COMPLETED, CLEANUP_BACKUPS),
            (EVENT_SEARCH_INDEX_CLEANUP_COMPLETED, CLEANUP_SEARCH_INDEX),
        ] {
            let kind_owned = kind.to_string();
            bus.register(
                event,
                "tenant_destroy",
                Arc::new(move |context: &mut HookContext| {
                    let Some(request_id) = context
                        .get("destroy_request_id")
                        .and_then(Value::as_str)
                        .and_then(|raw| Uuid::parse_str(raw).ok())
                    else {
                        return;
                    };
                    let confirmed_by = context
                        .get("actor")
                        .and_then(Value::as_str)
                        .unwrap_or("system")
                        .to_string();
                    confirm_external_cleanup_item(
                        &request_id,
                        &kind_owned,
                        &confirmed_by,
                        None,
                    );
                }),
            );
        }

        // Provisioning guard query: is this uuidv7 already destroyed?
        // Answered from the tenants row itself (status `deleted` /
        // `destroyed_at` set) through `is_destroyed_tenant()`, so
        // provisioning can never re-create a destroyed tenant or
        // its schema.
        bus.register(
            EVENT_DESTROYED_CHECK,
            "tenant_destroy",
            Arc::new(|context: &mut HookContext| {
                let Some(tenant_id) = context
                    .get("tenant_id")
                    .and_then(Value::as_str)
                    .and_then(|raw| Uuid::parse_str(raw).ok())
                else {
                    return;
                };
                // Fail closed: when the guard itself errors (the
                // hook bus was never wired), the tenant is reported
                // destroyed so provisioning can never re-create a
                // tenant whose destroy state is unknown.
                let destroyed = is_destroyed_tenant(&tenant_id).unwrap_or(true);
                context.add_result(json!({
                    "tenant_id": tenant_id.to_string(),
                    "destroyed": destroyed,
                    "schema": schema_name(tenant_id),
                }));
            }),
        );

        // Scheduler dispatch signal for `tenant.destroy_due`. The
        // handler only lists due requests; it never drops anything
        // (hook handlers have no `AppContext`). The scheduler arm
        // runs the sweep itself: it rebuilds the `AppContext`
        // from the handles stored at scheduler `init` and calls
        // `process_due_destroys(ctx)`.
        bus.register(
            DESTROY_DUE_KIND,
            "tenant_destroy",
            Arc::new(|context: &mut HookContext| {
                for due in due_destroy_requests() {
                    context.add_result(json!({
                        "destroy_request_id": due.id.to_string(),
                        "tenant_id": due.tenant_id.to_string(),
                        "grace_expires_at": due.grace_expires_at.map(|at| at.to_rfc3339()),
                    }));
                }
            }),
        );
    }

    fn route_info(&self) -> Vec<RouteInfo> {
        vec![
            RouteInfo { method: "POST", path: "/api/v1/tenants/destroy-requests", owner: "tenant_destroy", description: "Request a tenant destroy (mandatory export first)", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/tenants/destroy-requests", owner: "tenant_destroy", description: "List destroy requests (paginated, filter by status, tenant_id, requested_by)", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/tenants/destroy-requests/{id}", owner: "tenant_destroy", description: "One destroy request with full status and audit trail", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/tenants/destroy-requests/{id}/export-proof", owner: "tenant_destroy", description: "Attach an export job id as destroy proof", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/tenants/destroy-requests/{id}/confirm", owner: "tenant_destroy", description: "Typed slug confirmation (requester only)", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/tenants/destroy-requests/{id}/approve-second", owner: "tenant_destroy", description: "Second approver (must differ from the requester)", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/tenants/destroy-requests/{id}/cancel", owner: "tenant_destroy", description: "Cancel a destroy request (resumes the tenant during grace)", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/tenants/destroy-requests/{id}/execute", owner: "tenant_destroy", description: "Final destroy after grace expiry (verified schema drop)", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/tenants/destroy-requests/{id}/grace", owner: "tenant_destroy", description: "Grace period and expiry info for a destroy request", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/tenants/destroy-requests/{id}/external-cleanup/confirm", owner: "tenant_destroy", description: "Confirm one external cleanup item", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/tenants/destroy-requests/process-due", owner: "tenant_destroy", description: "Run the due-destroy sweep for grace-expired requests", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/tenants/destroy-settings", owner: "tenant_destroy", description: "Destroy flow settings (grace period, thresholds, lockout)", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "PUT", path: "/api/v1/tenants/destroy-settings", owner: "tenant_destroy", description: "Update destroy flow settings", auth: unified_core::auth::RouteAuth::Authenticated},
        ]
    }

    fn configure_routes(&self, cfg: &mut actix_web::web::ServiceConfig) {
        cfg.service(
            web::scope("/api/v1/tenants")
                .route("/destroy-requests", web::post().to(request_destroy))
                .route("/destroy-requests", web::get().to(list_destroy_requests))
                .route(
                    "/destroy-requests/process-due",
                    web::post().to(process_due),
                )
                .route("/destroy-requests/{id}", web::get().to(get_destroy_request))
                .route(
                    "/destroy-requests/{id}/export-proof",
                    web::post().to(attach_export_proof),
                )
                .route(
                    "/destroy-requests/{id}/confirm",
                    web::post().to(confirm_destroy),
                )
                .route(
                    "/destroy-requests/{id}/approve-second",
                    web::post().to(approve_second),
                )
                .route(
                    "/destroy-requests/{id}/cancel",
                    web::post().to(cancel_destroy),
                )
                .route(
                    "/destroy-requests/{id}/execute",
                    web::post().to(execute_destroy),
                )
                .route(
                    "/destroy-requests/{id}/grace",
                    web::get().to(get_grace_info),
                )
                .route(
                    "/destroy-requests/{id}/external-cleanup/confirm",
                    web::post().to(confirm_external_cleanup),
                )
                .route("/destroy-settings", web::get().to(get_settings))
                .route("/destroy-settings", web::put().to(update_settings)),
        );
    }
}

pub fn register(registry: &ModuleRegistry) {
    registry.register(Arc::new(TenantDestroyModule::new()));
}
