// Import / Export module -- CSV, JSON, and XML data exchange.
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
// payload sets `redact_pii: true` -- the destroy export must stay
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
// Tenant scoping: the caller's company is resolved server-side
// only, through the `user.company_requested` hook (same shape as
// the AI module): the users module looks the company up from the
// authenticated user id in its own store/database and answers with
// `company_id`. The `X-Company-Id` header is never read -- it is
// client-asserted, and trusting it let any caller holding the
// import/export permission claim any company id (or none) and read
// another tenant's data. Resolution fails closed: a caller with no
// server-side-resolved company is refused with 403 unless they hold
// the manage capability, in which case no company means platform
// scope. Every job, mapping, schedule, and session records the
// actor and the company it belongs to. List endpoints only show the
// caller's own company unless the caller holds the manage
// capability, and a single-object read of another company's record
// answers 404, never 403. Export rows contributed by a hook that
// carry a `company_id` different from the caller's company are
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
// schedule string (five fields, or an interval in seconds -- the same
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


// ---------------------------------------------------------------------------
// Configurable defaults.
// ---------------------------------------------------------------------------

pub mod ai_capabilities;
pub mod types;
pub mod events;
mod auth;
pub mod entities;
mod store;
pub mod jobs;
mod formats;
mod validation;
mod export;
mod handlers;
mod handlers_schedules;
pub mod module;

pub use self::ai_capabilities::AI_CAPABILITIES;
pub use self::types::*;
pub use self::events::*;
pub use self::auth::*;
pub use self::entities::*;
pub use self::store::*;
pub use self::jobs::*;
pub use self::formats::*;
pub use self::validation::*;
pub use self::export::*;
pub use self::handlers::*;
pub use self::handlers_schedules::*;
pub use self::module::*;
