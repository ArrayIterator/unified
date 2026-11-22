// Tenant Destroy module — the irreversible end of a tenant lifecycle.
//
// Destroy is NEVER one click. A tenant is destroyed only through the
// full flow below, and every step is reversible until the final schema
// drop. The state machine is:
//
//   requested
//     -> export_pending            (full export requested, proof pending)
//     -> export_ready              (export completed and downloadable)
//     -> awaiting_confirmation     (typed confirmation attempted / pending)
//     -> awaiting_second_approval  (only when the settings rule requires it)
//     -> suspended_grace           (tenant suspended, grace clock running)
//     -> destroyed_pending_external_cleanup
//     -> destroyed                 (terminal)
//   Any non-terminal state except `destroyed_pending_external_cleanup`
//   can move to `cancelled` (terminal). Cancellation during
//   `suspended_grace` resumes the tenant.
//
// Step-by-step:
//   1. REQUEST: an admin/owner with `tenants:destroy` opens a destroy
//      request for one tenant. The tenant record is resolved through
//      the tenants module (`tenant.lookup` hook query, see below) and
//      the flow refuses to proceed when the record cannot be resolved.
//      A full export is requested through the import/export module
//      (`export.requested`) and the request waits in `export_pending`.
//   2. EXPORT FIRST: the flow cannot advance past `export_pending`
//      until a completed, downloadable export is proven: either the
//      `export.completed` event is observed for the attached export
//      job, or the `export.lookup` hook query answers `completed`.
//      The export job id is stored on the request as the proof.
//   3. TYPED CONFIRMATION: the requester types the tenant slug
//      exactly (case-sensitive). A mismatch is rejected with a generic
//      error, counted, and after a configured number of failures the
//      confirmation locks for a configured lockout period.
//   4. SECOND APPROVER: when the settings rule says the tenant is
//      above the size/age threshold, a second admin — a different
//      user than the requester, enforced — must approve. The
//      requester's own approval never counts.
//   5. SUSPENSION + GRACE: on final confirmation the tenant is
//      suspended first (`tenant.suspended` is emitted, serving
//      stops) and a grace period starts (configurable setting,
//      default 30 days, stored as an explicit expiry timestamp).
//      During grace the destroy can still be cancelled, which
//      resumes the tenant (`tenant.resumed`).
//   6. FINAL DESTROY: only after grace expiry, only from
//      `suspended_grace`: the tenant moves to `deprovisioning`
//      through the tenants module's `begin_deprovision` helper
//      (the single source of truth for the tenant record — this
//      module keeps NO parallel destroyed-tenants store), the
//      tenant schema is dropped through
//      `crate::core::tenancy::drop_tenant_schema` (never by building
//      DDL in this file), the drop is verified with `schema_exists`
//      (must be false, else fail closed and alert), the export
//      reference is recorded with `set_export_reference`, the
//      tenant row moves to `deleted` with `destroyed_at` through
//      `finish_deprovision_as_deleted`, subdomains are revoked
//      through saas_domains events, and `tenant.destroyed` is
//      emitted with a full audit trail. A destroyed tenant uuidv7 is
//      NEVER reused: the tenants row itself (status `deleted`,
//      `destroyed_at` set) is the guard record, consulted by
//      provisioning through the `tenant.destroyed.check` hook query
//      and `is_destroyed_tenant()`.
//   7. EXTERNAL CLEANUP: dropping the schema removes the tenant's
//      database rows only. Stored files, backups, and search indexes
//      live outside the schema, so the flow emits
//      `tenant.storage_cleanup_requested`,
//      `tenant.backup_cleanup_requested`, and
//      `tenant.search_index_cleanup_requested` (tenant id plus schema
//      name) and stays in `destroyed_pending_external_cleanup` until
//      every item is confirmed — by the owning module's
//      `tenant.<area>_cleanup.completed` event, or by an admin through
//      the confirm endpoint. Only then is the request `destroyed`.
//
// TENANT-SCOPED DATA ACCESS (standing rule). Every destroy step
// resolves its data through tenant context and through the owning
// module's own events — never by querying another module's tables
// directly, and never against the main/control-plane database for
// tenant business data:
//   - The destroy request and the control-plane tenant row are
//     control-plane data owned by the tenants module: this module
//     reads the tenant record only through the `tenant.lookup` hook
//     query (answered by the tenants module) and moves the record
//     through the tenants module's public helpers
//     (`begin_deprovision` / `set_export_reference` /
//     `finish_deprovision_as_deleted`) over the ONE shared
//     `TenantsState` (see `set_shared_tenants_state`). It never
//     reads or writes the tenants tables itself, and it keeps no
//     parallel destroyed-tenants store: the tenants row with
//     status `deleted` is the only destroy record.
//   - The export goes through the import/export module's events.
//   - Subdomain revocation goes through saas_domains events.
//   - The schema drop goes through the tenancy helper.
//   - No tenant id, schema name, or package id is ever hard-coded:
//     every destroy step resolves the schema name from the tenant's
//     uuidv7 through `schema_name()` at execution time.
//   - Fail closed: when the tenant record or the tenancy plumbing
//     cannot resolve the tenant, the flow refuses to proceed instead
//     of falling back to any default or to the `public` schema.
//
// Routes live under `/api/v1/tenants` (same prefix as the tenants
// module, so the integrator merges the two route sets):
//
//   POST   /destroy-requests — request a destroy (destroy).
//   GET    /destroy-requests — list destroy requests (destroy_read).
//   GET    /destroy-requests/{id} — one destroy request (destroy_read).
//   POST   /destroy-requests/{id}/export-proof — attach an export
//          job id as proof (destroy).
//   POST   /destroy-requests/{id}/confirm — typed slug
//          confirmation (destroy, requester only).
//   POST   /destroy-requests/{id}/approve-second — second approver
//          (destroy, must differ from the requester).
//   POST   /destroy-requests/{id}/cancel — cancel (destroy).
//   POST   /destroy-requests/{id}/execute — final destroy, only
//          after grace expiry (destroy).
//   GET    /destroy-requests/{id}/grace — grace/expiry info
//          (destroy_read).
//   POST   /destroy-requests/{id}/external-cleanup/confirm —
//          confirm one external cleanup item (destroy).
//   POST   /destroy-requests/process-due — run the due-destroy
//          sweep for grace-expired requests (destroy).
//   GET    /destroy-settings — configurable defaults (destroy_read).
//   PUT    /destroy-settings — update the defaults (destroy).
//
// Events emitted on the hook bus:
//   - `tenant.destroy_requested` (tenancy constant
//     `EVENT_TENANT_DESTROY_REQUESTED`)
//   - `export.requested` (import/export module; full tenant export)
//   - `tenant.destroy.export_ready`
//   - `tenant.destroy.confirmation_failed`
//   - `tenant.destroy.second_approval_required`
//   - `tenant.suspended` (tenancy constant `EVENT_TENANT_SUSPENDED`)
//   - `tenant.resumed` (cancel during grace)
//   - `tenant.destroy.cancelled`
//   - `tenant.destroy_due` (scheduler kind; see below)
//   - `tenant.schema_dropped` (tenancy constant
//     `EVENT_TENANT_SCHEMA_DROPPED`)
//   - `saas_domains.subdomain.release_requested` (subdomain revoke)
//   - `tenant.soft_delete_requested` (control-plane soft-delete)
//   - `tenant.storage_cleanup_requested`,
//     `tenant.backup_cleanup_requested`,
//     `tenant.search_index_cleanup_requested`
//   - `tenant.destroyed` (tenancy constant `EVENT_TENANT_DESTROYED`)
//   - `tenant.destroy.external_cleanup_completed`
//   - `tenant.destroy.failed` (fail-closed alert: drop failed, or
//     the schema still exists after the drop)
//
// Events this module listens to (all read defensively — a missing
// field or an unknown request is skipped, never an error):
//   - `export.completed` — marks the matching request `export_ready`.
//   - `tenant.storage_cleanup.completed`,
//     `tenant.backup_cleanup.completed`,
//     `tenant.search_index_cleanup.completed` — confirm one external
//     cleanup item.
//   - `tenant.destroyed.check` — hook query answered with
//     `{"tenant_id", "destroyed"}` for the provisioning guard.
//   - `tenant.destroy_due` — scheduler dispatch signal; the handler
//     only lists due requests, it never drops anything (hook
//     handlers have no `AppContext`). The scheduler arm must call
//     `process_due_destroys(ctx)`.
//
// Scheduler integration: the kind offered to `scheduler.enqueue`
// is `tenant.destroy_due` (`DESTROY_DUE_KIND`). The scheduler needs
// a dispatch arm for it that calls `process_due_destroys(ctx)`; until
// then an admin triggers the same sweep through
// `POST /destroy-requests/process-due`.
//
// Hook queries this module sends (fail closed when unanswered):
//   - `tenant.lookup` — `{"tenant_id", "tenant": null}`; the tenants
//     module answers with the tenant record (or a `tenant` object in
//     the results). No answer means the tenant cannot be resolved
//     and the flow refuses to proceed.
//   - `export.lookup` — `{"export_id", "export": null}`; the
//     import/export module answers with the export job status.
//   - `role.check` — capability checks, fail closed.
//
// HONESTY — READ THIS FIRST. What is real in this module:
// in-memory destroy-request / destroyed-tenants management with the
// full state machine above; typed confirmation with attempt counting
// and lockout; settings-driven second-approver rule and grace
// period; suspension-first flow with an explicit grace expiry;
// final destroy through the tenancy helper with post-drop
// verification that fails closed; the destroyed-uuidv7 guard; and
// external-cleanup tracking that never claims the destroy is
// complete for files, backups, or search indexes until each item is
// confirmed. What is NOT built here: durable storage (the DashMap
// stores are authoritative; the migration snippet for the
// `tenant_destroy_requests` and `destroyed_tenants` tables is in the
// builder report for the integrator to add to the tenants
// migration — this module creates no second migration); the tenant
// record itself and its lifecycle statuses (the tenants module owns
// them); the export job itself (the import/export module owns it);
// subdomain storage and release (the saas_domains module owns it);
// the actual deletion of stored files, backups, and search indexes
// (the owning modules own that — this module only requests and
// tracks it); and the scheduler dispatch arm for `tenant.destroy_due`
// (the scheduler module owns it — this module only declares the kind
// and offers `process_due_destroys`).
//
// CONFIG-DRIVEN — READ THIS FIRST. Nothing behavioural is
// hard-coded: the grace period, the second-approver age/size
// thresholds, the failed-confirmation limit and lockout, and the
// export entity name all live in `TenantDestroySettings`, editable
// by an administrator through `PUT /destroy-settings`. The values
// below are CONFIGURABLE DEFAULTS. The only hard-coded vocabulary
// is disclosed in the OPEN notes at the end of this header: the
// closed destroy-request statuses, the three external-cleanup kinds,
// the event names, and the capability names.
//
// Actor identity: until the auth module provides an extractor, the
// caller is identified by the `X-User-Id` header (required) and
// `X-Role` carries the role slug the capability check runs against,
// exactly like the import/export module. Capability checks use the
// `role.check` hook and fail closed (no answer means not allowed).
//
// OPEN — known gaps, recorded honestly instead of hidden:
//   - The tenants module (`src/modules/tenants.rs`) does not exist
//     yet (parallel builder B2 owns it), so `tenant.lookup` has no
//     answerer today and every destroy step fails closed with a
//     clear "tenant record unavailable" error until B2 lands the
//     listener. B2 must answer `tenant.lookup` with the tenant
//     record: `id`, `slug`, `status`, `created_at` (required — the
//     second-approver age rule needs it), `data_size_bytes`
//     (optional — the size rule is skipped when absent),
//     `destroyed_at` and `export_reference` when present. B2 must
//     also listen to `tenant.suspended` (flip the tenant to
//     `suspended`), `tenant.resumed` (resume serving),
//     `tenant.soft_delete_requested` (keep the row with
//     `destroyed_at` plus the export reference, status `deleted`),
//     and `tenant.destroyed.check` is answered here, not by B2.
//   - The import/export module answers `export.requested` with
//     scope `full_tenant_export` by running the export job, and
//     answers `export.lookup` with the job view, so the destroy
//     flow's export-first gate works end to end without a manual
//     export. The manual path still exists: an admin can create
//     the full export through `POST /api/v1/import-export/exports`
//     and attach the job id through
//     `POST /destroy-requests/{id}/export-proof`. OPEN: no module
//     answers the data-provider query for entity `tenant` yet, so
//     the listener refuses a zero-row export as an error until the
//     tenants module adds the provider.
//   - The saas_domains module answers
//     `saas_domains.subdomain.release_requested`: the destroy flow
//     emits it after the verified schema drop, and the listener
//     releases the tenant's allocations immediately and its
//     product subdomains with the configured grace. The revoke
//     request is also recorded on the destroy request; the
//     destroy is not blocked on the release, because serving
//     already stopped at suspension.
//   - The storage, backup, and search modules have no listeners for
//     the three `tenant.<area>_cleanup_requested` events and no
//     emitters for the matching `.completed` events today, so a
//     destroyed tenant normally waits in
//     `destroyed_pending_external_cleanup` until an admin confirms
//     each item (or the owning modules add listeners). The destroy
//     is never reported as complete for those areas before that.
//   - The audit module records only events listed in its
//     `AUDITED_EVENTS` table; the integrator must add the
//     `tenant.destroy.*`, `tenant.suspended`, `tenant.resumed`, and
//     `tenant.destroyed` events there for the emitted audit trail to
//     be persisted.
//   - The final-drop core (`run_final_destroy`) is shared by the
//     scheduler sweep (`process_due_destroys`) and by the manual
//     `POST /destroy-requests/{id}/execute` endpoint, with identical
//     gates. The tenancy helper documents the drop as "never called
//     from a request handler"; the manual endpoint exists so an
//     operator can execute a due destroy on demand, and it runs the
//     same audited core as the scheduler path. If the integrator
//     wants strict compliance with that note, the manual endpoint
//     should only enqueue `tenant.destroy_due` and the scheduler
//     sweep should be the sole drop caller.
//   - `cargo check` has never run in this environment (no Rust
//     toolchain, crates.io blocked), so compilation is NOT
//     verified; see the builder report for the verification that
//     was done instead. No migration has ever run against live
//     PostgreSQL, so the destroy flow is NOT proven live.

use crate::core::context::AppContext;
use crate::core::error::{UnifiedError, UnifiedResult};
use crate::core::hooks::{HookBus, HookContext};
use crate::core::json;
use crate::core::modules::{Module, ModuleRegistry, RouteInfo};
use crate::core::pagination::{PageResult, PaginationParams};
use crate::core::tenancy::{
    drop_tenant_schema, is_valid_schema_name, schema_exists, schema_name,
    EVENT_TENANT_DESTROY_REQUESTED, EVENT_TENANT_DESTROYED, EVENT_TENANT_SUSPENDED,
};
use crate::db::DbPool;
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

/// Maximum lengths enforced on user supplied text.
const MAX_REASON_LEN: usize = 2000;
const MAX_SLUG_LEN: usize = 100;
const MAX_NOTE_LEN: usize = 2000;

/// Capability names checked through the `role.check` hook.
pub const PERM_TENANTS_DESTROY: &str = "tenants:destroy";
pub const PERM_TENANTS_DESTROY_READ: &str = "tenants:destroy_read";

/// Scheduler kind offered to `scheduler.enqueue`, and the event the
/// scheduler emits when it dispatches the run.
pub const DESTROY_DUE_KIND: &str = "tenant.destroy_due";
pub const SCHEDULER_ENQUEUE: &str = "scheduler.enqueue";

/// Event names this module emits (beyond the tenancy and
/// import/export constants imported above).
pub const EVENT_EXPORT_READY: &str = "tenant.destroy.export_ready";
pub const EVENT_CONFIRMATION_FAILED: &str = "tenant.destroy.confirmation_failed";
pub const EVENT_SECOND_APPROVAL_REQUIRED: &str = "tenant.destroy.second_approval_required";
pub const EVENT_TENANT_RESUMED: &str = "tenant.resumed";
pub const EVENT_DESTROY_CANCELLED: &str = "tenant.destroy.cancelled";
pub const EVENT_DESTROY_FAILED: &str = "tenant.destroy.failed";
pub const EVENT_SUBDOMAIN_RELEASE_REQUESTED: &str = "saas_domains.subdomain.release_requested";
/// Superseded: the control-plane tenant row is now moved to
/// `deleted` directly through the tenants module's
/// `finish_deprovision_as_deleted` helper (single source of truth),
/// so this module no longer asks for a soft-delete by event. The
/// constant is kept so existing emitters/listeners keep compiling;
/// no listener for it exists in the tenants module.
pub const EVENT_TENANT_SOFT_DELETE_REQUESTED: &str = "tenant.soft_delete_requested";
pub const EVENT_STORAGE_CLEANUP_REQUESTED: &str = "tenant.storage_cleanup_requested";
pub const EVENT_BACKUP_CLEANUP_REQUESTED: &str = "tenant.backup_cleanup_requested";
pub const EVENT_SEARCH_INDEX_CLEANUP_REQUESTED: &str = "tenant.search_index_cleanup_requested";
pub const EVENT_EXTERNAL_CLEANUP_COMPLETED: &str = "tenant.destroy.external_cleanup_completed";

/// Event names this module listens to.
pub const EVENT_STORAGE_CLEANUP_COMPLETED: &str = "tenant.storage_cleanup.completed";
pub const EVENT_BACKUP_CLEANUP_COMPLETED: &str = "tenant.backup_cleanup.completed";
pub const EVENT_SEARCH_INDEX_CLEANUP_COMPLETED: &str = "tenant.search_index_cleanup.completed";
pub const EVENT_DESTROYED_CHECK: &str = "tenant.destroyed.check";

/// Hook queries this module sends.
pub const QUERY_TENANT_LOOKUP: &str = "tenant.lookup";
pub const QUERY_EXPORT_LOOKUP: &str = "export.lookup";

/// Destroy-request lifecycle statuses. Closed vocabulary: the state
/// machine in the header is the only legal sequence, enforced by
/// every transition function in this file.
pub const STATUS_REQUESTED: &str = "requested";
pub const STATUS_EXPORT_PENDING: &str = "export_pending";
pub const STATUS_EXPORT_READY: &str = "export_ready";
pub const STATUS_AWAITING_CONFIRMATION: &str = "awaiting_confirmation";
pub const STATUS_AWAITING_SECOND_APPROVAL: &str = "awaiting_second_approval";
pub const STATUS_SUSPENDED_GRACE: &str = "suspended_grace";
pub const STATUS_DESTROYED_PENDING_EXTERNAL_CLEANUP: &str = "destroyed_pending_external_cleanup";
pub const STATUS_DESTROYED: &str = "destroyed";
pub const STATUS_CANCELLED: &str = "cancelled";

/// Statuses from which a destroy request can still be cancelled.
/// `destroyed_pending_external_cleanup` is deliberately absent: the
/// schema is already gone, so there is nothing left to resume.
const CANCELLABLE_STATUSES: &[&str] = &[
    STATUS_REQUESTED,
    STATUS_EXPORT_PENDING,
    STATUS_EXPORT_READY,
    STATUS_AWAITING_CONFIRMATION,
    STATUS_AWAITING_SECOND_APPROVAL,
    STATUS_SUSPENDED_GRACE,
];

/// Terminal statuses: no further transition is possible.
const TERMINAL_STATUSES: &[&str] = &[STATUS_DESTROYED, STATUS_CANCELLED];

/// External-cleanup kinds. Closed vocabulary from the brief: the
/// three areas that live outside the tenant schema. Each item is
/// tracked separately and must be confirmed before the request
/// reaches `destroyed`.
pub const CLEANUP_STORAGE: &str = "storage";
pub const CLEANUP_BACKUPS: &str = "backups";
pub const CLEANUP_SEARCH_INDEX: &str = "search_index";
pub const EXTERNAL_CLEANUP_KINDS: &[&str] = &[CLEANUP_STORAGE, CLEANUP_BACKUPS, CLEANUP_SEARCH_INDEX];

// ---------------------------------------------------------------------------
// Configurable settings (admin-editable; defaults are defaults only)
// ---------------------------------------------------------------------------

/// All behaviour-shaping defaults of this module. An administrator
/// edits these through `PUT /destroy-settings`; every validation and
/// default in this file reads the live value, so changing a value
/// here changes what the module accepts without any code change.
/// The values below are CONFIGURABLE DEFAULTS.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TenantDestroySettings {
    /// Grace period in days between suspension and the final destroy.
    /// Stored as an explicit expiry timestamp on each request.
    pub grace_period_days: i64,
    /// Minimum tenant age in days that requires a second approver.
    /// `None` disables the age rule.
    pub second_approval_min_tenant_age_days: Option<i64>,
    /// Minimum tenant data size in bytes that requires a second
    /// approver. `None` disables the size rule. When both rules are
    /// disabled, no second approval is ever required.
    pub second_approval_min_data_size_bytes: Option<u64>,
    /// Failed typed confirmations allowed before the confirmation
    /// locks for `confirmation_lockout_minutes`.
    pub max_failed_confirmations: u32,
    /// Lockout in minutes after `max_failed_confirmations` failures.
    pub confirmation_lockout_minutes: i64,
    /// Entity name requested from the import/export module for the
    /// full tenant export.
    pub export_entity: String,
    /// Maximum days a destroy request may sit in `export_pending`
    /// before it is reported as stale by the list view. The request
    /// is never auto-cancelled; this only drives the `export_stale`
    /// flag so operators can see stuck requests.
    pub export_pending_stale_days: i64,
}

impl Default for TenantDestroySettings {
    fn default() -> Self {
        Self {
            grace_period_days: 30,
            second_approval_min_tenant_age_days: Some(365),
            second_approval_min_data_size_bytes: Some(10 * 1024 * 1024 * 1024),
            max_failed_confirmations: 5,
            confirmation_lockout_minutes: 30,
            export_entity: "tenant".to_string(),
            export_pending_stale_days: 7,
        }
    }
}

static SETTINGS: OnceLock<RwLock<TenantDestroySettings>> = OnceLock::new();

fn settings_store() -> &'static RwLock<TenantDestroySettings> {
    SETTINGS.get_or_init(|| RwLock::new(TenantDestroySettings::default()))
}

/// Current settings (cloned). Public so the integrator and tests can
/// read the effective defaults without an HTTP call.
pub fn destroy_settings() -> TenantDestroySettings {
    settings_store()
        .read()
        .map(|guard| guard.clone())
        .unwrap_or_default()
}

// ---------------------------------------------------------------------------
// Records.
// ---------------------------------------------------------------------------

/// One audit entry on a destroy request: who did what, and when.
/// The trail is append-only; entries are never edited or removed.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DestroyAuditEntry {
    pub at: DateTime<Utc>,
    pub actor: String,
    pub action: String,
    pub detail: String,
}

/// One external-cleanup item: an area outside the tenant schema that
/// must be cleaned up by its owning module and confirmed before the
/// destroy request reaches `destroyed`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExternalCleanupItem {
    pub kind: String,
    pub status: String,
    pub confirmed_at: Option<DateTime<Utc>>,
    pub confirmed_by: Option<String>,
    pub note: Option<String>,
}

/// One destroy request: the full state of a tenant destroy flow.
/// The in-memory store is authoritative until the migration lands;
/// the table shape for the integrator is in the builder report.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DestroyRequest {
    pub id: Uuid,
    pub tenant_id: Uuid,
    /// Tenant slug snapshot taken from the tenants module at request
    /// time. The typed confirmation is matched against this snapshot
    /// (and the live slug is re-checked at confirmation time).
    pub tenant_slug: String,
    pub status: String,
    pub requested_by: String,
    pub requested_at: DateTime<Utc>,
    pub reason: Option<String>,
    /// Proof of the mandatory export: the import/export job id.
    pub export_job_id: Option<Uuid>,
    pub export_requested_at: Option<DateTime<Utc>>,
    pub export_completed_at: Option<DateTime<Utc>>,
    pub export_download_url: Option<String>,
    /// How the export completion was proven: `completed_event` or
    /// `export_lookup`.
    pub export_verified_via: Option<String>,
    /// True when the `export.requested` event was emitted and at
    /// least one handler answered with an export job id. False means
    /// the export must be created through the import/export endpoint
    /// and attached manually (see the OPEN notes).
    pub export_request_claimed: bool,
    pub confirmation_attempts: u32,
    pub failed_confirmation_attempts: u32,
    pub confirmation_locked_until: Option<DateTime<Utc>>,
    pub confirmed_at: Option<DateTime<Utc>>,
    pub confirmed_by: Option<String>,
    pub second_approval_required: bool,
    pub second_approval_reason: Option<String>,
    pub second_approver: Option<String>,
    pub second_approved_at: Option<DateTime<Utc>>,
    pub grace_period_days: i64,
    pub grace_expires_at: Option<DateTime<Utc>>,
    pub suspended_at: Option<DateTime<Utc>>,
    pub destroyed_at: Option<DateTime<Utc>>,
    /// Schema name resolved at execution time, kept only as an audit
    /// snapshot. The drop itself always re-resolves the name from
    /// the tenant id through `schema_name()` at execution time.
    pub schema_name_snapshot: Option<String>,
    /// Whether the subdomain revoke request was emitted.
    pub subdomains_revoke_requested: bool,
    pub external_cleanup: Vec<ExternalCleanupItem>,
    pub audit_trail: Vec<DestroyAuditEntry>,
    pub cancel_reason: Option<String>,
    pub cancelled_by: Option<String>,
    pub cancelled_at: Option<DateTime<Utc>>,
    pub last_error: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl DestroyRequest {
    /// True when the request can no longer change state.
    pub fn is_terminal(&self) -> bool {
        TERMINAL_STATUSES.contains(&self.status.as_str())
    }

    /// True when the request can still be cancelled by an admin.
    pub fn is_cancellable(&self) -> bool {
        CANCELLABLE_STATUSES.contains(&self.status.as_str())
    }

    /// True when the grace period has expired (only meaningful in
    /// `suspended_grace`).
    pub fn grace_expired(&self) -> bool {
        match self.grace_expires_at {
            Some(expires_at) => Utc::now() >= expires_at,
            None => false,
        }
    }

    /// Seconds of grace remaining, if any (never negative).
    pub fn grace_remaining_seconds(&self) -> Option<i64> {
        self.grace_expires_at.map(|expires_at| {
            (expires_at - Utc::now()).num_seconds().max(0)
        })
    }

    /// True when every external-cleanup item is confirmed.
    pub fn external_cleanup_complete(&self) -> bool {
        !self.external_cleanup.is_empty()
            && self
                .external_cleanup
                .iter()
                .all(|item| item.status == "confirmed")
    }
}

/// Destroyed tenants have NO record of their own in this module.
/// The tenants row itself is the guard record: after the final
/// destroy it carries status `deleted` plus `destroyed_at` and the
/// export reference (moved there by the tenants module's
/// `finish_deprovision_as_deleted`). Keeping a second
/// destroyed-tenants store here would let the two drift, so the
/// only destroy-state queries in this file are `tenant.lookup`
/// (the tenants row) and `is_destroyed_tenant()` below.

/// The tenant record as answered by the tenants module through the
/// `tenant.lookup` hook query. This module never reads the tenants
/// tables directly; this shape is the contract B2 must answer.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TenantRef {
    pub id: Uuid,
    pub slug: String,
    pub status: String,
    pub created_at: Option<DateTime<Utc>>,
    pub data_size_bytes: Option<u64>,
    pub destroyed_at: Option<DateTime<Utc>>,
    pub export_reference: Option<String>,
}

// ---------------------------------------------------------------------------
// Stores.
// ---------------------------------------------------------------------------

static DESTROY_REQUESTS: OnceLock<DashMap<Uuid, DestroyRequest>> = OnceLock::new();
static HOOK_BUS: OnceLock<Arc<HookBus>> = OnceLock::new();

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
static SHARED_TENANTS_STATE: OnceLock<TenantsState> = OnceLock::new();

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
fn tenants_state() -> UnifiedResult<TenantsState> {
    SHARED_TENANTS_STATE.get().cloned().ok_or_else(|| {
        UnifiedError::internal(
            "Shared TenantsState is not wired: hand one TenantsState clone to TenantsModule::with_state and the same clone to TenantDestroyModule::with_state (or tenant_destroy::set_shared_tenants_state); destroy refuses to proceed without it",
        )
    })
}

fn destroy_requests_store() -> &'static DashMap<Uuid, DestroyRequest> {
    DESTROY_REQUESTS.get_or_init(DashMap::new)
}

/// True when the tenants module's record for this tenant says it
/// is already destroyed. Answered through the `tenant.lookup`
/// hook query on the stored bus (hook handlers have no
/// AppContext), so the tenants row is the single source of truth.
/// A tenant the registry does not know is NOT destroyed: a
/// destroyed tenant always keeps its row (status `deleted`),
/// so "no record" can only mean "never existed".
/// Public so provisioning (tenants module / integrator) can
/// consult the guard without going through the hook bus.
pub fn is_destroyed_tenant(tenant_id: &Uuid) -> bool {
    let Some(bus) = HOOK_BUS.get() else {
        return false;
    };
    let context = bus.emit(
        QUERY_TENANT_LOOKUP,
        json!({ "tenant_id": tenant_id.to_string(), "tenant": null }),
    );
    tenant_ref_from_context(&context)
        .is_some_and(|tenant| tenant_already_destroyed(&tenant))
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

// ---------------------------------------------------------------------------
// Caller identity and capability checks (same trusted-parameter
// pattern as the import/export module: X-User-Id plus X-Role
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
    let context = ctx.hooks.emit(
        "role.check",
        json!({ "role": role, "permission": permission, "allowed": false }),
    );
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

fn parse_uuid(raw: &str, what: &str) -> UnifiedResult<Uuid> {
    Uuid::parse_str(raw.trim()).map_err(|_| UnifiedError::bad_request(format!("Invalid {what}: expected a UUID")))
}

fn validate_reason(raw: &Option<String>) -> UnifiedResult<Option<String>> {
    match raw {
        None => Ok(None),
        Some(value) => {
            let trimmed = value.trim();
            if trimmed.is_empty() {
                return Ok(None);
            }
            if trimmed.chars().count() > MAX_REASON_LEN {
                return Err(UnifiedError::bad_request(format!(
                    "reason must be at most {MAX_REASON_LEN} characters"
                )));
            }
            Ok(Some(trimmed.to_string()))
        }
    }
}

fn append_audit(request: &mut DestroyRequest, actor: &str, action: &str, detail: &str) {
    request.audit_trail.push(DestroyAuditEntry {
        at: now(),
        actor: actor.to_string(),
        action: action.to_string(),
        detail: detail.to_string(),
    });
    request.updated_at = now();
}

/// Emit a lifecycle event in the audit shape the audit module
/// records (`actor` / `actor_id`, `resource`, plus the payload).
/// The audit module only persists events listed in its
/// `AUDITED_EVENTS` table (see the OPEN notes).
fn emit_audit(ctx: &AppContext, event: &str, actor: &str, tenant_id: Uuid, extra: Value) {
    let mut payload = json!({
        "actor": actor,
        "actor_id": actor,
        "resource": format!("tenant:{tenant_id}"),
        "tenant_id": tenant_id.to_string(),
    });
    if let (Some(target), Some(source)) = (payload.as_object_mut(), extra.as_object()) {
        for (key, value) in source {
            target.insert(key.clone(), value.clone());
        }
    }
    ctx.hooks.emit(event, payload);
}

/// Emit an event from a hook handler (no `AppContext` there).
fn emit_from_hook(event: &str, payload: Value) {
    if let Some(bus) = HOOK_BUS.get() {
        bus.emit(event, payload);
    }
}

fn get_request(id: &Uuid) -> UnifiedResult<DestroyRequest> {
    destroy_requests_store()
        .get(id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found("Destroy request not found"))
}

fn save_request(request: &DestroyRequest) {
    destroy_requests_store().insert(request.id, request.clone());
}

// ---------------------------------------------------------------------------
// Tenant resolution seam — the ONLY way this module reads a tenant
// record. The tenants module answers the `tenant.lookup` hook query;
// this module never queries the tenants tables (or any other
// module's tables) directly. Fail closed: no answer, or an answer
// that does not match the requested id, refuses to proceed.
// ---------------------------------------------------------------------------

/// Parse one hook result value into a `TenantRef`. Accepts either a
/// bare tenant object (`{"id", "slug", ...}`) or an object carrying
/// a nested `tenant` object.
fn parse_tenant_ref(value: &Value) -> Option<TenantRef> {
    let object = value
        .get("tenant")
        .filter(|nested| nested.is_object())
        .unwrap_or(value);
    let id = object
        .get("id")
        .and_then(Value::as_str)
        .and_then(|raw| Uuid::parse_str(raw).ok())?;
    let slug = object.get("slug").and_then(Value::as_str)?.to_string();
    if slug.is_empty() || slug.chars().count() > MAX_SLUG_LEN {
        return None;
    }
    let status = object
        .get("status")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let created_at = object
        .get("created_at")
        .and_then(Value::as_str)
        .and_then(|raw| DateTime::parse_from_rfc3339(raw).ok())
        .map(|parsed| parsed.with_timezone(&Utc));
    let data_size_bytes = object
        .get("data_size_bytes")
        .and_then(Value::as_u64);
    let destroyed_at = object
        .get("destroyed_at")
        .and_then(Value::as_str)
        .and_then(|raw| DateTime::parse_from_rfc3339(raw).ok())
        .map(|parsed| parsed.with_timezone(&Utc));
    let export_reference = object
        .get("export_reference")
        .and_then(Value::as_str)
        .map(str::to_string);
    Some(TenantRef {
        id,
        slug,
        status,
        created_at,
        data_size_bytes,
        destroyed_at,
        export_reference,
    })
}

/// First parseable `TenantRef` in a `tenant.lookup` answer: the
/// `tenant` payload key first, then any result. Shared by
/// `resolve_tenant` (AppContext path) and `is_destroyed_tenant`
/// (stored-bus path) so both read the same answer shape.
fn tenant_ref_from_context(context: &HookContext) -> Option<TenantRef> {
    if let Some(found) = parse_tenant_ref(context.get("tenant").unwrap_or(&Value::Null)) {
        return Some(found);
    }
    context.results.iter().find_map(parse_tenant_ref)
}

/// Resolve the tenant record through the tenants module. Fails
/// closed when nothing answers the query, when the answer does not
/// carry the requested id, or when the record has no usable slug.
fn resolve_tenant(ctx: &AppContext, tenant_id: &Uuid) -> UnifiedResult<TenantRef> {
    let context = ctx.hooks.emit(
        QUERY_TENANT_LOOKUP,
        json!({ "tenant_id": tenant_id.to_string(), "tenant": null }),
    );
    let tenant = tenant_ref_from_context(&context).ok_or_else(|| {
        UnifiedError::internal(
            "Tenant record unavailable: the tenants module did not answer the tenant lookup; destroy refuses to proceed",
        )
    })?;
    if &tenant.id != tenant_id {
        return Err(UnifiedError::internal(
            "Tenant lookup answered with a different tenant id; destroy refuses to proceed",
        ));
    }
    Ok(tenant)
}

/// True when the tenant record says the tenant is already gone
/// (`deleted` status or a `destroyed_at` timestamp).
fn tenant_already_destroyed(tenant: &TenantRef) -> bool {
    tenant.status == STATUS_DESTROYED
        || tenant.status == "deleted"
        || tenant.destroyed_at.is_some()
}

// ---------------------------------------------------------------------------
// Export verification seam.
// ---------------------------------------------------------------------------

/// Ask the import/export module for one export job's status through
/// the `export.lookup` hook query. Returns `Some((status,
/// download_url))` when a handler answers, `None` when nothing
/// answers (the caller then relies on the `export.completed` event).
fn lookup_export(ctx: &AppContext, export_id: &Uuid) -> Option<(String, Option<String>)> {
    let context = ctx.hooks.emit(
        QUERY_EXPORT_LOOKUP,
        json!({ "export_id": export_id.to_string(), "export": null }),
    );
    let mut candidates: Vec<&Value> = Vec::new();
    if let Some(export) = context.get("export") {
        if export.is_object() {
            candidates.push(export);
        }
    }
    for result in &context.results {
        candidates.push(result);
    }
    for candidate in candidates {
        let object = candidate
            .get("export")
            .filter(|nested| nested.is_object())
            .unwrap_or(candidate);
        if let Some(status) = object.get("status").and_then(Value::as_str) {
            let download_url = object
                .get("download_url")
                .and_then(Value::as_str)
                .map(str::to_string);
            return Some((status.to_string(), download_url));
        }
    }
    None
}

/// Mark the export on a request as completed and downloadable, and
/// move the request to `export_ready`. Called from the
/// `export.completed` hook handler and from the attach-proof route
/// when the lookup query answers `completed`.
fn mark_export_ready(request: &mut DestroyRequest, download_url: Option<String>, via: &str) {
    let completed_at = now();
    request.export_completed_at = Some(completed_at);
    if download_url.is_some() {
        request.export_download_url = download_url;
    }
    request.export_verified_via = Some(via.to_string());
    request.status = STATUS_EXPORT_READY.to_string();
    append_audit(
        request,
        "system",
        "export_completed",
        &format!("Full tenant export completed and downloadable (verified via {via})."),
    );
}

// ---------------------------------------------------------------------------
// Second-approver rule (settings-driven, never hard-coded).
// ---------------------------------------------------------------------------

/// Evaluate the second-approver rule for a tenant. Returns
/// `Some(reason)` when a second approver is required, `None` when
/// the requester alone may proceed. The age rule needs the tenant
/// `created_at`; when it is missing the flow fails closed instead of
/// guessing. The size rule is skipped when the tenants module does
/// not report a data size.
fn second_approval_requirement(
    settings: &TenantDestroySettings,
    tenant: &TenantRef,
) -> UnifiedResult<Option<String>> {
    let mut reasons: Vec<String> = Vec::new();
    if let Some(min_age_days) = settings.second_approval_min_tenant_age_days {
        let created_at = tenant.created_at.ok_or_else(|| {
            UnifiedError::internal(
                "Tenant age unavailable: the tenants module did not report created_at; second-approver rule cannot be evaluated, destroy refuses to proceed",
            )
        })?;
        let age_days = (now() - created_at).num_days();
        if age_days >= min_age_days {
            reasons.push(format!(
                "tenant age {age_days} days meets the threshold of {min_age_days} days"
            ));
        }
    }
    if let Some(min_size) = settings.second_approval_min_data_size_bytes {
        if let Some(size) = tenant.data_size_bytes {
            if size >= min_size {
                reasons.push(format!(
                    "tenant data size {size} bytes meets the threshold of {min_size} bytes"
                ));
            }
        }
    }
    if reasons.is_empty() {
        Ok(None)
    } else {
        Ok(Some(reasons.join("; ")))
    }
}

// ---------------------------------------------------------------------------
// Suspension + grace (shared core: typed confirmation and second
// approval both end here).
// ---------------------------------------------------------------------------

/// Suspend the tenant and start the grace period. Only ever called
/// after export proof, typed confirmation, and (when required) the
/// second approval are all on the record. Emits `tenant.suspended`
/// so the tenants module stops serving the tenant; the grace expiry
/// is stored as an explicit timestamp on the request.
fn enter_suspension_and_grace(
    ctx: &AppContext,
    request: &mut DestroyRequest,
    actor: &str,
) -> UnifiedResult<()> {
    let tenant = resolve_tenant(ctx, &request.tenant_id)?;
    if tenant_already_destroyed(&tenant) {
        return Err(UnifiedError::conflict(
            "Tenant is already destroyed; the destroy request cannot proceed",
        ));
    }
    if tenant.slug != request.tenant_slug {
        return Err(UnifiedError::internal(
            "Tenant slug changed since the destroy was requested; destroy refuses to proceed",
        ));
    }
    let settings = destroy_settings();
    let schema = schema_name(request.tenant_id);
    if !is_valid_schema_name(&schema) {
        return Err(UnifiedError::internal(
            "Built tenant schema name failed validation; destroy refuses to proceed",
        ));
    }
    let grace_days = settings.grace_period_days;
    let suspended_at = now();
    let grace_expires_at = suspended_at + chrono::Duration::days(grace_days);
    request.suspended_at = Some(suspended_at);
    request.grace_period_days = grace_days;
    request.grace_expires_at = Some(grace_expires_at);
    request.schema_name_snapshot = Some(schema.clone());
    request.status = STATUS_SUSPENDED_GRACE.to_string();
    append_audit(
        request,
        actor,
        "suspended_grace_started",
        &format!(
            "Tenant suspended; grace period of {grace_days} days started, expiring at {grace_expires_at}."
        ),
    );
    save_request(request);
    emit_audit(
        ctx,
        EVENT_TENANT_SUSPENDED,
        actor,
        request.tenant_id,
        json!({
            "destroy_request_id": request.id.to_string(),
            "schema": schema,
            "reason": "destroy_confirmed",
            "grace_expires_at": grace_expires_at.to_rfc3339(),
        }),
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// Final destroy (shared core: manual execute endpoint and scheduler
// sweep both run this, with identical gates).
// ---------------------------------------------------------------------------

/// Record a failure that happened after the schema drop on the
/// request (last error + audit trail + `tenant.destroy.failed`
/// alert) and return the error. The schema is already gone at
/// this point, so the request must show exactly where the flow
/// stopped instead of looking retryable-clean.
fn fail_after_drop(
    ctx: &AppContext,
    request: &mut DestroyRequest,
    actor: &str,
    kind: &str,
    error: UnifiedError,
) -> UnifiedError {
    request.last_error = Some(error.message.clone());
    append_audit(
        request,
        actor,
        "destroy_failed",
        &format!("{kind}: {}.", error.message),
    );
    save_request(request);
    emit_audit(
        ctx,
        EVENT_DESTROY_FAILED,
        actor,
        request.tenant_id,
        json!({
            "destroy_request_id": request.id.to_string(),
            "kind": kind,
            "error": error.message,
        }),
    );
    error
}

/// Run the final destroy for one request. Preconditions, all
/// enforced here so no caller can reach the schema drop without
/// them: state `suspended_grace`, grace expired, export proof on
/// record, typed confirmation on record, second approval on record
/// when required. The tenant record moves through the tenants
/// module's helpers only (`begin_deprovision` before the drop,
/// `set_export_reference` + `finish_deprovision_as_deleted` after
/// the verified drop) over the one shared `TenantsState`. The schema name is resolved from the tenant
/// uuidv7 through `schema_name()` at execution time; the drop goes
/// through the tenancy helper only; the drop is verified with
/// `schema_exists` and any failure fails closed with an alert
/// event, leaving the request in `suspended_grace`.
fn run_final_destroy(
    ctx: &AppContext,
    request_id: &Uuid,
    actor: &str,
) -> UnifiedResult<DestroyRequest> {
    let mut request = get_request(request_id)?;
    if request.status != STATUS_SUSPENDED_GRACE {
        return Err(UnifiedError::conflict(format!(
            "Only a destroy request in status \"{}\" can be executed (current status: \"{}\")",
            STATUS_SUSPENDED_GRACE, request.status
        )));
    }
    if !request.grace_expired() {
        return Err(UnifiedError::conflict(
            "Grace period has not expired; the tenant cannot be destroyed yet",
        ));
    }
    if request.export_job_id.is_none() || request.export_completed_at.is_none() {
        return Err(UnifiedError::conflict(
            "No completed export proof is on record; the tenant cannot be destroyed",
        ));
    }
    if request.confirmed_at.is_none() {
        return Err(UnifiedError::conflict(
            "No typed confirmation is on record; the tenant cannot be destroyed",
        ));
    }
    if request.second_approval_required && request.second_approved_at.is_none() {
        return Err(UnifiedError::conflict(
            "Second approval is required and has not been recorded; the tenant cannot be destroyed",
        ));
    }
    let tenant = resolve_tenant(ctx, &request.tenant_id)?;
    if tenant_already_destroyed(&tenant) {
        return Err(UnifiedError::conflict(
            "Tenant is already destroyed; the destroy request cannot be executed again",
        ));
    }
    // Move the tenant record to `deprovisioning` through the
    // tenants module — the single source of truth for destroy
    // state. Skipped when a previous attempt already moved it
    // there, so a retry after a failed drop resumes instead of
    // failing the transition (`deprovisioning` ->
    // `deprovisioning` is not a legal transition).
    let state = tenants_state()?;
    if tenant.status != STATUS_DEPROVISIONING {
        begin_deprovision(
            ctx,
            &state,
            request.tenant_id,
            actor,
            request.reason.as_deref().unwrap_or("tenant destroy executed"),
        )?;
    }
    let pool: DbPool = ctx
        .db
        .read()
        .map_err(|_| UnifiedError::internal("Database lock poisoned"))?
        .clone()
        .ok_or_else(|| {
            UnifiedError::internal(
                "Database pool unavailable; destroy refuses to proceed",
            )
        })?;
    let schema = schema_name(request.tenant_id);
    if !is_valid_schema_name(&schema) {
        return Err(UnifiedError::internal(
            "Built tenant schema name failed validation; destroy refuses to proceed",
        ));
    }
    // The irreversible step. On any failure the request stays in
    // `suspended_grace` with the error recorded, and an alert event
    // is emitted; nothing after the drop runs.
    if let Err(error) = drop_tenant_schema(&pool, request.tenant_id) {
        request.last_error = Some(error.message.clone());
        append_audit(
            &mut request,
            actor,
            "destroy_failed",
            &format!("Schema drop failed: {}.", error.message),
        );
        save_request(&request);
        emit_audit(
            ctx,
            EVENT_DESTROY_FAILED,
            actor,
            request.tenant_id,
            json!({
                "destroy_request_id": request.id.to_string(),
                "kind": "drop_failed",
                "error": error.message,
            }),
        );
        return Err(error);
    }
    // Verify the schema is gone. A surviving schema means the drop
    // did not take effect: fail closed, alert, never proceed.
    match schema_exists(&pool, request.tenant_id) {
        Ok(true) => {
            let message = "Schema still exists after the drop; destroy refuses to proceed";
            request.last_error = Some(message.to_string());
            append_audit(&mut request, actor, "destroy_failed", message);
            save_request(&request);
            emit_audit(
                ctx,
                EVENT_DESTROY_FAILED,
                actor,
                request.tenant_id,
                json!({
                    "destroy_request_id": request.id.to_string(),
                    "kind": "drop_verification_failed",
                    "error": message,
                }),
            );
            return Err(UnifiedError::internal(message));
        }
        Ok(false) => {}
        Err(error) => {
            request.last_error = Some(error.message.clone());
            append_audit(
                &mut request,
                actor,
                "destroy_failed",
                &format!("Post-drop verification failed: {}.", error.message),
            );
            save_request(&request);
            emit_audit(
                ctx,
                EVENT_DESTROY_FAILED,
                actor,
                request.tenant_id,
                json!({
                    "destroy_request_id": request.id.to_string(),
                    "kind": "drop_verification_error",
                    "error": error.message,
                }),
            );
            return Err(error);
        }
    }
    let export_reference = request.export_job_id.map(|id| id.to_string());
    // The tenant record moves through the tenants module only:
    // record the export reference, then finish deprovisioning
    // (status `deleted`, `destroyed_at` stamped, `tenant.deleted`
    // emitted). The row IS the guard record now — no parallel
    // store is written. A failure here is recorded on the request
    // like a drop failure: the schema is already gone, so the
    // operator must see exactly where the flow stopped.
    if let Some(reference) = export_reference.as_deref() {
        if let Err(error) = set_export_reference(&state, request.tenant_id, reference) {
            return Err(fail_after_drop(
                ctx,
                &mut request,
                actor,
                "export_reference_failed",
                error,
            ));
        }
    }
    let deleted = match finish_deprovision_as_deleted(ctx, &state, request.tenant_id, actor) {
        Ok(tenant) => tenant,
        Err(error) => {
            return Err(fail_after_drop(
                ctx,
                &mut request,
                actor,
                "finish_deprovision_failed",
                error,
            ));
        }
    };
    let destroyed_at = deleted.destroyed_at.unwrap_or_else(now);
    request.destroyed_at = Some(destroyed_at);
    request.schema_name_snapshot = Some(schema.clone());
    request.status = STATUS_DESTROYED_PENDING_EXTERNAL_CLEANUP.to_string();
    request.external_cleanup = EXTERNAL_CLEANUP_KINDS
        .iter()
        .map(|kind| ExternalCleanupItem {
            kind: kind.to_string(),
            status: "pending".to_string(),
            confirmed_at: None,
            confirmed_by: None,
            note: None,
        })
        .collect();
    append_audit(
        &mut request,
        actor,
        "schema_dropped",
        &format!("Tenant schema {schema} dropped and verified gone."),
    );
    save_request(&request);
    // No guard record is written here: the tenants row moved to
    // `deleted` by `finish_deprovision_as_deleted` above IS the
    // guard, so the uuidv7 can never be provisioned or re-created
    // again.
    ctx.hooks.emit(
        crate::core::tenancy::EVENT_TENANT_SCHEMA_DROPPED,
        json!({
            "tenant_id": request.tenant_id.to_string(),
            "schema": schema,
            "destroy_request_id": request.id.to_string(),
            "actor": actor,
        }),
    );
    // Subdomain revocation is the saas_domains module's job; this
    // module only requests it. The revoke is not a blocker for the
    // destroy: serving already stopped at suspension.
    request.subdomains_revoke_requested = true;
    save_request(&request);
    ctx.hooks.emit(
        EVENT_SUBDOMAIN_RELEASE_REQUESTED,
        json!({
            "tenant_id": request.tenant_id.to_string(),
            "schema": schema,
            "destroy_request_id": request.id.to_string(),
            "actor": actor,
        }),
    );
    // External cleanup: the schema drop removed database rows only.
    // Stored files, backups, and search indexes are requested here
    // and tracked until each owning module confirms.
    for (event, kind) in [
        (EVENT_STORAGE_CLEANUP_REQUESTED, CLEANUP_STORAGE),
        (EVENT_BACKUP_CLEANUP_REQUESTED, CLEANUP_BACKUPS),
        (EVENT_SEARCH_INDEX_CLEANUP_REQUESTED, CLEANUP_SEARCH_INDEX),
    ] {
        ctx.hooks.emit(
            event,
            json!({
                "tenant_id": request.tenant_id.to_string(),
                "schema": schema,
                "destroy_request_id": request.id.to_string(),
                "cleanup_kind": kind,
                "export_reference": export_reference,
                "actor": actor,
            }),
        );
    }
    append_audit(
        &mut request,
        actor,
        "destroyed_pending_external_cleanup",
        "Schema dropped and verified; subdomain revoke, control-plane soft-delete, and external cleanup requested.",
    );
    save_request(&request);
    emit_audit(
        ctx,
        EVENT_TENANT_DESTROYED,
        actor,
        request.tenant_id,
        json!({
            "destroy_request_id": request.id.to_string(),
            "schema": schema,
            "export_id": export_reference,
            "destroyed_at": destroyed_at.to_rfc3339(),
            "external_cleanup_pending": EXTERNAL_CLEANUP_KINDS,
        }),
    );
    Ok(request)
}

/// Run the final destroy for every grace-expired request. Called by
/// the scheduler arm for `tenant.destroy_due` and by the manual
/// sweep endpoint. One failure never blocks the others: failures
/// stay recorded on their requests with a `tenant.destroy.failed`
/// event. Returns the ids that were destroyed in this run.
pub fn process_due_destroys(ctx: &AppContext) -> UnifiedResult<Vec<Uuid>> {
    let mut destroyed: Vec<Uuid> = Vec::new();
    for due in due_destroy_requests() {
        if run_final_destroy(ctx, &due.id, "system").is_ok() {
            destroyed.push(due.id);
        }
    }
    Ok(destroyed)
}

/// Confirm one external-cleanup item on a request. When every item
/// is confirmed the request reaches `destroyed`. Shared by the HTTP
/// confirm endpoint and the `tenant.<area>_cleanup.completed` hook
/// handlers. Idempotent: confirming an already confirmed item is a
/// no-op that returns the current request.
fn confirm_external_cleanup_item(
    request_id: &Uuid,
    kind: &str,
    confirmed_by: &str,
    note: Option<String>,
) -> Option<DestroyRequest> {
    let mut request = destroy_requests_store().get_mut(request_id)?;
    if request.status != STATUS_DESTROYED_PENDING_EXTERNAL_CLEANUP {
        return None;
    }
    let mut changed = false;
    for item in request.external_cleanup.iter_mut() {
        if item.kind == kind && item.status != "confirmed" {
            item.status = "confirmed".to_string();
            item.confirmed_at = Some(now());
            item.confirmed_by = Some(confirmed_by.to_string());
            item.note = note.clone();
            changed = true;
        }
    }
    if !changed {
        return Some(request.clone());
    }
    append_audit(
        &mut request,
        confirmed_by,
        "external_cleanup_confirmed",
        &format!("External cleanup confirmed for area \"{kind}\"."),
    );
    if request.external_cleanup_complete() {
        request.status = STATUS_DESTROYED.to_string();
        append_audit(
            &mut request,
            confirmed_by,
            "destroyed",
            "All external cleanup confirmed; tenant destroy is complete.",
        );
    }
    let completed = request.status == STATUS_DESTROYED;
    let finished = request.clone();
    drop(request);
    if completed {
        emit_from_hook(
            EVENT_EXTERNAL_CLEANUP_COMPLETED,
            json!({
                "tenant_id": finished.tenant_id.to_string(),
                "destroy_request_id": finished.id.to_string(),
                "actor": confirmed_by,
            }),
        );
    }
    Some(finished)
}

// ---------------------------------------------------------------------------
// Request bodies.
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct RequestDestroyBody {
    tenant_id: Uuid,
    reason: Option<String>,
}

#[derive(Debug, Deserialize)]
struct AttachExportProofBody {
    export_job_id: Uuid,
    download_url: Option<String>,
}

#[derive(Debug, Deserialize)]
struct TypedConfirmationBody {
    typed_slug: String,
}

#[derive(Debug, Deserialize)]
struct SecondApprovalBody {
    note: Option<String>,
}

#[derive(Debug, Deserialize)]
struct CancelDestroyBody {
    reason: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ConfirmExternalCleanupBody {
    item: String,
    note: Option<String>,
}

#[derive(Debug, Deserialize)]
struct UpdateSettingsBody {
    grace_period_days: Option<i64>,
    second_approval_min_tenant_age_days: Option<Option<i64>>,
    second_approval_min_data_size_bytes: Option<Option<u64>>,
    max_failed_confirmations: Option<u32>,
    confirmation_lockout_minutes: Option<i64>,
    export_entity: Option<String>,
    export_pending_stale_days: Option<i64>,
}

// ---------------------------------------------------------------------------
// HTTP handlers. Every handler checks a capability first.
// ---------------------------------------------------------------------------

async fn request_destroy(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    body: web::Json<RequestDestroyBody>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<DestroyRequest> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_TENANTS_DESTROY)?;
        let input = body.into_inner();
        let reason = validate_reason(&input.reason)?;
        // Fail closed: the tenant record must resolve before anything
        // is created, and a destroyed tenant can never be destroyed
        // again.
        let tenant = resolve_tenant(&ctx, &input.tenant_id)?;
        if tenant_already_destroyed(&tenant) {
            return Err(UnifiedError::conflict(
                "Tenant is already destroyed",
            ));
        }
        let active = destroy_requests_store()
            .iter()
            .map(|entry| entry.value().clone())
            .find(|existing| {
                existing.tenant_id == input.tenant_id && !existing.is_terminal()
            });
        if let Some(existing) = active {
            return Err(UnifiedError::conflict(format!(
                "Tenant already has an active destroy request {} in status \"{}\"",
                existing.id, existing.status
            )));
        }
        let settings = destroy_settings();
        let created_at = now();
        let mut request = DestroyRequest {
            id: Uuid::now_v7(),
            tenant_id: input.tenant_id,
            tenant_slug: tenant.slug.clone(),
            status: STATUS_REQUESTED.to_string(),
            requested_by: caller.actor.clone(),
            requested_at: created_at,
            reason: reason.clone(),
            export_job_id: None,
            export_requested_at: None,
            export_completed_at: None,
            export_download_url: None,
            export_verified_via: None,
            export_request_claimed: false,
            confirmation_attempts: 0,
            failed_confirmation_attempts: 0,
            confirmation_locked_until: None,
            confirmed_at: None,
            confirmed_by: None,
            second_approval_required: false,
            second_approval_reason: None,
            second_approver: None,
            second_approved_at: None,
            grace_period_days: settings.grace_period_days,
            grace_expires_at: None,
            suspended_at: None,
            destroyed_at: None,
            schema_name_snapshot: None,
            subdomains_revoke_requested: false,
            external_cleanup: Vec::new(),
            audit_trail: Vec::new(),
            cancel_reason: None,
            cancelled_by: None,
            cancelled_at: None,
            last_error: None,
            created_at,
            updated_at: created_at,
        };
        append_audit(
            &mut request,
            &caller.actor,
            "requested",
            &format!(
                "Destroy requested for tenant \"{}\" ({}).",
                tenant.slug, tenant.id
            ),
        );
        emit_audit(
            &ctx,
            EVENT_TENANT_DESTROY_REQUESTED,
            &caller.actor,
            input.tenant_id,
            json!({
                "destroy_request_id": request.id.to_string(),
                "tenant_slug": tenant.slug,
                "reason": reason,
            }),
        );
        // Mandatory export first: ask the import/export module for a
        // full tenant export. A handler that answers with an export
        // job id claims the request; otherwise the export is created
        // through the import/export endpoint and attached later.
        let export_context = ctx.hooks.emit(
            import_export_events::EXPORT_REQUESTED,
            json!({
                "entity": settings.export_entity,
                "tenant_id": input.tenant_id.to_string(),
                "scope": "full_tenant_export",
                "destroy_request_id": request.id.to_string(),
                "format": "json",
                "actor": caller.actor,
            }),
        );
        let mut claimed_job: Option<Uuid> = None;
        for result in &export_context.results {
            let candidate = result
                .get("export_id")
                .or_else(|| result.get("export_job_id"))
                .or_else(|| result.get("job_id"));
            if let Some(raw) = candidate.and_then(Value::as_str) {
                if let Ok(job_id) = Uuid::parse_str(raw) {
                    claimed_job = Some(job_id);
                    break;
                }
            }
        }
        request.export_requested_at = Some(now());
        request.export_job_id = claimed_job;
        request.export_request_claimed = claimed_job.is_some();
        request.status = STATUS_EXPORT_PENDING.to_string();
        append_audit(
            &mut request,
            &caller.actor,
            "export_requested",
            if claimed_job.is_some() {
                "Full tenant export requested; the export module claimed the request."
            } else {
                "Full tenant export requested; no export handler answered, so the export job must be created through the import/export endpoint and attached as proof."
            },
        );
        save_request(&request);
        Ok(request)
    })();
    match result {
        Ok(view) => json::success_response(view, 201),
        Err(error) => json::error_response(&error, debug),
    }
}

async fn list_destroy_requests(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    params: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_TENANTS_DESTROY_READ)?;
        let settings = destroy_settings();
        let stale_after =
            now() - chrono::Duration::days(settings.export_pending_stale_days);
        let mut items: Vec<Value> = destroy_requests_store()
            .iter()
            .map(|entry| entry.value().clone())
            .collect::<Vec<DestroyRequest>>()
            .into_iter()
            .map(|request| {
                let mut view = serde_json::to_value(&request).unwrap_or(Value::Null);
                if request.status == STATUS_EXPORT_PENDING && request.created_at < stale_after {
                    view["export_stale"] = Value::Bool(true);
                } else {
                    view["export_stale"] = Value::Bool(false);
                }
                view
            })
            .collect();
        if let Some(status) = params.filters.get("status") {
            items.retain(|item| item.get("status").and_then(Value::as_str) == Some(status.as_str()));
        }
        if let Some(tenant_id) = params.filters.get("tenant_id") {
            items.retain(|item| {
                item.get("tenant_id").and_then(Value::as_str) == Some(tenant_id.as_str())
            });
        }
        if let Some(requested_by) = params.filters.get("requested_by") {
            items.retain(|item| {
                item.get("requested_by").and_then(Value::as_str) == Some(requested_by.as_str())
            });
        }
        items.sort_by(|a, b| {
            let a_at = a.get("created_at").and_then(Value::as_str).unwrap_or("");
            let b_at = b.get("created_at").and_then(Value::as_str).unwrap_or("");
            b_at.cmp(a_at)
        });
        if params.normalized().order == "asc" {
            items.reverse();
        }
        Ok(paginate(items, &params).to_json())
    })();
    json::respond(result, debug)
}

async fn get_destroy_request(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    id: web::Path<Uuid>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<DestroyRequest> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_TENANTS_DESTROY_READ)?;
        get_request(&id.into_inner())
    })();
    json::respond(result, debug)
}

async fn attach_export_proof(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    id: web::Path<Uuid>,
    body: web::Json<AttachExportProofBody>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<DestroyRequest> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_TENANTS_DESTROY)?;
        let input = body.into_inner();
        let mut request = get_request(&id.into_inner())?;
        if request.status != STATUS_REQUESTED && request.status != STATUS_EXPORT_PENDING {
            return Err(UnifiedError::conflict(format!(
                "Export proof can only be attached while the request is \"{}\" or \"{}\" (current status: \"{}\")",
                STATUS_REQUESTED, STATUS_EXPORT_PENDING, request.status
            )));
        }
        request.export_job_id = Some(input.export_job_id);
        request.export_request_claimed = true;
        if let Some(url) = input.download_url {
            if url.trim().is_empty() || url.chars().count() > MAX_NOTE_LEN {
                return Err(UnifiedError::bad_request(
                    "download_url must be a non-empty URL of at most 2000 characters",
                ));
            }
            request.export_download_url = Some(url.trim().to_string());
        }
        append_audit(
            &mut request,
            &caller.actor,
            "export_proof_attached",
            &format!("Export job {} attached as proof.", input.export_job_id),
        );
        // Verify the job through the import/export module when it
        // answers; otherwise the `export.completed` event completes
        // the proof later.
        match lookup_export(&ctx, &input.export_job_id) {
            Some((status, download_url)) if status == "completed" => {
                mark_export_ready(&mut request, download_url, "export_lookup");
                save_request(&request);
                emit_audit(
                    &ctx,
                    EVENT_EXPORT_READY,
                    &caller.actor,
                    request.tenant_id,
                    json!({
                        "destroy_request_id": request.id.to_string(),
                        "export_id": input.export_job_id.to_string(),
                        "verified_via": "export_lookup",
                    }),
                );
            }
            Some((status, _)) => {
                append_audit(
                    &mut request,
                    &caller.actor,
                    "export_proof_pending",
                    &format!("Export job is in status \"{status}\"; waiting for completion."),
                );
                save_request(&request);
            }
            None => {
                save_request(&request);
            }
        }
        Ok(request)
    })();
    json::respond(result, debug)
}

async fn confirm_destroy(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    id: web::Path<Uuid>,
    body: web::Json<TypedConfirmationBody>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<DestroyRequest> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_TENANTS_DESTROY)?;
        let input = body.into_inner();
        let mut request = get_request(&id.into_inner())?;
        if request.status != STATUS_EXPORT_READY && request.status != STATUS_AWAITING_CONFIRMATION
        {
            return Err(UnifiedError::conflict(format!(
                "Typed confirmation requires status \"{}\" or \"{}\" (current status: \"{}\")",
                STATUS_EXPORT_READY, STATUS_AWAITING_CONFIRMATION, request.status
            )));
        }
        if caller.actor != request.requested_by {
            return Err(UnifiedError::forbidden(
                "Only the admin who requested the destroy may type the confirmation",
            ));
        }
        let settings = destroy_settings();
        if let Some(locked_until) = request.confirmation_locked_until {
            if now() < locked_until {
                return Err(UnifiedError::new(
                    "Confirmation is locked after too many failed attempts; try again later",
                    429,
                )
                .with_metadata("locked_until", Value::String(locked_until.to_rfc3339())));
            }
        }
        if input.typed_slug.is_empty() || input.typed_slug.chars().count() > MAX_SLUG_LEN {
            return Err(UnifiedError::bad_request(
                "typed_slug must be a non-empty slug of at most 100 characters",
            ));
        }
        // Case-sensitive exact match against the stored slug. The
        // error is generic on purpose: it never reveals the slug.
        if input.typed_slug != request.tenant_slug {
            request.confirmation_attempts += 1;
            request.failed_confirmation_attempts += 1;
            let remaining = settings
                .max_failed_confirmations
                .saturating_sub(request.failed_confirmation_attempts);
            if request.failed_confirmation_attempts >= settings.max_failed_confirmations {
                let locked_until =
                    now() + chrono::Duration::minutes(settings.confirmation_lockout_minutes);
                request.confirmation_locked_until = Some(locked_until);
                request.status = STATUS_AWAITING_CONFIRMATION.to_string();
                append_audit(
                    &mut request,
                    &caller.actor,
                    "confirmation_failed",
                    &format!(
                        "Typed confirmation did not match; confirmation locked until {locked_until}."
                    ),
                );
                save_request(&request);
                emit_audit(
                    &ctx,
                    EVENT_CONFIRMATION_FAILED,
                    &caller.actor,
                    request.tenant_id,
                    json!({
                        "destroy_request_id": request.id.to_string(),
                        "failed_attempts": request.failed_confirmation_attempts,
                        "locked_until": locked_until.to_rfc3339(),
                    }),
                );
                return Err(UnifiedError::new(
                    "Confirmation does not match; confirmation is now locked after too many failed attempts",
                    429,
                )
                .with_metadata("locked_until", Value::String(locked_until.to_rfc3339())));
            }
            request.status = STATUS_AWAITING_CONFIRMATION.to_string();
            append_audit(
                &mut request,
                &caller.actor,
                "confirmation_failed",
                "Typed confirmation did not match.",
            );
            save_request(&request);
            emit_audit(
                &ctx,
                EVENT_CONFIRMATION_FAILED,
                &caller.actor,
                request.tenant_id,
                json!({
                    "destroy_request_id": request.id.to_string(),
                    "failed_attempts": request.failed_confirmation_attempts,
                }),
            );
            return Err(UnifiedError::bad_request(
                "Confirmation does not match the tenant slug",
            )
            .with_metadata("attempts_remaining", Value::from(remaining)));
        }
        // Match. Re-check the live slug: the tenant must not have
        // been renamed since the request was opened.
        let tenant = resolve_tenant(&ctx, &request.tenant_id)?;
        if tenant.slug != request.tenant_slug {
            return Err(UnifiedError::internal(
                "Tenant slug changed since the destroy was requested; destroy refuses to proceed",
            ));
        }
        request.confirmation_attempts += 1;
        request.failed_confirmation_attempts = 0;
        request.confirmation_locked_until = None;
        request.confirmed_at = Some(now());
        request.confirmed_by = Some(caller.actor.clone());
        append_audit(
            &mut request,
            &caller.actor,
            "confirmed",
            "Typed confirmation matched the tenant slug.",
        );
        let requirement = second_approval_requirement(&settings, &tenant)?;
        match requirement {
            Some(reason) => {
                request.second_approval_required = true;
                request.second_approval_reason = Some(reason.clone());
                request.status = STATUS_AWAITING_SECOND_APPROVAL.to_string();
                append_audit(
                    &mut request,
                    &caller.actor,
                    "second_approval_required",
                    &format!("Second approval required: {reason}."),
                );
                save_request(&request);
                emit_audit(
                    &ctx,
                    EVENT_SECOND_APPROVAL_REQUIRED,
                    &caller.actor,
                    request.tenant_id,
                    json!({
                        "destroy_request_id": request.id.to_string(),
                        "reason": reason,
                    }),
                );
            }
            None => {
                request.second_approval_required = false;
                save_request(&request);
                enter_suspension_and_grace(&ctx, &mut request, &caller.actor)?;
            }
        }
        Ok(request)
    })();
    json::respond(result, debug)
}

async fn approve_second(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    id: web::Path<Uuid>,
    body: web::Json<SecondApprovalBody>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<DestroyRequest> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_TENANTS_DESTROY)?;
        let note = validate_reason(&body.into_inner().note)?;
        let mut request = get_request(&id.into_inner())?;
        if request.status != STATUS_AWAITING_SECOND_APPROVAL {
            return Err(UnifiedError::conflict(format!(
                "Second approval requires status \"{}\" (current status: \"{}\")",
                STATUS_AWAITING_SECOND_APPROVAL, request.status
            )));
        }
        // The requester's own approval never counts: the second
        // approver must be a different user.
        if caller.actor == request.requested_by {
            return Err(UnifiedError::forbidden(
                "The second approver must be a different admin than the requester",
            ));
        }
        request.second_approver = Some(caller.actor.clone());
        request.second_approved_at = Some(now());
        append_audit(
            &mut request,
            &caller.actor,
            "second_approved",
            &format!(
                "Second approval recorded.{}",
                note.as_deref()
                    .map(|text| format!(" Note: {text}"))
                    .unwrap_or_default()
            ),
        );
        save_request(&request);
        enter_suspension_and_grace(&ctx, &mut request, &caller.actor)?;
        Ok(request)
    })();
    json::respond(result, debug)
}

async fn cancel_destroy(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    id: web::Path<Uuid>,
    body: web::Json<CancelDestroyBody>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<DestroyRequest> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_TENANTS_DESTROY)?;
        let reason = validate_reason(&body.into_inner().reason)?;
        let mut request = get_request(&id.into_inner())?;
        if !request.is_cancellable() {
            return Err(UnifiedError::conflict(format!(
                "Destroy request in status \"{}\" can no longer be cancelled",
                request.status
            )));
        }
        let was_in_grace = request.status == STATUS_SUSPENDED_GRACE;
        request.status = STATUS_CANCELLED.to_string();
        request.cancel_reason = reason.clone();
        request.cancelled_by = Some(caller.actor.clone());
        request.cancelled_at = Some(now());
        append_audit(
            &mut request,
            &caller.actor,
            "cancelled",
            &format!(
                "Destroy cancelled.{}",
                reason
                    .as_deref()
                    .map(|text| format!(" Reason: {text}"))
                    .unwrap_or_default()
            ),
        );
        save_request(&request);
        // Cancelling during grace resumes the tenant: serving
        // restarts and the grace clock is abandoned.
        if was_in_grace {
            ctx.hooks.emit(
                EVENT_TENANT_RESUMED,
                json!({
                    "tenant_id": request.tenant_id.to_string(),
                    "destroy_request_id": request.id.to_string(),
                    "reason": "destroy_cancelled_during_grace",
                    "actor": caller.actor,
                }),
            );
        }
        emit_audit(
            &ctx,
            EVENT_DESTROY_CANCELLED,
            &caller.actor,
            request.tenant_id,
            json!({
                "destroy_request_id": request.id.to_string(),
                "reason": reason,
                "resumed": was_in_grace,
            }),
        );
        Ok(request)
    })();
    json::respond(result, debug)
}

async fn execute_destroy(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    id: web::Path<Uuid>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<DestroyRequest> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_TENANTS_DESTROY)?;
        let request_id = id.into_inner();
        run_final_destroy(&ctx, &request_id, &caller.actor)
    })();
    json::respond(result, debug)
}

async fn get_grace_info(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    id: web::Path<Uuid>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_TENANTS_DESTROY_READ)?;
        let request = get_request(&id.into_inner())?;
        Ok(json!({
            "destroy_request_id": request.id,
            "tenant_id": request.tenant_id,
            "tenant_slug": request.tenant_slug,
            "status": request.status,
            "suspended_at": request.suspended_at,
            "grace_period_days": request.grace_period_days,
            "grace_expires_at": request.grace_expires_at,
            "grace_expired": request.grace_expired(),
            "grace_remaining_seconds": request.grace_remaining_seconds(),
            "can_cancel": request.is_cancellable(),
            "can_execute": request.status == STATUS_SUSPENDED_GRACE && request.grace_expired(),
            "export_job_id": request.export_job_id,
            "export_completed_at": request.export_completed_at,
            "second_approval_required": request.second_approval_required,
            "second_approved_at": request.second_approved_at,
            "external_cleanup": request.external_cleanup,
        }))
    })();
    json::respond(result, debug)
}

async fn confirm_external_cleanup(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    id: web::Path<Uuid>,
    body: web::Json<ConfirmExternalCleanupBody>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<DestroyRequest> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_TENANTS_DESTROY)?;
        let input = body.into_inner();
        let note = validate_reason(&input.note)?;
        if !EXTERNAL_CLEANUP_KINDS.contains(&input.item.as_str()) {
            return Err(UnifiedError::bad_request(format!(
                "item must be one of {}",
                EXTERNAL_CLEANUP_KINDS.join(", ")
            )));
        }
        let request_id = id.into_inner();
        let current = get_request(&request_id)?;
        if current.status != STATUS_DESTROYED_PENDING_EXTERNAL_CLEANUP {
            return Err(UnifiedError::conflict(format!(
                "External cleanup can only be confirmed while the request is \"{}\" (current status: \"{}\")",
                STATUS_DESTROYED_PENDING_EXTERNAL_CLEANUP, current.status
            )));
        }
        let updated = confirm_external_cleanup_item(
            &request_id,
            &input.item,
            &caller.actor,
            note,
        )
        .ok_or_else(|| UnifiedError::not_found("Destroy request not found"))?;
        if updated.status == STATUS_DESTROYED {
            emit_audit(
                &ctx,
                EVENT_EXTERNAL_CLEANUP_COMPLETED,
                &caller.actor,
                updated.tenant_id,
                json!({
                    "destroy_request_id": updated.id.to_string(),
                    "item": input.item,
                }),
            );
        }
        Ok(updated)
    })();
    json::respond(result, debug)
}

async fn process_due(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_TENANTS_DESTROY)?;
        let destroyed = process_due_destroys(&ctx)?;
        Ok(json!({
            "destroyed_request_ids": destroyed,
            "destroyed_count": destroyed.len(),
            "due_remaining": due_destroy_requests().len(),
        }))
    })();
    json::respond(result, debug)
}

async fn get_settings(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<TenantDestroySettings> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_TENANTS_DESTROY_READ)?;
        Ok(destroy_settings())
    })();
    json::respond(result, debug)
}

async fn update_settings(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    body: web::Json<UpdateSettingsBody>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<TenantDestroySettings> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_TENANTS_DESTROY)?;
        let input = body.into_inner();
        let mut settings = destroy_settings();
        if let Some(days) = input.grace_period_days {
            if !(0..=3650).contains(&days) {
                return Err(UnifiedError::bad_request(
                    "grace_period_days must be between 0 and 3650",
                ));
            }
            settings.grace_period_days = days;
        }
        if let Some(age) = input.second_approval_min_tenant_age_days {
            if let Some(days) = age {
                if days < 0 {
                    return Err(UnifiedError::bad_request(
                        "second_approval_min_tenant_age_days must be zero or more",
                    ));
                }
            }
            settings.second_approval_min_tenant_age_days = age;
        }
        if let Some(size) = input.second_approval_min_data_size_bytes {
            settings.second_approval_min_data_size_bytes = size;
        }
        if let Some(max) = input.max_failed_confirmations {
            if !(1..=100).contains(&max) {
                return Err(UnifiedError::bad_request(
                    "max_failed_confirmations must be between 1 and 100",
                ));
            }
            settings.max_failed_confirmations = max;
        }
        if let Some(minutes) = input.confirmation_lockout_minutes {
            if !(0..=1440).contains(&minutes) {
                return Err(UnifiedError::bad_request(
                    "confirmation_lockout_minutes must be between 0 and 1440",
                ));
            }
            settings.confirmation_lockout_minutes = minutes;
        }
        if let Some(entity) = input.export_entity {
            let trimmed = entity.trim().to_string();
            if trimmed.is_empty() || trimmed.chars().count() > MAX_SLUG_LEN {
                return Err(UnifiedError::bad_request(
                    "export_entity must be a non-empty name of at most 100 characters",
                ));
            }
            settings.export_entity = trimmed;
        }
        if let Some(days) = input.export_pending_stale_days {
            if !(0..=365).contains(&days) {
                return Err(UnifiedError::bad_request(
                    "export_pending_stale_days must be between 0 and 365",
                ));
            }
            settings.export_pending_stale_days = days;
        }
        if let Ok(mut guard) = settings_store().write() {
            *guard = settings.clone();
        }
        ctx.hooks.emit(
            "tenant.destroy.settings.updated",
            json!({
                "actor": caller.actor,
                "grace_period_days": settings.grace_period_days,
            }),
        );
        Ok(settings)
    })();
    json::respond(result, debug)
}

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
        // skipped, never an error.
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
                let download_url = context
                    .get("download_url")
                    .and_then(Value::as_str)
                    .map(str::to_string);
                for mut entry in destroy_requests_store().iter_mut() {
                    let request = entry.value_mut();
                    if request.export_job_id == Some(export_id)
                        && request.status == STATUS_EXPORT_PENDING
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
                let destroyed = is_destroyed_tenant(&tenant_id);
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
        // must call `process_due_destroys(ctx)`.
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
            RouteInfo { method: "POST", path: "/api/v1/tenants/destroy-requests", owner: "tenant_destroy", description: "Request a tenant destroy (mandatory export first)" },
            RouteInfo { method: "GET", path: "/api/v1/tenants/destroy-requests", owner: "tenant_destroy", description: "List destroy requests (paginated, filter by status, tenant_id, requested_by)" },
            RouteInfo { method: "GET", path: "/api/v1/tenants/destroy-requests/{id}", owner: "tenant_destroy", description: "One destroy request with full status and audit trail" },
            RouteInfo { method: "POST", path: "/api/v1/tenants/destroy-requests/{id}/export-proof", owner: "tenant_destroy", description: "Attach an export job id as destroy proof" },
            RouteInfo { method: "POST", path: "/api/v1/tenants/destroy-requests/{id}/confirm", owner: "tenant_destroy", description: "Typed slug confirmation (requester only)" },
            RouteInfo { method: "POST", path: "/api/v1/tenants/destroy-requests/{id}/approve-second", owner: "tenant_destroy", description: "Second approver (must differ from the requester)" },
            RouteInfo { method: "POST", path: "/api/v1/tenants/destroy-requests/{id}/cancel", owner: "tenant_destroy", description: "Cancel a destroy request (resumes the tenant during grace)" },
            RouteInfo { method: "POST", path: "/api/v1/tenants/destroy-requests/{id}/execute", owner: "tenant_destroy", description: "Final destroy after grace expiry (verified schema drop)" },
            RouteInfo { method: "GET", path: "/api/v1/tenants/destroy-requests/{id}/grace", owner: "tenant_destroy", description: "Grace period and expiry info for a destroy request" },
            RouteInfo { method: "POST", path: "/api/v1/tenants/destroy-requests/{id}/external-cleanup/confirm", owner: "tenant_destroy", description: "Confirm one external cleanup item" },
            RouteInfo { method: "POST", path: "/api/v1/tenants/destroy-requests/process-due", owner: "tenant_destroy", description: "Run the due-destroy sweep for grace-expired requests" },
            RouteInfo { method: "GET", path: "/api/v1/tenants/destroy-settings", owner: "tenant_destroy", description: "Destroy flow settings (grace period, thresholds, lockout)" },
            RouteInfo { method: "PUT", path: "/api/v1/tenants/destroy-settings", owner: "tenant_destroy", description: "Update destroy flow settings" },
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
