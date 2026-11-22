// Import / Export module — CSV, JSON, and XML data exchange.
//
// Routes under `/api/v1/import-export`:
//
//   GET    /entities                  entity catalog with columns and types
//   GET    /settings                  effective limits and defaults
//   PATCH  /settings                  update limits and defaults (manage)
//   GET    /exports                   list export jobs (paginated, filterable)
//   POST   /exports                   start an export job (JSON, CSV, or XML)
//   GET    /exports/{id}              export job status
//   GET    /exports/{id}/download     download the finished export file
//   GET    /imports                   list import jobs (paginated, filterable)
//   POST   /imports                   submit a file for validation (dry run)
//   GET    /imports/{id}              import job with per-row results
//   GET    /imports/{id}/errors/download   row-error report as CSV
//   POST   /imports/{id}/confirm      confirm a validated import (stages rows)
//   GET    /mappings                  list saved field-mapping templates
//   POST   /mappings                  save a field-mapping template
//   GET    /mappings/{id}             one mapping template
//   DELETE /mappings/{id}             delete a mapping template
//   GET    /export-schedules          list scheduled exports
//   POST   /export-schedules          create a scheduled export
//   GET    /export-schedules/{id}     one scheduled export
//   PATCH  /export-schedules/{id}     update a scheduled export
//   DELETE /export-schedules/{id}     delete a scheduled export
//   POST   /export-schedules/{id}/run-now   run a scheduled export now
//   GET    /import-sessions           list chunked import sessions
//   POST   /import-sessions           open a chunked import session
//   GET    /import-sessions/{id}      one chunked import session
//   POST   /import-sessions/{id}/chunks    append one validated chunk
//   POST   /import-sessions/{id}/complete  finalize a session into an import job
//
// Import flow: submit -> dry-run validation (row errors reported,
// nothing written) -> confirm -> rows are staged on the job and an
// `import.completed` hook is emitted carrying the staged rows, so the
// owning module can apply them. The owning modules expose no public
// write/import function today (verified: `users`, `software`, `store`,
// `billing`, and `licensing` only expose their `Module` struct and
// `register`), so this module deliberately does NOT invent a write
// path into their tables. Chunked imports follow the same rule: every
// chunk is validated before it is accepted, and nothing is written
// until the session is completed and the resulting job is confirmed.
//
// Export flow: an export job emits `export.requested` on the hook bus
// and collects rows contributed by owning modules through hook
// results (`{"rows": [...]}` or a bare array). Field selection trims
// the columns. When no module contributes rows the job still
// completes, with zero rows and `data_provider: "none"`, so the gap
// is visible instead of silent.
//
// Full-tenant export: `export.requested` with `scope:
// "full_tenant_export"` is answered here by turning the request
// into a job through `run_export` (entity `tenant` by default,
// scoped to the payload's `tenant_id`, unredacted unless the
// payload sets `redact_pii: true` — the destroy export must stay
// restorable). The answer carries `export_id` / `export_job_id` /
// `job_id` plus the job view; a job with no rows is answered as an
// error, never as a usable export. `export.lookup` answers the same
// job view for a job id. OPEN: no module answers the data-provider
// query for entity `tenant` yet, so a full-tenant export produces
// zero rows and is refused as an error until the tenants module
// adds the provider.
//
// Security: every route requires `X-User-Id` / `X-Role` headers and
// a capability checked against the roles module through the
// `role.check` hook (fail closed: no answer means not allowed).
// Capabilities: `import_export:read` (catalog, lists, single reads,
// error reports), `import_export:import` (submit, confirm, chunked
// sessions), `import_export:export` (create and download exports),
// `import_export:export_pii` (export personal data unredacted),
// `import_export:manage` (mappings, schedules, settings). The
// `admin` role holds `*` and therefore holds all of these; any other
// role needs an explicit grant through the roles API.
//
// Tenant scoping: the caller's company comes from the `X-Company-Id`
// header with a fallback to the `user.company_requested` hook (same
// shape as the AI module). Every job, mapping, schedule, and session
// records the actor and the company it belongs to. List endpoints
// only show the caller's own company unless the caller holds the
// manage capability, and a single-object read of another company's
// record answers 404, never 403. Export rows contributed by a hook
// that carry a `company_id` different from the caller's company are
// dropped and counted as `rows_excluded_by_tenant_scope`; the hook
// payload also carries the caller company so providers can scope
// server-side. Import rows whose `company_id` column disagrees with
// the caller's company fail validation for that row.
//
// PII redaction: exports redact personal data through the privacy
// module's `redact_value` helper (reused, never duplicated here).
// Which columns count as personal data is declared per entity in the
// catalog below. An export is unredacted only when the caller holds
// `import_export:export_pii` and explicitly asks for it; otherwise
// redaction is forced and recorded on the job (`pii_redacted`,
// `redacted_fields`), and an `export.pii_redacted` event is emitted.
// Scheduled exports capture the creator's PII capability at creation
// time (`pii_allowed`); a later run never widens it.
//
// Audit: every import/export/mapping/schedule/session lifecycle event
// is emitted with `actor`, `actor_id`, `resource`, and row counts, in
// the shape the audit log records. The audit module only records
// events listed in its `AUDITED_EVENTS` table, so the integrator must
// add the `import.*` / `export.*` event names there (see the
// integration notes file for the exact snippet).
//
// Scheduled exports: a schedule is stored here with its cron-style
// schedule string (five fields, or an interval in seconds — the same
// rule the scheduler module validates with). Creating or updating an
// enabled schedule also offers it to the scheduler through
// `scheduler.enqueue` with kind `export.run` and records the outcome
// on the schedule; the scheduler's known-kind list does not include
// `export.run` yet, so that offer is refused today and `run-now` plus
// the `export.run_requested` hook handler are the working paths
// (see the integration notes file for the scheduler snippet).
//
// Limits are configurable defaults, not hard-coded behaviour: see
// `ImportExportSettings` and `GET / PATCH /api/v1/import-export/settings`.
// The in-memory stores are authoritative, matching the rest of this
// module; the migration `000079` mirrors the new tables and the new
// job columns for persistence later.

use crate::core::context::AppContext;
use crate::core::error::{UnifiedError, UnifiedResult};
use crate::core::hooks::{HookBus, HookContext};
use crate::core::json;
use crate::core::modules::{Module, ModuleRegistry};
use crate::core::pagination::{PageResult, PaginationParams};
use crate::modules::privacy::redact_value;
use actix_web::{web, HttpRequest, HttpResponse};
use base64::Engine;
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::sync::{Arc, OnceLock, RwLock};
use uuid::Uuid;

// ---------------------------------------------------------------------------
// Configurable defaults.
// ---------------------------------------------------------------------------

/// Maximum accepted import payload in bytes (10 MiB).
const DEFAULT_MAX_FILE_BYTES: usize = 10 * 1024 * 1024;
/// Maximum rows per single-shot import file.
const DEFAULT_MAX_ROWS_PER_FILE: usize = 10_000;
/// Maximum bytes per chunked-import chunk (2 MiB).
const DEFAULT_MAX_CHUNK_BYTES: usize = 2 * 1024 * 1024;
/// Maximum rows across all chunks of one import session.
const DEFAULT_MAX_ROWS_PER_SESSION: usize = 50_000;
/// Maximum rows per export job (also the streaming cap).
const DEFAULT_MAX_EXPORT_ROWS: usize = 10_000;
/// Maximum export content kept in memory per job (50 MiB).
const DEFAULT_MAX_EXPORT_BYTES: usize = 50 * 1024 * 1024;
/// Default formats. Admin-editable through the settings endpoint.
const DEFAULT_ALLOWED_FORMATS: &[&str] = &["csv", "json", "xml"];

/// Effective limits and defaults for this module. These are the
/// configurable defaults: an admin may change them through
/// `PATCH /api/v1/import-export/settings`; nothing else in this file
/// hard-codes a limit.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImportExportSettings {
    pub max_file_bytes: usize,
    pub max_rows_per_file: usize,
    pub max_chunk_bytes: usize,
    pub max_rows_per_session: usize,
    pub max_export_rows: usize,
    pub max_export_bytes: usize,
    /// Redact personal data on exports unless the caller explicitly
    /// opts out (which still requires `import_export:export_pii`).
    pub default_redact_pii: bool,
    pub allowed_formats: Vec<String>,
}

impl Default for ImportExportSettings {
    fn default() -> Self {
        Self {
            max_file_bytes: DEFAULT_MAX_FILE_BYTES,
            max_rows_per_file: DEFAULT_MAX_ROWS_PER_FILE,
            max_chunk_bytes: DEFAULT_MAX_CHUNK_BYTES,
            max_rows_per_session: DEFAULT_MAX_ROWS_PER_SESSION,
            max_export_rows: DEFAULT_MAX_EXPORT_ROWS,
            max_export_bytes: DEFAULT_MAX_EXPORT_BYTES,
            default_redact_pii: false,
            allowed_formats: DEFAULT_ALLOWED_FORMATS
                .iter()
                .map(|format| format.to_string())
                .collect(),
        }
    }
}

static SETTINGS: OnceLock<RwLock<ImportExportSettings>> = OnceLock::new();

fn settings_store() -> &'static RwLock<ImportExportSettings> {
    SETTINGS.get_or_init(|| RwLock::new(ImportExportSettings::default()))
}

fn settings() -> ImportExportSettings {
    settings_store()
        .read()
        .map(|guard| guard.clone())
        .unwrap_or_default()
}

/// Well-known events emitted by this module.
pub mod import_export_events {
    pub const IMPORT_CREATED: &str = "import.created";
    pub const IMPORT_COMPLETED: &str = "import.completed";
    pub const IMPORT_CONFIRMED: &str = "import.confirmed";
    pub const IMPORT_FAILED: &str = "import.failed";
    pub const IMPORT_SESSION_COMPLETED: &str = "import.session.completed";
    pub const EXPORT_CREATED: &str = "export.created";
    pub const EXPORT_COMPLETED: &str = "export.completed";
    pub const EXPORT_REQUESTED: &str = "export.requested";
    pub const EXPORT_LOOKUP: &str = "export.lookup";
    pub const EXPORT_PII_REDACTED: &str = "export.pii_redacted";
    pub const EXPORT_RUN_REQUESTED: &str = "export.run_requested";
    pub const EXPORT_SCHEDULE_CREATED: &str = "export.schedule.created";
    pub const EXPORT_SCHEDULE_UPDATED: &str = "export.schedule.updated";
    pub const EXPORT_SCHEDULE_DELETED: &str = "export.schedule.deleted";
    pub const EXPORT_SCHEDULE_RUN: &str = "export.schedule.run";
    pub const MAPPING_CREATED: &str = "mapping.created";
    pub const MAPPING_DELETED: &str = "mapping.deleted";
    pub const SETTINGS_UPDATED: &str = "import_export.settings.updated";
}

/// Scheduler integration: the kind offered to `scheduler.enqueue`
/// and the event the scheduler emits when it dispatches the run.
pub const EXPORT_RUN_KIND: &str = "export.run";
pub const SCHEDULER_ENQUEUE: &str = "scheduler.enqueue";

/// Capabilities enforced by this module (checked through the
/// `role.check` hook; the `admin` role holds `*` and passes all).
pub const PERM_IMPORT_EXPORT_READ: &str = "import_export:read";
pub const PERM_IMPORT_EXPORT_IMPORT: &str = "import_export:import";
pub const PERM_IMPORT_EXPORT_EXPORT: &str = "import_export:export";
pub const PERM_IMPORT_EXPORT_EXPORT_PII: &str = "import_export:export_pii";
pub const PERM_IMPORT_EXPORT_MANAGE: &str = "import_export:manage";

/// Column-name fragments that are never allowed in an import file.
/// Matching is case-insensitive on the normalized (lowercase,
/// non-alphanumeric-stripped) column name.
const FORBIDDEN_COLUMN_FRAGMENTS: &[&str] = &[
    "password",
    "passwd",
    "secret",
    "token",
    "apikey",
    "api_key",
    "privatekey",
    "private_key",
    "credential",
    "hash",
    "salt",
    "totp",
    "otpsecret",
];

/// Root element names accepted for an XML import document.
const XML_ROOT_ELEMENTS: &[&str] = &["rows", "export", "items", "dataset", "records"];
/// Child element names accepted as one row inside an XML document.
const XML_ROW_ELEMENTS: &[&str] = &["row", "item", "record"];

// ---------------------------------------------------------------------------
// Caller identity, capabilities, and tenant scope.
// ---------------------------------------------------------------------------

/// The caller, from the headers every module in this codebase uses:
/// `X-User-Id` identifies the actor, `X-Role` carries the role slug
/// or id the capability check runs against.
struct Caller {
    actor: String,
    role: String,
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
/// (Same fail-closed shape as the AI module's capability check.)
fn check_capability(ctx: &AppContext, role: &str, permission: &str) -> bool {
    let context = ctx.hooks.emit(
        "role.check",
        serde_json::json!({ "role": role, "permission": permission, "allowed": false }),
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

/// The caller's company id: `X-Company-Id` header first, then the
/// `user.company_requested` hook. `None` when neither resolves;
/// callers without a company are platform admins.
fn resolve_caller_company(ctx: &AppContext, req: &HttpRequest, caller: &Caller) -> Option<Uuid> {
    if let Some(raw) = req
        .headers()
        .get("x-company-id")
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        if let Ok(id) = Uuid::parse_str(raw) {
            return Some(id);
        }
    }
    let hook = ctx
        .hooks
        .emit("user.company_requested", serde_json::json!({ "user_id": caller.actor }));
    hook.get("company_id")
        .and_then(|value| value.as_str())
        .and_then(|raw| Uuid::parse_str(raw).ok())
}

/// True when a caller in `caller_company` may see a record owned by
/// `record_company`. A manage-capable caller sees everything; a
/// platform caller (no company) sees everything; a company caller
/// only sees records of their own company, never platform-wide
/// records owned by nobody.
fn can_view_company(
    caller_company: Option<Uuid>,
    record_company: Option<Uuid>,
    can_manage: bool,
) -> bool {
    if can_manage {
        return true;
    }
    match (caller_company, record_company) {
        (Some(caller), Some(record)) => caller == record,
        (Some(_), None) => false,
        (None, _) => true,
    }
}

/// Keep only the records a caller may see. Used by every list
/// endpoint so tenant scoping cannot be forgotten per handler.
fn scope_visible<T: Clone>(
    items: Vec<T>,
    company_of: impl Fn(&T) -> Option<Uuid>,
    caller_company: Option<Uuid>,
    can_manage: bool,
) -> Vec<T> {
    items
        .into_iter()
        .filter(|item| can_view_company(caller_company, company_of(item), can_manage))
        .collect()
}

// ---------------------------------------------------------------------------
// Entity catalog.
// ---------------------------------------------------------------------------

/// One column of one importable/exportable entity.
#[derive(Debug, Clone, Serialize)]
pub struct EntityColumn {
    pub name: &'static str,
    /// `string`, `integer`, `number`, `boolean`, `date`, `email`, `uuid`.
    #[serde(rename = "type")]
    pub kind: &'static str,
    pub required: bool,
    /// Short description of the column.
    pub description: &'static str,
}

/// One importable/exportable entity.
#[derive(Debug, Clone, Serialize)]
pub struct EntitySpec {
    pub entity: &'static str,
    pub description: &'static str,
    /// Column used for idempotency when present in the file.
    pub external_id_column: Option<&'static str>,
    /// Columns that hold personal data and are redacted on export
    /// unless the caller holds `import_export:export_pii`.
    pub pii_columns: &'static [&'static str],
    pub columns: &'static [EntityColumn],
}

const USERS_COLUMNS: &[EntityColumn] = &[
    EntityColumn { name: "external_id", kind: "string", required: false, description: "Caller-supplied stable id for idempotency" },
    EntityColumn { name: "email", kind: "email", required: true, description: "User email address" },
    EntityColumn { name: "name", kind: "string", required: true, description: "Display name" },
    EntityColumn { name: "is_active", kind: "boolean", required: false, description: "Whether the account is active" },
    EntityColumn { name: "company_id", kind: "uuid", required: false, description: "Owning company id" },
];

const SOFTWARE_COLUMNS: &[EntityColumn] = &[
    EntityColumn { name: "external_id", kind: "string", required: false, description: "Caller-supplied stable id for idempotency" },
    EntityColumn { name: "name", kind: "string", required: true, description: "Software name" },
    EntityColumn { name: "slug", kind: "string", required: true, description: "URL slug, unique" },
    EntityColumn { name: "version", kind: "string", required: false, description: "Current version, semver" },
    EntityColumn { name: "description", kind: "string", required: false, description: "Short description" },
    EntityColumn { name: "status", kind: "string", required: false, description: "draft, published, deprecated, or archived" },
];

const PRODUCTS_COLUMNS: &[EntityColumn] = &[
    EntityColumn { name: "external_id", kind: "string", required: false, description: "Caller-supplied stable id for idempotency" },
    EntityColumn { name: "name", kind: "string", required: true, description: "Product name" },
    EntityColumn { name: "slug", kind: "string", required: true, description: "URL slug, unique" },
    EntityColumn { name: "price", kind: "number", required: true, description: "Price in the store currency" },
    EntityColumn { name: "currency", kind: "string", required: false, description: "ISO currency code, for example USD" },
    EntityColumn { name: "is_active", kind: "boolean", required: false, description: "Whether the product is on sale" },
];

const ORDERS_COLUMNS: &[EntityColumn] = &[
    EntityColumn { name: "external_id", kind: "string", required: false, description: "Caller-supplied stable id for idempotency" },
    EntityColumn { name: "customer_email", kind: "email", required: true, description: "Buyer email address" },
    EntityColumn { name: "product_slug", kind: "string", required: true, description: "Ordered product slug" },
    EntityColumn { name: "quantity", kind: "integer", required: false, description: "Quantity, default 1" },
    EntityColumn { name: "total", kind: "number", required: true, description: "Order total in the store currency" },
    EntityColumn { name: "currency", kind: "string", required: false, description: "ISO currency code, for example USD" },
    EntityColumn { name: "status", kind: "string", required: false, description: "pending, paid, refunded, or cancelled" },
];

const INVOICES_COLUMNS: &[EntityColumn] = &[
    EntityColumn { name: "external_id", kind: "string", required: false, description: "Caller-supplied stable id for idempotency" },
    EntityColumn { name: "order_external_id", kind: "string", required: true, description: "External id of the order this invoice belongs to" },
    EntityColumn { name: "number", kind: "string", required: true, description: "Invoice number, unique" },
    EntityColumn { name: "total", kind: "number", required: true, description: "Invoice total in the store currency" },
    EntityColumn { name: "currency", kind: "string", required: false, description: "ISO currency code, for example USD" },
    EntityColumn { name: "status", kind: "string", required: false, description: "draft, issued, paid, void, or overdue" },
    EntityColumn { name: "due_date", kind: "date", required: false, description: "Due date (RFC 3339 or YYYY-MM-DD)" },
];

const TASKS_COLUMNS: &[EntityColumn] = &[
    EntityColumn { name: "external_id", kind: "string", required: false, description: "Caller-supplied stable id for idempotency" },
    EntityColumn { name: "title", kind: "string", required: true, description: "Task title" },
    EntityColumn { name: "description", kind: "string", required: false, description: "Task description" },
    EntityColumn { name: "status", kind: "string", required: false, description: "open, in_progress, done, or cancelled" },
    EntityColumn { name: "priority", kind: "string", required: false, description: "low, normal, high, or urgent" },
    EntityColumn { name: "assignee_email", kind: "email", required: false, description: "Assignee email address" },
    EntityColumn { name: "due_date", kind: "date", required: false, description: "Due date (RFC 3339 or YYYY-MM-DD)" },
];

const LICENSES_COLUMNS: &[EntityColumn] = &[
    EntityColumn { name: "external_id", kind: "string", required: false, description: "Caller-supplied stable id for idempotency" },
    EntityColumn { name: "software_slug", kind: "string", required: true, description: "Licensed software slug" },
    EntityColumn { name: "owner_email", kind: "email", required: true, description: "License owner email address" },
    EntityColumn { name: "seats", kind: "integer", required: false, description: "Seat count, default 1" },
    EntityColumn { name: "expires_at", kind: "date", required: false, description: "Expiry (RFC 3339 or YYYY-MM-DD)" },
    EntityColumn { name: "status", kind: "string", required: false, description: "active, suspended, or revoked" },
];

const TENANT_COLUMNS: &[EntityColumn] = &[
    EntityColumn { name: "id", kind: "uuid", required: true, description: "Tenant uuidv7" },
    EntityColumn { name: "slug", kind: "string", required: true, description: "Tenant slug" },
    EntityColumn { name: "display_name", kind: "string", required: false, description: "Tenant display name" },
    EntityColumn { name: "status", kind: "string", required: false, description: "provisioning, provision_failed, active, suspended, deprovisioning, or deleted" },
    EntityColumn { name: "owner_user_id", kind: "uuid", required: false, description: "Owning user id" },
    EntityColumn { name: "package_id", kind: "uuid", required: false, description: "Package the tenant was provisioned under" },
    EntityColumn { name: "preset_id", kind: "uuid", required: false, description: "Preset the tenant was provisioned under" },
    EntityColumn { name: "plan_id", kind: "uuid", required: false, description: "Billing plan id" },
    EntityColumn { name: "subdomain", kind: "string", required: false, description: "Tenant subdomain label" },
    EntityColumn { name: "subdomain_pending", kind: "boolean", required: false, description: "Whether the subdomain claim is pending approval" },
    EntityColumn { name: "schema_name", kind: "string", required: false, description: "Tenant database schema name" },
    EntityColumn { name: "suspended_at", kind: "date", required: false, description: "Suspension time (RFC 3339 or YYYY-MM-DD)" },
    EntityColumn { name: "suspended_reason", kind: "string", required: false, description: "Why the tenant was suspended" },
    EntityColumn { name: "destroyed_at", kind: "date", required: false, description: "Destroy time (RFC 3339 or YYYY-MM-DD)" },
    EntityColumn { name: "export_reference", kind: "string", required: false, description: "Export job id recorded before destroy" },
    EntityColumn { name: "created_at", kind: "date", required: false, description: "Creation time (RFC 3339 or YYYY-MM-DD)" },
    EntityColumn { name: "updated_at", kind: "date", required: false, description: "Last update time (RFC 3339 or YYYY-MM-DD)" },
];

const ENTITY_SPECS: &[EntitySpec] = &[
    EntitySpec { entity: "users", description: "User accounts", external_id_column: Some("external_id"), pii_columns: &["email", "name"], columns: USERS_COLUMNS },
    EntitySpec { entity: "software", description: "Software products and releases", external_id_column: Some("external_id"), pii_columns: &[], columns: SOFTWARE_COLUMNS },
    EntitySpec { entity: "products", description: "Store products", external_id_column: Some("external_id"), pii_columns: &[], columns: PRODUCTS_COLUMNS },
    EntitySpec { entity: "orders", description: "Store orders", external_id_column: Some("external_id"), pii_columns: &["customer_email"], columns: ORDERS_COLUMNS },
    EntitySpec { entity: "invoices", description: "Billing invoices", external_id_column: Some("external_id"), pii_columns: &[], columns: INVOICES_COLUMNS },
    EntitySpec { entity: "tasks", description: "Tasks", external_id_column: Some("external_id"), pii_columns: &["assignee_email"], columns: TASKS_COLUMNS },
    EntitySpec { entity: "licenses", description: "Software licenses", external_id_column: Some("external_id"), pii_columns: &["owner_email"], columns: LICENSES_COLUMNS },
    EntitySpec { entity: "tenant", description: "Tenant control-plane record (destroy export)", external_id_column: None, pii_columns: &[], columns: TENANT_COLUMNS },
];

fn entity_spec(name: &str) -> Option<&'static EntitySpec> {
    ENTITY_SPECS.iter().find(|spec| spec.entity == name)
}

// ---------------------------------------------------------------------------
// State.
// ---------------------------------------------------------------------------

static HOOK_BUS: OnceLock<Arc<HookBus>> = OnceLock::new();
static EXPORT_JOBS: OnceLock<DashMap<Uuid, ExportJob>> = OnceLock::new();
static IMPORT_JOBS: OnceLock<DashMap<Uuid, ImportJob>> = OnceLock::new();
static MAPPINGS: OnceLock<DashMap<Uuid, MappingTemplate>> = OnceLock::new();
static EXPORT_SCHEDULES: OnceLock<DashMap<Uuid, ExportSchedule>> = OnceLock::new();
static IMPORT_SESSIONS: OnceLock<DashMap<Uuid, ImportSession>> = OnceLock::new();

fn export_jobs_store() -> &'static DashMap<Uuid, ExportJob> {
    EXPORT_JOBS.get_or_init(DashMap::new)
}

fn import_jobs_store() -> &'static DashMap<Uuid, ImportJob> {
    IMPORT_JOBS.get_or_init(DashMap::new)
}

fn mappings_store() -> &'static DashMap<Uuid, MappingTemplate> {
    MAPPINGS.get_or_init(DashMap::new)
}

fn export_schedules_store() -> &'static DashMap<Uuid, ExportSchedule> {
    EXPORT_SCHEDULES.get_or_init(DashMap::new)
}

fn import_sessions_store() -> &'static DashMap<Uuid, ImportSession> {
    IMPORT_SESSIONS.get_or_init(DashMap::new)
}

fn emit(event: &str, payload: Value) -> Option<HookContext> {
    HOOK_BUS.get().map(|bus| bus.emit(event, payload))
}

/// Emit a lifecycle event in the shape the audit log records:
/// `actor` / `actor_id`, `resource`, and the row counts. The audit
/// module only records events listed in its `AUDITED_EVENTS` table;
/// the integrator snippet for that lives in the integration notes.
fn emit_audit(event: &str, actor: &str, company_id: Option<Uuid>, resource: &str, extra: Value) {
    let mut payload = serde_json::json!({
        "actor": actor,
        "actor_id": actor,
        "resource": resource,
    });
    if let Some(company) = company_id {
        payload["company_id"] = Value::String(company.to_string());
    }
    if let (Some(target), Some(source)) = (payload.as_object_mut(), extra.as_object()) {
        for (key, value) in source {
            target.insert(key.clone(), value.clone());
        }
    }
    emit(event, payload);
}

/// The public shape of one export job as hook answers carry it:
/// id, entity, status, download URL, row count, format, and
/// redaction flag. Used by the `export.lookup` answerer and by the
/// full-tenant export answer so both speak the same shape the
/// tenant-destroy export gate reads (`export.status`,
/// `export.download_url`).
fn export_job_view(job: &ExportJob) -> Value {
    serde_json::json!({
        "id": job.id,
        "export_id": job.id,
        "entity": job.entity,
        "status": job.status,
        "download_url": job.download_url,
        "row_count": job.row_count,
        "format": job.format,
        "pii_redacted": job.pii_redacted,
        "data_provider": job.data_provider,
        "created_at": job.created_at,
    })
}

// ---------------------------------------------------------------------------
// Shared types.
// ---------------------------------------------------------------------------

/// One row-level validation problem.
#[derive(Debug, Clone, Serialize)]
pub struct RowError {
    /// 1-based row number in the file (header excluded for CSV).
    pub row: usize,
    pub column: Option<String>,
    pub message: String,
}

/// Outcome of one row after confirmation.
#[derive(Debug, Clone, Serialize)]
pub struct RowResult {
    pub row: usize,
    pub external_id: Option<String>,
    /// `staged`, `skipped` (duplicate external id), or `error`.
    pub status: String,
    pub message: Option<String>,
}

/// Lifecycle of an import job.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImportStatus {
    Validated,
    Failed,
    Staged,
}

impl ImportStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Validated => "validated",
            Self::Failed => "failed",
            Self::Staged => "staged",
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct ImportJob {
    pub id: Uuid,
    pub entity: String,
    pub format: String,
    pub status: String,
    pub total_rows: usize,
    pub valid_rows: usize,
    pub invalid_rows: usize,
    pub row_errors: Vec<RowError>,
    pub results: Vec<RowResult>,
    /// Actor who submitted the import (`X-User-Id` header).
    pub actor: String,
    /// Owning company, when the caller belongs to one.
    pub company_id: Option<Uuid>,
    /// Validated rows, kept so confirmation can stage them. Only the
    /// validated column values are kept, never raw file bytes.
    #[serde(skip_serializing)]
    pub staged_rows: Vec<Value>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Lifecycle of an export job.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExportStatus {
    Completed,
    Failed,
}

impl ExportStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Completed => "completed",
            Self::Failed => "failed",
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct ExportJob {
    pub id: Uuid,
    pub entity: String,
    pub format: String,
    pub status: String,
    pub fields: Vec<String>,
    pub row_count: usize,
    /// `hook` when rows came from an owning module, `none` when no
    /// module contributed rows for this entity.
    pub data_provider: String,
    pub download_url: String,
    pub error: Option<String>,
    /// True when personal data in the rows was redacted (forced when
    /// the caller lacks `import_export:export_pii`).
    pub pii_redacted: bool,
    /// Personal-data columns that were redacted, if any.
    pub redacted_fields: Vec<String>,
    /// Requested row window (chunked export): rows start at `offset`
    /// and at most `limit` rows are returned.
    pub limit: usize,
    pub offset: usize,
    /// True when the provider returned exactly `limit` rows, which
    /// suggests more rows exist past this window. This is a heuristic:
    /// providers that support `offset`/`limit` answer precisely, and
    /// providers that ignore the window return their full set.
    pub has_more: bool,
    /// Rows dropped because their `company_id` did not match the
    /// caller's company.
    pub rows_excluded_by_tenant_scope: usize,
    /// Actor who requested the export (`X-User-Id` header).
    pub actor: String,
    /// Owning company, when the caller belongs to one.
    pub company_id: Option<Uuid>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    /// Finished export bytes, kept in memory for download.
    #[serde(skip_serializing)]
    pub content: Vec<u8>,
}

/// One saved field-mapping template: file column name -> entity
/// column name, reusable across imports of the same entity.
#[derive(Debug, Clone, Serialize)]
pub struct MappingTemplate {
    pub id: Uuid,
    pub name: String,
    pub entity: String,
    pub mapping: HashMap<String, String>,
    pub actor: String,
    pub company_id: Option<Uuid>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Outcome of offering a schedule to the scheduler module.
#[derive(Debug, Clone, Serialize)]
pub struct SchedulingOutcome {
    pub enqueued: bool,
    pub message: String,
}

/// One scheduled export: a stored export definition plus its cron
/// schedule. Runs are produced by `run-now`, by the
/// `export.run_requested` hook handler, or — once the scheduler
/// gains an `export.run` dispatch arm — by the scheduler.
#[derive(Debug, Clone, Serialize)]
pub struct ExportSchedule {
    pub id: Uuid,
    pub name: String,
    pub entity: String,
    pub format: String,
    pub fields: Vec<String>,
    pub filters: HashMap<String, String>,
    pub redact_pii: bool,
    pub redact_names: bool,
    /// True when the creator held `import_export:export_pii` at
    /// creation time. A scheduled run never exports personal data
    /// unredacted unless this is true; the flag is never widened by
    /// a run, only by a manage-capable update from a caller who holds
    /// the PII capability at that moment.
    pub pii_allowed: bool,
    /// Five cron-like fields, or an interval in seconds (same rule
    /// the scheduler module validates with).
    pub schedule: String,
    pub enabled: bool,
    pub company_id: Option<Uuid>,
    pub actor: String,
    pub last_run_at: Option<DateTime<Utc>>,
    pub last_export_id: Option<Uuid>,
    pub last_error: Option<String>,
    pub run_count: u64,
    pub scheduling: SchedulingOutcome,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// One chunked import session. Each chunk is a self-contained
/// document (CSV chunk repeats its header, JSON chunk is an array,
/// XML chunk is a `<rows>` document) validated on arrival; nothing
/// is written anywhere until the session is completed into an
/// import job and that job is confirmed.
#[derive(Debug, Clone, Serialize)]
pub struct ImportSession {
    pub id: Uuid,
    pub entity: String,
    pub format: String,
    /// `open`, `completed`, or `failed`.
    pub status: String,
    /// Declared chunk count, when the client knows it up front.
    pub total_chunks: Option<usize>,
    pub received_chunks: Vec<usize>,
    pub total_rows: usize,
    pub valid_rows: usize,
    pub invalid_rows: usize,
    pub row_errors: Vec<RowError>,
    /// Resolved rename map for the whole session.
    pub mapping: HashMap<String, String>,
    pub actor: String,
    pub company_id: Option<Uuid>,
    #[serde(skip_serializing)]
    pub staged_rows: Vec<Value>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

// ---------------------------------------------------------------------------
// CSV parsing (hand-rolled: no csv crate is available).
// ---------------------------------------------------------------------------

/// Parse CSV text into rows of fields. Handles quoted fields,
/// doubled-quote escapes, embedded commas/newlines, and CRLF endings.
/// Returns an error naming the first malformed line.
fn parse_csv(text: &str) -> Result<Vec<Vec<String>>, String> {
    let mut rows: Vec<Vec<String>> = Vec::new();
    let mut row: Vec<String> = Vec::new();
    let mut field = String::new();
    let mut in_quotes = false;
    let mut chars = text.chars().peekable();
    let mut line: usize = 1;
    while let Some(ch) = chars.next() {
        match ch {
            '"' => {
                if in_quotes {
                    if chars.peek() == Some(&'"') {
                        chars.next();
                        field.push('"');
                    } else {
                        in_quotes = false;
                    }
                } else if field.is_empty() {
                    in_quotes = true;
                } else {
                    return Err(format!("Line {line}: unexpected quote inside an unquoted field"));
                }
            }
            ',' if !in_quotes => {
                row.push(std::mem::take(&mut field));
            }
            '\r' => {
                if in_quotes {
                    field.push('\r');
                } else if chars.peek() == Some(&'\n') {
                    chars.next();
                    row.push(std::mem::take(&mut field));
                    rows.push(std::mem::take(&mut row));
                    line += 1;
                } else {
                    row.push(std::mem::take(&mut field));
                    rows.push(std::mem::take(&mut row));
                    line += 1;
                }
            }
            '\n' if !in_quotes => {
                row.push(std::mem::take(&mut field));
                rows.push(std::mem::take(&mut row));
                line += 1;
            }
            other => field.push(other),
        }
    }
    if in_quotes {
        return Err(format!("Line {line}: unterminated quoted field"));
    }
    if !field.is_empty() || !row.is_empty() {
        row.push(field);
        rows.push(row);
    }
    // Drop a trailing empty row produced by a final newline.
    while let Some(last) = rows.last() {
        if last.len() == 1 && last[0].is_empty() {
            rows.pop();
        } else {
            break;
        }
    }
    Ok(rows)
}

fn escape_csv_field(field: &str) -> String {
    if field.contains(',') || field.contains('"') || field.contains('\n') || field.contains('\r') {
        format!("\"{}\"", field.replace('"', "\"\""))
    } else {
        field.to_string()
    }
}

fn value_to_csv_cell(value: &Value) -> String {
    match value {
        Value::Null => String::new(),
        Value::Bool(flag) => flag.to_string(),
        Value::Number(number) => number.to_string(),
        Value::String(text) => text.clone(),
        other => serde_json::to_string(other).unwrap_or_default(),
    }
}

// ---------------------------------------------------------------------------
// XML parsing and serialization (hand-rolled: no xml crate is available).
// Rows are flat: one element per row, one child element per column.
// A DOCTYPE declaration is rejected outright so external entities
// can never be resolved.
// ---------------------------------------------------------------------------

fn escape_xml_text(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            other => out.push(other),
        }
    }
    out
}

fn unescape_xml_text(text: &str) -> String {
    let mut out = text
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&amp;", "&");
    // Numeric character references (`&#65;` and `&#x41;`).
    while let Some(start) = out.find("&#") {
        let rest = &out[start + 2..];
        let (digits, hex) = if let Some(stripped) = rest.strip_prefix('x').or_else(|| rest.strip_prefix('X')) {
            (stripped, true)
        } else {
            (rest, false)
        };
        let end = digits.find(';');
        let Some(end) = end else { break };
        let number = &digits[..end];
        let parsed = if hex {
            u32::from_str_radix(number, 16).ok()
        } else {
            number.parse::<u32>().ok()
        };
        let Some(code) = parsed.and_then(char::from_u32) else { break };
        let prefix_len = if hex { 3 } else { 2 };
        out.replace_range(start..start + prefix_len + end + 1, &code.to_string());
    }
    out
}

#[derive(Debug, Clone)]
struct XmlElement {
    name: String,
    text: String,
    children: Vec<XmlElement>,
}

struct XmlParser {
    chars: Vec<char>,
    pos: usize,
}

impl XmlParser {
    fn new(source: &str) -> Self {
        Self { chars: source.chars().collect(), pos: 0 }
    }

    fn peek(&self) -> Option<char> {
        self.chars.get(self.pos).copied()
    }

    fn peek_at(&self, offset: usize) -> Option<char> {
        self.chars.get(self.pos + offset).copied()
    }

    fn starts_with(&self, text: &str) -> bool {
        let wanted: Vec<char> = text.chars().collect();
        self.chars[self.pos..].starts_with(&wanted)
    }

    fn skip_whitespace(&mut self) {
        while matches!(self.peek(), Some(ch) if ch.is_whitespace()) {
            self.pos += 1;
        }
    }

    /// Skip an XML declaration, a comment, or a processing
    /// instruction. Returns false when there is nothing to skip.
    fn skip_misc(&mut self) -> bool {
        if self.starts_with("<?") {
            while let Some(ch) = self.peek() {
                self.pos += 1;
                if ch == '?' && self.peek() == Some('>') {
                    self.pos += 1;
                    break;
                }
            }
            true
        } else if self.starts_with("<!--") {
            while self.pos + 2 < self.chars.len() && !self.starts_with("-->") {
                self.pos += 1;
            }
            self.pos = (self.pos + 3).min(self.chars.len());
            true
        } else {
            false
        }
    }

    fn read_name(&mut self) -> Result<String, String> {
        let start = self.pos;
        while let Some(ch) = self.peek() {
            if ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-' | '.' | ':') {
                self.pos += 1;
            } else {
                break;
            }
        }
        if self.pos == start {
            return Err("Expected an element name".to_string());
        }
        Ok(self.chars[start..self.pos].iter().collect())
    }

    /// Skip attributes up to `>` or `/>`, honouring quoted values.
    fn skip_attributes(&mut self) -> Result<bool, String> {
        loop {
            match self.peek() {
                None => return Err("Unterminated element".to_string()),
                Some('>') => {
                    self.pos += 1;
                    return Ok(false);
                }
                Some('/') if self.peek_at(1) == Some('>') => {
                    self.pos += 2;
                    return Ok(true);
                }
                Some(quote @ ('"' | '\'')) => {
                    self.pos += 1;
                    while let Some(ch) = self.peek() {
                        self.pos += 1;
                        if ch == quote {
                            break;
                        }
                    }
                }
                Some(_) => {
                    self.pos += 1;
                }
            }
        }
    }

    fn parse_element(&mut self, depth: usize) -> Result<XmlElement, String> {
        if depth > 4 {
            return Err("XML nesting is too deep: rows must be flat".to_string());
        }
        if self.peek() != Some('<') {
            return Err("Expected '<' at the start of an element".to_string());
        }
        self.pos += 1;
        let name = self.read_name()?;
        let self_closing = self.skip_attributes()?;
        if self_closing {
            return Ok(XmlElement { name, text: String::new(), children: Vec::new() });
        }
        let mut text = String::new();
        let mut children = Vec::new();
        loop {
            match self.peek() {
                None => return Err(format!("Unterminated element <{name}>")),
                Some('<') if self.starts_with("</") => {
                    self.pos += 2;
                    let closing = self.read_name()?;
                    if closing != name {
                        return Err(format!(
                            "Mismatched closing tag </{closing}> for <{name}>"
                        ));
                    }
                    self.skip_whitespace();
                    if self.peek() != Some('>') {
                        return Err(format!("Expected '>' after </{closing}>"));
                    }
                    self.pos += 1;
                    break;
                }
                Some('<') if self.starts_with("<!--") => {
                    self.skip_misc();
                }
                Some('<') => {
                    children.push(self.parse_element(depth + 1)?);
                }
                Some(_) => {
                    let start = self.pos;
                    while let Some(ch) = self.peek() {
                        if ch == '<' {
                            break;
                        }
                        self.pos += 1;
                    }
                    text.push_str(&self.chars[start..self.pos].iter().collect::<String>());
                }
            }
        }
        Ok(XmlElement { name, text: unescape_xml_text(text.trim()), children })
    }
}

fn parse_xml_document(text: &str) -> Result<XmlElement, String> {
    let lowered = text.to_ascii_lowercase();
    if lowered.contains("<!doctype") || lowered.contains("<!entity") {
        return Err("DOCTYPE and ENTITY declarations are not allowed".to_string());
    }
    let mut parser = XmlParser::new(text);
    loop {
        parser.skip_whitespace();
        if !parser.skip_misc() {
            break;
        }
    }
    let root = parser.parse_element(0)?;
    loop {
        parser.skip_whitespace();
        if !parser.skip_misc() {
            break;
        }
    }
    if parser.pos != parser.chars.len() {
        return Err("Unexpected content after the root element".to_string());
    }
    Ok(root)
}

fn value_to_xml_cell(value: &Value) -> String {
    match value {
        Value::Null => String::new(),
        Value::Bool(flag) => flag.to_string(),
        Value::Number(number) => number.to_string(),
        Value::String(text) => text.clone(),
        other => serde_json::to_string(other).unwrap_or_default(),
    }
}

// ---------------------------------------------------------------------------
// Schedule validation (same rule the scheduler module validates
// with: an interval in seconds, or five cron-like fields where each
// field is `*`, `*/N`, or a plain number).
// ---------------------------------------------------------------------------

fn valid_schedule(schedule: &str) -> bool {
    let schedule = schedule.trim();
    if schedule.is_empty() {
        return false;
    }
    if schedule.parse::<u64>().is_ok() {
        return true;
    }
    let fields: Vec<&str> = schedule.split_whitespace().collect();
    if fields.len() != 5 {
        return false;
    }
    fields.iter().all(|field| {
        if *field == "*" {
            return true;
        }
        if let Some(step) = field.strip_prefix("*/") {
            return step.parse::<u32>().is_ok_and(|n| n > 0);
        }
        field.parse::<u32>().is_ok()
    })
}

// ---------------------------------------------------------------------------
// Validation.
// ---------------------------------------------------------------------------

fn normalized_column(name: &str) -> String {
    name.chars()
        .filter(|ch| ch.is_ascii_alphanumeric())
        .flat_map(|ch| ch.to_lowercase())
        .collect()
}

fn is_forbidden_column(name: &str) -> bool {
    let normalized = normalized_column(name);
    FORBIDDEN_COLUMN_FRAGMENTS
        .iter()
        .any(|fragment| normalized.contains(fragment))
}

fn type_error(row: usize, column: &str, kind: &str, value: &str) -> RowError {
    RowError {
        row,
        column: Some(column.to_string()),
        message: format!("Value \"{value}\" is not a valid {kind}"),
    }
}

/// Validate and coerce one cell value. Returns the JSON value, or a
/// row error. Empty strings become JSON null for optional columns.
fn coerce_value(row: usize, column: &EntityColumn, raw: &str) -> Result<Value, RowError> {
    let raw = raw.trim();
    if raw.is_empty() {
        return Ok(Value::Null);
    }
    match column.kind {
        "string" => Ok(Value::String(raw.to_string())),
        "integer" => raw.parse::<i64>().map(Value::from).map_err(|_| {
            type_error(row, column.name, "integer", raw)
        }),
        "number" => raw
            .parse::<f64>()
            .map_err(|_| type_error(row, column.name, "number", raw))
            .and_then(|number| {
                serde_json::Number::from_f64(number)
                    .map(Value::Number)
                    .ok_or_else(|| type_error(row, column.name, "number", raw))
            }),
        "boolean" => match raw.to_ascii_lowercase().as_str() {
            "true" | "1" | "yes" | "y" => Ok(Value::Bool(true)),
            "false" | "0" | "no" | "n" => Ok(Value::Bool(false)),
            _ => Err(type_error(row, column.name, "boolean", raw)),
        },
        "email" => {
            let parts: Vec<&str> = raw.split('@').collect();
            let valid = parts.len() == 2
                && !parts[0].is_empty()
                && parts[1].contains('.')
                && !parts[1].starts_with('.')
                && !raw.contains(char::is_whitespace);
            if valid {
                Ok(Value::String(raw.to_string()))
            } else {
                Err(type_error(row, column.name, "email", raw))
            }
        }
        "uuid" => raw.parse::<Uuid>().map(|id| Value::String(id.to_string())).map_err(|_| {
            type_error(row, column.name, "uuid", raw)
        }),
        "date" => {
            let parsed = DateTime::parse_from_rfc3339(raw)
                .map(|date| date.with_timezone(&Utc))
                .or_else(|_| {
                    chrono::NaiveDate::parse_from_str(raw, "%Y-%m-%d")
                        .map(|date| date.and_hms_opt(0, 0, 0).expect("midnight is valid").and_utc())
                });
            match parsed {
                Ok(date) => Ok(Value::String(date.to_rfc3339())),
                Err(_) => Err(type_error(row, column.name, "date", raw)),
            }
        }
        _ => Ok(Value::String(raw.to_string())),
    }
}

/// Validate raw rows (each a map of column name to raw string) against
/// an entity spec. Returns validated rows plus every row error found.
/// A row whose `company_id` disagrees with the caller's company
/// fails validation: a tenant can only import its own rows.
fn validate_rows(
    spec: &EntitySpec,
    raw_rows: Vec<HashMap<String, String>>,
    caller_company: Option<Uuid>,
) -> (Vec<Value>, Vec<RowError>) {
    let mut valid: Vec<Value> = Vec::new();
    let mut errors: Vec<RowError> = Vec::new();
    for (index, raw_row) in raw_rows.iter().enumerate() {
        let row_number = index + 1;
        let mut object = serde_json::Map::new();
        let mut row_errors: Vec<RowError> = Vec::new();
        for column in spec.columns {
            match raw_row.get(column.name) {
                Some(raw) => match coerce_value(row_number, column, raw) {
                    Ok(value) => {
                        if !value.is_null() {
                            if column.name == "company_id" {
                                if let (Some(company), Some(text)) =
                                    (caller_company, value.as_str())
                                {
                                    if text != company.to_string() {
                                        row_errors.push(RowError {
                                            row: row_number,
                                            column: Some(column.name.to_string()),
                                            message: "company_id does not match the caller's company"
                                                .to_string(),
                                        });
                                        continue;
                                    }
                                }
                            }
                            object.insert(column.name.to_string(), value);
                        }
                    }
                    Err(error) => row_errors.push(error),
                },
                None => {
                    if column.required {
                        row_errors.push(RowError {
                            row: row_number,
                            column: Some(column.name.to_string()),
                            message: format!("Missing required column \"{}\"", column.name),
                        });
                    }
                }
            }
        }
        if row_errors.is_empty() {
            valid.push(Value::Object(object));
        } else {
            errors.extend(row_errors);
        }
    }
    (valid, errors)
}

/// Shared column-name gate for every import format: reject secrets,
/// reject unknown columns.
fn check_column_name(spec: &EntitySpec, name: &str) -> Result<(), String> {
    if is_forbidden_column(name) {
        return Err(format!(
            "Column \"{name}\" looks like a secret or credential and is never imported"
        ));
    }
    if !spec.columns.iter().any(|column| column.name == name) {
        return Err(format!(
            "Unknown column \"{name}\" for entity \"{}\"",
            spec.entity
        ));
    }
    Ok(())
}

/// Validate a rename map: every target must be a known, non-secret
/// column of the entity, and every source name must be non-empty.
fn validate_mapping(spec: &EntitySpec, mapping: &HashMap<String, String>) -> UnifiedResult<()> {
    for (source, target) in mapping {
        if source.trim().is_empty() {
            return Err(UnifiedError::bad_request(
                "Mapping source column names must not be empty",
            ));
        }
        check_column_name(spec, target).map_err(UnifiedError::bad_request)?;
    }
    Ok(())
}

/// Validate an export field list against an entity spec.
fn validate_fields(spec: &EntitySpec, fields: &[String]) -> UnifiedResult<()> {
    if fields.is_empty() {
        return Err(UnifiedError::bad_request(
            "fields must not be empty when provided",
        ));
    }
    for field in fields {
        if !spec.columns.iter().any(|column| column.name == field) {
            return Err(UnifiedError::bad_request(format!(
                "Unknown field \"{field}\" for entity \"{}\"",
                spec.entity
            )));
        }
    }
    Ok(())
}

fn require_text(value: &str, field: &str, max: usize) -> UnifiedResult<String> {
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

fn valid_format(format: &str, allowed: &[String]) -> bool {
    allowed.iter().any(|entry| entry == format)
}

// ---------------------------------------------------------------------------
// Request types.
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct CreateImportRequest {
    entity: String,
    /// `csv`, `json`, or `xml`.
    format: String,
    /// File content: UTF-8 text, or base64 when `encoding` is `base64`.
    content: String,
    /// `utf8` (default) or `base64`.
    #[serde(default)]
    encoding: Option<String>,
    /// Optional rename map: file column name -> entity column name.
    #[serde(default)]
    mapping: Option<HashMap<String, String>>,
    /// Optional saved mapping template id. A per-request `mapping`
    /// wins over the template on conflicting keys.
    #[serde(default)]
    mapping_id: Option<Uuid>,
}

#[derive(Debug, Deserialize)]
struct CreateExportRequest {
    entity: String,
    /// `csv`, `json`, or `xml` (default `json`).
    #[serde(default)]
    format: Option<String>,
    /// Columns to include, in order. Defaults to every column.
    #[serde(default)]
    fields: Option<Vec<String>>,
    /// Exact-match filters forwarded to the data provider hook.
    #[serde(default)]
    filters: Option<HashMap<String, String>>,
    /// Redact personal data in the exported rows. Defaults to the
    /// module setting; forced on when the caller lacks
    /// `import_export:export_pii`.
    #[serde(default)]
    redact_pii: Option<bool>,
    /// Also redact personal names when redacting (default false:
    /// names stay readable, emails and phones are still redacted).
    #[serde(default)]
    redact_names: Option<bool>,
    /// Chunked export window: at most this many rows (default: the
    /// configured maximum). Must not exceed the configured maximum.
    #[serde(default)]
    limit: Option<usize>,
    /// Chunked export window: rows start at this offset (default 0).
    /// Forwarded to the data provider hook.
    #[serde(default)]
    offset: Option<usize>,
}

#[derive(Debug, Deserialize)]
struct CreateMappingRequest {
    name: String,
    entity: String,
    mapping: HashMap<String, String>,
}

#[derive(Debug, Deserialize)]
struct CreateScheduleRequest {
    name: String,
    entity: String,
    #[serde(default)]
    format: Option<String>,
    #[serde(default)]
    fields: Option<Vec<String>>,
    #[serde(default)]
    filters: Option<HashMap<String, String>>,
    #[serde(default)]
    redact_pii: Option<bool>,
    #[serde(default)]
    redact_names: Option<bool>,
    schedule: String,
    #[serde(default)]
    enabled: Option<bool>,
}

#[derive(Debug, Deserialize)]
struct UpdateScheduleRequest {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    format: Option<String>,
    #[serde(default)]
    fields: Option<Vec<String>>,
    #[serde(default)]
    filters: Option<HashMap<String, String>>,
    #[serde(default)]
    redact_pii: Option<bool>,
    #[serde(default)]
    redact_names: Option<bool>,
    #[serde(default)]
    schedule: Option<String>,
    #[serde(default)]
    enabled: Option<bool>,
}

#[derive(Debug, Deserialize)]
struct UpdateSettingsRequest {
    #[serde(default)]
    max_file_bytes: Option<usize>,
    #[serde(default)]
    max_rows_per_file: Option<usize>,
    #[serde(default)]
    max_chunk_bytes: Option<usize>,
    #[serde(default)]
    max_rows_per_session: Option<usize>,
    #[serde(default)]
    max_export_rows: Option<usize>,
    #[serde(default)]
    max_export_bytes: Option<usize>,
    #[serde(default)]
    default_redact_pii: Option<bool>,
    #[serde(default)]
    allowed_formats: Option<Vec<String>>,
}

#[derive(Debug, Deserialize)]
struct CreateSessionRequest {
    entity: String,
    /// `csv`, `json`, or `xml`.
    format: String,
    /// Optional rename map for the whole session.
    #[serde(default)]
    mapping: Option<HashMap<String, String>>,
    /// Optional saved mapping template id (same merge rule as imports).
    #[serde(default)]
    mapping_id: Option<Uuid>,
    /// Declared chunk count, when the client knows it up front.
    #[serde(default)]
    total_chunks: Option<usize>,
}

#[derive(Debug, Deserialize)]
struct AppendChunkRequest {
    /// 0-based chunk index. Each index may be sent only once.
    chunk_index: usize,
    /// Chunk content: UTF-8 text, or base64 when `encoding` is `base64`.
    content: String,
    /// `utf8` (default) or `base64`.
    #[serde(default)]
    encoding: Option<String>,
}

fn paginate<T: Serialize + Clone>(items: Vec<T>, params: &PaginationParams) -> PageResult<T> {
    let pagination = params.normalized();
    let total = items.len() as u64;
    let start = ((pagination.page - 1) * pagination.per_page) as usize;
    let end = (start + pagination.per_page as usize).min(items.len());
    let page_items = if start >= items.len() {
        Vec::new()
    } else {
        items[start..end].to_vec()
    };
    PageResult::new(page_items, total, &pagination)
}

fn decode_content(content: &str, encoding: Option<&str>, max_bytes: usize) -> UnifiedResult<String> {
    let bytes = match encoding.unwrap_or("utf8") {
        "base64" => base64::engine::general_purpose::STANDARD
            .decode(content.trim())
            .map_err(|_| UnifiedError::bad_request("content is not valid base64"))?,
        "utf8" => content.to_string().into_bytes(),
        other => {
            return Err(UnifiedError::bad_request(format!(
                "encoding must be \"utf8\" or \"base64\", got \"{other}\""
            )))
        }
    };
    if bytes.len() > max_bytes {
        return Err(UnifiedError::bad_request(format!(
            "Content is too large: maximum is {max_bytes} bytes"
        )));
    }
    String::from_utf8(bytes)
        .map_err(|_| UnifiedError::bad_request("content is not valid UTF-8 text"))
}

/// Resolve the effective rename map for an import: a saved template
/// (tenant-scoped, same entity) merged under a per-request map.
fn resolve_mapping(
    entity: &str,
    mapping: Option<HashMap<String, String>>,
    mapping_id: Option<Uuid>,
    caller_company: Option<Uuid>,
    can_manage: bool,
) -> UnifiedResult<HashMap<String, String>> {
    let mut resolved: HashMap<String, String> = HashMap::new();
    if let Some(id) = mapping_id {
        let template = mappings_store()
            .get(&id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Mapping template not found"))?;
        if !can_view_company(caller_company, template.company_id, can_manage) {
            return Err(UnifiedError::not_found("Mapping template not found"));
        }
        if template.entity != entity {
            return Err(UnifiedError::bad_request(format!(
                "Mapping template is for entity \"{}\" but the import is for \"{entity}\"",
                template.entity
            )));
        }
        resolved = template.mapping;
    }
    if let Some(request_mapping) = mapping {
        for (source, target) in request_mapping {
            resolved.insert(source, target);
        }
    }
    Ok(resolved)
}

/// Turn submitted content into raw rows: each row maps entity column
/// names to raw strings. Applies the optional rename mapping first.
fn content_to_raw_rows(
    spec: &EntitySpec,
    format: &str,
    text: &str,
    mapping: Option<&HashMap<String, String>>,
) -> UnifiedResult<Vec<HashMap<String, String>>> {
    let apply_mapping = |name: &str| -> String {
        mapping
            .and_then(|map| map.get(name))
            .cloned()
            .unwrap_or_else(|| name.to_string())
    };
    match format {
        "csv" => {
            let rows = parse_csv(text)
                .map_err(|error| UnifiedError::bad_request(format!("Invalid CSV: {error}")))?;
            if rows.is_empty() {
                return Err(UnifiedError::bad_request("CSV file is empty"));
            }
            let header: Vec<String> = rows[0].iter().map(|name| apply_mapping(name.trim())).collect();
            if header.iter().any(|name| name.trim().is_empty()) {
                return Err(UnifiedError::bad_request("CSV header contains an empty column name"));
            }
            let mut seen: Vec<String> = Vec::new();
            for name in &header {
                if seen.contains(name) {
                    return Err(UnifiedError::bad_request(format!(
                        "CSV header contains duplicate column \"{name}\""
                    )));
                }
                seen.push(name.clone());
            }
            for name in &header {
                check_column_name(spec, name).map_err(UnifiedError::bad_request)?;
            }
            let mut raw_rows = Vec::new();
            for (index, row) in rows.iter().skip(1).enumerate() {
                if row.len() != header.len() {
                    return Err(UnifiedError::bad_request(format!(
                        "CSV row {} has {} fields but the header has {}",
                        index + 2,
                        row.len(),
                        header.len()
                    )));
                }
                let mut map = HashMap::new();
                for (name, value) in header.iter().zip(row.iter()) {
                    map.insert(name.clone(), value.clone());
                }
                raw_rows.push(map);
            }
            Ok(raw_rows)
        }
        "json" => {
            let value: Value = serde_json::from_str(text)
                .map_err(|error| UnifiedError::bad_request(format!("Invalid JSON: {error}")))?;
            let array = value.as_array().ok_or_else(|| {
                UnifiedError::bad_request("JSON import must be an array of objects")
            })?;
            let mut raw_rows = Vec::new();
            for (index, item) in array.iter().enumerate() {
                let object = item.as_object().ok_or_else(|| {
                    UnifiedError::bad_request(format!("JSON row {} is not an object", index + 1))
                })?;
                let mut map = HashMap::new();
                for (key, value) in object {
                    let name = apply_mapping(key);
                    check_column_name(spec, &name).map_err(UnifiedError::bad_request)?;
                    let raw = match value {
                        Value::Null => String::new(),
                        Value::String(text) => text.clone(),
                        other => other.to_string(),
                    };
                    map.insert(name, raw);
                }
                raw_rows.push(map);
            }
            Ok(raw_rows)
        }
        "xml" => {
            let root = parse_xml_document(text)
                .map_err(|error| UnifiedError::bad_request(format!("Invalid XML: {error}")))?;
            if !XML_ROOT_ELEMENTS.contains(&root.name.as_str()) {
                return Err(UnifiedError::bad_request(format!(
                    "XML root element must be one of {}, got \"{}\"",
                    XML_ROOT_ELEMENTS.join(", "),
                    root.name
                )));
            }
            if !root.text.is_empty() {
                return Err(UnifiedError::bad_request(
                    "XML root element must only contain row elements",
                ));
            }
            let mut raw_rows = Vec::new();
            for (index, row_element) in root.children.iter().enumerate() {
                if !XML_ROW_ELEMENTS.contains(&row_element.name.as_str()) {
                    return Err(UnifiedError::bad_request(format!(
                        "XML row {} uses element \"{}\"; expected one of {}",
                        index + 1,
                        row_element.name,
                        XML_ROW_ELEMENTS.join(", ")
                    )));
                }
                let mut map = HashMap::new();
                for column_element in &row_element.children {
                    if !column_element.children.is_empty() {
                        return Err(UnifiedError::bad_request(format!(
                            "XML row {} column \"{}\" must be flat text, not nested elements",
                            index + 1,
                            column_element.name
                        )));
                    }
                    let name = apply_mapping(&column_element.name);
                    check_column_name(spec, &name).map_err(UnifiedError::bad_request)?;
                    if map.contains_key(&name) {
                        return Err(UnifiedError::bad_request(format!(
                            "XML row {} repeats column \"{name}\"",
                            index + 1
                        )));
                    }
                    map.insert(name, column_element.text.clone());
                }
                raw_rows.push(map);
            }
            Ok(raw_rows)
        }
        other => Err(UnifiedError::bad_request(format!(
            "format must be one of csv, json, or xml, got \"{other}\""
        ))),
    }
}

// ---------------------------------------------------------------------------
// Shared export runner (used by the export endpoint, by scheduled
// runs, and by the `export.run_requested` hook handler).
// ---------------------------------------------------------------------------

struct ExportSpec {
    entity: String,
    format: String,
    fields: Option<Vec<String>>,
    filters: HashMap<String, String>,
    redact_pii: Option<bool>,
    redact_names: Option<bool>,
    limit: Option<usize>,
    offset: Option<usize>,
}

/// Build one export job: collect rows from owning modules through
/// the hook bus, scope them to the caller's company, redact personal
/// data through the privacy module, serialize, store, and audit.
/// Unredacted personal data requires `can_export_pii`; anything else
/// forces redaction.
fn run_export(
    actor: &str,
    company_id: Option<Uuid>,
    can_export_pii: bool,
    spec: &ExportSpec,
) -> UnifiedResult<ExportJob> {
    let config = settings();
    let entity = spec.entity.trim();
    let entity_spec = entity_spec(entity).ok_or_else(|| {
        UnifiedError::bad_request(format!(
            "Unknown entity \"{entity}\"; see GET /api/v1/import-export/entities"
        ))
    })?;
    let format = spec.format.trim().to_lowercase();
    if !valid_format(&format, &config.allowed_formats) {
        return Err(UnifiedError::bad_request(format!(
            "format must be one of {}, got \"{format}\"",
            config.allowed_formats.join(", ")
        )));
    }
    let fields: Vec<String> = match &spec.fields {
        Some(fields) => {
            validate_fields(entity_spec, fields)?;
            fields.clone()
        }
        None => entity_spec
            .columns
            .iter()
            .map(|column| column.name.to_string())
            .collect(),
    };
    for key in spec.filters.keys() {
        if !entity_spec.columns.iter().any(|column| column.name == key) {
            return Err(UnifiedError::bad_request(format!(
                "Unknown filter \"{key}\" for entity \"{entity}\""
            )));
        }
    }
    let limit = spec.limit.unwrap_or(config.max_export_rows);
    if limit == 0 || limit > config.max_export_rows {
        return Err(UnifiedError::bad_request(format!(
            "limit must be between 1 and {}",
            config.max_export_rows
        )));
    }
    let offset = spec.offset.unwrap_or(0);
    // Ask owning modules for rows through the hook bus. Each
    // handler contributes `{"rows": [...]}` or a bare array.
    let mut rows: Vec<Value> = Vec::new();
    let mut provider = "none".to_string();
    if let Some(context) = emit(
        import_export_events::EXPORT_REQUESTED,
        serde_json::json!({
            "entity": entity_spec.entity,
            "fields": fields,
            "filters": spec.filters,
            "limit": limit,
            "offset": offset,
            "company_id": company_id.map(|id| id.to_string()),
            "actor": actor,
        }),
    ) {
        for result in &context.results {
            let contributed: Option<&Vec<Value>> = result
                .get("rows")
                .and_then(Value::as_array)
                .or_else(|| result.as_array());
            if let Some(list) = contributed {
                if !list.is_empty() {
                    provider = "hook".to_string();
                }
                for row in list {
                    if rows.len() >= limit {
                        break;
                    }
                    rows.push(row.clone());
                }
            }
        }
    }
    // Tenant scope: drop rows tagged with another company. Rows
    // without a `company_id` tag are kept; providers are expected to
    // scope server-side from the hook payload, and this drop is the
    // second line of defence for providers that tag but do not scope.
    let mut rows_excluded_by_tenant_scope = 0usize;
    if let Some(company) = company_id {
        rows.retain(|row| {
            let tagged = row
                .get("company_id")
                .and_then(Value::as_str)
                .and_then(|raw| Uuid::parse_str(raw).ok());
            match tagged {
                Some(tagged) if tagged != company => {
                    rows_excluded_by_tenant_scope += 1;
                    false
                }
                _ => true,
            }
        });
    }
    // Trim every row to the requested fields.
    let mut trimmed: Vec<Value> = Vec::new();
    for row in rows {
        if let Some(object) = row.as_object() {
            let mut kept = serde_json::Map::new();
            for field in &fields {
                if let Some(value) = object.get(field) {
                    kept.insert(field.clone(), value.clone());
                }
            }
            trimmed.push(Value::Object(kept));
        }
    }
    // PII redaction through the privacy module (reused, never
    // duplicated here). Forced on when the caller may not export
    // personal data unredacted, or when the module default says so.
    let requested_redact = spec.redact_pii.unwrap_or(config.default_redact_pii);
    let redact = requested_redact || !can_export_pii;
    let redact_names = spec.redact_names.unwrap_or(false);
    let redacted_fields: Vec<String> = if redact {
        entity_spec
            .pii_columns
            .iter()
            .filter(|column| fields.iter().any(|field| field == *column))
            .map(|column| column.to_string())
            .collect()
    } else {
        Vec::new()
    };
    if redact {
        for row in &mut trimmed {
            *row = redact_value(row, redact_names);
        }
    }
    let has_more = trimmed.len() == limit;
    let content: Vec<u8> = if format == "csv" {
        let mut text = fields
            .iter()
            .map(|field| escape_csv_field(field))
            .collect::<Vec<_>>()
            .join(",");
        text.push('\n');
        for row in &trimmed {
            let cells: Vec<String> = fields
                .iter()
                .map(|field| {
                    escape_csv_field(&value_to_csv_cell(
                        row.get(field).unwrap_or(&Value::Null),
                    ))
                })
                .collect();
            text.push_str(&cells.join(","));
            text.push('\n');
        }
        text.into_bytes()
    } else if format == "xml" {
        let mut text = format!(
            "<export entity=\"{}\" generated_at=\"{}\" row_count=\"{}\">",
            escape_xml_text(entity_spec.entity),
            Utc::now().to_rfc3339(),
            trimmed.len()
        );
        for row in &trimmed {
            text.push_str("<row>");
            for field in &fields {
                let cell = value_to_xml_cell(row.get(field).unwrap_or(&Value::Null));
                text.push_str(&format!(
                    "<{field}>{}</{field}>",
                    escape_xml_text(&cell)
                ));
            }
            text.push_str("</row>");
        }
        text.push_str("</export>");
        text.into_bytes()
    } else {
        serde_json::to_vec_pretty(&trimmed)
            .map_err(|error| UnifiedError::internal(format!("Cannot serialize export: {error}")))?
    };
    if content.len() > config.max_export_bytes {
        return Err(UnifiedError::internal(
            "Export is too large to hold in memory; narrow it with filters or field selection",
        ));
    }
    let now = Utc::now();
    let id = Uuid::now_v7();
    let job = ExportJob {
        id,
        entity: entity_spec.entity.to_string(),
        format,
        status: ExportStatus::Completed.as_str().to_string(),
        fields,
        row_count: trimmed.len(),
        data_provider: provider.clone(),
        download_url: format!("/api/v1/import-export/exports/{id}/download"),
        error: None,
        pii_redacted: redact,
        redacted_fields: redacted_fields.clone(),
        limit,
        offset,
        has_more,
        rows_excluded_by_tenant_scope,
        actor: actor.to_string(),
        company_id,
        created_at: now,
        updated_at: now,
        content,
    };
    let view = job.clone();
    export_jobs_store().insert(id, job);
    let resource = format!("export:{id}");
    emit_audit(
        import_export_events::EXPORT_CREATED,
        actor,
        company_id,
        &resource,
        serde_json::json!({
            "entity": entity_spec.entity,
            "format": view.format,
            "row_count": view.row_count,
            "pii_redacted": redact,
            "limit": limit,
            "offset": offset,
        }),
    );
    if redact && !redacted_fields.is_empty() {
        emit_audit(
            import_export_events::EXPORT_PII_REDACTED,
            actor,
            company_id,
            &resource,
            serde_json::json!({
                "entity": entity_spec.entity,
                "redacted_fields": redacted_fields,
                "redact_names": redact_names,
            }),
        );
    }
    emit(
        import_export_events::EXPORT_COMPLETED,
        serde_json::json!({
            "export_id": id,
            "entity": entity_spec.entity,
            "row_count": view.row_count,
            "data_provider": view.data_provider.clone(),
            "actor": actor,
            "actor_id": actor,
            "resource": resource,
            "company_id": company_id.map(|id| id.to_string()),
            "pii_redacted": redact,
        }),
    );
    Ok(view)
}

/// Offer a schedule to the scheduler module through
/// `scheduler.enqueue` with kind `export.run`, the way the backup
/// module offers its jobs. A refusal is recorded on the schedule
/// instead of failing the request, because run-now keeps working.
fn notify_scheduler(hooks: &HookBus, schedule_id: Uuid) -> SchedulingOutcome {
    let context = hooks.emit(
        SCHEDULER_ENQUEUE,
        serde_json::json!({
            "kind": EXPORT_RUN_KIND,
            "payload": { "export_schedule_id": schedule_id, "job_id": schedule_id },
        }),
    );
    for result in &context.results {
        if let Some(queued) = result.get("queued").and_then(|value| value.as_bool()) {
            if queued {
                return SchedulingOutcome {
                    enqueued: true,
                    message: "Scheduled with the scheduler queue.".to_string(),
                };
            }
            let detail = result
                .get("error")
                .and_then(|value| value.as_str())
                .unwrap_or("Scheduler refused the export schedule.");
            return SchedulingOutcome {
                enqueued: false,
                message: format!(
                    "Scheduler did not accept the schedule ({detail}). Run-now still works; the scheduler needs an \"export.run\" dispatch arm."
                ),
            };
        }
    }
    SchedulingOutcome {
        enqueued: false,
        message: "No scheduler.enqueue handler is registered; the scheduler module may be disabled."
            .to_string(),
    }
}

/// Run one stored schedule through the shared export runner and
/// record the outcome on the schedule. Returns the new export job.
fn run_schedule(schedule: &ExportSchedule) -> UnifiedResult<ExportJob> {
    let spec = ExportSpec {
        entity: schedule.entity.clone(),
        format: schedule.format.clone(),
        fields: Some(schedule.fields.clone()),
        filters: schedule.filters.clone(),
        redact_pii: Some(schedule.redact_pii),
        redact_names: Some(schedule.redact_names),
        limit: None,
        offset: None,
    };
    let job = run_export(
        &schedule.actor,
        schedule.company_id,
        schedule.pii_allowed,
        &spec,
    )?;
    let now = Utc::now();
    if let Some(mut stored) = export_schedules_store().get_mut(&schedule.id) {
        stored.last_run_at = Some(now);
        stored.last_export_id = Some(job.id);
        stored.last_error = None;
        stored.run_count += 1;
        stored.updated_at = now;
    }
    emit_audit(
        import_export_events::EXPORT_SCHEDULE_RUN,
        &schedule.actor,
        schedule.company_id,
        &format!("export_schedule:{}", schedule.id),
        serde_json::json!({
            "entity": schedule.entity,
            "export_id": job.id,
            "row_count": job.row_count,
        }),
    );
    Ok(job)
}

// ---------------------------------------------------------------------------
// Handlers: entities, settings, and exports.
// ---------------------------------------------------------------------------

async fn list_entities(ctx: web::Data<Arc<AppContext>>, req: HttpRequest) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_IMPORT_EXPORT_READ)?;
        let config = settings();
        Ok(serde_json::json!({
            "entities": ENTITY_SPECS,
            "max_file_bytes": config.max_file_bytes,
            "max_rows_per_file": config.max_rows_per_file,
            "max_chunk_bytes": config.max_chunk_bytes,
            "max_rows_per_session": config.max_rows_per_session,
            "max_export_rows": config.max_export_rows,
            "formats": config.allowed_formats,
            "default_redact_pii": config.default_redact_pii,
        }))
    })();
    json::respond(result, debug)
}

async fn get_settings(ctx: web::Data<Arc<AppContext>>, req: HttpRequest) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<ImportExportSettings> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_IMPORT_EXPORT_READ)?;
        Ok(settings())
    })();
    json::respond(result, debug)
}

async fn update_settings(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    body: web::Json<UpdateSettingsRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<ImportExportSettings> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_IMPORT_EXPORT_MANAGE)?;
        let input = body.into_inner();
        let mut updated = settings();
        if let Some(value) = input.max_file_bytes {
            if !(1_024..=100 * 1024 * 1024).contains(&value) {
                return Err(UnifiedError::bad_request(
                    "max_file_bytes must be between 1024 and 104857600",
                ));
            }
            updated.max_file_bytes = value;
        }
        if let Some(value) = input.max_rows_per_file {
            if !(1..=100_000).contains(&value) {
                return Err(UnifiedError::bad_request(
                    "max_rows_per_file must be between 1 and 100000",
                ));
            }
            updated.max_rows_per_file = value;
        }
        if let Some(value) = input.max_chunk_bytes {
            if !(1_024..=10 * 1024 * 1024).contains(&value) {
                return Err(UnifiedError::bad_request(
                    "max_chunk_bytes must be between 1024 and 10485760",
                ));
            }
            updated.max_chunk_bytes = value;
        }
        if let Some(value) = input.max_rows_per_session {
            if !(1..=500_000).contains(&value) {
                return Err(UnifiedError::bad_request(
                    "max_rows_per_session must be between 1 and 500000",
                ));
            }
            updated.max_rows_per_session = value;
        }
        if let Some(value) = input.max_export_rows {
            if !(1..=100_000).contains(&value) {
                return Err(UnifiedError::bad_request(
                    "max_export_rows must be between 1 and 100000",
                ));
            }
            updated.max_export_rows = value;
        }
        if let Some(value) = input.max_export_bytes {
            if !(1_024 * 1_024..=500 * 1024 * 1024).contains(&value) {
                return Err(UnifiedError::bad_request(
                    "max_export_bytes must be between 1048576 and 524288000",
                ));
            }
            updated.max_export_bytes = value;
        }
        if let Some(value) = input.default_redact_pii {
            updated.default_redact_pii = value;
        }
        if let Some(formats) = input.allowed_formats {
            if formats.is_empty() {
                return Err(UnifiedError::bad_request(
                    "allowed_formats must not be empty",
                ));
            }
            for format in &formats {
                if !["csv", "json", "xml"].contains(&format.as_str()) {
                    return Err(UnifiedError::bad_request(format!(
                        "Unknown format \"{format}\"; allowed formats are csv, json, xml"
                    )));
                }
            }
            updated.allowed_formats = formats;
        }
        if let Some(mut guard) = settings_store().write().ok() {
            *guard = updated.clone();
        }
        emit_audit(
            import_export_events::SETTINGS_UPDATED,
            &caller.actor,
            None,
            "import_export:settings",
            serde_json::json!({
                "max_file_bytes": updated.max_file_bytes,
                "max_rows_per_file": updated.max_rows_per_file,
                "max_export_rows": updated.max_export_rows,
                "default_redact_pii": updated.default_redact_pii,
            }),
        );
        Ok(updated)
    })();
    json::respond(result, debug)
}

async fn create_export(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    body: web::Json<CreateExportRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<ExportJob> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_IMPORT_EXPORT_EXPORT)?;
        let can_export_pii = check_capability(&ctx, &caller.role, PERM_IMPORT_EXPORT_EXPORT_PII);
        let company_id = resolve_caller_company(&ctx, &req, &caller);
        let input = body.into_inner();
        let spec = ExportSpec {
            entity: input.entity,
            format: input.format.unwrap_or_else(|| "json".to_string()),
            fields: input.fields,
            filters: input.filters.unwrap_or_default(),
            redact_pii: input.redact_pii,
            redact_names: input.redact_names,
            limit: input.limit,
            offset: input.offset,
        };
        run_export(&caller.actor, company_id, can_export_pii, &spec)
    })();
    match result {
        Ok(view) => json::success_response(view, 201),
        Err(error) => json::error_response(&error, debug),
    }
}

async fn list_exports(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    params: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_IMPORT_EXPORT_READ)?;
        let company_id = resolve_caller_company(&ctx, &req, &caller);
        let can_manage = check_capability(&ctx, &caller.role, PERM_IMPORT_EXPORT_MANAGE);
        let mut items: Vec<ExportJob> = export_jobs_store()
            .iter()
            .map(|entry| entry.value().clone())
            .collect();
        items = scope_visible(items, |job| job.company_id, company_id, can_manage);
        if let Some(entity) = params.filters.get("entity") {
            items.retain(|job| job.entity == *entity);
        }
        if let Some(format) = params.filters.get("format") {
            items.retain(|job| job.format == *format);
        }
        if let Some(status) = params.filters.get("status") {
            items.retain(|job| job.status == *status);
        }
        if let Some(company) = params.filters.get("company_id") {
            if can_manage || company_id.map(|id| id.to_string()) == Some(company.clone()) {
                let wanted = Uuid::parse_str(company).ok();
                items.retain(|job| job.company_id == wanted);
            }
        }
        items.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        if params.normalized().order == "asc" {
            items.reverse();
        }
        Ok(paginate(items, &params).to_json())
    })();
    json::respond(result, debug)
}

async fn get_export(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    id: web::Path<Uuid>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<ExportJob> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_IMPORT_EXPORT_READ)?;
        let company_id = resolve_caller_company(&ctx, &req, &caller);
        let can_manage = check_capability(&ctx, &caller.role, PERM_IMPORT_EXPORT_MANAGE);
        let job = export_jobs_store()
            .get(&id.into_inner())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Export job not found"))?;
        if !can_view_company(company_id, job.company_id, can_manage) {
            return Err(UnifiedError::not_found("Export job not found"));
        }
        Ok(job)
    })();
    json::respond(result, debug)
}

async fn download_export(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    id: web::Path<Uuid>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let caller = match caller(&req) {
        Ok(caller) => caller,
        Err(error) => return json::error_response(&error, debug),
    };
    if let Err(error) = require_permission(&ctx, &caller.role, PERM_IMPORT_EXPORT_EXPORT) {
        return json::error_response(&error, debug);
    }
    let company_id = resolve_caller_company(&ctx, &req, &caller);
    let can_manage = check_capability(&ctx, &caller.role, PERM_IMPORT_EXPORT_MANAGE);
    let Some(job) = export_jobs_store()
        .get(&id.into_inner())
        .map(|entry| entry.value().clone())
    else {
        return json::error_response(&UnifiedError::not_found("Export job not found"), debug);
    };
    if !can_view_company(company_id, job.company_id, can_manage) {
        return json::error_response(&UnifiedError::not_found("Export job not found"), debug);
    }
    if job.status != ExportStatus::Completed.as_str() {
        return json::error_response(
            &UnifiedError::conflict(format!(
                "Export job is in status \"{}\"; nothing to download",
                job.status
            )),
            debug,
        );
    }
    let (content_type, filename) = if job.format == "csv" {
        ("text/csv; charset=utf-8", format!("{}-export-{}.csv", job.entity, job.id))
    } else if job.format == "xml" {
        ("application/xml; charset=utf-8", format!("{}-export-{}.xml", job.entity, job.id))
    } else {
        (
            "application/json; charset=utf-8",
            format!("{}-export-{}.json", job.entity, job.id),
        )
    };
    HttpResponse::Ok()
        .content_type(content_type)
        .insert_header((
            "Content-Disposition",
            format!("attachment; filename=\"{filename}\""),
        ))
        .body(job.content)
}

// ---------------------------------------------------------------------------
// Handlers: imports.
// ---------------------------------------------------------------------------

async fn create_import(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    body: web::Json<CreateImportRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<ImportJob> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_IMPORT_EXPORT_IMPORT)?;
        let company_id = resolve_caller_company(&ctx, &req, &caller);
        let can_manage = check_capability(&ctx, &caller.role, PERM_IMPORT_EXPORT_MANAGE);
        let config = settings();
        let input = body.into_inner();
        let spec = entity_spec(input.entity.trim()).ok_or_else(|| {
            UnifiedError::bad_request(format!(
                "Unknown entity \"{}\"; see GET /api/v1/import-export/entities",
                input.entity
            ))
        })?;
        let format = input.format.trim().to_lowercase();
        if !valid_format(&format, &config.allowed_formats) {
            return Err(UnifiedError::bad_request(format!(
                "format must be one of {}, got \"{format}\"",
                config.allowed_formats.join(", ")
            )));
        }
        let mapping = resolve_mapping(
            spec.entity,
            input.mapping,
            input.mapping_id,
            company_id,
            can_manage,
        )?;
        validate_mapping(spec, &mapping)?;
        let text = decode_content(&input.content, input.encoding.as_deref(), config.max_file_bytes)?;
        let raw_rows = content_to_raw_rows(spec, &format, &text, Some(&mapping))?;
        if raw_rows.len() > config.max_rows_per_file {
            return Err(UnifiedError::bad_request(format!(
                "File has {} rows; the maximum is {}",
                raw_rows.len(),
                config.max_rows_per_file
            )));
        }
        let (valid_rows, row_errors) = validate_rows(spec, raw_rows, company_id);
        // Row errors are counted per invalid row, not per problem, so
        // a row with three bad columns is still one invalid row.
        let mut invalid_row_numbers: Vec<usize> = row_errors.iter().map(|error| error.row).collect();
        invalid_row_numbers.sort_unstable();
        invalid_row_numbers.dedup();
        let invalid_rows = invalid_row_numbers.len();
        let total = valid_rows.len() + invalid_rows;
        let now = Utc::now();
        let id = Uuid::now_v7();
        let status = if invalid_rows == 0 {
            ImportStatus::Validated.as_str()
        } else {
            ImportStatus::Failed.as_str()
        };
        let job = ImportJob {
            id,
            entity: spec.entity.to_string(),
            format,
            status: status.to_string(),
            total_rows: total,
            valid_rows: valid_rows.len(),
            invalid_rows,
            row_errors,
            results: Vec::new(),
            actor: caller.actor.clone(),
            company_id,
            staged_rows: valid_rows,
            created_at: now,
            updated_at: now,
        };
        let view = job.clone();
        import_jobs_store().insert(id, job);
        let resource = format!("import:{id}");
        emit_audit(
            import_export_events::IMPORT_CREATED,
            &caller.actor,
            company_id,
            &resource,
            serde_json::json!({
                "entity": spec.entity,
                "format": view.format,
                "total_rows": total,
                "valid_rows": view.valid_rows,
                "invalid_rows": invalid_rows,
            }),
        );
        if invalid_rows > 0 {
            emit_audit(
                import_export_events::IMPORT_FAILED,
                &caller.actor,
                company_id,
                &resource,
                serde_json::json!({
                    "entity": spec.entity,
                    "invalid_rows": invalid_rows,
                    "total_rows": total,
                }),
            );
        }
        Ok(view)
    })();
    match result {
        Ok(view) => json::success_response(view, 201),
        Err(error) => json::error_response(&error, debug),
    }
}

async fn list_imports(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    params: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_IMPORT_EXPORT_READ)?;
        let company_id = resolve_caller_company(&ctx, &req, &caller);
        let can_manage = check_capability(&ctx, &caller.role, PERM_IMPORT_EXPORT_MANAGE);
        let mut items: Vec<ImportJob> = import_jobs_store()
            .iter()
            .map(|entry| entry.value().clone())
            .collect();
        items = scope_visible(items, |job| job.company_id, company_id, can_manage);
        if let Some(entity) = params.filters.get("entity") {
            items.retain(|job| job.entity == *entity);
        }
        if let Some(status) = params.filters.get("status") {
            items.retain(|job| job.status == *status);
        }
        items.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        if params.normalized().order == "asc" {
            items.reverse();
        }
        Ok(paginate(items, &params).to_json())
    })();
    json::respond(result, debug)
}

async fn get_import(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    id: web::Path<Uuid>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<ImportJob> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_IMPORT_EXPORT_READ)?;
        let company_id = resolve_caller_company(&ctx, &req, &caller);
        let can_manage = check_capability(&ctx, &caller.role, PERM_IMPORT_EXPORT_MANAGE);
        let job = import_jobs_store()
            .get(&id.into_inner())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Import job not found"))?;
        if !can_view_company(company_id, job.company_id, can_manage) {
            return Err(UnifiedError::not_found("Import job not found"));
        }
        Ok(job)
    })();
    json::respond(result, debug)
}

async fn download_import_errors(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    id: web::Path<Uuid>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let caller = match caller(&req) {
        Ok(caller) => caller,
        Err(error) => return json::error_response(&error, debug),
    };
    if let Err(error) = require_permission(&ctx, &caller.role, PERM_IMPORT_EXPORT_READ) {
        return json::error_response(&error, debug);
    }
    let company_id = resolve_caller_company(&ctx, &req, &caller);
    let can_manage = check_capability(&ctx, &caller.role, PERM_IMPORT_EXPORT_MANAGE);
    let Some(job) = import_jobs_store()
        .get(&id.into_inner())
        .map(|entry| entry.value().clone())
    else {
        return json::error_response(&UnifiedError::not_found("Import job not found"), debug);
    };
    if !can_view_company(company_id, job.company_id, can_manage) {
        return json::error_response(&UnifiedError::not_found("Import job not found"), debug);
    }
    let mut text = String::from("row,column,message\n");
    for error in &job.row_errors {
        text.push_str(&format!(
            "{},{},{}\n",
            error.row,
            escape_csv_field(error.column.as_deref().unwrap_or("")),
            escape_csv_field(&error.message),
        ));
    }
    HttpResponse::Ok()
        .content_type("text/csv; charset=utf-8")
        .insert_header((
            "Content-Disposition",
            format!(
                "attachment; filename=\"{}-import-{}-errors.csv\"",
                job.entity, job.id
            ),
        ))
        .body(text)
}

async fn confirm_import(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    id: web::Path<Uuid>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let id = id.into_inner();
    let result: UnifiedResult<ImportJob> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_IMPORT_EXPORT_IMPORT)?;
        let company_id = resolve_caller_company(&ctx, &req, &caller);
        let can_manage = check_capability(&ctx, &caller.role, PERM_IMPORT_EXPORT_MANAGE);
        let existing = import_jobs_store()
            .get(&id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Import job not found"))?;
        if !can_view_company(company_id, existing.company_id, can_manage) {
            return Err(UnifiedError::not_found("Import job not found"));
        }
        let mut job = import_jobs_store()
            .get_mut(&id)
            .ok_or_else(|| UnifiedError::not_found("Import job not found"))?;
        if job.status != ImportStatus::Validated.as_str() {
            return Err(UnifiedError::conflict(format!(
                "Cannot confirm an import in status \"{}\"; only a validated import can be confirmed",
                job.status
            )));
        }
        // Idempotency: within one job, a repeated external id stages
        // the first row and skips later duplicates.
        let mut seen_external_ids: Vec<String> = Vec::new();
        let mut results: Vec<RowResult> = Vec::new();
        for (index, row) in job.staged_rows.iter().enumerate() {
            let external_id = row
                .get("external_id")
                .and_then(Value::as_str)
                .map(str::to_string);
            if let Some(external_id) = &external_id {
                if seen_external_ids.contains(external_id) {
                    results.push(RowResult {
                        row: index + 1,
                        external_id: Some(external_id.clone()),
                        status: "skipped".to_string(),
                        message: Some("Duplicate external_id inside this file".to_string()),
                    });
                    continue;
                }
                seen_external_ids.push(external_id.clone());
            }
            results.push(RowResult {
                row: index + 1,
                external_id,
                status: "staged".to_string(),
                message: None,
            });
        }
        let staged_count = results
            .iter()
            .filter(|result| result.status == "staged")
            .count();
        job.results = results;
        job.status = ImportStatus::Staged.as_str().to_string();
        job.updated_at = Utc::now();
        let staged_rows = job.staged_rows.clone();
        let entity = job.entity.clone();
        let job_company = job.company_id;
        let view = job.clone();
        drop(job);
        let resource = format!("import:{id}");
        emit_audit(
            import_export_events::IMPORT_CONFIRMED,
            &caller.actor,
            job_company,
            &resource,
            serde_json::json!({
                "entity": entity,
                "staged_rows": staged_count,
                "total_rows": view.total_rows,
            }),
        );
        // Hand the staged rows to the owning module through the hook
        // bus. No owning module consumes them yet; see the wiring
        // snippet in the coordinator report.
        emit(
            import_export_events::IMPORT_COMPLETED,
            serde_json::json!({
                "import_id": id,
                "entity": entity,
                "staged_rows": staged_count,
                "rows": staged_rows,
                "actor": caller.actor,
                "actor_id": caller.actor,
                "resource": resource,
                "company_id": job_company.map(|id| id.to_string()),
            }),
        );
        Ok(view)
    })();
    json::respond(result, debug)
}

// ---------------------------------------------------------------------------
// Handlers: saved field-mapping templates.
// ---------------------------------------------------------------------------

async fn create_mapping(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    body: web::Json<CreateMappingRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<MappingTemplate> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_IMPORT_EXPORT_MANAGE)?;
        let company_id = resolve_caller_company(&ctx, &req, &caller);
        let input = body.into_inner();
        let name = require_text(&input.name, "name", 128)?;
        let spec = entity_spec(input.entity.trim()).ok_or_else(|| {
            UnifiedError::bad_request(format!(
                "Unknown entity \"{}\"; see GET /api/v1/import-export/entities",
                input.entity
            ))
        })?;
        if input.mapping.is_empty() {
            return Err(UnifiedError::bad_request("mapping must not be empty"));
        }
        validate_mapping(spec, &input.mapping)?;
        let now = Utc::now();
        let id = Uuid::now_v7();
        let template = MappingTemplate {
            id,
            name,
            entity: spec.entity.to_string(),
            mapping: input.mapping,
            actor: caller.actor.clone(),
            company_id,
            created_at: now,
            updated_at: now,
        };
        mappings_store().insert(id, template.clone());
        emit_audit(
            import_export_events::MAPPING_CREATED,
            &caller.actor,
            company_id,
            &format!("mapping:{id}"),
            serde_json::json!({ "entity": spec.entity, "name": template.name }),
        );
        Ok(template)
    })();
    match result {
        Ok(view) => json::success_response(view, 201),
        Err(error) => json::error_response(&error, debug),
    }
}

async fn list_mappings(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    params: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_IMPORT_EXPORT_READ)?;
        let company_id = resolve_caller_company(&ctx, &req, &caller);
        let can_manage = check_capability(&ctx, &caller.role, PERM_IMPORT_EXPORT_MANAGE);
        let mut items: Vec<MappingTemplate> = mappings_store()
            .iter()
            .map(|entry| entry.value().clone())
            .collect();
        items = scope_visible(items, |item| item.company_id, company_id, can_manage);
        if let Some(entity) = params.filters.get("entity") {
            items.retain(|item| item.entity == *entity);
        }
        items.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        if params.normalized().order == "asc" {
            items.reverse();
        }
        Ok(paginate(items, &params).to_json())
    })();
    json::respond(result, debug)
}

async fn get_mapping(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    id: web::Path<Uuid>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<MappingTemplate> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_IMPORT_EXPORT_READ)?;
        let company_id = resolve_caller_company(&ctx, &req, &caller);
        let can_manage = check_capability(&ctx, &caller.role, PERM_IMPORT_EXPORT_MANAGE);
        let template = mappings_store()
            .get(&id.into_inner())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Mapping template not found"))?;
        if !can_view_company(company_id, template.company_id, can_manage) {
            return Err(UnifiedError::not_found("Mapping template not found"));
        }
        Ok(template)
    })();
    json::respond(result, debug)
}

async fn delete_mapping(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    id: web::Path<Uuid>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let id = id.into_inner();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_IMPORT_EXPORT_MANAGE)?;
        let company_id = resolve_caller_company(&ctx, &req, &caller);
        let can_manage = check_capability(&ctx, &caller.role, PERM_IMPORT_EXPORT_MANAGE);
        let template = mappings_store()
            .get(&id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Mapping template not found"))?;
        if !can_view_company(company_id, template.company_id, can_manage) {
            return Err(UnifiedError::not_found("Mapping template not found"));
        }
        mappings_store().remove(&id);
        emit_audit(
            import_export_events::MAPPING_DELETED,
            &caller.actor,
            template.company_id,
            &format!("mapping:{id}"),
            serde_json::json!({ "entity": template.entity, "name": template.name }),
        );
        Ok(serde_json::json!({ "deleted": true, "id": id }))
    })();
    json::respond(result, debug)
}

// ---------------------------------------------------------------------------
// Handlers: scheduled exports.
// ---------------------------------------------------------------------------

async fn create_schedule(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    body: web::Json<CreateScheduleRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<ExportSchedule> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_IMPORT_EXPORT_MANAGE)?;
        let company_id = resolve_caller_company(&ctx, &req, &caller);
        let can_export_pii = check_capability(&ctx, &caller.role, PERM_IMPORT_EXPORT_EXPORT_PII);
        let config = settings();
        let input = body.into_inner();
        let name = require_text(&input.name, "name", 128)?;
        let spec = entity_spec(input.entity.trim()).ok_or_else(|| {
            UnifiedError::bad_request(format!(
                "Unknown entity \"{}\"; see GET /api/v1/import-export/entities",
                input.entity
            ))
        })?;
        let format = input.format.unwrap_or_else(|| "json".to_string());
        if !valid_format(&format, &config.allowed_formats) {
            return Err(UnifiedError::bad_request(format!(
                "format must be one of {}, got \"{format}\"",
                config.allowed_formats.join(", ")
            )));
        }
        let fields: Vec<String> = match &input.fields {
            Some(fields) => {
                validate_fields(spec, fields)?;
                fields.clone()
            }
            None => spec
                .columns
                .iter()
                .map(|column| column.name.to_string())
                .collect(),
        };
        let filters = input.filters.unwrap_or_default();
        for key in filters.keys() {
            if !spec.columns.iter().any(|column| column.name == key) {
                return Err(UnifiedError::bad_request(format!(
                    "Unknown filter \"{key}\" for entity \"{}\"",
                    spec.entity
                )));
            }
        }
        let schedule_text = input.schedule.trim().to_string();
        if !valid_schedule(&schedule_text) {
            return Err(UnifiedError::bad_request(
                "schedule must be an interval in seconds or five cron-like fields",
            ));
        }
        let redact_pii = input.redact_pii.unwrap_or(config.default_redact_pii);
        let redact_names = input.redact_names.unwrap_or(false);
        let enabled = input.enabled.unwrap_or(true);
        let now = Utc::now();
        let id = Uuid::now_v7();
        // A schedule that exports personal data unredacted is only
        // created by a caller who holds the PII capability now; the
        // flag is captured here and never widened by a run.
        let pii_allowed = can_export_pii && !redact_pii;
        let scheduling = if enabled {
            notify_scheduler(&ctx.hooks, id)
        } else {
            SchedulingOutcome {
                enqueued: false,
                message: "Manual-only schedule: the schedule is disabled.".to_string(),
            }
        };
        let schedule = ExportSchedule {
            id,
            name,
            entity: spec.entity.to_string(),
            format,
            fields,
            filters,
            redact_pii,
            redact_names,
            pii_allowed,
            schedule: schedule_text,
            enabled,
            company_id,
            actor: caller.actor.clone(),
            last_run_at: None,
            last_export_id: None,
            last_error: None,
            run_count: 0,
            scheduling,
            created_at: now,
            updated_at: now,
        };
        export_schedules_store().insert(id, schedule.clone());
        emit_audit(
            import_export_events::EXPORT_SCHEDULE_CREATED,
            &caller.actor,
            company_id,
            &format!("export_schedule:{id}"),
            serde_json::json!({
                "entity": spec.entity,
                "schedule": schedule.schedule,
                "enabled": enabled,
                "pii_redacted": redact_pii,
            }),
        );
        Ok(schedule)
    })();
    match result {
        Ok(view) => json::success_response(view, 201),
        Err(error) => json::error_response(&error, debug),
    }
}

async fn list_schedules(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    params: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_IMPORT_EXPORT_READ)?;
        let company_id = resolve_caller_company(&ctx, &req, &caller);
        let can_manage = check_capability(&ctx, &caller.role, PERM_IMPORT_EXPORT_MANAGE);
        let mut items: Vec<ExportSchedule> = export_schedules_store()
            .iter()
            .map(|entry| entry.value().clone())
            .collect();
        items = scope_visible(items, |item| item.company_id, company_id, can_manage);
        if let Some(entity) = params.filters.get("entity") {
            items.retain(|item| item.entity == *entity);
        }
        if let Some(enabled) = params.filters.get("enabled") {
            let wanted = enabled == "true";
            items.retain(|item| item.enabled == wanted);
        }
        items.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        if params.normalized().order == "asc" {
            items.reverse();
        }
        Ok(paginate(items, &params).to_json())
    })();
    json::respond(result, debug)
}

async fn get_schedule(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    id: web::Path<Uuid>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<ExportSchedule> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_IMPORT_EXPORT_READ)?;
        let company_id = resolve_caller_company(&ctx, &req, &caller);
        let can_manage = check_capability(&ctx, &caller.role, PERM_IMPORT_EXPORT_MANAGE);
        let schedule = export_schedules_store()
            .get(&id.into_inner())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Export schedule not found"))?;
        if !can_view_company(company_id, schedule.company_id, can_manage) {
            return Err(UnifiedError::not_found("Export schedule not found"));
        }
        Ok(schedule)
    })();
    json::respond(result, debug)
}

async fn update_schedule(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    id: web::Path<Uuid>,
    body: web::Json<UpdateScheduleRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let id = id.into_inner();
    let result: UnifiedResult<ExportSchedule> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_IMPORT_EXPORT_MANAGE)?;
        let company_id = resolve_caller_company(&ctx, &req, &caller);
        let can_manage = check_capability(&ctx, &caller.role, PERM_IMPORT_EXPORT_MANAGE);
        let can_export_pii = check_capability(&ctx, &caller.role, PERM_IMPORT_EXPORT_EXPORT_PII);
        let config = settings();
        let input = body.into_inner();
        let stored = export_schedules_store()
            .get(&id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Export schedule not found"))?;
        if !can_view_company(company_id, stored.company_id, can_manage) {
            return Err(UnifiedError::not_found("Export schedule not found"));
        }
        let spec = entity_spec(&stored.entity).ok_or_else(|| {
            UnifiedError::internal(format!(
                "Stored schedule references unknown entity \"{}\"",
                stored.entity
            ))
        })?;
        let mut updated = stored.clone();
        if let Some(name) = input.name {
            updated.name = require_text(&name, "name", 128)?;
        }
        if let Some(format) = input.format {
            if !valid_format(&format, &config.allowed_formats) {
                return Err(UnifiedError::bad_request(format!(
                    "format must be one of {}, got \"{format}\"",
                    config.allowed_formats.join(", ")
                )));
            }
            updated.format = format;
        }
        if let Some(fields) = input.fields {
            validate_fields(spec, &fields)?;
            updated.fields = fields;
        }
        if let Some(filters) = input.filters {
            for key in filters.keys() {
                if !spec.columns.iter().any(|column| column.name == key) {
                    return Err(UnifiedError::bad_request(format!(
                        "Unknown filter \"{key}\" for entity \"{}\"",
                        spec.entity
                    )));
                }
            }
            updated.filters = filters;
        }
        if let Some(redact_pii) = input.redact_pii {
            updated.redact_pii = redact_pii;
            // The PII flag is only widened by a caller who holds the
            // capability at this moment; turning redaction back on
            // always narrows it.
            if !redact_pii && can_export_pii {
                updated.pii_allowed = true;
            }
            if redact_pii {
                updated.pii_allowed = false;
            }
        }
        if let Some(redact_names) = input.redact_names {
            updated.redact_names = redact_names;
        }
        let mut reschedule = false;
        if let Some(schedule_text) = input.schedule {
            let schedule_text = schedule_text.trim().to_string();
            if !valid_schedule(&schedule_text) {
                return Err(UnifiedError::bad_request(
                    "schedule must be an interval in seconds or five cron-like fields",
                ));
            }
            if schedule_text != updated.schedule {
                reschedule = true;
            }
            updated.schedule = schedule_text;
        }
        if let Some(enabled) = input.enabled {
            if enabled != updated.enabled {
                reschedule = true;
            }
            updated.enabled = enabled;
        }
        if reschedule {
            updated.scheduling = if updated.enabled {
                notify_scheduler(&ctx.hooks, id)
            } else {
                SchedulingOutcome {
                    enqueued: false,
                    message: "Manual-only schedule: the schedule is disabled.".to_string(),
                }
            };
        }
        updated.updated_at = Utc::now();
        export_schedules_store().insert(id, updated.clone());
        emit_audit(
            import_export_events::EXPORT_SCHEDULE_UPDATED,
            &caller.actor,
            updated.company_id,
            &format!("export_schedule:{id}"),
            serde_json::json!({
                "entity": updated.entity,
                "schedule": updated.schedule,
                "enabled": updated.enabled,
            }),
        );
        Ok(updated)
    })();
    json::respond(result, debug)
}

async fn delete_schedule(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    id: web::Path<Uuid>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let id = id.into_inner();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_IMPORT_EXPORT_MANAGE)?;
        let company_id = resolve_caller_company(&ctx, &req, &caller);
        let can_manage = check_capability(&ctx, &caller.role, PERM_IMPORT_EXPORT_MANAGE);
        let schedule = export_schedules_store()
            .get(&id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Export schedule not found"))?;
        if !can_view_company(company_id, schedule.company_id, can_manage) {
            return Err(UnifiedError::not_found("Export schedule not found"));
        }
        export_schedules_store().remove(&id);
        emit_audit(
            import_export_events::EXPORT_SCHEDULE_DELETED,
            &caller.actor,
            schedule.company_id,
            &format!("export_schedule:{id}"),
            serde_json::json!({ "entity": schedule.entity, "name": schedule.name }),
        );
        Ok(serde_json::json!({ "deleted": true, "id": id }))
    })();
    json::respond(result, debug)
}

async fn run_schedule_now(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    id: web::Path<Uuid>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<ExportJob> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_IMPORT_EXPORT_EXPORT)?;
        let company_id = resolve_caller_company(&ctx, &req, &caller);
        let can_manage = check_capability(&ctx, &caller.role, PERM_IMPORT_EXPORT_MANAGE);
        let can_export_pii = check_capability(&ctx, &caller.role, PERM_IMPORT_EXPORT_EXPORT_PII);
        let schedule = export_schedules_store()
            .get(&id.into_inner())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Export schedule not found"))?;
        if !can_view_company(company_id, schedule.company_id, can_manage) {
            return Err(UnifiedError::not_found("Export schedule not found"));
        }
        // A run-now uses the caller's live PII capability when it is
        // wider than what was captured at creation time.
        let mut effective = schedule.clone();
        if can_export_pii && !schedule.redact_pii {
            effective.pii_allowed = true;
        }
        let spec = ExportSpec {
            entity: effective.entity.clone(),
            format: effective.format.clone(),
            fields: Some(effective.fields.clone()),
            filters: effective.filters.clone(),
            redact_pii: Some(effective.redact_pii),
            redact_names: Some(effective.redact_names),
            limit: None,
            offset: None,
        };
        let job = run_export(
            &caller.actor,
            schedule.company_id,
            effective.pii_allowed,
            &spec,
        )?;
        let now = Utc::now();
        if let Some(mut stored) = export_schedules_store().get_mut(&schedule.id) {
            stored.last_run_at = Some(now);
            stored.last_export_id = Some(job.id);
            stored.last_error = None;
            stored.run_count += 1;
            stored.updated_at = now;
        }
        emit_audit(
            import_export_events::EXPORT_SCHEDULE_RUN,
            &caller.actor,
            schedule.company_id,
            &format!("export_schedule:{}", schedule.id),
            serde_json::json!({
                "entity": schedule.entity,
                "export_id": job.id,
                "row_count": job.row_count,
                "via": "run-now",
            }),
        );
        Ok(job)
    })();
    match result {
        Ok(view) => json::success_response(view, 201),
        Err(error) => json::error_response(&error, debug),
    }
}

// ---------------------------------------------------------------------------
// Handlers: chunked import sessions.
// ---------------------------------------------------------------------------

async fn create_session(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    body: web::Json<CreateSessionRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<ImportSession> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_IMPORT_EXPORT_IMPORT)?;
        let company_id = resolve_caller_company(&ctx, &req, &caller);
        let can_manage = check_capability(&ctx, &caller.role, PERM_IMPORT_EXPORT_MANAGE);
        let config = settings();
        let input = body.into_inner();
        let spec = entity_spec(input.entity.trim()).ok_or_else(|| {
            UnifiedError::bad_request(format!(
                "Unknown entity \"{}\"; see GET /api/v1/import-export/entities",
                input.entity
            ))
        })?;
        let format = input.format.trim().to_lowercase();
        if !valid_format(&format, &config.allowed_formats) {
            return Err(UnifiedError::bad_request(format!(
                "format must be one of {}, got \"{format}\"",
                config.allowed_formats.join(", ")
            )));
        }
        if let Some(total_chunks) = input.total_chunks {
            if total_chunks == 0 {
                return Err(UnifiedError::bad_request(
                    "total_chunks must be at least 1 when provided",
                ));
            }
        }
        let mapping = resolve_mapping(
            spec.entity,
            input.mapping,
            input.mapping_id,
            company_id,
            can_manage,
        )?;
        validate_mapping(spec, &mapping)?;
        let now = Utc::now();
        let id = Uuid::now_v7();
        let session = ImportSession {
            id,
            entity: spec.entity.to_string(),
            format,
            status: "open".to_string(),
            total_chunks: input.total_chunks,
            received_chunks: Vec::new(),
            total_rows: 0,
            valid_rows: 0,
            invalid_rows: 0,
            row_errors: Vec::new(),
            mapping,
            actor: caller.actor.clone(),
            company_id,
            staged_rows: Vec::new(),
            created_at: now,
            updated_at: now,
        };
        import_sessions_store().insert(id, session.clone());
        Ok(session)
    })();
    match result {
        Ok(view) => json::success_response(view, 201),
        Err(error) => json::error_response(&error, debug),
    }
}

async fn list_sessions(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    params: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_IMPORT_EXPORT_READ)?;
        let company_id = resolve_caller_company(&ctx, &req, &caller);
        let can_manage = check_capability(&ctx, &caller.role, PERM_IMPORT_EXPORT_MANAGE);
        let mut items: Vec<ImportSession> = import_sessions_store()
            .iter()
            .map(|entry| entry.value().clone())
            .collect();
        items = scope_visible(items, |item| item.company_id, company_id, can_manage);
        if let Some(entity) = params.filters.get("entity") {
            items.retain(|item| item.entity == *entity);
        }
        if let Some(status) = params.filters.get("status") {
            items.retain(|item| item.status == *status);
        }
        items.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        if params.normalized().order == "asc" {
            items.reverse();
        }
        Ok(paginate(items, &params).to_json())
    })();
    json::respond(result, debug)
}

async fn get_session(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    id: web::Path<Uuid>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<ImportSession> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_IMPORT_EXPORT_READ)?;
        let company_id = resolve_caller_company(&ctx, &req, &caller);
        let can_manage = check_capability(&ctx, &caller.role, PERM_IMPORT_EXPORT_MANAGE);
        let session = import_sessions_store()
            .get(&id.into_inner())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Import session not found"))?;
        if !can_view_company(company_id, session.company_id, can_manage) {
            return Err(UnifiedError::not_found("Import session not found"));
        }
        Ok(session)
    })();
    json::respond(result, debug)
}

async fn append_chunk(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    id: web::Path<Uuid>,
    body: web::Json<AppendChunkRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let id = id.into_inner();
    let result: UnifiedResult<ImportSession> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_IMPORT_EXPORT_IMPORT)?;
        let company_id = resolve_caller_company(&ctx, &req, &caller);
        let can_manage = check_capability(&ctx, &caller.role, PERM_IMPORT_EXPORT_MANAGE);
        let config = settings();
        let input = body.into_inner();
        let mut session = import_sessions_store()
            .get_mut(&id)
            .ok_or_else(|| UnifiedError::not_found("Import session not found"))?;
        if !can_view_company(company_id, session.company_id, can_manage) {
            return Err(UnifiedError::not_found("Import session not found"));
        }
        if session.status != "open" {
            return Err(UnifiedError::conflict(format!(
                "Cannot append to a session in status \"{}\"",
                session.status
            )));
        }
        if let Some(total_chunks) = session.total_chunks {
            if input.chunk_index >= total_chunks {
                return Err(UnifiedError::bad_request(format!(
                    "chunk_index {} is out of range for total_chunks {total_chunks}",
                    input.chunk_index
                )));
            }
        }
        if session.received_chunks.contains(&input.chunk_index) {
            return Err(UnifiedError::conflict(format!(
                "Chunk {} was already received for this session",
                input.chunk_index
            )));
        }
        let spec = entity_spec(&session.entity).ok_or_else(|| {
            UnifiedError::internal(format!(
                "Stored session references unknown entity \"{}\"",
                session.entity
            ))
        })?;
        let text = decode_content(
            &input.content,
            input.encoding.as_deref(),
            config.max_chunk_bytes,
        )?;
        let raw_rows = content_to_raw_rows(spec, &session.format, &text, Some(&session.mapping))?;
        if raw_rows.len() > config.max_rows_per_file {
            return Err(UnifiedError::bad_request(format!(
                "Chunk has {} rows; the maximum per chunk is {}",
                raw_rows.len(),
                config.max_rows_per_file
            )));
        }
        if session.total_rows + raw_rows.len() > config.max_rows_per_session {
            return Err(UnifiedError::bad_request(format!(
                "Session would exceed the maximum of {} rows",
                config.max_rows_per_session
            )));
        }
        // Validate this chunk before accepting anything from it:
        // a chunk with invalid rows contributes its errors and its
        // valid rows, but nothing is written anywhere yet.
        let (valid_rows, mut chunk_errors) = validate_rows(spec, raw_rows, session.company_id);
        let chunk_total = valid_rows.len()
            + {
                let mut numbers: Vec<usize> =
                    chunk_errors.iter().map(|error| error.row).collect();
                numbers.sort_unstable();
                numbers.dedup();
                numbers.len()
            };
        // Row numbers in the error report are session-wide, not
        // chunk-local, so the report stays readable after completion.
        let row_offset = session.total_rows;
        for error in &mut chunk_errors {
            error.row += row_offset;
        }
        let mut invalid_row_numbers: Vec<usize> =
            chunk_errors.iter().map(|error| error.row).collect();
        invalid_row_numbers.sort_unstable();
        invalid_row_numbers.dedup();
        session.total_rows += chunk_total;
        session.valid_rows += valid_rows.len();
        session.invalid_rows += invalid_row_numbers.len();
        session.row_errors.append(&mut chunk_errors);
        session.staged_rows.extend(valid_rows);
        session.received_chunks.push(input.chunk_index);
        session.received_chunks.sort_unstable();
        session.updated_at = Utc::now();
        Ok(session.clone())
    })();
    json::respond(result, debug)
}

async fn complete_session(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    id: web::Path<Uuid>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let id = id.into_inner();
    let result: UnifiedResult<ImportJob> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_IMPORT_EXPORT_IMPORT)?;
        let company_id = resolve_caller_company(&ctx, &req, &caller);
        let can_manage = check_capability(&ctx, &caller.role, PERM_IMPORT_EXPORT_MANAGE);
        let mut session = import_sessions_store()
            .get_mut(&id)
            .ok_or_else(|| UnifiedError::not_found("Import session not found"))?;
        if !can_view_company(company_id, session.company_id, can_manage) {
            return Err(UnifiedError::not_found("Import session not found"));
        }
        if session.status != "open" {
            return Err(UnifiedError::conflict(format!(
                "Cannot complete a session in status \"{}\"",
                session.status
            )));
        }
        if session.received_chunks.is_empty() {
            return Err(UnifiedError::bad_request(
                "Session has no chunks yet; append at least one chunk first",
            ));
        }
        if let Some(total_chunks) = session.total_chunks {
            if session.received_chunks.len() != total_chunks {
                return Err(UnifiedError::bad_request(format!(
                    "Session declared {total_chunks} chunks but only {} were received",
                    session.received_chunks.len()
                )));
            }
        }
        let status = if session.invalid_rows == 0 {
            ImportStatus::Validated.as_str()
        } else {
            ImportStatus::Failed.as_str()
        };
        let now = Utc::now();
        let job_id = Uuid::now_v7();
        let job = ImportJob {
            id: job_id,
            entity: session.entity.clone(),
            format: session.format.clone(),
            status: status.to_string(),
            total_rows: session.total_rows,
            valid_rows: session.valid_rows,
            invalid_rows: session.invalid_rows,
            row_errors: session.row_errors.clone(),
            results: Vec::new(),
            actor: session.actor.clone(),
            company_id: session.company_id,
            staged_rows: session.staged_rows.clone(),
            created_at: now,
            updated_at: now,
        };
        session.status = status.to_string();
        session.updated_at = now;
        let session_status = session.status.clone();
        let session_entity = session.entity.clone();
        drop(session);
        let view = job.clone();
        import_jobs_store().insert(job_id, job);
        let resource = format!("import:{job_id}");
        emit_audit(
            import_export_events::IMPORT_SESSION_COMPLETED,
            &caller.actor,
            company_id,
            &resource,
            serde_json::json!({
                "entity": session_entity,
                "session_id": id,
                "session_status": session_status,
                "total_rows": view.total_rows,
                "valid_rows": view.valid_rows,
                "invalid_rows": view.invalid_rows,
            }),
        );
        emit_audit(
            import_export_events::IMPORT_CREATED,
            &caller.actor,
            company_id,
            &resource,
            serde_json::json!({
                "entity": session_entity,
                "format": view.format,
                "total_rows": view.total_rows,
                "valid_rows": view.valid_rows,
                "invalid_rows": view.invalid_rows,
            }),
        );
        Ok(view)
    })();
    match result {
        Ok(view) => json::success_response(view, 201),
        Err(error) => json::error_response(&error, debug),
    }
}

// ---------------------------------------------------------------------------
// Module definition.
// ---------------------------------------------------------------------------

pub struct ImportExportModule;

impl Module for ImportExportModule {
    fn id(&self) -> &'static str {
        "import_export"
    }

    fn name(&self) -> &'static str {
        "Import / Export"
    }

    fn description(&self) -> &'static str {
        "CSV, JSON, and XML import/export for users, software, products, orders, invoices, tasks, and licenses: dry-run validation with per-row errors, confirmed staging with idempotency by external id, saved field-mapping templates, chunked imports, scheduled exports, capability-checked and tenant-scoped jobs with PII redaction through the privacy module, and audit-logged lifecycle events."
    }

    fn requires(&self) -> &[&'static str] {
        &["users", "software", "store", "billing", "tasks", "licensing"]
    }

    fn requires_libraries(&self) -> &[&'static str] {
        &["serde_json", "base64"]
    }

    fn init(&self, ctx: &AppContext) -> UnifiedResult<()> {
        let _ = HOOK_BUS.set(Arc::clone(&ctx.hooks));
        Ok(())
    }

    fn register_hooks(&self, bus: &HookBus) {
        // Run a stored schedule when the scheduler dispatches
        // `export.run` (emitted as `export.run_requested`). The
        // scheduler has no `export.run` dispatch arm yet, so today
        // this handler is reached through `run-now` flows and through
        // any future scheduler arm; see the integration notes.
        bus.register(
            import_export_events::EXPORT_RUN_REQUESTED,
            "import_export",
            Arc::new(move |ctx: &mut HookContext| {
                let schedule_id = ctx
                    .get("export_schedule_id")
                    .or_else(|| ctx.get("job_id"))
                    .and_then(|value| value.as_str())
                    .unwrap_or("")
                    .to_string();
                let Ok(schedule_id) = Uuid::parse_str(&schedule_id) else {
                    ctx.add_result(serde_json::json!({
                        "error": "export.run_requested requires 'export_schedule_id'"
                    }));
                    return;
                };
                let Some(schedule) = export_schedules_store()
                    .get(&schedule_id)
                    .map(|entry| entry.value().clone())
                else {
                    ctx.add_result(serde_json::json!({
                        "error": "Export schedule not found"
                    }));
                    return;
                };
                if !schedule.enabled {
                    ctx.add_result(serde_json::json!({
                        "skipped": true,
                        "reason": "Export schedule is disabled"
                    }));
                    return;
                }
                match run_schedule(&schedule) {
                    Ok(job) => {
                        ctx.add_result(serde_json::json!({
                            "started": true,
                            "export_schedule_id": schedule_id,
                            "export_id": job.id,
                            "row_count": job.row_count,
                        }));
                    }
                    Err(error) => {
                        let now = Utc::now();
                        if let Some(mut stored) =
                            export_schedules_store().get_mut(&schedule_id)
                        {
                            stored.last_run_at = Some(now);
                            stored.last_error = Some(error.message.clone());
                            stored.updated_at = now;
                        }
                        ctx.add_result(serde_json::json!({ "error": error.message }));
                    }
                }
            }),
        );

        // Full-tenant export, requested by the tenant-destroy flow
        // through `export.requested` with `scope:
        // "full_tenant_export"`. The destroy flow's export-first
        // gate depends on this answer: it reads `export_id` (or
        // `export_job_id` / `job_id`) and then proves the export
        // through `export.lookup` before the schema drop. The
        // `scope` gate is also the recursion guard: the
        // data-provider query `run_export` emits carries no
        // `scope` key, so this handler never answers its own
        // emit. Other scopes are ignored — this module does not
        // turn arbitrary `export.requested` events into jobs.
        bus.register(
            import_export_events::EXPORT_REQUESTED,
            "import_export",
            Arc::new(move |ctx: &mut HookContext| {
                if ctx.get("scope").and_then(Value::as_str) != Some("full_tenant_export") {
                    return;
                }
                let entity = ctx
                    .get("entity")
                    .and_then(Value::as_str)
                    .unwrap_or("tenant")
                    .to_string();
                // A full-tenant export without a tenant id would
                // run unscoped (every tenant's rows), so the
                // request is refused instead of answered.
                let Some(tenant_id) = ctx
                    .get("tenant_id")
                    .or_else(|| ctx.get("company_id"))
                    .and_then(Value::as_str)
                    .and_then(|raw| Uuid::parse_str(raw).ok())
                else {
                    ctx.add_result(serde_json::json!({
                        "error": "export.requested with scope \"full_tenant_export\" requires 'tenant_id'"
                    }));
                    return;
                };
                let actor = ctx
                    .get("actor")
                    .and_then(Value::as_str)
                    .unwrap_or("system")
                    .to_string();
                let format = ctx
                    .get("format")
                    .and_then(Value::as_str)
                    .unwrap_or("json")
                    .to_string();
                // The destroy export must be restorable, so personal
                // data stays unredacted unless the requester
                // explicitly asks for redaction. Trust basis: the
                // destroy flow is already gated on
                // `tenants:destroy`, and the export file is the
                // only way the tenant's data survives the drop.
                let redact_pii = ctx
                    .get("redact_pii")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                let mut filters = HashMap::new();
                filters.insert("id".to_string(), tenant_id.to_string());
                let spec = ExportSpec {
                    entity,
                    format,
                    fields: None,
                    filters,
                    redact_pii: Some(redact_pii),
                    redact_names: None,
                    limit: None,
                    offset: None,
                };
                match run_export(&actor, Some(tenant_id), !redact_pii, &spec) {
                    Ok(job) => {
                        // A full-tenant export with no rows is not
                        // a restorable export: the tenant being
                        // destroyed exists, so zero rows means no
                        // owning module answered the data-provider
                        // query. Answer an error (the stored job
                        // stays inspectable) instead of a job id
                        // the destroy gate would accept as proof.
                        if job.data_provider == "none" || job.row_count == 0 {
                            ctx.add_result(serde_json::json!({
                                "error": format!(
                                    "Full-tenant export produced no rows for entity \"{}\": no owning module answered export.requested for it (data_provider: \"{}\")",
                                    job.entity, job.data_provider
                                ),
                            }));
                            return;
                        }
                        ctx.add_result(serde_json::json!({
                            "export_id": job.id,
                            "export_job_id": job.id,
                            "job_id": job.id,
                            "export": export_job_view(&job),
                        }));
                    }
                    Err(error) => {
                        ctx.add_result(serde_json::json!({ "error": error.message }));
                    }
                }
            }),
        );

        // Answer `export.lookup` (the tenant-destroy export-first
        // gate asks it for `{"export": {"status",
        // "download_url"}}`). Unknown job ids answer
        // `{"exists": false}` instead of staying silent, so the
        // requester can tell "no such export" from "no answerer".
        bus.register(
            import_export_events::EXPORT_LOOKUP,
            "import_export",
            Arc::new(move |ctx: &mut HookContext| {
                let raw = ctx
                    .get("export_id")
                    .or_else(|| ctx.get("export_job_id"))
                    .or_else(|| ctx.get("job_id"))
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string();
                let Ok(id) = Uuid::parse_str(&raw) else {
                    ctx.add_result(serde_json::json!({
                        "error": "export.lookup requires 'export_id'"
                    }));
                    return;
                };
                match export_jobs_store().get(&id).map(|entry| entry.value().clone()) {
                    Some(job) => {
                        ctx.add_result(serde_json::json!({
                            "exists": true,
                            "export": export_job_view(&job),
                        }));
                    }
                    None => {
                        ctx.add_result(serde_json::json!({
                            "exists": false,
                            "export_id": id,
                        }));
                    }
                }
            }),
        );
    }

    fn configure_routes(&self, cfg: &mut actix_web::web::ServiceConfig) {
        cfg.service(
            web::scope("/api/v1/import-export")
                .route("/entities", web::get().to(list_entities))
                .route("/settings", web::get().to(get_settings))
                .route("/settings", web::patch().to(update_settings))
                .route("/exports", web::get().to(list_exports))
                .route("/exports", web::post().to(create_export))
                .route("/exports/{id}", web::get().to(get_export))
                .route("/exports/{id}/download", web::get().to(download_export))
                .route("/imports", web::get().to(list_imports))
                .route("/imports", web::post().to(create_import))
                .route("/imports/{id}", web::get().to(get_import))
                .route("/imports/{id}/errors/download", web::get().to(download_import_errors))
                .route("/imports/{id}/confirm", web::post().to(confirm_import))
                .route("/mappings", web::get().to(list_mappings))
                .route("/mappings", web::post().to(create_mapping))
                .route("/mappings/{id}", web::get().to(get_mapping))
                .route("/mappings/{id}", web::delete().to(delete_mapping))
                .route("/export-schedules", web::get().to(list_schedules))
                .route("/export-schedules", web::post().to(create_schedule))
                .route("/export-schedules/{id}", web::get().to(get_schedule))
                .route("/export-schedules/{id}", web::patch().to(update_schedule))
                .route("/export-schedules/{id}", web::delete().to(delete_schedule))
                .route("/export-schedules/{id}/run-now", web::post().to(run_schedule_now))
                .route("/import-sessions", web::get().to(list_sessions))
                .route("/import-sessions", web::post().to(create_session))
                .route("/import-sessions/{id}", web::get().to(get_session))
                .route("/import-sessions/{id}/chunks", web::post().to(append_chunk))
                .route("/import-sessions/{id}/complete", web::post().to(complete_session)),
        );
    }

    fn route_info(&self) -> Vec<crate::core::modules::RouteInfo> {
        use crate::core::modules::RouteInfo;
        vec![
            RouteInfo { method: "GET", path: "/api/v1/import-export/entities", owner: "import_export", description: "Entity catalog with columns and types" },
            RouteInfo { method: "GET", path: "/api/v1/import-export/settings", owner: "import_export", description: "Effective import/export limits and defaults" },
            RouteInfo { method: "PATCH", path: "/api/v1/import-export/settings", owner: "import_export", description: "Update import/export limits and defaults (manage)" },
            RouteInfo { method: "GET", path: "/api/v1/import-export/exports", owner: "import_export", description: "List export jobs (paginated)" },
            RouteInfo { method: "POST", path: "/api/v1/import-export/exports", owner: "import_export", description: "Start an export job (JSON, CSV, or XML)" },
            RouteInfo { method: "GET", path: "/api/v1/import-export/exports/{id}", owner: "import_export", description: "Export job status" },
            RouteInfo { method: "GET", path: "/api/v1/import-export/exports/{id}/download", owner: "import_export", description: "Download the finished export file" },
            RouteInfo { method: "GET", path: "/api/v1/import-export/imports", owner: "import_export", description: "List import jobs (paginated)" },
            RouteInfo { method: "POST", path: "/api/v1/import-export/imports", owner: "import_export", description: "Submit a file for dry-run validation" },
            RouteInfo { method: "GET", path: "/api/v1/import-export/imports/{id}", owner: "import_export", description: "Import job with per-row results" },
            RouteInfo { method: "GET", path: "/api/v1/import-export/imports/{id}/errors/download", owner: "import_export", description: "Download the row-error report as CSV" },
            RouteInfo { method: "POST", path: "/api/v1/import-export/imports/{id}/confirm", owner: "import_export", description: "Confirm a validated import and stage its rows" },
            RouteInfo { method: "GET", path: "/api/v1/import-export/mappings", owner: "import_export", description: "List saved field-mapping templates (paginated)" },
            RouteInfo { method: "POST", path: "/api/v1/import-export/mappings", owner: "import_export", description: "Save a field-mapping template" },
            RouteInfo { method: "GET", path: "/api/v1/import-export/mappings/{id}", owner: "import_export", description: "One field-mapping template" },
            RouteInfo { method: "DELETE", path: "/api/v1/import-export/mappings/{id}", owner: "import_export", description: "Delete a field-mapping template" },
            RouteInfo { method: "GET", path: "/api/v1/import-export/export-schedules", owner: "import_export", description: "List scheduled exports (paginated)" },
            RouteInfo { method: "POST", path: "/api/v1/import-export/export-schedules", owner: "import_export", description: "Create a scheduled export" },
            RouteInfo { method: "GET", path: "/api/v1/import-export/export-schedules/{id}", owner: "import_export", description: "One scheduled export" },
            RouteInfo { method: "PATCH", path: "/api/v1/import-export/export-schedules/{id}", owner: "import_export", description: "Update a scheduled export" },
            RouteInfo { method: "DELETE", path: "/api/v1/import-export/export-schedules/{id}", owner: "import_export", description: "Delete a scheduled export" },
            RouteInfo { method: "POST", path: "/api/v1/import-export/export-schedules/{id}/run-now", owner: "import_export", description: "Run a scheduled export now" },
            RouteInfo { method: "GET", path: "/api/v1/import-export/import-sessions", owner: "import_export", description: "List chunked import sessions (paginated)" },
            RouteInfo { method: "POST", path: "/api/v1/import-export/import-sessions", owner: "import_export", description: "Open a chunked import session" },
            RouteInfo { method: "GET", path: "/api/v1/import-export/import-sessions/{id}", owner: "import_export", description: "One chunked import session" },
            RouteInfo { method: "POST", path: "/api/v1/import-export/import-sessions/{id}/chunks", owner: "import_export", description: "Append one validated chunk to a session" },
            RouteInfo { method: "POST", path: "/api/v1/import-export/import-sessions/{id}/complete", owner: "import_export", description: "Finalize a session into an import job" },
        ]
    }
}

pub fn register(registry: &ModuleRegistry) {
    registry.register(Arc::new(ImportExportModule));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn schedule_validator_accepts_intervals_and_cron() {
        assert!(valid_schedule("3600"));
        assert!(valid_schedule("0 2 * * *"));
        assert!(valid_schedule("*/15 * * * *"));
        assert!(!valid_schedule(""));
        assert!(!valid_schedule("daily"));
        assert!(!valid_schedule("0 2 * *"));
        assert!(!valid_schedule("*/0 * * * *"));
    }

    #[test]
    fn xml_escaping_round_trips() {
        let escaped = escape_xml_text("a & b <c> \"d\" 'e'");
        assert_eq!(escaped, "a &amp; b &lt;c&gt; &quot;d&quot; &apos;e&apos;");
        assert_eq!(unescape_xml_text(&escaped), "a & b <c> \"d\" 'e'");
        assert_eq!(unescape_xml_text("&#65;&#x42;"), "AB");
    }

    #[test]
    fn xml_document_parses_flat_rows() {
        let root = parse_xml_document(
            "<rows><row><email>jane@example.com</email><name>Jane</name></row>\
             <row><email>bob@example.com</email><name>Bob</name></row></rows>",
        )
        .expect("flat rows parse");
        assert_eq!(root.name, "rows");
        assert_eq!(root.children.len(), 2);
        assert_eq!(root.children[0].children[0].text, "jane@example.com");
    }

    #[test]
    fn xml_document_rejects_doctype() {
        let error = parse_xml_document(
            "<!DOCTYPE foo [<!ENTITY xxe SYSTEM \"file:///etc/passwd\">]><rows/>",
        )
        .expect_err("doctype must be rejected");
        assert!(error.contains("DOCTYPE"));
    }

    #[test]
    fn xml_rows_map_to_raw_rows() {
        let spec = entity_spec("users").expect("users spec exists");
        let rows = content_to_raw_rows(
            spec,
            "xml",
            "<export><row><email>jane@example.com</email><name>Jane</name></row></export>",
            None,
        )
        .expect("xml import maps");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].get("email").map(String::as_str), Some("jane@example.com"));
    }

    #[test]
    fn company_mismatch_fails_that_row_only() {
        let spec = entity_spec("users").expect("users spec exists");
        let company = Uuid::now_v7();
        let other = Uuid::now_v7();
        let mut row = HashMap::new();
        row.insert("email".to_string(), "jane@example.com".to_string());
        row.insert("name".to_string(), "Jane".to_string());
        row.insert("company_id".to_string(), other.to_string());
        let (valid, errors) = validate_rows(spec, vec![row], Some(company));
        assert!(valid.is_empty());
        assert_eq!(errors.len(), 1);
        assert!(errors[0].message.contains("company_id"));
    }
}
