// Tenants — control-plane tenant registry, lifecycle, and provisioning.
//
// This module OWNS the control-plane tenant tables (`tenants`,
// `tenant_provisioning_steps`, `tenant_module_grants`; migration
// `000094_tenant_isolation`). It is control-plane infrastructure,
// not a sellable product (`available_as_product` is false): it
// never appears in the module offerings and no customer buys it.
//
// TENANT-SCOPED DATA ACCESS (standing rule). Every module resolves
// its data through the tenant context — tenant id (uuidv7) +
// tenant schema + package entitlements — and never queries the
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
//   1. `create_record` — tenant row (uuidv7 generated server-side
//      via `Uuid::now_v7()`, never client-supplied; slug validated
//      against the configurable rules and the reserved-label
//      deny-list from the tenant settings).
//   2. `create_schema` — schema creation through the tenancy
//      helper only (see the integration seam below; this file
//      builds no schema statements itself).
//   3. `run_migrations` — tenant migrations inside that schema
//      through the tenancy helper.
//   4. `register_subdomain` — emits `site.provision_requested`
//      for the domains module to claim; never calls the domains
//      module internals directly.
//   5. `grant_modules` — enables the purchased modules ONLY after
//      `check_module_entitlement` passes for each one; a module
//      the tenant is not entitled to is recorded as skipped with
//      a warning and is never provisioned.
//   6. `activate` — status becomes `active` and `tenant.provisioned`
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
// `resolve_host_with`) plus `src/core/middleware.rs` (the contract).
// This module's `register_hooks` answers a different query,
// `tenant.lookup`, for the destroy flow (B3) — the tenant record
// by id — and registers no resolver for `tenant.resolve_requested`.
// Suspension stops serving through the middleware contract, which
// already blocks suspended tenants: this module emits
// `tenant.suspended` plus an audit record and does not re-implement
// blocking.
//
// Persistence. The live data lives in in-memory DashMap stores,
// following the licensing / SaaS-packages precedent; the migration
// tables are the durable persistence targets and the integrator
// wires them into `src/db/schema.rs` / `src/db/models.rs` from
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
// Integration seams (B1 — `crate::core::tenancy`). The tenancy
// helpers live in `src/core/tenancy.rs`, owned by builder B1; this
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
// Destroy-flow extension point (B3 — `tenant_destroy.rs`). The
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
//   - `tenants:view` — read tenants, provisioning status, grants,
//     settings, tenant context.
//   - `tenants:manage` — suspend, resume, grant, revoke, settings.
//   - `tenants:provision` — create tenants and retry provisioning.

use crate::core::context::AppContext;
use crate::core::error::{UnifiedError, UnifiedResult};
use crate::core::hooks::{HookBus, HookContext};
use crate::core::json;
use crate::core::modules::{Module, ModuleRegistry, RouteInfo, RouteVersionSupport};
use crate::core::pagination::{PageResult, PaginationParams};
use crate::core::tenancy::{run_tenant_migrations, schema_name, TenantContext};
use crate::modules::licensing::check_module_entitlement;
use actix_web::{web, HttpRequest, HttpResponse};
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashSet;
use std::sync::{Arc, RwLock};
use uuid::Uuid;

/// Module id, as registered in the module registry.
pub const MODULE_ID: &str = "tenants";

/// Capabilities enforced on every route (via the `role.check` hook,
/// fail closed: no answer means not allowed).
pub const PERM_TENANTS_VIEW: &str = "tenants:view";
pub const PERM_TENANTS_MANAGE: &str = "tenants:manage";
pub const PERM_TENANTS_PROVISION: &str = "tenants:provision";
pub const PERM_TENANTS_SUSPEND: &str = "tenants:suspend";

// ---------------------------------------------------------------------------
// Lifecycle vocabulary (data, not code paths).
// ---------------------------------------------------------------------------

/// Tenant lifecycle statuses. The vocabulary is data: the state
/// machine below is the only place that gives it meaning.
pub const STATUS_PROVISIONING: &str = "provisioning";
pub const STATUS_ACTIVE: &str = "active";
pub const STATUS_SUSPENDED: &str = "suspended";
pub const STATUS_DEPROVISIONING: &str = "deprovisioning";
pub const STATUS_DELETED: &str = "deleted";
pub const STATUS_PROVISION_FAILED: &str = "provision_failed";

/// Every lifecycle status, in lifecycle order.
pub const TENANT_STATUSES: &[&str] = &[
    STATUS_PROVISIONING,
    STATUS_ACTIVE,
    STATUS_SUSPENDED,
    STATUS_DEPROVISIONING,
    STATUS_DELETED,
    STATUS_PROVISION_FAILED,
];

/// Allowed transitions, as data: (from, to) pairs. Anything not
/// listed is rejected with 409. `deleted` is terminal; a failed
/// provision returns to `provisioning` through the retry endpoint,
/// never straight to `active`.
pub const TENANT_TRANSITIONS: &[(&str, &str)] = &[
    (STATUS_PROVISIONING, STATUS_ACTIVE),
    (STATUS_PROVISIONING, STATUS_PROVISION_FAILED),
    (STATUS_PROVISIONING, STATUS_DEPROVISIONING),
    (STATUS_PROVISION_FAILED, STATUS_PROVISIONING),
    (STATUS_PROVISION_FAILED, STATUS_DEPROVISIONING),
    (STATUS_ACTIVE, STATUS_SUSPENDED),
    (STATUS_ACTIVE, STATUS_DEPROVISIONING),
    (STATUS_SUSPENDED, STATUS_ACTIVE),
    (STATUS_SUSPENDED, STATUS_DEPROVISIONING),
    (STATUS_DEPROVISIONING, STATUS_DELETED),
];

/// True when the lifecycle allows moving from `from` to `to`.
pub fn transition_allowed(from: &str, to: &str) -> bool {
    TENANT_TRANSITIONS.contains(&(from, to))
}

// ---------------------------------------------------------------------------
// Provisioning pipeline vocabulary (data).
// ---------------------------------------------------------------------------

/// Pipeline steps, in execution order. A re-run resumes at the
/// first step whose recorded status is not `completed`.
pub const STEP_CREATE_RECORD: &str = "create_record";
pub const STEP_CREATE_SCHEMA: &str = "create_schema";
pub const STEP_RUN_MIGRATIONS: &str = "run_migrations";
pub const STEP_REGISTER_SUBDOMAIN: &str = "register_subdomain";
pub const STEP_GRANT_MODULES: &str = "grant_modules";
pub const STEP_ACTIVATE: &str = "activate";

/// Every provisioning step, in execution order.
pub const PROVISION_STEPS: &[&str] = &[
    STEP_CREATE_RECORD,
    STEP_CREATE_SCHEMA,
    STEP_RUN_MIGRATIONS,
    STEP_REGISTER_SUBDOMAIN,
    STEP_GRANT_MODULES,
    STEP_ACTIVATE,
];

/// Step statuses. `skipped` is a recorded, explained outcome —
///
/// never a silent drop.
pub const STEP_PENDING: &str = "pending";
pub const STEP_RUNNING: &str = "running";
pub const STEP_COMPLETED: &str = "completed";
pub const STEP_FAILED: &str = "failed";
pub const STEP_SKIPPED: &str = "skipped";

// ---------------------------------------------------------------------------
// Events.
// ---------------------------------------------------------------------------

/// Hook event names this module emits. Add each one to
/// `AUDITED_EVENTS` in `audit.rs` (see the module header).
pub const EVENT_TENANT_CREATED: &str = "tenant.created";
pub const EVENT_TENANT_PROVISIONED: &str = "tenant.provisioned";
pub const EVENT_TENANT_PROVISION_FAILED: &str = "tenant.provision_failed";
pub const EVENT_TENANT_SUSPENDED: &str = "tenant.suspended";
pub const EVENT_TENANT_RESUMED: &str = "tenant.resumed";
pub const EVENT_TENANT_MODULE_GRANTED: &str = "tenant.module.granted";
pub const EVENT_TENANT_MODULE_SKIPPED: &str = "tenant.module.skipped";
pub const EVENT_TENANT_MODULE_REVOKED: &str = "tenant.module.revoked";
pub const EVENT_TENANT_DEPROVISIONING_STARTED: &str = "tenant.deprovisioning_started";
pub const EVENT_TENANT_DELETED: &str = "tenant.deleted";
pub const EVENT_TENANT_SETTINGS_UPDATED: &str = "tenant.settings.updated";

/// Hook query this module ANSWERS for the destroy flow (B3):
/// `tenant.lookup` with `{"tenant_id", "tenant": null}`; the
/// answerer sets the `tenant` payload key and adds the tenant
/// record as a result. The destroy module resolves tenants only
/// through this query and never reads the tenants tables directly.
pub const EVENT_TENANT_LOOKUP: &str = "tenant.lookup";

/// Event emitted to ask the domains module to claim a subdomain
/// for a tenant. Answered by `saas_domains`; this module only
/// emits. The payload is `{"tenant_id", "company_id",
/// "subdomain", "domain", "package_id"?, "source": "tenants"}`.
pub const EVENT_SITE_PROVISION_REQUESTED: &str = "site.provision_requested";

/// Event asked to resolve a package id to its module list.
/// Answered by `saas_packages` with `{"module_ids": [...]}`.
pub const EVENT_PACKAGE_MODULES_REQUESTED: &str = "saas_packages.package.modules_requested";

// ---------------------------------------------------------------------------
// Settings (configurable defaults, stored as settings — nothing
// behavioural is hard-coded outside this struct's defaults).
// ---------------------------------------------------------------------------

/// Default reserved subdomain labels. These are CONFIGURABLE
/// DEFAULTS, not a fixed rule: an administrator edits them through
/// `PUT /api/v1/tenants/settings`, and provisioning reads the live
/// settings every time. The defaults cover the labels that would
/// collide with platform, mail, and infrastructure hosts.
fn default_reserved_slugs() -> Vec<String> {
    [
        "admin", "api", "app", "www", "mail", "support", "help", "status", "blog", "shop",
        "store", "dashboard", "login", "auth", "system", "root", "null", "undefined", "saas",
        "tenant", "tenants", "control", "cpanel", "webmail", "ftp", "smtp", "ns1", "ns2",
        "static", "cdn", "assets", "media", "docs", "dev", "staging", "test", "demo", "example",
        "localhost",
    ]
    .iter()
    .map(|value| value.to_string())
    .collect()
}

fn default_slug_min_length() -> u32 {
    3
}

fn default_slug_max_length() -> u32 {
    63
}

/// Default suspension grace in days: the window a suspended tenant
/// has before the destroy flow may consider it for removal. This
/// is a default for the destroy flow (B3) to read; nothing in this
/// module deletes a tenant when the grace ends.
fn default_suspension_grace_days() -> u32 {
    7
}

/// Default maximum tenants one owner may hold. Zero means
/// unlimited. This is a configurable default, not a fixed quota.
fn default_max_tenants_per_owner() -> u32 {
    0
}

/// Admin settings for tenant defaults. Every rule the provisioning
/// flow enforces about slugs, grace, and quotas lives here, so an
/// administrator can change behaviour without a code change.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TenantsSettings {
    /// Subdomain labels no tenant may claim (lowercase, compared
    /// case-insensitively). Configurable default; see
    /// `default_reserved_slugs`.
    #[serde(default = "default_reserved_slugs")]
    pub reserved_slugs: Vec<String>,
    /// Minimum slug length in characters. Configurable default: 3.
    #[serde(default = "default_slug_min_length")]
    pub slug_min_length: u32,
    /// Maximum slug length in characters. Configurable default: 63
    /// (the DNS label limit).
    #[serde(default = "default_slug_max_length")]
    pub slug_max_length: u32,
    /// Suspension grace in days. Configurable default: 7.
    #[serde(default = "default_suspension_grace_days")]
    pub suspension_grace_days: u32,
    /// Maximum tenants per owner (0 = unlimited). Configurable
    /// default: 0.
    #[serde(default = "default_max_tenants_per_owner")]
    pub max_tenants_per_owner: u32,
    /// Module ids granted to a tenant when the create request names
    /// neither explicit module ids nor a package. Empty by
    /// default: a tenant with no package and no explicit list gets
    /// no modules. Each id is still entitlement-checked.
    #[serde(default)]
    pub default_module_ids: Vec<String>,
}

impl Default for TenantsSettings {
    fn default() -> Self {
        Self {
            reserved_slugs: default_reserved_slugs(),
            slug_min_length: default_slug_min_length(),
            slug_max_length: default_slug_max_length(),
            suspension_grace_days: default_suspension_grace_days(),
            max_tenants_per_owner: default_max_tenants_per_owner(),
            default_module_ids: Vec::new(),
        }
    }
}

impl TenantsSettings {
    /// Validate a settings value before it is stored. Slugs are
    /// normalized to lowercase and de-duplicated; lengths are
    /// clamped to the DNS label range.
    pub fn validated(mut self) -> UnifiedResult<Self> {
        if self.slug_min_length < 1 {
            return Err(UnifiedError::bad_request(
                "slug_min_length must be at least 1",
            ));
        }
        if self.slug_max_length > 63 {
            return Err(UnifiedError::bad_request(
                "slug_max_length must be at most 63",
            ));
        }
        if self.slug_min_length > self.slug_max_length {
            return Err(UnifiedError::bad_request(
                "slug_min_length must not exceed slug_max_length",
            ));
        }
        let mut seen: HashSet<String> = HashSet::new();
        let mut reserved: Vec<String> = Vec::new();
        for slug in self.reserved_slugs.drain(..) {
            let normalized = slug.trim().to_ascii_lowercase();
            if normalized.is_empty() || !seen.insert(normalized.clone()) {
                continue;
            }
            reserved.push(normalized);
        }
        self.reserved_slugs = reserved;
        let mut modules: Vec<String> = Vec::new();
        for module_id in self.default_module_ids.drain(..) {
            let trimmed = module_id.trim().to_string();
            if !trimmed.is_empty() && !modules.contains(&trimmed) {
                modules.push(trimmed);
            }
        }
        self.default_module_ids = modules;
        Ok(self)
    }
}

// ---------------------------------------------------------------------------
// Records.
// ---------------------------------------------------------------------------

/// One tenant: the control-plane record for a provisioned schema.
/// The tenant id is a uuidv7 and doubles as the company id the
/// rest of the platform keys tenant-scoped records by. The schema
/// name is derived from it and never typed by hand.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Tenant {
    pub id: Uuid,
    pub slug: String,
    pub display_name: String,
    pub status: String,
    pub owner_user_id: Uuid,
    pub package_id: Option<Uuid>,
    pub preset_id: Option<Uuid>,
    pub plan_id: Option<String>,
    pub subdomain: Option<String>,
    pub subdomain_pending: bool,
    pub schema_name: String,
    pub suspended_at: Option<DateTime<Utc>>,
    pub suspended_reason: Option<String>,
    pub destroyed_at: Option<DateTime<Utc>>,
    pub export_reference: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// One recorded provisioning step: the idempotency record that
/// lets a half-finished provision resume instead of duplicating
/// work. One row per (tenant, step).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProvisioningStep {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub step: String,
    pub status: String,
    pub attempt: i32,
    pub error: Option<String>,
    pub detail: Value,
    pub started_at: Option<DateTime<Utc>>,
    pub completed_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// One module grant: the durable record that a tenant was granted
/// a module, and through what. Revocation is a soft delete
/// (`revoked_at`); the row is kept as history.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModuleGrant {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub module_id: String,
    pub granted_via: String,
    pub granted_via_id: Option<Uuid>,
    pub granted_at: DateTime<Utc>,
    pub expires_at: Option<DateTime<Utc>>,
    pub granted_by: Option<String>,
    pub revoked_at: Option<DateTime<Utc>>,
    pub revoke_reason: Option<String>,
}

/// A module the grant step skipped, with the reason. Skips are
/// recorded, never silent: a non-entitled module is never
/// provisioned, and the caller can see exactly why.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkippedModule {
    pub module_id: String,
    pub reason: String,
}

/// Provisioning status as returned by `GET /{id}/provision`: the
/// tenant, every recorded step, and the first step that still
/// needs work (`None` when provisioning finished).
#[derive(Debug, Clone, Serialize)]
pub struct ProvisionStatus {
    pub tenant: Tenant,
    pub steps: Vec<ProvisioningStep>,
    pub next_step: Option<String>,
    pub completed: bool,
}

/// The tenant context as resolved for one tenant: the values every
/// other module must carry to resolve tenant data (tenant id,
/// schema, entitlement scope). Served by `GET /{id}/context` so
/// operators and the integrator can verify the resolution path.
#[derive(Debug, Clone, Serialize)]
pub struct TenantContextView {
    pub tenant_id: Uuid,
    pub schema: String,
    pub status: String,
    pub suspended: bool,
    pub subdomain: Option<String>,
    pub subdomain_pending: bool,
    pub modules: Vec<String>,
}

/// Create-tenant request. The tenant id is never accepted from
/// the client: it is generated server-side. `module_ids` is an
/// explicit module list; `package_id` resolves its list through
/// the packages event; `preset_id` is recorded on the tenant and
/// its module list is resolved by the presets apply flow (see the
/// grant step — a preset id alone resolves no modules today).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateTenantRequest {
    pub slug: String,
    pub display_name: String,
    pub owner_user_id: Uuid,
    pub package_id: Option<Uuid>,
    pub preset_id: Option<Uuid>,
    pub plan_id: Option<String>,
    pub module_ids: Option<Vec<String>>,
}

/// Suspend request: a reason is required so the audit trail can
/// explain every suspension.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SuspendTenantRequest {
    pub reason: String,
}

/// Grant-modules request: explicit module ids to grant on top of
/// the tenant's current grants. Every id is entitlement-checked.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GrantModulesRequest {
    pub module_ids: Vec<String>,
    pub expires_at: Option<DateTime<Utc>>,
}

/// Revoke-grant request: a reason is required for the audit trail.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RevokeGrantRequest {
    pub reason: String,
}

// ---------------------------------------------------------------------------
// State.
// ---------------------------------------------------------------------------

/// Runtime state. The DashMap stores are the runtime authority for
/// this round (see the module header); the migration tables are the
/// durable target the integrator wires in. The maps are Arc-backed
/// so one `TenantsState` clone can be shared with the destroy
/// module (B3) through `TenantsModule::with_state`.
#[derive(Debug, Clone)]
pub struct TenantsState {
    pub tenants: Arc<DashMap<Uuid, Tenant>>,
    pub steps: Arc<DashMap<String, ProvisioningStep>>,
    pub grants: Arc<DashMap<Uuid, ModuleGrant>>,
    pub settings: Arc<RwLock<TenantsSettings>>,
}

impl Default for TenantsState {
    fn default() -> Self {
        Self {
            tenants: Arc::new(DashMap::new()),
            steps: Arc::new(DashMap::new()),
            grants: Arc::new(DashMap::new()),
            settings: Arc::new(RwLock::new(TenantsSettings::default())),
        }
    }
}

/// Create a fresh, empty state. The integrator creates one state
/// and hands the same clone to this module and to the destroy
/// module so both see the same rows.
pub fn new_state() -> TenantsState {
    TenantsState::default()
}

impl TenantsState {
    /// Current settings (cloned; the stored value is the only
    /// source of truth for slug rules, grace, and quotas).
    pub fn settings(&self) -> TenantsSettings {
        self.settings
            .read()
            .expect("tenants settings lock poisoned")
            .clone()
    }

    /// Key for the provisioning-step map: one row per
    /// (tenant, step).
    fn step_key(tenant_id: Uuid, step: &str) -> String {
        format!("{tenant_id}|{step}")
    }

    /// Load a recorded step, if one exists.
    pub fn step(&self, tenant_id: Uuid, step: &str) -> Option<ProvisioningStep> {
        self.steps
            .get(&Self::step_key(tenant_id, step))
            .map(|entry| entry.value().clone())
    }

    /// Steps for one tenant, in pipeline order.
    pub fn steps_for(&self, tenant_id: Uuid) -> Vec<ProvisioningStep> {
        PROVISION_STEPS
            .iter()
            .filter_map(|step| self.step(tenant_id, step))
            .collect()
    }

    /// Live (unrevoked) grants for one tenant, ordered by module id.
    pub fn live_grants_for(&self, tenant_id: Uuid) -> Vec<ModuleGrant> {
        let mut grants: Vec<ModuleGrant> = self
            .grants
            .iter()
            .filter(|entry| {
                entry.value().tenant_id == tenant_id && entry.value().revoked_at.is_none()
            })
            .map(|entry| entry.value().clone())
            .collect();
        grants.sort_by(|a, b| a.module_id.cmp(&b.module_id));
        grants
    }

    /// A live grant for one (tenant, module), if one exists.
    pub fn live_grant(&self, tenant_id: Uuid, module_id: &str) -> Option<ModuleGrant> {
        self.grants.iter().find_map(|entry| {
            let grant = entry.value();
            if grant.tenant_id == tenant_id
                && grant.module_id == module_id
                && grant.revoked_at.is_none()
            {
                Some(grant.clone())
            } else {
                None
            }
        })
    }
}

pub fn register(registry: &ModuleRegistry) {
    registry.register(Arc::new(TenantsModule::new()));
}

// ---------------------------------------------------------------------------
// Viewer identity and capability checks (same trusted-parameter
// pattern as the SaaS-packages module: X-User-Id plus X-Role
// headers, `role.check` hook, fail closed).
// ---------------------------------------------------------------------------

/// Caller identity: `X-User-Id` (required) plus `X-Role` (required
/// for capability checks).
pub struct Caller {
    pub actor: String,
    pub role: String,
}

fn caller(req: &HttpRequest) -> UnifiedResult<Caller> {
    let actor = req
        .headers()
        .get("x-user-id")
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .ok_or_else(|| UnifiedError::unauthorized("Missing X-User-Id header"))?;
    let role = req
        .headers()
        .get("x-role")
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .ok_or_else(|| UnifiedError::unauthorized("Missing X-Role header"))?;
    Ok(Caller { actor, role })
}

/// Ask the roles module whether `role` holds `permission` through
/// the `role.check` hook. Fails closed: no answer means not allowed.
fn check_capability(ctx: &AppContext, role: &str, permission: &str) -> bool {
    let context = ctx
        .hooks
        .emit("role.check", json!({ "role": role, "permission": permission, "allowed": false }));
    context
        .get("allowed")
        .and_then(|value| value.as_bool())
        .unwrap_or(false)
}

fn require_permission(ctx: &AppContext, role: &str, permission: &str) -> UnifiedResult<()> {
    if check_capability(ctx, role, permission) {
        Ok(())
    } else {
        Err(UnifiedError::forbidden(format!(
            "Role \"{role}\" does not hold permission \"{permission}\""
        )))
    }
}

// ---------------------------------------------------------------------------
// Helpers.
// ---------------------------------------------------------------------------

fn now() -> DateTime<Utc> {
    Utc::now()
}

fn ok<T: Serialize>(data: T) -> HttpResponse {
    json::success_response(data, 200)
}

fn created<T: Serialize>(data: T) -> HttpResponse {
    json::success_response(data, 201)
}

fn err(error: UnifiedError, debug: bool) -> HttpResponse {
    json::error_response(&error, debug)
}

fn paginate<T: Serialize + Clone>(items: Vec<T>, params: &PaginationParams) -> PageResult<T> {
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

/// Database pool attached to the context, if one is configured.
/// Provisioning steps that need the database fail closed when
/// there is no pool instead of pretending to succeed.
fn db_pool(ctx: &AppContext) -> Option<crate::db::DbPool> {
    ctx.db.read().expect("db lock poisoned").clone()
}

/// Emit one audit-shaped event on the hook bus: the audit module
/// records events by name, so the payload carries the actor, the
/// action, and the target in a fixed shape.
fn emit_audit(
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

/// Validate and normalize a tenant slug against the live settings:
/// lowercase, within the configured length range, DNS-label
/// characters only, never starting or ending with a hyphen, and
/// never on the reserved-label deny-list. Returns the normalized
/// slug or a 400/409 error; the caller checks uniqueness.
fn validate_slug(raw: &str, settings: &TenantsSettings) -> UnifiedResult<String> {
    let slug = raw.trim().to_ascii_lowercase();
    if slug.is_empty() {
        return Err(UnifiedError::bad_request("slug must not be empty"));
    }
    let len = slug.chars().count() as u32;
    if len < settings.slug_min_length {
        return Err(UnifiedError::bad_request(format!(
            "slug must be at least {} characters",
            settings.slug_min_length
        )));
    }
    if len > settings.slug_max_length {
        return Err(UnifiedError::bad_request(format!(
            "slug must be at most {} characters",
            settings.slug_max_length
        )));
    }
    if slug.starts_with('-') || slug.ends_with('-') {
        return Err(UnifiedError::bad_request(
            "slug must not start or end with a hyphen",
        ));
    }
    if !slug
        .chars()
        .all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '-')
    {
        return Err(UnifiedError::bad_request(
            "slug may only contain lowercase letters, digits, and hyphens",
        ));
    }
    if settings
        .reserved_slugs
        .iter()
        .any(|reserved| reserved == &slug)
    {
        return Err(UnifiedError::bad_request(format!(
            "slug \"{slug}\" is reserved and cannot be claimed"
        )));
    }
    Ok(slug)
}

/// Load a tenant by id or slug, or 404. Slugs are unique, so a
/// slug lookup can never return another tenant's record.
pub fn get_tenant(state: &TenantsState, raw_id: &str) -> UnifiedResult<Tenant> {
    let trimmed = raw_id.trim();
    if let Ok(id) = Uuid::parse_str(trimmed) {
        if let Some(tenant) = state.tenants.get(&id) {
            return Ok(tenant.value().clone());
        }
    }
    let slug = trimmed.to_ascii_lowercase();
    for entry in state.tenants.iter() {
        if entry.value().slug == slug {
            return Ok(entry.value().clone());
        }
    }
    Err(UnifiedError::not_found(format!(
        "Tenant \"{trimmed}\" not found"
    )))
}

/// Move a tenant to a new status through the state machine. Any
/// transition not listed in `TENANT_TRANSITIONS` is rejected with
/// 409, so the lifecycle graph is enforced in exactly one place.
fn transition_tenant(
    state: &TenantsState,
    tenant: &mut Tenant,
    to: &str,
) -> UnifiedResult<()> {
    if tenant.status == to {
        return Ok(());
    }
    if !transition_allowed(&tenant.status, to) {
        return Err(UnifiedError::conflict(format!(
            "Tenant cannot move from \"{}\" to \"{to}\"",
            tenant.status
        ))
        .with_metadata("from", json!(tenant.status))
        .with_metadata("to", json!(to)));
    }
    tenant.status = to.to_string();
    tenant.updated_at = now();
    state.tenants.insert(tenant.id, tenant.clone());
    Ok(())
}

// ---------------------------------------------------------------------------
// Provisioning pipeline (idempotent, resumable, audit-logged).
// ---------------------------------------------------------------------------

/// Record a step row for (tenant, step) if none exists yet, and
/// return it. Re-running a provision never duplicates step rows:
/// the (tenant, step) key is unique in the store and in the
/// migration table.
fn record_step(state: &TenantsState, tenant_id: Uuid, step: &str) -> ProvisioningStep {
    let key = TenantsState::step_key(tenant_id, step);
    if let Some(existing) = state.steps.get(&key) {
        return existing.value().clone();
    }
    let timestamp = now();
    let row = ProvisioningStep {
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
    };
    state.steps.insert(key, row.clone());
    row
}

/// Mark a step running (attempt counter included, so a retried
/// step shows how many times it was attempted).
fn start_step(state: &TenantsState, step: &mut ProvisioningStep) {
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
fn complete_step(state: &TenantsState, step: &mut ProvisioningStep, detail: Value) {
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
fn fail_step(state: &TenantsState, step: &mut ProvisioningStep, error: String) {
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
fn skip_step(state: &TenantsState, step: &mut ProvisioningStep, reason: String) {
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
fn resolve_module_ids(
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
/// modules, `license` for sellable non-core modules (the
/// entitlement check already passed, so a license must exist).
fn granted_via_for(ctx: &AppContext, tenant: &Tenant, module_id: &str) -> String {
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
            } else {
                "license".to_string()
            }
        }
    }
}

/// Grant one module to one tenant after the entitlement check.
/// Returns the grant on success, or the skip reason on any
/// failure. The entitlement check is the gate: a module the
/// tenant is not entitled to is never recorded as granted, and an
/// unknown module id is never granted either (fail closed).
fn grant_one_module(
    ctx: &AppContext,
    state: &TenantsState,
    tenant: &Tenant,
    module_id: &str,
    actor: &str,
    expires_at: Option<DateTime<Utc>>,
) -> Result<ModuleGrant, String> {
    if state.live_grant(tenant.id, module_id).is_some() {
        return Err(format!("Module \"{module_id}\" is already granted"));
    }
    if ctx.modules.get(module_id).is_none() {
        return Err(format!(
            "Module \"{module_id}\" is not registered; it cannot be granted until its module registers"
        ));
    }
    let company_id = tenant.id.to_string();
    if let Err(error) = check_module_entitlement(ctx, &company_id, module_id) {
        return Err(format!(
            "Module \"{module_id}\" is not entitled for this tenant: {}",
            error.message
        ));
    }
    let granted_via = granted_via_for(ctx, tenant, module_id);
    let granted_via_id = match granted_via.as_str() {
        "package" => tenant.package_id,
        "preset" => tenant.preset_id,
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
fn register_subdomain_step(
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
fn run_provisioning_pipeline(
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
fn get_tenant_by_id(state: &TenantsState, tenant_id: Uuid) -> UnifiedResult<Tenant> {
    state
        .tenants
        .get(&tenant_id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Tenant \"{tenant_id}\" not found")))
}

/// Mark a tenant `provision_failed` after a failed step, emit the
/// failure event plus an audit record, and report that the
/// pipeline did not complete. The tenant stays retryable.
fn mark_provision_failed(
    ctx: &AppContext,
    state: &TenantsState,
    tenant: &mut Tenant,
    actor: &str,
) -> bool {
    let _ = transition_tenant(state, tenant, STATUS_PROVISION_FAILED);
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
fn grant_modules_for_tenant(
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

// ---------------------------------------------------------------------------
// Handlers.
// ---------------------------------------------------------------------------

/// List tenants (paginated; filter by `status`, `owner_user_id`,
/// or `package_id`; `search` matches slug or display name).
async fn list_tenants(
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
async fn create_tenant(
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
        state.tenants.insert(id, tenant.clone());
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
async fn get_settings(
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
async fn update_settings(
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
async fn get_tenant_handler(
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
async fn get_tenant_context(
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
async fn get_provision_status(
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
async fn list_provision_steps(
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
async fn retry_provisioning(
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
        transition_tenant(&state, &mut tenant, STATUS_PROVISIONING)?;
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
async fn suspend_tenant(
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
async fn resume_tenant(
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
async fn list_tenant_modules(
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
async fn grant_tenant_modules(
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
async fn revoke_tenant_module(
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

// ---------------------------------------------------------------------------
// Module definition.
// ---------------------------------------------------------------------------

/// The Tenants module: control-plane tenant registry, lifecycle,
/// and provisioning. Not a sellable product.
pub struct TenantsModule {
    state: TenantsState,
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
            RouteInfo { method: "GET", path: "/api/v1/tenants", owner: "tenants", description: "List tenants (paginated; filter by status, owner_user_id, or package_id; search slug or display name) (view)." },
            RouteInfo { method: "POST", path: "/api/v1/tenants", owner: "tenants", description: "Create a tenant and start provisioning; returns the tenant (active, or provision_failed when a step fails) (provision)." },
            RouteInfo { method: "GET", path: "/api/v1/tenants/settings", owner: "tenants", description: "Get the admin settings for tenant defaults: slug rules, grace defaults, quotas, default modules (view)." },
            RouteInfo { method: "PUT", path: "/api/v1/tenants/settings", owner: "tenants", description: "Update the admin settings for tenant defaults (manage)." },
            RouteInfo { method: "GET", path: "/api/v1/tenants/{id}", owner: "tenants", description: "Get one tenant by id or slug (view)." },
            RouteInfo { method: "GET", path: "/api/v1/tenants/{id}/context", owner: "tenants", description: "Get the resolved tenant context: tenant id, schema, status, live modules (view; fails closed on unknown tenants)." },
            RouteInfo { method: "GET", path: "/api/v1/tenants/{id}/provision", owner: "tenants", description: "Get provisioning status with every recorded step and the next incomplete step (view)." },
            RouteInfo { method: "GET", path: "/api/v1/tenants/{id}/provision/steps", owner: "tenants", description: "List the recorded provisioning steps for one tenant, in pipeline order (view)." },
            RouteInfo { method: "POST", path: "/api/v1/tenants/{id}/provision/retry", owner: "tenants", description: "Retry provisioning: resume at the first incomplete step instead of duplicating work (provision)." },
            RouteInfo { method: "POST", path: "/api/v1/tenants/{id}/suspend", owner: "tenants", description: "Suspend a tenant (reason required); the middleware contract blocks suspended tenants from serving (suspend)." },
            RouteInfo { method: "POST", path: "/api/v1/tenants/{id}/resume", owner: "tenants", description: "Resume a suspended tenant back to active (manage)." },
            RouteInfo { method: "GET", path: "/api/v1/tenants/{id}/modules", owner: "tenants", description: "List the live module grants for one tenant (view)." },
            RouteInfo { method: "POST", path: "/api/v1/tenants/{id}/modules", owner: "tenants", description: "Grant additional modules to an active tenant; every id is entitlement-checked and non-entitled modules are skipped with a reason (manage)." },
            RouteInfo { method: "DELETE", path: "/api/v1/tenants/{id}/modules/{module_id}", owner: "tenants", description: "Revoke one live module grant (soft delete; reason required; tenant data is never deleted) (manage)." },
        ]
    }
}

// ---------------------------------------------------------------------------
// Tenancy integration seam (B1 — `crate::core::tenancy`).
//
// Every call into the B1-owned tenancy helpers goes through these
// functions and nowhere else, so a signature change in
// `src/core/tenancy.rs` is fixed in one place. Verified against
// the real `src/core/tenancy.rs` at integration time (the assumed
// API this seam was first written against did not exist):
//   - `tenancy::schema_name(tenant_id: Uuid) -> String`: the
//     schema name derived from a tenant id. This is the ONLY
//     place a tenant schema name is ever built.
//   - `tenancy::TenantContext`: a struct with pub fields
//     (`tenant_id`, `schema_name`, `company_id`, `site_id`); there
//     is no `TenantContext::new` constructor and no `schema()`
//     accessor. Build it literally here, or from the middleware's
//     resolved context via `TenantContext::from_resolved` /
//     `from_http_request` at the HTTP edge.
//   - `tenancy::run_tenant_migrations(pool: &DbPool, tenant_id:
//     Uuid) -> UnifiedResult<()>`: creates the schema if missing
//     (`CREATE SCHEMA IF NOT EXISTS`) and runs the embedded
//     migrations inside it. Schema creation is folded inside it;
//     there is no separate `create_tenant_schema` helper and no
//     `run_tenant_migrations(pool, schema)` form.
// No statement that creates or removes a schema is built anywhere
// in this file: all of that lives behind these helpers.
// ---------------------------------------------------------------------------

/// Schema name for a tenant id, through the B1 helper.
pub fn tenant_schema_name(tenant_id: Uuid) -> String {
    schema_name(tenant_id)
}

/// Build the tenant context for a tenant id. The B1 type has no
/// constructor: it is built literally from its pub fields, with
/// the schema name derived by `schema_name` and no company / site
/// scope (this seam serves provisioning, not a request).
pub fn tenant_context_for(tenant_id: Uuid) -> TenantContext {
    TenantContext {
        tenant_id,
        schema_name: schema_name(tenant_id),
        company_id: None,
        site_id: None,
    }
}

/// Schema a tenant context resolves to: the pub `schema_name`
/// field, never rebuilt from the id.
pub fn tenant_context_schema(context: &TenantContext) -> &str {
    &context.schema_name
}

/// Create the tenant schema. There is no separate create helper
/// in B1: `run_tenant_migrations` creates the schema if missing
/// before migrating it, so this step and the migration step below
/// both run through it (the second call is a no-op for creation
/// and resumes any unapplied migrations).
fn ensure_tenant_schema(pool: &crate::db::DbPool, tenant_id: Uuid) -> UnifiedResult<()> {
    run_tenant_migrations(pool, tenant_id)
}

/// Run the tenant migrations inside the tenant schema, through
/// the B1 helper (schema creation is folded inside it).
fn migrate_tenant_schema(pool: &crate::db::DbPool, tenant_id: Uuid) -> UnifiedResult<()> {
    run_tenant_migrations(pool, tenant_id)
}
