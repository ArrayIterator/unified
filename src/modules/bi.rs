// Business Intelligence + AI Insights module -- custom report builder,
// report execution over source-module data, scheduled delivery,
// transparent forecasting (moving average + linear trend), churn
// signals, and KPI dashboards.
//
// Routes live under `/api/v1/bi`:
//
//   Reports:
//     GET/POST /reports -- list saved reports / create a report.
//     GET/PATCH/DELETE /reports/{id}.
//     POST /reports/{id}/run -- execute a saved report (rows + totals).
//     GET  /reports/{id}/export?format=csv|json -- export the latest
//          run (max 10,000 rows).
//   Scheduled delivery:
//     GET/POST /reports/{id}/schedules -- list / create a schedule.
//     GET/DELETE /reports/{id}/schedules/{schedule_id}.
//     POST /reports/{id}/schedules/{schedule_id}/run-now -- run and
//          deliver now.
//   Forecasting:
//     GET  /forecast/revenue?source=billing&currency=USD&horizon_days=30
//     GET  /forecast/cashflow?currency=USD&horizon_days=30
//     GET  /forecast/churn?inactive_days=30
//   KPI dashboards:
//     GET/POST /kpi-dashboards -- list / create a KPI dashboard.
//     GET/PATCH/DELETE /kpi-dashboards/{id}.
//     GET  /kpi-dashboards/{id}/compute -- compute every KPI on read.
//
// HONESTY -- READ THIS FIRST. What is real in this module:
// in-memory saved reports, schedules, and KPI dashboards; report
// execution that aggregates a local mirror of source-module activity
// plus any rows a source module contributes through its
// `{source}.data.requested` read hook; CSV/JSON export capped at
// 10,000 rows; revenue and cash-flow forecasts built only from
// moving averages plus a least-squares linear trend, with the method,
// the inputs, and the horizon returned in every response; churn
// signals derived from the last recorded activity per customer and
// from suspended / revoked licenses and refunded orders; KPI values
// computed on read from the same data. What is NOT built here: no
// direct database access to other modules' tables (the DashMap
// mirror plus read hooks are the only data paths, by design); no
// source module currently answers `{source}.data.requested`, so
// today the mirror -- fed by the events listed below -- is the live
// data path and a source with no mirrored rows answers with an
// explicit "insufficient data" error instead of a fabricated
// number; no black-box scores anywhere (every forecast names its
// method and inputs); no actual email / push delivery (the
// notifications module owns delivery -- this module only emits
// `notification.requested` and `bi.report.delivered`); and the
// `bi.report.deliver` scheduler kind is NOT in the scheduler's
// `KNOWN_KINDS` list yet, so today the scheduler answers
// `{"queued": false, "error": "Unknown job kind: ..."}` and the
// schedule row records `scheduler_status: "declined"` instead of
// pretending it was scheduled (the follow-up is to add
// `"bi.report.deliver"` to `KNOWN_KINDS` in `scheduler.rs` and arm
// the dispatch so the job emits `bi.report.deliver_requested` with
// the schedule payload -- see the integrator note in the module
// report).
//
// Public-access note: this module has NO public routes. Every route
// requires the `X-User-Id` / `X-Role` headers and a capability
// check (see the actor-identity note below). Do NOT treat any route
// here as public.
//
// Actor identity: until the auth module provides an extractor, the
// caller is identified by the `X-User-Id` header (required) and
// `X-Role` carries the role slug the capability check runs
// against, exactly like the ai and events modules. Capability
// checks use the `role.check` hook and fail closed (no answer means
// not allowed).
//
// Events emitted on the hook bus:
//   - `bi.report.created`, `bi.report.updated`, `bi.report.deleted`
//   - `bi.report.delivered`
//   - `bi.forecast.generated`
//   - `notification.requested` (report delivery) with the
//     notifications module's payload shape: `{"kind", "user_id",
//     "title", "body", "module": "bi", "link", "priority",
//     "channels"}`.
//   - `scheduler.enqueue` with `{"kind": "bi.report.deliver",
//     "payload": {"report_id", "schedule_id", ...}}` when a
//     schedule is created.
// Events this module listens to (feeding the source mirror):
//   - `invoice.payment.recorded`, `invoice.paid` -- billing
//     payments (revenue rows).
//   - `payment.captured` -- captured payments (revenue rows).
//   - `store.order.completed`, `order.paid` -- completed store
//     orders (revenue rows).
//   - `store.order.refunded` -- refunded orders (negative rows for
//     the cash-flow projection).
//   - `ticket.created`, `ticket.updated` -- ticket activity rows.
//   - `license.issued`, `license.activated`, `license.suspended`,
//     `license.revoked` -- license activity rows (suspended /
//     revoked feed the churn signals).
//   - `project.created`, `project.task.completed`, `task.completed`,
//     `task.created` -- project / task activity rows.
// Read hooks this module emits (answered by source modules that
// choose to expose their in-memory stores; none do today):
//   - `{source}.data.requested` with `{"source", "company_id",
//     "from", "to", "limit"}`; a handler answers by adding a
//     result `{"rows": [...]}` where each row is an object with
//     optional `amount`, `currency`, `occurred_at`, `customer_id`,
//     `kind`, and `ref_id` keys.

pub mod ai_capabilities;
pub mod types;
pub mod state;
mod validation;
mod engine;
mod events;
mod handlers;
pub mod module;

pub use self::ai_capabilities::AI_CAPABILITIES;
pub use self::types::*;
pub use self::state::*;
pub use self::validation::*;
pub use self::engine::*;
pub use self::events::*;
pub use self::handlers::*;
pub use self::module::*;
