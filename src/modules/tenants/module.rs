//! tenants - module.

// Tenants - control-plane tenant registry, lifecycle, and provisioning.
//
// This module OWNS the control-plane tenant tables (`tenants`,
// `tenant_provisioning_steps`, `tenant_module_grants`; migration
// `000094_tenant_isolation`). It is control-plane infrastructure,
// not a sellable product (`available_as_product` is false): it
// never appears in the module offerings and no customer buys it.
//
// TENANT-SCOPED DATA ACCESS (standing rule). Every module resolves
// its data through the tenant context - tenant id (uuidv7) +
// tenant schema + package entitlements - and never queries the
// main / control-plane database directly for tenant business data.
// The control-plane tables (`tenants`, subscriptions, domains,
// billing, licensing) live in the `public` schema and are accessed
// only through their own modules and events. This module is the
// sanctioned owner path for the `tenants` tables: its in-memory
// stores are the runtime authority for this round and the migration
// tables are the durable target (see the persistence note below).
// This module never reads another module's tenant business data
// directly: no direct queries against billing, subscription, or
// license tables for tenant data. Entitlement decisions go through
// `licensing::check_module_entitlement`, package module lists go
// through the `saas_packages.package.modules_requested` event, and
// subdomain allocation goes through the `site.provision_requested`
// event answered by the domains module. Provisioning resolves
// everything through the tenant context: a server-generated uuidv7
// becomes the tenant id, `tenancy::schema_name` derives the schema
// from it, and the per-schema migrations run inside that schema.
// No tenant id, schema name, or package id is ever hard-coded here.
// Until other modules adopt the TenantContext plumbing (the B1
// contract), they fail closed rather than reading control-plane
// data directly: a missing or unresolvable tenant context is an
// error, never a silent fallback to the control plane.
//
// Lifecycle. Statuses are a data-driven vocabulary
// (`TENANT_STATUSES`) with the state machine enforced server-side
// (`transition_allowed`): `provisioning` -> `active` ->
// `suspended` -> `deprovisioning` -> `deleted`, plus
// `provision_failed` (retryable, back to `provisioning`). A
// half-finished provision resumes at the first incomplete step
// instead of duplicating anything: every step is recorded in
// `tenant_provisioning_steps` first and consulted before it runs.
//
// Provisioning pipeline (idempotent, every step audit-logged):
//   1. `create_record` - tenant row (uuidv7 generated server-side
//      via `Uuid::now_v7()`, never client-supplied; slug validated
//      against the configurable rules and the reserved-label
//      deny-list from the tenant settings).
//   2. `create_schema` - schema creation through the tenancy
//      helper only (see the integration seam below; this file
//      builds no schema statements itself).
//   3. `run_migrations` - tenant migrations inside that schema
//      through the tenancy helper.
//   4. `register_subdomain` - emits `site.provision_requested`
//      for the domains module to claim; never calls the domains
//      module internals directly.
//   5. `grant_modules` - enables the purchased modules ONLY after
//      `check_module_entitlement` passes for each one; a module
//      the tenant is not entitled to is recorded as skipped with
//      a warning and is never provisioned.
//   6. `activate` - status becomes `active` and `tenant.provisioned`
//      is emitted.
// A failed step marks the tenant `provision_failed` and stops the
// pipeline; `POST /{id}/provision/retry` resumes it. The subdomain
// step is the exception: when the domains flow declines the claim
// it is recorded as `skipped` with the reason and the tenant keeps
// `subdomain_pending: true`, so provisioning can still complete
// and the claim can be retried later.
//
// Tenant resolution. The middleware emits `tenant.resolve_requested`
// and the DOMAINS module answers it (`saas_domains::resolve_host`);
// this module deliberately does NOT register a second answerer, so
// resolution stays in exactly one place. The convergence point is
// `src/modules/saas_domains.rs` (`resolve_host` /
// `resolve_host_with`) plus crates/unified-core/src/middleware.rs` (the contract).
// This module's `register_hooks` answers a different query,
// `tenant.lookup`, for the destroy flow (B3) - the tenant record
// by id - and registers no resolver for `tenant.resolve_requested`.
// Suspension stops serving through the middleware contract, which
// already blocks suspended tenants: this module emits
// `tenant.suspended` plus an audit record and does not re-implement
// blocking.
//
// Persistence. The live data lives in in-memory DashMap stores,
// following the licensing / SaaS-packages precedent; the migration
// tables are the durable persistence targets and the integrator
// wires them into crates/unified-core/src/db/schema.rs` / crates/unified-core/src/db/models.rs` from
// these Diesel snippets:
//
//   diesel::table! {
//       tenants (id) {
//           id -> Uuid,
//           slug -> Varchar,
//           display_name -> Varchar,
//           status -> Varchar,
//           owner_user_id -> Uuid,
//           package_id -> Nullable<Uuid>,
//           preset_id -> Nullable<Uuid>,
//           plan_id -> Nullable<Varchar>,
//           subdomain -> Nullable<Varchar>,
//           subdomain_pending -> Bool,
//           schema_name -> Varchar,
//           suspended_at -> Nullable<Timestamptz>,
//           suspended_reason -> Nullable<Text>,
//           destroyed_at -> Nullable<Timestamptz>,
//           export_reference -> Nullable<Text>,
//           created_at -> Timestamptz,
//           updated_at -> Timestamptz,
//       }
//   }
//   diesel::table! {
//       tenant_provisioning_steps (id) {
//           id -> Uuid,
//           tenant_id -> Uuid,
//           step -> Varchar,
//           status -> Varchar,
//           attempt -> Int4,
//           error -> Nullable<Text>,
//           detail -> Jsonb,
//           started_at -> Nullable<Timestamptz>,
//           completed_at -> Nullable<Timestamptz>,
//           created_at -> Timestamptz,
//           updated_at -> Timestamptz,
//       }
//   }
//   diesel::table! {
//       tenant_module_grants (id) {
//           id -> Uuid,
//           tenant_id -> Uuid,
//           module_id -> Varchar,
//           granted_via -> Varchar,
//           granted_via_id -> Nullable<Uuid>,
//           granted_at -> Timestamptz,
//           expires_at -> Nullable<Timestamptz>,
//           granted_by -> Nullable<Varchar>,
//           revoked_at -> Nullable<Timestamptz>,
//           revoke_reason -> Nullable<Text>,
//       }
//   }
//
// Events emitted (add every name to `AUDITED_EVENTS` in `audit.rs`
// so the audit module records them):
//   - `tenant.created`, `tenant.provisioned`,
//     `tenant.provision_failed`, `tenant.suspended`,
//     `tenant.resumed`, `tenant.module.granted`,
//     `tenant.module.skipped`, `tenant.module.revoked`,
//     `tenant.deprovisioning_started`, `tenant.deleted`,
//     `tenant.settings.updated`.
// Events consumed (emitted by other modules, answered here):
//   - `tenant.lookup` (`{"tenant_id"}` -> the tenant record,
//     answered in `register_hooks` for the destroy flow, B3).
//   - `saas_packages.package.modules_requested`
//     (`{"package_id"}` -> `{"module_ids": [...]}`), asked during
//     the grant step when a package id is supplied.
//   - `site.provision_requested` is EMITTED by this module, never
//     answered: the domains module answers it.
//
// Integration seams (B1 - `unified_core::tenancy`). The tenancy
// helpers live in crates/unified-core/src/tenancy.rs`, owned by builder B1; this
// file imports `schema_name`, `TenantContext`, and
// `run_tenant_migrations` from there and calls them only through
// the small seam functions at the bottom of this file, so a
// signature change is fixed in one place. Verified B1 API (read
// from the real file at integration time; the assumed API this
// file was first written against did not exist):
//   - `tenancy::schema_name(tenant_id: Uuid) -> String`
//   - `tenancy::TenantContext` is a struct with pub fields
//     (`tenant_id`, `schema_name`, `company_id`, `site_id`); no
//     `TenantContext::new` constructor, no `schema()` accessor.
//   - `tenancy::run_tenant_migrations(pool: &DbPool, tenant_id:
//     Uuid) -> UnifiedResult<()>` (creates the schema if missing
//     and runs the embedded migrations inside it; there is no
//     separate `create_tenant_schema` and no schema-string form)
//
// Destroy-flow extension point (B3 - `tenant_destroy.rs`). The
// destroy flow is NOT in this file. It extends this module through
// the public helpers `begin_deprovision`,
// `set_export_reference`, and `finish_deprovision_as_deleted`
// plus `get_tenant`: B3 drives
// export, confirmation, and the schema removal, and calls these
// helpers to move the record through `deprovisioning` -> `deleted`
// with the matching events and audit records. No HTTP destroy route
// is registered here. The shared-state wiring is
// `TenantsModule::with_state`: the integrator creates one
// `TenantsState` (via `new_state()`) and hands the same clone to
// both this module and the destroy module, so both see the same
// rows.
//
// Open items recorded at write time (see the report for the full
// list): the domains module's `site.provision_requested` handler
// only trusts source `saas_packages` and only claims product
// subdomains, so a tenant-level claim from this module is declined
// today and recorded as skipped; the presets module exposes no
// `modules_requested` event, so a preset id alone cannot resolve
// a module list yet; no database pool means the schema steps fail
// closed and provisioning waits for a retry once a pool exists.
//
// Routes (all capability-checked, standard envelope):
//   GET    /api/v1/tenants
//   POST   /api/v1/tenants
//   GET    /api/v1/tenants/settings
//   PUT    /api/v1/tenants/settings
//   GET    /api/v1/tenants/{id}
//   GET    /api/v1/tenants/{id}/context
//   GET    /api/v1/tenants/{id}/provision
//   GET    /api/v1/tenants/{id}/provision/steps
//   POST   /api/v1/tenants/{id}/provision/retry
//   POST   /api/v1/tenants/{id}/suspend
//   POST   /api/v1/tenants/{id}/resume
//   GET    /api/v1/tenants/{id}/modules
//   POST   /api/v1/tenants/{id}/modules
//   DELETE /api/v1/tenants/{id}/modules/{module_id}
//
// Capabilities:
//   - `tenants:view` - read tenants, provisioning status, grants,
//     settings, tenant context.
//   - `tenants:manage` - suspend, resume, grant, revoke, settings.
//   - `tenants:provision` - create tenants and retry provisioning.

use unified_core::hooks::{HookBus, HookContext};
use unified_core::modules::{Module, RouteInfo, RouteVersionSupport};
use unified_core::tenancy::{run_tenant_migrations, schema_name, TenantContext};
use crate::modules::licensing::check_module_entitlement;
use actix_web::{web};
use serde_json::{Value};
use std::sync::{Arc};
use super::{constants::*, vocabulary::*, events::*, settings::*, records::*, state::*, auth::*, helpers::*, provisioning::*, destroy::*, handlers::*, tenancy::*};



// Module definition.
/// The Tenants module: control-plane tenant registry, lifecycle,
/// and provisioning. Not a sellable product.
pub struct TenantsModule {
    pub(crate) state: TenantsState,
}


impl TenantsModule {
    pub fn new() -> Self {
        Self {
            state: TenantsState::default(),
        }
    }

    /// Build the module around a shared state. The integrator
    /// creates one `TenantsState` (via `new_state()`) and hands
    /// the same clone to this module and to the destroy module
    /// (B3) so both see the same rows.
    pub fn with_state(state: TenantsState) -> Self {
        Self { state }
    }

    /// The shared state this module was built around, for the
    /// integrator to hand to the destroy module.
    pub fn state(&self) -> TenantsState {
        self.state.clone()
    }
}


impl Default for TenantsModule {
    fn default() -> Self {
        Self::new()
    }
}


impl Module for TenantsModule {
    fn id(&self) -> &'static str {
        MODULE_ID
    }

    fn name(&self) -> &'static str {
        "Tenant Management"
    }

    fn description(&self) -> &'static str {
        "Control-plane tenant registry, lifecycle, and provisioning: one schema per tenant, idempotent resumable provisioning, entitlement-gated module grants, suspension, and the tenant-context resolution other modules build on."
    }

    fn dependencies(&self) -> Vec<&'static str> {
        vec!["licensing"]
    }

    fn requires(&self) -> &[&'static str] {
        &["licensing"]
    }

    fn available_as_product(&self) -> bool {
        false
    }

    fn product_category(&self) -> Option<&'static str> {
        None
    }

    fn route_version_support(&self) -> RouteVersionSupport {
        RouteVersionSupport::All
    }

    fn register_hooks(&self, bus: &HookBus) {
        // Tenant lookup (destroy-flow contract): answer
        // `tenant.lookup` with the tenant record, so the destroy
        // module can resolve tenants without touching the tenants
        // tables. The answer sets the `tenant` payload key and
        // adds the record as a result; an unknown id answers
        // nothing, and the caller fails closed.
        // Deliberately registers NO resolver for
        // `tenant.resolve_requested`: the domains module already
        // answers it (`saas_domains::resolve_host`), and a second
        // answerer would split resolution across two places.
        let state = self.state.clone();
        bus.register(
            EVENT_TENANT_LOOKUP,
            "tenants",
            Arc::new(move |context: &mut HookContext| {
                let Some(raw_id) = context.get("tenant_id").and_then(Value::as_str) else {
                    return;
                };
                let Ok(tenant) = get_tenant(&state, raw_id) else {
                    return;
                };
                if let Ok(value) = serde_json::to_value(&tenant) {
                    context.set("tenant", value.clone());
                    context.add_result(value);
                }
            }),
        );
    }

    fn configure_routes(&self, cfg: &mut web::ServiceConfig) {
        let state = web::Data::new(self.state.clone());
        cfg.service(
            web::scope("/api/v1/tenants")
                .app_data(state)
                // Static segments first: `/{id}` would otherwise
                // swallow `settings`.
                .route("/settings", web::get().to(get_settings))
                .route("/settings", web::put().to(update_settings))
                .route("", web::get().to(list_tenants))
                .route("", web::post().to(create_tenant))
                .route("/{id}", web::get().to(get_tenant_handler))
                .route("/{id}/context", web::get().to(get_tenant_context))
                .route("/{id}/provision", web::get().to(get_provision_status))
                .route(
                    "/{id}/provision/steps",
                    web::get().to(list_provision_steps),
                )
                .route(
                    "/{id}/provision/retry",
                    web::post().to(retry_provisioning),
                )
                .route("/{id}/suspend", web::post().to(suspend_tenant))
                .route("/{id}/resume", web::post().to(resume_tenant))
                .route("/{id}/modules", web::get().to(list_tenant_modules))
                .route("/{id}/modules", web::post().to(grant_tenant_modules))
                .route(
                    "/{id}/modules/{module_id}",
                    web::delete().to(revoke_tenant_module),
                ),
        );
    }

    fn route_info(&self) -> Vec<RouteInfo> {
        vec![
            RouteInfo { method: "GET", path: "/api/v1/tenants", owner: "tenants", description: "List tenants (paginated; filter by status, owner_user_id, or package_id; search slug or display name) (view).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/tenants", owner: "tenants", description: "Create a tenant and start provisioning; returns the tenant (active, or provision_failed when a step fails) (provision).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/tenants/settings", owner: "tenants", description: "Get the admin settings for tenant defaults: slug rules, grace defaults, quotas, default modules (view).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "PUT", path: "/api/v1/tenants/settings", owner: "tenants", description: "Update the admin settings for tenant defaults (manage).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/tenants/{id}", owner: "tenants", description: "Get one tenant by id or slug (view).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/tenants/{id}/context", owner: "tenants", description: "Get the resolved tenant context: tenant id, schema, status, live modules (view; fails closed on unknown tenants).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/tenants/{id}/provision", owner: "tenants", description: "Get provisioning status with every recorded step and the next incomplete step (view).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/tenants/{id}/provision/steps", owner: "tenants", description: "List the recorded provisioning steps for one tenant, in pipeline order (view).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/tenants/{id}/provision/retry", owner: "tenants", description: "Retry provisioning: resume at the first incomplete step instead of duplicating work (provision).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/tenants/{id}/suspend", owner: "tenants", description: "Suspend a tenant (reason required); the middleware contract blocks suspended tenants from serving (suspend).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/tenants/{id}/resume", owner: "tenants", description: "Resume a suspended tenant back to active (manage).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/tenants/{id}/modules", owner: "tenants", description: "List the live module grants for one tenant (view).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/tenants/{id}/modules", owner: "tenants", description: "Grant additional modules to an active tenant; every id is entitlement-checked and non-entitled modules are skipped with a reason (manage).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "DELETE", path: "/api/v1/tenants/{id}/modules/{module_id}", owner: "tenants", description: "Revoke one live module grant (soft delete; reason required; tenant data is never deleted) (manage).", auth: unified_core::auth::RouteAuth::Authenticated},
        ]
    }
}
