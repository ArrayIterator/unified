// ESG module - Environmental, Social, and Governance
// sustainability reporting: admin-defined metric definitions
// and reporting-framework definitions as data, reporting
// periods with open / closed status, metric values per period
// per scope (company / site reference ids) with source
// notes, document references, and full value history,
// per-metric targets with direction and deadline plus exact
// progress math in basis points, period summaries with JSON
// and CSV export, an append-only audit trail, and an AI
// capability that summarizes a period from its stored facts.
//
// Routes live under `/api/v1/esg`:
//
//   Frameworks:
//     GET/POST /frameworks - list / create.
//     GET/PATCH/DELETE /frameworks/{framework_id}.
//   Metrics:
//     GET/POST /metrics - list / create.
//     GET/PATCH/DELETE /metrics/{metric_id}.
//   Periods:
//     GET/POST /periods - list / create.
//     GET/PATCH/DELETE /periods/{period_id}.
//     POST /periods/{period_id}/close, /reopen.
//     GET  /periods/{period_id}/summary.
//     GET  /periods/{period_id}/export.json, /export.csv.
//   Entries:
//     GET/POST /entries - list / record (upsert per
//       metric, period, scope).
//     GET/PATCH/DELETE /entries/{entry_id}.
//     GET  /entries/{entry_id}/history.
//   Targets:
//     GET/POST /targets - list / set (replace per metric,
//       period, scope).
//     GET/PATCH/DELETE /targets/{target_id}.
//   Audit:
//     GET  /audit - list the audit trail.
//   AI:
//     POST /ai/{capability} - one registered capability
//       through the AI module's gated runner.
//
// HONESTY - READ THIS FIRST. What is real in this module:
// in-memory framework, metric, period, entry, history,
// target, and audit management; metric definitions as data
// (category, unit, data type, frequency, framework tags are
// all admin-entered, and frameworks themselves are data
// rows); periods with validated date ranges and an explicit
// close / reopen freeze; entries with value normalization
// per metric data type (numbers as canonical decimal
// strings, booleans as `true` / `false`), one entry per
// (metric, period, scope) with full value history and audit
// rows; targets with increase / decrease direction and exact
// attainment math in basis points on an integer-mantissa
// decimal type; period summaries computed from stored rows
// (metrics by category, completion, target attainment,
// average attainment capped at 100% per metric); JSON and
// CSV export of a period; tenant scoping on every row with
// cross-tenant access reading as 404, never 403; hook events
// for metric definition, entry record / update, target set /
// achieved, and period close; `user.deleted` anonymization
// that keeps reporting history. What is NOT built here:
// durable storage until the migration runs (the DashMap
// stores are authoritative; migration
// `2026-11-22-000119_esg` is the persistence target);
// anything from the compliance module - ESG is
// sustainability metrics, not regulatory compliance, and
// this module does not duplicate compliance features; live
// verification of scope ids against the company module
// (scopes are plain reference ids - the company module emits
// no company.created / company.deleted lifecycle events, so
// there is nothing to listen to, and this module never
// assumes another module's tables); deletion of a framework,
// metric, or period never deletes the rows that reference
// it, by design.
//
// Actor identity: the caller is the server-verified caller from
// `unified_core::caller::require_caller` (user id plus
// server-resolved role, both required for capability checks),
// exactly like the sign module. Capability checks use the
// `role.check` hook and fail closed (no answer means not allowed).
// Handler signatures take `web::Data<Arc<AppContext>>` because
// the server registers `Arc<AppContext>` (see
// `src/http/server.rs`: `run_server(ctx: Arc<AppContext>)`);
// `web::Data<AppContext>` would fail to extract.
//
// Events emitted on the hook bus:
//   - `esg.metric.defined` (metric and framework creation)
//   - `esg.entry.recorded`, `esg.entry.updated`
//   - `esg.target.set`, `esg.target.achieved` (on crossing
//     to 100% attainment or matching a boolean target)
//   - `esg.period.closed`
//   - `notification.requested` (helper available; no handler
//     sends one today - delivery belongs to the notifications
//     module when a future flow needs it).
// Events this module listens to:
//   - `user.deleted` - anonymizes the user's frameworks,
//     metrics, periods, entries, history, targets, and audit
//     rows to `deleted-user`; rows are never deleted.
//   - No company events: the company module emits no
//     lifecycle events (verified from source - it only
//     answers `company.exists` and `company.create_requested`
//     requests), so scopes stay plain reference ids.

pub mod ai_capabilities;
mod handlers_ai;
mod handlers_entries;
mod handlers_frameworks;
mod handlers_metrics;
mod handlers_periods;
mod handlers_reports;
mod handlers_targets;
mod helpers;
mod hooks;
mod module;
pub mod state;
pub mod types;

pub use self::ai_capabilities::AI_CAPABILITIES;
pub use self::module::EsgModule;
pub use self::module::register;
pub use self::state::EsgState;
pub use self::types::{
    AuditRecord, Category, CategorySummary, CreateEntryRequest, CreateFrameworkRequest,
    CreateMetricRequest, CreatePeriodRequest, CreateTargetRequest, Decimal, Entry, EntryHistory,
    Framework, Metric, MetricDataType, MetricProgress, Period, PeriodStatus, PeriodSummary, Target,
    TargetDirection, UpdateEntryRequest, UpdateFrameworkRequest, UpdateMetricRequest,
    UpdatePeriodRequest, UpdateTargetRequest, attainment_bps, attainment_label,
};
