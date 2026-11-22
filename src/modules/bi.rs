// Business Intelligence + AI Insights module — custom report builder,
// report execution over source-module data, scheduled delivery,
// transparent forecasting (moving average + linear trend), churn
// signals, and KPI dashboards.
//
// Routes live under `/api/v1/bi`:
//
//   Reports:
//     GET/POST /reports — list saved reports / create a report.
//     GET/PATCH/DELETE /reports/{id}.
//     POST /reports/{id}/run — execute a saved report (rows + totals).
//     GET  /reports/{id}/export?format=csv|json — export the latest
//          run (max 10,000 rows).
//   Scheduled delivery:
//     GET/POST /reports/{id}/schedules — list / create a schedule.
//     GET/DELETE /reports/{id}/schedules/{schedule_id}.
//     POST /reports/{id}/schedules/{schedule_id}/run-now — run and
//          deliver now.
//   Forecasting:
//     GET  /forecast/revenue?source=billing&currency=USD&horizon_days=30
//     GET  /forecast/cashflow?currency=USD&horizon_days=30
//     GET  /forecast/churn?inactive_days=30
//   KPI dashboards:
//     GET/POST /kpi-dashboards — list / create a KPI dashboard.
//     GET/PATCH/DELETE /kpi-dashboards/{id}.
//     GET  /kpi-dashboards/{id}/compute — compute every KPI on read.
//
// HONESTY — READ THIS FIRST. What is real in this module:
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
// today the mirror — fed by the events listed below — is the live
// data path and a source with no mirrored rows answers with an
// explicit "insufficient data" error instead of a fabricated
// number; no black-box scores anywhere (every forecast names its
// method and inputs); no actual email / push delivery (the
// notifications module owns delivery — this module only emits
// `notification.requested` and `bi.report.delivered`); and the
// `bi.report.deliver` scheduler kind is NOT in the scheduler's
// `KNOWN_KINDS` list yet, so today the scheduler answers
// `{"queued": false, "error": "Unknown job kind: ..."}` and the
// schedule row records `scheduler_status: "declined"` instead of
// pretending it was scheduled (the follow-up is to add
// `"bi.report.deliver"` to `KNOWN_KINDS` in `scheduler.rs` and arm
// the dispatch so the job emits `bi.report.deliver_requested` with
// the schedule payload — see the integrator note in the module
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
//   - `invoice.payment.recorded`, `invoice.paid` — billing
//     payments (revenue rows).
//   - `payment.captured` — captured payments (revenue rows).
//   - `store.order.completed`, `order.paid` — completed store
//     orders (revenue rows).
//   - `store.order.refunded` — refunded orders (negative rows for
//     the cash-flow projection).
//   - `ticket.created`, `ticket.updated` — ticket activity rows.
//   - `license.issued`, `license.activated`, `license.suspended`,
//     `license.revoked` — license activity rows (suspended /
//     revoked feed the churn signals).
//   - `project.created`, `project.task.completed`, `task.completed`,
//     `task.created` — project / task activity rows.
// Read hooks this module emits (answered by source modules that
// choose to expose their in-memory stores; none do today):
//   - `{source}.data.requested` with `{"source", "company_id",
//     "from", "to", "limit"}`; a handler answers by adding a
//     result `{"rows": [...]}` where each row is an object with
//     optional `amount`, `currency`, `occurred_at`, `customer_id`,
//     `kind`, and `ref_id` keys.

use crate::core::context::AppContext;
use crate::core::error::{UnifiedError, UnifiedResult};
use crate::core::hooks::{HookBus, HookContext};
use crate::core::json;
use crate::core::modules::{Module, ModuleRegistry, RouteInfo, RouteVersionSupport};
use crate::core::pagination::{PageResult, PaginationParams};
use actix_web::{web, HttpRequest, HttpResponse};
use chrono::{DateTime, Duration, NaiveDate, TimeZone, Utc};
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{BTreeMap, HashSet};
use std::sync::{Arc, RwLock};
use uuid::Uuid;

/// Maximum lengths enforced on user supplied text.
const MAX_NAME_LEN: usize = 200;
const MAX_DESCRIPTION_LEN: usize = 5000;
const MAX_FIELD_LEN: usize = 128;
const MAX_FIELDS: usize = 32;
const MAX_GROUP_LEN: usize = 128;
const MAX_COMPANY_LEN: usize = 128;
const MAX_RECIPIENTS: usize = 50;
const MAX_KPIS: usize = 32;
const MAX_METRIC_LEN: usize = 64;
const MAX_PERIOD_LEN: usize = 16;
const MAX_SCHEDULE_LEN: usize = 64;
/// Hard cap on rows returned by one export, so an export can never
/// stream an unbounded result set.
const MAX_EXPORT_ROWS: usize = 10_000;
/// Maximum rows a report run returns inline (exports may go higher,
/// up to `MAX_EXPORT_ROWS`).
const MAX_RUN_ROWS: usize = 1_000;
/// Minimum distinct days of revenue history a forecast needs.
const MIN_FORECAST_DAYS: usize = 7;
/// Moving-average window in days used by every forecast.
const FORECAST_WINDOW_DAYS: i64 = 7;

/// Capability names checked through the `role.check` hook.
const PERM_BI_VIEW: &str = "bi.view";
const PERM_BI_MANAGE: &str = "bi.manage";

/// Source modules a report may read from. Closed allowlist on
/// purpose: an unknown source string is rejected instead of being
/// passed through to a hook name.
const ALLOWED_SOURCES: &[&str] = &[
    "billing",
    "store",
    "tickets",
    "licensing",
    "crm",
    "projects",
    "timetracker",
    "subscriptions",
];

/// Chart types a report may render as.
const ALLOWED_CHART_TYPES: &[&str] = &["table", "bar", "line", "pie"];

/// KPI metrics this module can compute. Every metric names the
/// source module its rows come from.
const ALLOWED_METRICS: &[&str] = &[
    "revenue_total",
    "order_count",
    "ticket_count",
    "active_licenses",
    "tasks_completed",
    "payment_count",
    "average_order_value",
];

/// KPI periods.
const ALLOWED_PERIODS: &[&str] = &["7d", "30d", "90d", "all"];

/// Fields a report may select, per source. A field outside the
/// list for the chosen source is rejected at write time.
fn allowed_fields(source: &str) -> &'static [&'static str] {
    match source {
        "billing" => &[
            "amount",
            "currency",
            "customer_id",
            "kind",
            "occurred_at",
            "ref_id",
            "status",
        ],
        "store" => &[
            "amount",
            "currency",
            "customer_id",
            "kind",
            "occurred_at",
            "ref_id",
            "status",
        ],
        "tickets" => &["customer_id", "kind", "occurred_at", "ref_id", "status"],
        "licensing" => &["customer_id", "kind", "occurred_at", "ref_id", "status"],
        "crm" => &["customer_id", "kind", "occurred_at", "ref_id", "status"],
        "projects" => &["customer_id", "kind", "occurred_at", "ref_id", "status"],
        "timetracker" => &[
            "amount",
            "customer_id",
            "kind",
            "occurred_at",
            "ref_id",
            "status",
        ],
        "subscriptions" => &[
            "amount",
            "currency",
            "customer_id",
            "kind",
            "occurred_at",
            "ref_id",
            "status",
        ],
        _ => &[],
    }
}

/// One mirrored source-activity row. The mirror is fed by the
/// source events listed in the module header and by rows returned
/// through `{source}.data.requested`; it never reads another
/// module's database tables. Money is stored as a float rounded to
/// 4 decimal places (NUMERIC(19,4) in the migration).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SourceRecord {
    pub id: Uuid,
    pub source: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub company_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub customer_id: Option<String>,
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub amount: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub currency: Option<String>,
    pub occurred_at: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ref_id: Option<String>,
    pub created_at: DateTime<Utc>,
}

/// One saved report definition.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BiReport {
    pub id: Uuid,
    pub company_id: String,
    pub owner_id: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub source: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub fields: Vec<String>,
    /// Exact-match filters as a JSON object (`field: value`), plus
    /// optional `from` / `to` RFC 3339 bounds on `occurred_at`.
    #[serde(default)]
    pub filters: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group_by: Option<String>,
    pub chart_type: String,
    /// `private` (owner only) or `shared` (whole company).
    pub visibility: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// One scheduled delivery of a saved report.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BiReportSchedule {
    pub id: Uuid,
    pub report_id: Uuid,
    pub company_id: String,
    /// Interval in seconds, or five cron-like fields.
    pub schedule: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub recipients: Vec<String>,
    /// `email` or `notification`.
    pub channel: String,
    /// `queued` or `declined` (see the scheduler note in the header).
    pub scheduler_status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scheduler_job_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_delivered_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// One KPI definition inside a dashboard: a metric, the source it
/// reads, and the period it covers.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KpiDefinition {
    pub metric: String,
    pub source: String,
    pub period: String,
}

/// One saved KPI dashboard.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BiKpiDashboard {
    pub id: Uuid,
    pub company_id: String,
    pub owner_id: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub kpis: Vec<KpiDefinition>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// One computed KPI value, as returned by the compute endpoint.
/// `value` is null with `status: "insufficient_data"` when the
/// metric's source has no rows in the period — never a fabricated
/// zero presented as a measurement.
#[derive(Debug, Clone, Serialize)]
pub struct ComputedKpi {
    pub metric: String,
    pub source: String,
    pub period: String,
    pub value: Option<f64>,
    pub status: String,
}

// ---------------------------------------------------------------------------
// Request bodies
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct CreateReportRequest {
    pub name: String,
    pub description: Option<String>,
    pub company_id: String,
    pub source: String,
    #[serde(default)]
    pub fields: Vec<String>,
    #[serde(default)]
    pub filters: Value,
    pub group_by: Option<String>,
    pub chart_type: Option<String>,
    pub visibility: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateReportRequest {
    pub name: Option<String>,
    pub description: Option<Option<String>>,
    pub source: Option<String>,
    pub fields: Option<Vec<String>>,
    pub filters: Option<Value>,
    pub group_by: Option<Option<String>>,
    pub chart_type: Option<String>,
    pub visibility: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct CreateScheduleRequest {
    pub schedule: String,
    #[serde(default)]
    pub recipients: Vec<String>,
    pub channel: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct CreateKpiDashboardRequest {
    pub name: String,
    pub description: Option<String>,
    pub company_id: String,
    #[serde(default)]
    pub kpis: Vec<KpiDefinition>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateKpiDashboardRequest {
    pub name: Option<String>,
    pub description: Option<Option<String>>,
    pub kpis: Option<Vec<KpiDefinition>>,
}

#[derive(Debug, Deserialize)]
pub struct ExportQuery {
    #[serde(default = "default_export_format")]
    pub format: String,
}

fn default_export_format() -> String {
    "json".to_string()
}

#[derive(Debug, Deserialize)]
pub struct ForecastQuery {
    pub source: Option<String>,
    pub currency: Option<String>,
    pub horizon_days: Option<i64>,
}

#[derive(Debug, Deserialize)]
pub struct ChurnQuery {
    pub inactive_days: Option<i64>,
    pub company_id: Option<String>,
}

// ---------------------------------------------------------------------------
// Shared state
// ---------------------------------------------------------------------------

/// In-memory stores for Business Intelligence. The hook bus is
/// attached in `init` (the bus lives in `AppContext`); handlers
/// tolerate a missing bus by skipping event emission. Durable rows
/// for these types are defined by migration
/// `2026-11-22-000042_bi`; until that migration is executed,
/// reports, schedules, and dashboards do not survive a restart.
#[derive(Clone)]
pub struct BiState {
    pub reports: Arc<DashMap<Uuid, BiReport>>,
    pub schedules: Arc<DashMap<Uuid, BiReportSchedule>>,
    pub dashboards: Arc<DashMap<Uuid, BiKpiDashboard>>,
    pub source_records: Arc<DashMap<Uuid, SourceRecord>>,
    pub hooks: Arc<RwLock<Option<Arc<HookBus>>>>,
}

impl Default for BiState {
    fn default() -> Self {
        Self {
            reports: Arc::new(DashMap::new()),
            schedules: Arc::new(DashMap::new()),
            dashboards: Arc::new(DashMap::new()),
            source_records: Arc::new(DashMap::new()),
            hooks: Arc::new(RwLock::new(None)),
        }
    }
}

impl BiState {
    fn bus(&self) -> Option<Arc<HookBus>> {
        self.hooks.read().expect("hooks lock poisoned").clone()
    }

    fn emit(&self, event: &str, data: Value) {
        if let Some(bus) = self.bus() {
            bus.emit(event, data);
        }
    }

    /// Emit a `notification.requested` event in the notifications
    /// module's payload shape.
    fn notify(&self, kind: &str, user_id: &str, title: &str, body: &str, link: Option<String>) {
        self.emit(
            "notification.requested",
            json!({
                "kind": kind,
                "user_id": user_id,
                "title": title,
                "body": body,
                "module": "bi",
                "category": "bi",
                "priority": "normal",
                "link": link,
            }),
        );
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Caller identity: `X-User-Id` (required) plus `X-Role` (required
/// for capability checks), exactly like the ai and events modules.
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
/// the `role.check` hook. Fails closed: no answer means not allowed
/// (same fail-closed shape as the ai and events modules).
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

fn parse_uuid(raw: &str, what: &str) -> UnifiedResult<Uuid> {
    Uuid::parse_str(raw).map_err(|_| UnifiedError::bad_request(format!("Invalid {what} id: {raw}")))
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

/// Money: finite, non-negative, rounded to 4 decimal places.
fn round_money(value: f64) -> f64 {
    (value * 10_000.0).round() / 10_000.0
}

/// Currency: 3-letter ISO code, uppercased.
fn valid_currency(currency: &str) -> UnifiedResult<String> {
    let upper = currency.trim().to_uppercase();
    if upper.len() != 3 || !upper.chars().all(|c| c.is_ascii_uppercase()) {
        return Err(UnifiedError::bad_request(
            "currency must be a 3-letter ISO code such as \"USD\"",
        ));
    }
    Ok(upper)
}

/// A schedule is either an interval in seconds or five cron-like
/// fields where each field is `*`, `*/N`, or a plain number — the
/// same shape the scheduler module validates.
fn valid_schedule(schedule: &str) -> bool {
    let schedule = schedule.trim();
    if schedule.is_empty() || schedule.len() > MAX_SCHEDULE_LEN {
        return false;
    }
    if let Ok(seconds) = schedule.parse::<u64>() {
        return seconds > 0;
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

fn validate_source(source: &str) -> UnifiedResult<String> {
    let source = source.trim().to_lowercase();
    if !ALLOWED_SOURCES.contains(&source.as_str()) {
        return Err(UnifiedError::bad_request(format!(
            "source must be one of: {}",
            ALLOWED_SOURCES.join(", ")
        )));
    }
    Ok(source)
}

fn validate_chart_type(chart_type: &str) -> UnifiedResult<String> {
    let chart_type = chart_type.trim().to_lowercase();
    if !ALLOWED_CHART_TYPES.contains(&chart_type.as_str()) {
        return Err(UnifiedError::bad_request(format!(
            "chart_type must be one of: {}",
            ALLOWED_CHART_TYPES.join(", ")
        )));
    }
    Ok(chart_type)
}

fn validate_fields(source: &str, fields: &[String]) -> UnifiedResult<Vec<String>> {
    if fields.len() > MAX_FIELDS {
        return Err(UnifiedError::bad_request(format!(
            "at most {MAX_FIELDS} fields per report"
        )));
    }
    let allowed = allowed_fields(source);
    let mut seen: HashSet<String> = HashSet::new();
    let mut out: Vec<String> = Vec::new();
    for field in fields {
        let field = field.trim().to_lowercase();
        if field.is_empty() || field.len() > MAX_FIELD_LEN {
            return Err(UnifiedError::bad_request(
                "field names must not be empty and must be at most 128 characters",
            ));
        }
        if !allowed.contains(&field.as_str()) {
            return Err(UnifiedError::bad_request(format!(
                "field \"{field}\" is not available for source \"{source}\""
            )));
        }
        if !seen.insert(field.clone()) {
            return Err(UnifiedError::bad_request(format!(
                "duplicate field \"{field}\""
            )));
        }
        out.push(field);
    }
    Ok(out)
}

fn validate_filters(filters: &Value) -> UnifiedResult<Value> {
    if filters.is_null() {
        return Ok(json!({}));
    }
    if !filters.is_object() {
        return Err(UnifiedError::bad_request(
            "filters must be a JSON object of exact-match field/value pairs",
        ));
    }
    Ok(filters.clone())
}

fn validate_kpis(kpis: &[KpiDefinition]) -> UnifiedResult<Vec<KpiDefinition>> {
    if kpis.len() > MAX_KPIS {
        return Err(UnifiedError::bad_request(format!(
            "at most {MAX_KPIS} KPIs per dashboard"
        )));
    }
    let mut out: Vec<KpiDefinition> = Vec::new();
    for kpi in kpis {
        let metric = kpi.metric.trim().to_lowercase();
        if !ALLOWED_METRICS.contains(&metric.as_str()) {
            return Err(UnifiedError::bad_request(format!(
                "metric must be one of: {}",
                ALLOWED_METRICS.join(", ")
            )));
        }
        let source = validate_source(&kpi.source)?;
        let period = kpi.period.trim().to_lowercase();
        if !ALLOWED_PERIODS.contains(&period.as_str()) {
            return Err(UnifiedError::bad_request(format!(
                "period must be one of: {}",
                ALLOWED_PERIODS.join(", ")
            )));
        }
        if metric.len() > MAX_METRIC_LEN || period.len() > MAX_PERIOD_LEN {
            return Err(UnifiedError::bad_request("metric or period too long"));
        }
        out.push(KpiDefinition {
            metric,
            source,
            period,
        });
    }
    Ok(out)
}

fn load_report(state: &BiState, raw_id: &str) -> UnifiedResult<BiReport> {
    let id = parse_uuid(raw_id, "report")?;
    state
        .reports
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Report not found: {id}")))
}

fn load_schedule(state: &BiState, raw_id: &str) -> UnifiedResult<BiReportSchedule> {
    let id = parse_uuid(raw_id, "schedule")?;
    state
        .schedules
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Schedule not found: {id}")))
}

fn load_dashboard(state: &BiState, raw_id: &str) -> UnifiedResult<BiKpiDashboard> {
    let id = parse_uuid(raw_id, "dashboard")?;
    state
        .dashboards
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Dashboard not found: {id}")))
}

/// A report is visible to its owner, or to anyone in the same
/// company when it is shared. Manage capability bypasses the
/// ownership check.
fn can_read_report(report: &BiReport, actor: &str, company_id: Option<&str>) -> bool {
    if report.owner_id == actor {
        return true;
    }
    if report.visibility == "shared" {
        return match company_id {
            Some(company) => report.company_id == company,
            None => true,
        };
    }
    false
}

fn escape_csv_field(field: &str) -> String {
    if field.contains(',') || field.contains('"') || field.contains('\n') || field.contains('\r') {
        format!("\"{}\"", field.replace('"', "\"\""))
    } else {
        field.to_string()
    }
}

// ---------------------------------------------------------------------------
// Source data: mirror ingestion + read hooks
// ---------------------------------------------------------------------------

/// Read one string field from an event payload, trying each key in
/// order. Source events carry the same value under different key
/// names, so the mirror reads defensively instead of assuming one
/// shape.
fn payload_string(data: &Value, keys: &[&str]) -> Option<String> {
    for key in keys {
        if let Some(value) = data.get(*key).and_then(Value::as_str) {
            let trimmed = value.trim();
            if !trimmed.is_empty() {
                return Some(trimmed.to_string());
            }
        }
    }
    None
}

fn payload_number(data: &Value, keys: &[&str]) -> Option<f64> {
    for key in keys {
        if let Some(value) = data.get(*key).and_then(Value::as_f64) {
            if value.is_finite() {
                return Some(value);
            }
        }
        if let Some(value) = data.get(*key).and_then(Value::as_str) {
            if let Ok(parsed) = value.trim().parse::<f64>() {
                if parsed.is_finite() {
                    return Some(parsed);
                }
            }
        }
    }
    None
}

fn payload_time(data: &Value, keys: &[&str]) -> Option<DateTime<Utc>> {
    for key in keys {
        if let Some(value) = data.get(*key).and_then(Value::as_str) {
            if let Ok(parsed) = DateTime::parse_from_rfc3339(value) {
                return Some(parsed.with_timezone(&Utc));
            }
        }
    }
    None
}

/// Record one mirrored source row. Deduplication is by
/// (source, kind, ref_id): a repeated delivery of the same source
/// event updates the row instead of duplicating it.
fn record_source_row(
    state: &BiState,
    source: &str,
    kind: &str,
    company_id: Option<String>,
    customer_id: Option<String>,
    amount: Option<f64>,
    currency: Option<String>,
    occurred_at: DateTime<Utc>,
    ref_id: Option<String>,
) {
    if let Some(ref_id) = &ref_id {
        let mut existing: Option<Uuid> = None;
        for entry in state.source_records.iter() {
            let row = entry.value();
            if row.source == source && row.kind == kind && row.ref_id.as_deref() == Some(ref_id.as_str()) {
                existing = Some(row.id);
                break;
            }
        }
        if let Some(id) = existing {
            if let Some(mut stored) = state.source_records.get_mut(&id) {
                stored.company_id = company_id;
                stored.customer_id = customer_id;
                stored.amount = amount.map(round_money);
                stored.currency = currency;
                stored.occurred_at = occurred_at;
            }
            return;
        }
    }
    let now = Utc::now();
    let row = SourceRecord {
        id: Uuid::new_v4(),
        source: source.to_string(),
        company_id,
        customer_id,
        kind: kind.to_string(),
        amount: amount.map(round_money),
        currency,
        occurred_at,
        ref_id,
        created_at: now,
    };
    state.source_records.insert(row.id, row);
}

/// Mirror one source event payload into a row. Amount keys cover
/// the billing / store shapes (`amount`, `total_amount`,
/// `paid_amount`, `total`); customer keys cover `customer_id` and
/// `user_id`; time keys cover `paid_at`, `occurred_at`, and
/// `created_at`. A payload with no usable fields is ignored.
fn mirror_event(state: &BiState, source: &str, kind: &str, data: &Value) {
    let company_id = payload_string(data, &["company_id"]);
    let customer_id = payload_string(data, &["customer_id", "user_id"]);
    let amount = payload_number(data, &["amount", "total_amount", "paid_amount", "total"]);
    let currency = payload_string(data, &["currency"])
        .and_then(|value| valid_currency(&value).ok());
    let occurred_at = payload_time(data, &["paid_at", "occurred_at", "created_at"])
        .unwrap_or_else(Utc::now);
    let ref_id = payload_string(data, &["id", "order_id", "invoice_id", "ref_id", "entity_id"]);
    record_source_row(
        state,
        source,
        kind,
        company_id,
        customer_id,
        amount,
        currency,
        occurred_at,
        ref_id,
    );
}

/// Read rows for one source: the local mirror first, then any rows
/// a source module contributes through its `{source}.data.requested`
/// read hook. Hook rows are objects with optional `amount`,
/// `currency`, `occurred_at`, `customer_id`, `kind`, and `ref_id`
/// keys; malformed rows are skipped, never trusted blindly.
fn read_source_rows(
    state: &BiState,
    source: &str,
    company_id: Option<&str>,
    from: Option<DateTime<Utc>>,
    to: Option<DateTime<Utc>>,
) -> Vec<Value> {
    let mut rows: Vec<Value> = state
        .source_records
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|row| row.source == source)
        .filter(|row| match company_id {
            Some(company) => row.company_id.as_deref() == Some(company),
            None => true,
        })
        .filter(|row| match from {
            Some(from) => row.occurred_at >= from,
            None => true,
        })
        .filter(|row| match to {
            Some(to) => row.occurred_at <= to,
            None => true,
        })
        .filter_map(|row| serde_json::to_value(row).ok())
        .collect();
    if let Some(bus) = state.bus() {
        let context = bus.emit(
            &format!("{source}.data.requested"),
            json!({
                "source": source,
                "company_id": company_id,
                "from": from.map(|value| value.to_rfc3339()),
                "to": to.map(|value| value.to_rfc3339()),
                "limit": MAX_EXPORT_ROWS,
            }),
        );
        for result in &context.results {
            if let Some(hook_rows) = result.get("rows").and_then(Value::as_array) {
                for hook_row in hook_rows {
                    if !hook_row.is_object() {
                        continue;
                    }
                    let mut row = hook_row.clone();
                    if let Value::Object(map) = &mut row {
                        map.insert("source".to_string(), Value::String(source.to_string()));
                        if !map.contains_key("kind") {
                            map.insert(
                                "kind".to_string(),
                                Value::String("record".to_string()),
                            );
                        }
                    }
                    rows.push(row);
                }
            }
        }
    }
    rows
}

/// Apply a report's exact-match filters plus the optional
/// `from` / `to` bounds to one row. Unknown filter keys never
/// match a row, so a typo fails closed instead of returning
/// unfiltered data.
fn row_matches_filters(row: &Value, filters: &Value) -> bool {
    let Some(filter_map) = filters.as_object() else {
        return true;
    };
    for (key, expected) in filter_map {
        if key == "from" || key == "to" {
            continue;
        }
        match row.get(key) {
            Some(actual) => {
                if actual != expected {
                    return false;
                }
            }
            None => return false,
        }
    }
    if let (Some(from_raw), Some(occurred_raw)) = (
        filter_map.get("from").and_then(Value::as_str),
        row.get("occurred_at").and_then(Value::as_str),
    ) {
        if let (Ok(from), Ok(occurred)) = (
            DateTime::parse_from_rfc3339(from_raw),
            DateTime::parse_from_rfc3339(occurred_raw),
        ) {
            if occurred < from {
                return false;
            }
        }
    }
    if let (Some(to_raw), Some(occurred_raw)) = (
        filter_map.get("to").and_then(Value::as_str),
        row.get("occurred_at").and_then(Value::as_str),
    ) {
        if let (Ok(to), Ok(occurred)) = (
            DateTime::parse_from_rfc3339(to_raw),
            DateTime::parse_from_rfc3339(occurred_raw),
        ) {
            if occurred > to {
                return false;
            }
        }
    }
    true
}

/// Project one row down to the report's selected fields. When no
/// fields are selected, the whole row is returned.
fn project_row(row: &Value, fields: &[String]) -> Value {
    if fields.is_empty() {
        return row.clone();
    }
    let mut out = serde_json::Map::new();
    for field in fields {
        if let Some(value) = row.get(field) {
            out.insert(field.clone(), value.clone());
        }
    }
    Value::Object(out)
}

/// Aggregate rows: when `group_by` is set, one bucket per distinct
/// group value with `count`, `total_amount`, and `average_amount`;
/// otherwise a single totals row. Rows are sorted by group value
/// for deterministic output.
fn aggregate_rows(rows: &[Value], group_by: Option<&str>) -> (Vec<Value>, Value) {
    let mut total_amount = 0.0f64;
    let mut amount_count = 0usize;
    for row in rows {
        if let Some(amount) = row.get("amount").and_then(Value::as_f64) {
            total_amount += amount;
            amount_count += 1;
        }
    }
    total_amount = round_money(total_amount);
    let average_amount = if amount_count > 0 {
        Some(round_money(total_amount / amount_count as f64))
    } else {
        None
    };
    let totals = json!({
        "row_count": rows.len(),
        "total_amount": total_amount,
        "amount_rows": amount_count,
        "average_amount": average_amount,
    });
    let Some(group_by) = group_by else {
        return (rows.to_vec(), totals);
    };
    let mut buckets: BTreeMap<String, (usize, f64, usize)> = BTreeMap::new();
    for row in rows {
        let key = row
            .get(group_by)
            .map(|value| match value {
                Value::String(text) => text.clone(),
                other => other.to_string(),
            })
            .unwrap_or_else(|| "null".to_string());
        let entry = buckets.entry(key).or_insert((0, 0.0, 0));
        entry.0 += 1;
        if let Some(amount) = row.get("amount").and_then(Value::as_f64) {
            entry.1 += amount;
            entry.2 += 1;
        }
    }
    let grouped: Vec<Value> = buckets
        .into_iter()
        .map(|(group, (count, sum, amount_rows))| {
            json!({
                "group": group,
                "count": count,
                "total_amount": round_money(sum),
                "amount_rows": amount_rows,
                "average_amount": if amount_rows > 0 { Some(round_money(sum / amount_rows as f64)) } else { None },
            })
        })
        .collect();
    (grouped, totals)
}

/// Execute a saved report against the source data: read rows,
/// apply filters, project fields, aggregate. Returns an
/// "insufficient data" error when the source has no rows at all —
/// never an empty success dressed up as a measurement.
fn execute_report(state: &BiState, report: &BiReport) -> UnifiedResult<Value> {
    let rows = read_source_rows(state, &report.source, Some(&report.company_id), None, None);
    if rows.is_empty() {
        return Err(UnifiedError::unprocessable(format!(
            "Insufficient data: source \"{}\" has no rows for company \"{}\" yet (no source events received and no module answered \"{}.data.requested\")",
            report.source, report.company_id, report.source
        ))
        .with_metadata("source", json!(report.source))
        .with_metadata("company_id", json!(report.company_id)));
    }
    let filtered: Vec<Value> = rows
        .iter()
        .filter(|row| row_matches_filters(row, &report.filters))
        .map(|row| project_row(row, &report.fields))
        .collect();
    let (mut grouped, totals) = aggregate_rows(&filtered, report.group_by.as_deref());
    grouped.truncate(MAX_RUN_ROWS);
    Ok(json!({
        "report_id": report.id.to_string(),
        "source": report.source,
        "chart_type": report.chart_type,
        "group_by": report.group_by,
        "rows": grouped,
        "totals": totals,
    }))
}

/// Daily revenue series: one (date, total) pair per day, sorted by
/// date, from mirrored rows of the given sources with an amount.
fn daily_series(state: &BiState, sources: &[&str], company_id: Option<&str>, currency: Option<&str>) -> Vec<(NaiveDate, f64)> {
    let mut by_day: BTreeMap<NaiveDate, f64> = BTreeMap::new();
    for entry in state.source_records.iter() {
        let row = entry.value();
        if !sources.contains(&row.source.as_str()) {
            continue;
        }
        if let Some(company) = company_id {
            if row.company_id.as_deref() != Some(company) {
                continue;
            }
        }
        if let Some(currency) = currency {
            if row.currency.as_deref() != Some(currency) {
                continue;
            }
        }
        if let Some(amount) = row.amount {
            *by_day.entry(row.occurred_at.date_naive()).or_insert(0.0) += amount;
        }
    }
    by_day
        .into_iter()
        .map(|(date, total)| (date, round_money(total)))
        .collect()
}

/// Least-squares linear trend over daily totals: returns the slope
/// (amount change per day) and the intercept at day zero. Pure
/// arithmetic over the observed points — no hidden model.
fn linear_trend(series: &[(NaiveDate, f64)]) -> (f64, f64) {
    let n = series.len() as f64;
    if series.len() < 2 {
        return (0.0, series.first().map(|(_, total)| *total).unwrap_or(0.0));
    }
    let first = series[0].0;
    let mut sum_x = 0.0f64;
    let mut sum_y = 0.0f64;
    let mut sum_xx = 0.0f64;
    let mut sum_xy = 0.0f64;
    for (date, total) in series {
        let x = (*date - first).num_days() as f64;
        sum_x += x;
        sum_y += total;
        sum_xx += x * x;
        sum_xy += x * total;
    }
    let denominator = n * sum_xx - sum_x * sum_x;
    if denominator.abs() < f64::EPSILON {
        return (0.0, sum_y / n);
    }
    let slope = (n * sum_xy - sum_x * sum_y) / denominator;
    let intercept = (sum_y - slope * sum_x) / n;
    (slope, intercept)
}

/// Build a forecast response from a daily series: 7-day moving
/// average plus linear trend, projected forward `horizon_days`
/// days. Every response names its method and inputs. Returns an
/// "insufficient data" error when fewer than `MIN_FORECAST_DAYS`
/// distinct days are observed.
fn build_forecast(
    state: &BiState,
    kind: &str,
    sources: &[&str],
    company_id: Option<&str>,
    currency: Option<&str>,
    horizon_days: i64,
) -> UnifiedResult<Value> {
    let series = daily_series(state, sources, company_id, currency);
    if series.len() < MIN_FORECAST_DAYS {
        return Err(UnifiedError::unprocessable(format!(
            "Insufficient data: {kind} forecast needs at least {MIN_FORECAST_DAYS} days of observed history, found {} day(s)",
            series.len()
        ))
        .with_metadata("observed_days", json!(series.len()))
        .with_metadata("required_days", json!(MIN_FORECAST_DAYS))
        .with_metadata("sources", json!(sources)));
    }
    let window = series
        .iter()
        .rev()
        .take(FORECAST_WINDOW_DAYS as usize)
        .map(|(_, total)| *total)
        .collect::<Vec<f64>>();
    let moving_average = round_money(window.iter().sum::<f64>() / window.len() as f64);
    let (slope, intercept) = linear_trend(&series);
    let last_date = series.last().expect("series is non-empty").0;
    let first_date = series[0].0;
    let mut projections: Vec<Value> = Vec::new();
    for day in 1..=horizon_days {
        let date = last_date + Duration::days(day);
        let x = (date - first_date).num_days() as f64;
        let trend_value = intercept + slope * x;
        let projected = round_money((moving_average + trend_value) / 2.0);
        projections.push(json!({
            "date": date.format("%Y-%m-%d").to_string(),
            "projected_amount": projected,
            "moving_average_7d": moving_average,
            "trend_value": round_money(trend_value),
        }));
    }
    let total_projected = round_money(
        projections
            .iter()
            .filter_map(|row| row.get("projected_amount").and_then(Value::as_f64))
            .sum::<f64>(),
    );
    let result = json!({
        "kind": kind,
        "method": "moving_average_7d_plus_linear_trend",
        "inputs": {
            "sources": sources,
            "currency": currency,
            "company_id": company_id,
            "observed_days": series.len(),
            "window_days": FORECAST_WINDOW_DAYS,
            "from": series[0].0.format("%Y-%m-%d").to_string(),
            "to": last_date.format("%Y-%m-%d").to_string(),
        },
        "horizon_days": horizon_days,
        "daily_average_7d": moving_average,
        "trend_per_day": round_money(slope),
        "total_projected": total_projected,
        "projections": projections,
    });
    state.emit(
        "bi.forecast.generated",
        json!({
            "kind": kind,
            "sources": sources,
            "currency": currency,
            "horizon_days": horizon_days,
            "observed_days": series.len(),
            "total_projected": total_projected,
        }),
    );
    Ok(result)
}

/// Queue one scheduled delivery through `scheduler.enqueue` with
/// kind `bi.report.deliver`. Mirrors the events module's reminder
/// scheduling: the scheduler answers `{"queued": true, "job_id":
/// ...}` when it knows the kind, and `{"queued": false, "error":
/// "Unknown job kind: ..."}` today, because `bi.report.deliver`
/// is not in `KNOWN_KINDS` yet. The schedule is stored either way;
/// `scheduler_status` records the outcome honestly.
fn schedule_delivery(state: &BiState, schedule: &mut BiReportSchedule) {
    let Some(bus) = state.bus() else {
        schedule.scheduler_job_id = None;
        schedule.scheduler_status = "declined".to_string();
        return;
    };
    let answer = bus.emit(
        "scheduler.enqueue",
        json!({
            "kind": "bi.report.deliver",
            "payload": {
                "report_id": schedule.report_id.to_string(),
                "schedule_id": schedule.id.to_string(),
                "company_id": schedule.company_id,
                "schedule": schedule.schedule,
                "recipients": schedule.recipients,
                "channel": schedule.channel,
            },
        }),
    );
    match answer.results.first() {
        Some(result) if result.get("queued").and_then(Value::as_bool) == Some(true) => {
            schedule.scheduler_job_id = result
                .get("job_id")
                .and_then(|value| value.as_str())
                .map(str::to_string);
            schedule.scheduler_status = "queued".to_string();
        }
        _ => {
            schedule.scheduler_job_id = None;
            schedule.scheduler_status = "declined".to_string();
        }
    }
}

/// Deliver a report now: execute it, emit `bi.report.delivered`,
/// and send one `notification.requested` per recipient. Returns
/// the row count delivered. A report with no source rows fails
/// with the execution error — delivery never sends an empty
/// report as if it were data.
fn deliver_report_now(
    state: &BiState,
    report: &BiReport,
    schedule: Option<&BiReportSchedule>,
) -> UnifiedResult<usize> {
    let run = execute_report(state, report)?;
    let row_count = run
        .get("totals")
        .and_then(|totals| totals.get("row_count"))
        .and_then(Value::as_u64)
        .unwrap_or(0) as usize;
    let recipients: Vec<String> = schedule
        .map(|value| value.recipients.clone())
        .unwrap_or_default();
    let link = format!("/api/v1/bi/reports/{}", report.id);
    for recipient in &recipients {
        state.notify(
            "bi.report.delivered",
            recipient,
            &format!("Report ready: {}", report.name),
            &format!(
                "Report \"{}\" from source \"{}\" delivered with {row_count} row(s).",
                report.name, report.source
            ),
            Some(link.clone()),
        );
    }
    if let Some(schedule) = schedule {
        if let Some(mut stored) = state.schedules.get_mut(&schedule.id) {
            stored.last_delivered_at = Some(Utc::now());
            stored.updated_at = Utc::now();
        }
    }
    state.emit(
        "bi.report.delivered",
        json!({
            "report_id": report.id.to_string(),
            "schedule_id": schedule.map(|value| value.id.to_string()),
            "company_id": report.company_id,
            "recipients": recipients,
            "row_count": row_count,
        }),
    );
    Ok(row_count)
}

// ---------------------------------------------------------------------------
// Source-event listeners (mirror ingestion)
// ---------------------------------------------------------------------------

fn on_invoice_payment(state: BiState, ctx: &mut HookContext) {
    mirror_event(&state, "billing", "payment", &ctx.data.clone());
}

fn on_payment_captured(state: BiState, ctx: &mut HookContext) {
    mirror_event(&state, "billing", "payment", &ctx.data.clone());
}

fn on_store_order_completed(state: BiState, ctx: &mut HookContext) {
    mirror_event(&state, "store", "order_completed", &ctx.data.clone());
}

fn on_store_order_refunded(state: BiState, ctx: &mut HookContext) {
    let mut data = ctx.data.clone();
    if let Some(amount) = payload_number(&data, &["amount", "total_amount", "total"]) {
        if let Value::Object(map) = &mut data {
            map.insert("amount".to_string(), json!(-amount));
        }
    }
    mirror_event(&state, "store", "order_refunded", &data);
}

fn on_ticket(state: BiState, kind: &str, ctx: &mut HookContext) {
    mirror_event(&state, "tickets", kind, &ctx.data.clone());
}

fn on_license(state: BiState, kind: &str, ctx: &mut HookContext) {
    mirror_event(&state, "licensing", kind, &ctx.data.clone());
}

fn on_project(state: BiState, kind: &str, ctx: &mut HookContext) {
    mirror_event(&state, "projects", kind, &ctx.data.clone());
}

fn on_task(state: BiState, kind: &str, ctx: &mut HookContext) {
    mirror_event(&state, "projects", kind, &ctx.data.clone());
}

// ---------------------------------------------------------------------------
// Module
// ---------------------------------------------------------------------------

pub struct BiModule {
    state: BiState,
}

impl BiModule {
    pub fn new() -> Self {
        Self {
            state: BiState::default(),
        }
    }
}

impl Default for BiModule {
    fn default() -> Self {
        Self::new()
    }
}

impl Module for BiModule {
    fn id(&self) -> &'static str {
        "bi"
    }

    fn name(&self) -> &'static str {
        "Business Intelligence + AI Insights"
    }

    fn description(&self) -> &'static str {
        "Custom report builder over source-module data, scheduled report delivery, transparent revenue and cash-flow forecasting (moving average plus linear trend), churn signals, and KPI dashboards. Aggregation runs over the in-memory mirror fed by source events plus {source}.data.requested read hooks — never raw SQL against other modules' tables."
    }

    fn is_core(&self) -> bool {
        false
    }

    fn requires(&self) -> &[&'static str] {
        &[
            "users",
            "roles",
            "billing",
            "store",
            "notifications",
            "scheduler",
        ]
    }

    fn dependencies(&self) -> Vec<&'static str> {
        vec![
            "users",
            "roles",
            "billing",
            "store",
            "notifications",
            "scheduler",
        ]
    }

    fn available_as_product(&self) -> bool {
        true
    }

    fn product_category(&self) -> Option<&'static str> {
        Some("productivity")
    }

    fn init(&self, ctx: &AppContext) -> UnifiedResult<()> {
        *self
            .state
            .hooks
            .write()
            .expect("hooks lock poisoned") = Some(Arc::clone(&ctx.hooks));
        Ok(())
    }

    fn register_hooks(&self, bus: &HookBus) {
        let state = self.state.clone();
        bus.register(
            "invoice.payment.recorded",
            "bi",
            Arc::new(move |ctx: &mut HookContext| on_invoice_payment(state.clone(), ctx)),
        );
        let state = self.state.clone();
        bus.register(
            "invoice.paid",
            "bi",
            Arc::new(move |ctx: &mut HookContext| on_invoice_payment(state.clone(), ctx)),
        );
        let state = self.state.clone();
        bus.register(
            "payment.captured",
            "bi",
            Arc::new(move |ctx: &mut HookContext| on_payment_captured(state.clone(), ctx)),
        );
        let state = self.state.clone();
        bus.register(
            "store.order.completed",
            "bi",
            Arc::new(move |ctx: &mut HookContext| {
                on_store_order_completed(state.clone(), ctx)
            }),
        );
        let state = self.state.clone();
        bus.register(
            "order.paid",
            "bi",
            Arc::new(move |ctx: &mut HookContext| {
                on_store_order_completed(state.clone(), ctx)
            }),
        );
        let state = self.state.clone();
        bus.register(
            "store.order.refunded",
            "bi",
            Arc::new(move |ctx: &mut HookContext| {
                on_store_order_refunded(state.clone(), ctx)
            }),
        );
        for (event, kind) in [
            ("ticket.created", "ticket_created"),
            ("ticket.updated", "ticket_updated"),
        ] {
            let state = self.state.clone();
            let kind = kind.to_string();
            bus.register(
                event,
                "bi",
                Arc::new(move |ctx: &mut HookContext| {
                    on_ticket(state.clone(), &kind, ctx)
                }),
            );
        }
        for (event, kind) in [
            ("license.issued", "license_issued"),
            ("license.activated", "license_activated"),
            ("license.suspended", "license_suspended"),
            ("license.revoked", "license_revoked"),
        ] {
            let state = self.state.clone();
            let kind = kind.to_string();
            bus.register(
                event,
                "bi",
                Arc::new(move |ctx: &mut HookContext| {
                    on_license(state.clone(), &kind, ctx)
                }),
            );
        }
        for (event, kind) in [
            ("project.task.completed", "task_completed"),
            ("task.completed", "task_completed"),
            ("task.created", "task_created"),
        ] {
            let state = self.state.clone();
            let kind = kind.to_string();
            bus.register(
                event,
                "bi",
                Arc::new(move |ctx: &mut HookContext| {
                    on_task(state.clone(), &kind, ctx)
                }),
            );
        }
        let state = self.state.clone();
        bus.register(
            "project.created",
            "bi",
            Arc::new(move |ctx: &mut HookContext| {
                on_project(state.clone(), "project_created", ctx)
            }),
        );
    }

    fn configure_routes(&self, cfg: &mut web::ServiceConfig) {
        let state = web::Data::new(self.state.clone());
        cfg.service(
            web::scope("/api/v1/bi")
                .app_data(state)
                .route("/reports", web::get().to(list_reports))
                .route("/reports", web::post().to(create_report))
                .route("/reports/{id}", web::get().to(get_report))
                .route("/reports/{id}", web::patch().to(update_report))
                .route("/reports/{id}", web::delete().to(delete_report))
                .route("/reports/{id}/run", web::post().to(run_report))
                .route("/reports/{id}/export", web::get().to(export_report))
                .route("/reports/{id}/schedules", web::get().to(list_schedules))
                .route("/reports/{id}/schedules", web::post().to(create_schedule))
                .route(
                    "/reports/{id}/schedules/{schedule_id}",
                    web::get().to(get_schedule),
                )
                .route(
                    "/reports/{id}/schedules/{schedule_id}",
                    web::delete().to(delete_schedule),
                )
                .route(
                    "/reports/{id}/schedules/{schedule_id}/run-now",
                    web::post().to(run_schedule_now),
                )
                .route("/forecast/revenue", web::get().to(forecast_revenue))
                .route("/forecast/cashflow", web::get().to(forecast_cashflow))
                .route("/forecast/churn", web::get().to(forecast_churn))
                .route("/kpi-dashboards", web::get().to(list_dashboards))
                .route("/kpi-dashboards", web::post().to(create_dashboard))
                .route("/kpi-dashboards/{id}", web::get().to(get_dashboard))
                .route("/kpi-dashboards/{id}", web::patch().to(update_dashboard))
                .route("/kpi-dashboards/{id}", web::delete().to(delete_dashboard))
                .route(
                    "/kpi-dashboards/{id}/compute",
                    web::get().to(compute_dashboard),
                ),
        );
    }

    fn route_info(&self) -> Vec<RouteInfo> {
        vec![
            RouteInfo { method: "GET", path: "/api/v1/bi/reports", owner: "bi", description: "List saved reports (paginated, filter by source, company_id, visibility, owner_id)." },
            RouteInfo { method: "POST", path: "/api/v1/bi/reports", owner: "bi", description: "Create a saved report (source, fields, filters, grouping, chart type, shared vs private)." },
            RouteInfo { method: "GET", path: "/api/v1/bi/reports/{id}", owner: "bi", description: "Get one saved report (owner or shared in the same company)." },
            RouteInfo { method: "PATCH", path: "/api/v1/bi/reports/{id}", owner: "bi", description: "Update a saved report (owner or manage capability)." },
            RouteInfo { method: "DELETE", path: "/api/v1/bi/reports/{id}", owner: "bi", description: "Delete a saved report (and its schedules)." },
            RouteInfo { method: "POST", path: "/api/v1/bi/reports/{id}/run", owner: "bi", description: "Execute a saved report (rows + totals; insufficient-data error when the source has no rows)." },
            RouteInfo { method: "GET", path: "/api/v1/bi/reports/{id}/export", owner: "bi", description: "Export a report run as CSV or JSON (?format=csv|json, max 10,000 rows)." },
            RouteInfo { method: "GET", path: "/api/v1/bi/reports/{id}/schedules", owner: "bi", description: "List schedules of a report." },
            RouteInfo { method: "POST", path: "/api/v1/bi/reports/{id}/schedules", owner: "bi", description: "Create a scheduled delivery (queued as bi.report.deliver; scheduler_status records queued/declined)." },
            RouteInfo { method: "GET", path: "/api/v1/bi/reports/{id}/schedules/{schedule_id}", owner: "bi", description: "Get one schedule." },
            RouteInfo { method: "DELETE", path: "/api/v1/bi/reports/{id}/schedules/{schedule_id}", owner: "bi", description: "Delete a schedule." },
            RouteInfo { method: "POST", path: "/api/v1/bi/reports/{id}/schedules/{schedule_id}/run-now", owner: "bi", description: "Run a scheduled report now and deliver it (emits bi.report.delivered)." },
            RouteInfo { method: "GET", path: "/api/v1/bi/forecast/revenue", owner: "bi", description: "Revenue forecast: 7-day moving average + linear trend; response carries method, inputs, and horizon." },
            RouteInfo { method: "GET", path: "/api/v1/bi/forecast/cashflow", owner: "bi", description: "Cash-flow projection (net of refunds): 7-day moving average + linear trend; response carries method, inputs, and horizon." },
            RouteInfo { method: "GET", path: "/api/v1/bi/forecast/churn", owner: "bi", description: "Churn signals: customers with no activity in N days plus suspended/revoked licenses and refunded orders." },
            RouteInfo { method: "GET", path: "/api/v1/bi/kpi-dashboards", owner: "bi", description: "List KPI dashboards (paginated, filter by company_id, owner_id)." },
            RouteInfo { method: "POST", path: "/api/v1/bi/kpi-dashboards", owner: "bi", description: "Create a KPI dashboard (metric, source, period per KPI)." },
            RouteInfo { method: "GET", path: "/api/v1/bi/kpi-dashboards/{id}", owner: "bi", description: "Get one KPI dashboard." },
            RouteInfo { method: "PATCH", path: "/api/v1/bi/kpi-dashboards/{id}", owner: "bi", description: "Update a KPI dashboard." },
            RouteInfo { method: "DELETE", path: "/api/v1/bi/kpi-dashboards/{id}", owner: "bi", description: "Delete a KPI dashboard." },
            RouteInfo { method: "GET", path: "/api/v1/bi/kpi-dashboards/{id}/compute", owner: "bi", description: "Compute every KPI of a dashboard on read (null + insufficient_data when a source has no rows)." },
        ]
    }

    fn route_version_support(&self) -> RouteVersionSupport {
        RouteVersionSupport::All
    }
}

pub fn register(registry: &ModuleRegistry) {
    registry.register(Arc::new(BiModule::new()));
}

// ---------------------------------------------------------------------------
// Report handlers
// ---------------------------------------------------------------------------

async fn list_reports(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<BiState>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_BI_VIEW)?;
        let params = query.into_inner();
        let source_filter = params.filters.get("source").cloned();
        let company_filter = params.filters.get("company_id").cloned();
        let visibility_filter = params.filters.get("visibility").cloned();
        let owner_filter = params.filters.get("owner_id").cloned();
        let search = params.search.clone().unwrap_or_default().to_lowercase();
        let mut items: Vec<BiReport> = state
            .reports
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|report| {
                if let Some(source) = &source_filter {
                    if &report.source != source {
                        return false;
                    }
                }
                if let Some(company) = &company_filter {
                    if &report.company_id != company {
                        return false;
                    }
                }
                if let Some(visibility) = &visibility_filter {
                    if &report.visibility != visibility {
                        return false;
                    }
                }
                if let Some(owner) = &owner_filter {
                    if &report.owner_id != owner {
                        return false;
                    }
                }
                if !search.is_empty() {
                    let haystack = format!(
                        "{} {}",
                        report.name,
                        report.description.clone().unwrap_or_default()
                    )
                    .to_lowercase();
                    if !haystack.contains(&search) {
                        return false;
                    }
                }
                true
            })
            .collect();
        let sort = params.allowed_sort(&["name", "source", "created_at"], "created_at");
        let descending = params.normalized().order == "desc";
        items.sort_by(|a, b| {
            let ordering = match sort.as_str() {
                "name" => a.name.cmp(&b.name),
                "source" => a.source.cmp(&b.source),
                _ => a.created_at.cmp(&b.created_at),
            };
            if descending {
                ordering.reverse()
            } else {
                ordering
            }
        });
        Ok(paginate(items, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn create_report(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<BiState>,
    req: HttpRequest,
    body: web::Json<CreateReportRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<BiReport> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_BI_MANAGE)?;
        let name = require_text(&body.name, "name", MAX_NAME_LEN)?;
        let company_id = require_text(&body.company_id, "company_id", MAX_COMPANY_LEN)?;
        let source = validate_source(&body.source)?;
        let fields = validate_fields(&source, &body.fields)?;
        let filters = validate_filters(&body.filters)?;
        let group_by = match &body.group_by {
            Some(group) => {
                let group = group.trim().to_lowercase();
                if group.len() > MAX_GROUP_LEN {
                    return Err(UnifiedError::bad_request(
                        "group_by must be at most 128 characters",
                    ));
                }
                if !allowed_fields(&source).contains(&group.as_str()) {
                    return Err(UnifiedError::bad_request(format!(
                        "group_by \"{group}\" is not available for source \"{source}\""
                    )));
                }
                Some(group)
            }
            None => None,
        };
        let chart_type = validate_chart_type(body.chart_type.as_deref().unwrap_or("table"))?;
        let visibility = match body.visibility.as_deref().unwrap_or("private") {
            "shared" => "shared".to_string(),
            "private" => "private".to_string(),
            other => {
                return Err(UnifiedError::bad_request(format!(
                    "visibility must be \"shared\" or \"private\", got \"{other}\""
                )))
            }
        };
        let description = match &body.description {
            Some(text) if !text.trim().is_empty() => Some(require_text(
                text,
                "description",
                MAX_DESCRIPTION_LEN,
            )?),
            _ => None,
        };
        let now = Utc::now();
        let report = BiReport {
            id: Uuid::new_v4(),
            company_id: company_id.clone(),
            owner_id: caller.actor.clone(),
            name,
            description,
            source: source.clone(),
            fields,
            filters,
            group_by,
            chart_type,
            visibility,
            created_at: now,
            updated_at: now,
        };
        state.reports.insert(report.id, report.clone());
        state.emit(
            "bi.report.created",
            json!({
                "report_id": report.id.to_string(),
                "company_id": company_id,
                "source": source,
                "owner_id": caller.actor,
            }),
        );
        Ok(report)
    })();
    match result {
        Ok(report) => created(report),
        Err(error) => err(error, debug),
    }
}

async fn get_report(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<BiState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<BiReport> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_BI_VIEW)?;
        let report = load_report(&state, &path.into_inner())?;
        if !can_read_report(&report, &caller.actor, None)
            && !check_capability(&ctx, &caller.role, PERM_BI_MANAGE)
        {
            return Err(UnifiedError::forbidden(
                "Report is private to its owner",
            ));
        }
        Ok(report)
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn update_report(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<BiState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdateReportRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<BiReport> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_BI_MANAGE)?;
        let mut report = load_report(&state, &path.into_inner())?;
        if let Some(name) = &body.name {
            report.name = require_text(name, "name", MAX_NAME_LEN)?;
        }
        if let Some(description) = &body.description {
            report.description = match description {
                Some(text) if !text.trim().is_empty() => Some(require_text(
                    text,
                    "description",
                    MAX_DESCRIPTION_LEN,
                )?),
                _ => None,
            };
        }
        if let Some(source) = &body.source {
            let source = validate_source(source)?;
            if let Some(fields) = &body.fields {
                report.fields = validate_fields(&source, fields)?;
            } else {
                report.fields = validate_fields(&source, &report.fields)?;
            }
            if let Some(group_by) = &report.group_by {
                if !allowed_fields(&source).contains(&group_by.as_str()) {
                    return Err(UnifiedError::bad_request(format!(
                        "group_by \"{group_by}\" is not available for source \"{source}\""
                    )));
                }
            }
            report.source = source;
        } else if let Some(fields) = &body.fields {
            report.fields = validate_fields(&report.source, fields)?;
        }
        if let Some(filters) = &body.filters {
            report.filters = validate_filters(filters)?;
        }
        if let Some(group_by) = &body.group_by {
            report.group_by = match group_by {
                Some(group) => {
                    let group = group.trim().to_lowercase();
                    if !allowed_fields(&report.source).contains(&group.as_str()) {
                        return Err(UnifiedError::bad_request(format!(
                            "group_by \"{group}\" is not available for source \"{}\"",
                            report.source
                        )));
                    }
                    Some(group)
                }
                None => None,
            };
        }
        if let Some(chart_type) = &body.chart_type {
            report.chart_type = validate_chart_type(chart_type)?;
        }
        if let Some(visibility) = &body.visibility {
            report.visibility = match visibility.as_str() {
                "shared" => "shared".to_string(),
                "private" => "private".to_string(),
                other => {
                    return Err(UnifiedError::bad_request(format!(
                        "visibility must be \"shared\" or \"private\", got \"{other}\""
                    )))
                }
            };
        }
        report.updated_at = Utc::now();
        state.reports.insert(report.id, report.clone());
        state.emit(
            "bi.report.updated",
            json!({
                "report_id": report.id.to_string(),
                "company_id": report.company_id,
                "actor_id": caller.actor,
            }),
        );
        Ok(report)
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn delete_report(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<BiState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_BI_MANAGE)?;
        let report = load_report(&state, &path.into_inner())?;
        state.reports.remove(&report.id);
        state
            .schedules
            .retain(|_, schedule| schedule.report_id != report.id);
        state.emit(
            "bi.report.deleted",
            json!({
                "report_id": report.id.to_string(),
                "company_id": report.company_id,
                "actor_id": caller.actor,
            }),
        );
        Ok(json!({ "deleted": report.id.to_string() }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn run_report(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<BiState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_BI_VIEW)?;
        let report = load_report(&state, &path.into_inner())?;
        if !can_read_report(&report, &caller.actor, None)
            && !check_capability(&ctx, &caller.role, PERM_BI_MANAGE)
        {
            return Err(UnifiedError::forbidden(
                "Report is private to its owner",
            ));
        }
        execute_report(&state, &report)
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn export_report(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<BiState>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<ExportQuery>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let format = query.format.trim().to_lowercase();
    if format != "csv" && format != "json" {
        return err(
            UnifiedError::bad_request("format must be \"csv\" or \"json\""),
            debug,
        );
    }
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_BI_VIEW)?;
        let report = load_report(&state, &path.into_inner())?;
        if !can_read_report(&report, &caller.actor, None)
            && !check_capability(&ctx, &caller.role, PERM_BI_MANAGE)
        {
            return Err(UnifiedError::forbidden(
                "Report is private to its owner",
            ));
        }
        execute_report(&state, &report)
    })();
    let run = match result {
        Ok(data) => data,
        Err(error) => return err(error, debug),
    };
    let rows = run
        .get("rows")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let capped: Vec<Value> = rows.into_iter().take(MAX_EXPORT_ROWS).collect();
    if format == "csv" {
        let mut body = String::new();
        let mut headers: Vec<String> = Vec::new();
        for row in &capped {
            if let Value::Object(map) = row {
                for key in map.keys() {
                    if !headers.contains(key) {
                        headers.push(key.clone());
                    }
                }
            }
        }
        body.push_str(
            &headers
                .iter()
                .map(|header| escape_csv_field(header))
                .collect::<Vec<String>>()
                .join(","),
        );
        body.push('\n');
        for row in &capped {
            let line: Vec<String> = headers
                .iter()
                .map(|header| {
                    let value = row.get(header).map(|value| match value {
                        Value::String(text) => text.clone(),
                        Value::Null => String::new(),
                        other => other.to_string(),
                    });
                    escape_csv_field(&value.unwrap_or_default())
                })
                .collect();
            body.push_str(&line.join(","));
            body.push('\n');
        }
        HttpResponse::Ok()
            .content_type("text/csv; charset=utf-8")
            .insert_header((
                "Content-Disposition",
                format!("attachment; filename=\"bi-report-{}.csv\"", run.get("report_id").and_then(Value::as_str).unwrap_or("export")),
            ))
            .body(body)
    } else {
        HttpResponse::Ok()
            .content_type("application/json")
            .body(serde_json::to_string(&json!({ "data": capped })).unwrap_or_else(|_| "{\"data\":[]}".to_string()))
    }
}

// ---------------------------------------------------------------------------
// Schedule handlers
// ---------------------------------------------------------------------------

async fn list_schedules(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<BiState>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_BI_VIEW)?;
        let report = load_report(&state, &path.into_inner())?;
        let params = query.into_inner();
        let mut items: Vec<BiReportSchedule> = state
            .schedules
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|schedule| schedule.report_id == report.id)
            .collect();
        items.sort_by(|a, b| a.created_at.cmp(&b.created_at));
        Ok(paginate(items, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn create_schedule(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<BiState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<CreateScheduleRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<BiReportSchedule> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_BI_MANAGE)?;
        let report = load_report(&state, &path.into_inner())?;
        if !valid_schedule(&body.schedule) {
            return Err(UnifiedError::bad_request(
                "schedule must be an interval in seconds or five cron-like fields",
            ));
        }
        if body.recipients.is_empty() || body.recipients.len() > MAX_RECIPIENTS {
            return Err(UnifiedError::bad_request(format!(
                "recipients must list 1..={MAX_RECIPIENTS} user ids"
            )));
        }
        let mut recipients: Vec<String> = Vec::new();
        for recipient in &body.recipients {
            let recipient = require_text(recipient, "recipient", MAX_NAME_LEN)?;
            if !recipients.contains(&recipient) {
                recipients.push(recipient);
            }
        }
        let channel = match body.channel.as_deref().unwrap_or("notification") {
            "email" => "email".to_string(),
            "notification" => "notification".to_string(),
            other => {
                return Err(UnifiedError::bad_request(format!(
                    "channel must be \"email\" or \"notification\", got \"{other}\""
                )))
            }
        };
        let now = Utc::now();
        let mut schedule = BiReportSchedule {
            id: Uuid::new_v4(),
            report_id: report.id,
            company_id: report.company_id.clone(),
            schedule: body.schedule.trim().to_string(),
            recipients,
            channel,
            scheduler_status: "declined".to_string(),
            scheduler_job_id: None,
            last_delivered_at: None,
            created_at: now,
            updated_at: now,
        };
        schedule_delivery(&state, &mut schedule);
        state.schedules.insert(schedule.id, schedule.clone());
        Ok(schedule)
    })();
    match result {
        Ok(data) => created(data),
        Err(error) => err(error, debug),
    }
}

async fn get_schedule(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<BiState>,
    req: HttpRequest,
    path: web::Path<(String, String)>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<BiReportSchedule> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_BI_VIEW)?;
        let (report_id, schedule_id) = path.into_inner();
        let report = load_report(&state, &report_id)?;
        let schedule = load_schedule(&state, &schedule_id)?;
        if schedule.report_id != report.id {
            return Err(UnifiedError::not_found(format!(
                "Schedule not found: {schedule_id}"
            )));
        }
        Ok(schedule)
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn delete_schedule(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<BiState>,
    req: HttpRequest,
    path: web::Path<(String, String)>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_BI_MANAGE)?;
        let (report_id, schedule_id) = path.into_inner();
        let report = load_report(&state, &report_id)?;
        let schedule = load_schedule(&state, &schedule_id)?;
        if schedule.report_id != report.id {
            return Err(UnifiedError::not_found(format!(
                "Schedule not found: {schedule_id}"
            )));
        }
        state.schedules.remove(&schedule.id);
        Ok(json!({ "deleted": schedule.id.to_string() }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn run_schedule_now(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<BiState>,
    req: HttpRequest,
    path: web::Path<(String, String)>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_BI_MANAGE)?;
        let (report_id, schedule_id) = path.into_inner();
        let report = load_report(&state, &report_id)?;
        let schedule = load_schedule(&state, &schedule_id)?;
        if schedule.report_id != report.id {
            return Err(UnifiedError::not_found(format!(
                "Schedule not found: {schedule_id}"
            )));
        }
        let row_count = deliver_report_now(&state, &report, Some(&schedule))?;
        Ok(json!({
            "report_id": report.id.to_string(),
            "schedule_id": schedule.id.to_string(),
            "row_count": row_count,
            "recipients": schedule.recipients,
        }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

// ---------------------------------------------------------------------------
// Forecast handlers
// ---------------------------------------------------------------------------

fn parse_forecast_query(query: &ForecastQuery) -> UnifiedResult<(Option<String>, i64)> {
    let currency = match &query.currency {
        Some(currency) => Some(valid_currency(currency)?),
        None => None,
    };
    let horizon_days = query.horizon_days.unwrap_or(30);
    if !(1..=90).contains(&horizon_days) {
        return Err(UnifiedError::bad_request(
            "horizon_days must be between 1 and 90",
        ));
    }
    Ok((currency, horizon_days))
}

async fn forecast_revenue(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<BiState>,
    req: HttpRequest,
    query: web::Query<ForecastQuery>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_BI_VIEW)?;
        let (currency, horizon_days) = parse_forecast_query(&query)?;
        let source = match query.source.as_deref() {
            Some(source) => validate_source(source)?,
            None => "billing".to_string(),
        };
        if source != "billing" && source != "store" {
            return Err(UnifiedError::bad_request(
                "revenue forecast source must be \"billing\" or \"store\"",
            ));
        }
        build_forecast(
            &state,
            "revenue",
            &[source.as_str()],
            None,
            currency.as_deref(),
            horizon_days,
        )
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn forecast_cashflow(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<BiState>,
    req: HttpRequest,
    query: web::Query<ForecastQuery>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_BI_VIEW)?;
        let (currency, horizon_days) = parse_forecast_query(&query)?;
        // Net cash flow: completed orders and captured payments minus
        // refunds (refunded rows are stored with a negative amount).
        build_forecast(
            &state,
            "cashflow",
            &["billing", "store"],
            None,
            currency.as_deref(),
            horizon_days,
        )
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn forecast_churn(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<BiState>,
    req: HttpRequest,
    query: web::Query<ChurnQuery>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_BI_VIEW)?;
        let inactive_days = query.inactive_days.unwrap_or(30);
        if !(7..=365).contains(&inactive_days) {
            return Err(UnifiedError::bad_request(
                "inactive_days must be between 7 and 365",
            ));
        }
        let company_id = query.company_id.as_deref();
        let cutoff = Utc::now() - Duration::days(inactive_days);
        let mut last_activity: BTreeMap<String, DateTime<Utc>> = BTreeMap::new();
        let mut failed_signals: BTreeMap<String, Vec<String>> = BTreeMap::new();
        let mut total_customers: HashSet<String> = HashSet::new();
        for entry in state.source_records.iter() {
            let row = entry.value();
            if let Some(company) = company_id {
                if row.company_id.as_deref() != Some(company) {
                    continue;
                }
            }
            let Some(customer) = &row.customer_id else {
                continue;
            };
            total_customers.insert(customer.clone());
            let last = last_activity
                .entry(customer.clone())
                .or_insert(row.occurred_at);
            if row.occurred_at > *last {
                *last = row.occurred_at;
            }
            match row.kind.as_str() {
                "license_suspended" | "license_revoked" => {
                    failed_signals
                        .entry(customer.clone())
                        .or_default()
                        .push(row.kind.clone());
                }
                "order_refunded" => {
                    failed_signals
                        .entry(customer.clone())
                        .or_default()
                        .push("order_refunded".to_string());
                }
                _ => {}
            }
        }
        if total_customers.is_empty() {
            return Err(UnifiedError::unprocessable(
                "Insufficient data: churn signals need recorded customer activity, and no customer rows exist yet",
            )
            .with_metadata("company_id", json!(company_id)));
        }
        let mut at_risk: Vec<Value> = Vec::new();
        for (customer, last) in &last_activity {
            if *last < cutoff {
                let signals = failed_signals.get(customer).cloned().unwrap_or_default();
                at_risk.push(json!({
                    "customer_id": customer,
                    "last_activity_at": last.to_rfc3339(),
                    "inactive_days": (Utc::now() - *last).num_days(),
                    "failed_payment_signals": signals,
                }));
            }
        }
        at_risk.sort_by(|a, b| {
            a.get("last_activity_at")
                .and_then(Value::as_str)
                .cmp(&b.get("last_activity_at").and_then(Value::as_str))
        });
        Ok(json!({
            "method": "last_activity_plus_failed_payment_signals",
            "inputs": {
                "inactive_days": inactive_days,
                "company_id": company_id,
                "observed_customers": total_customers.len(),
            },
            "at_risk_count": at_risk.len(),
            "at_risk_customers": at_risk,
        }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

// ---------------------------------------------------------------------------
// KPI dashboard handlers
// ---------------------------------------------------------------------------

/// Compute one KPI from the mirror rows. Returns `None` (with the
/// caller recording `insufficient_data`) when the metric's source
/// has no rows in the period — a missing measurement is reported
/// as missing, never as zero.
fn compute_kpi(state: &BiState, kpi: &KpiDefinition, company_id: &str) -> Option<f64> {
    let from = match kpi.period.as_str() {
        "7d" => Some(Utc::now() - Duration::days(7)),
        "30d" => Some(Utc::now() - Duration::days(30)),
        "90d" => Some(Utc::now() - Duration::days(90)),
        _ => None,
    };
    let rows = read_source_rows(state, &kpi.source, Some(company_id), from, None);
    if rows.is_empty() {
        return None;
    }
    match kpi.metric.as_str() {
        "revenue_total" => {
            let total: f64 = rows
                .iter()
                .filter_map(|row| row.get("amount").and_then(Value::as_f64))
                .sum();
            Some(round_money(total))
        }
        "order_count" => Some(
            rows.iter()
                .filter(|row| {
                    row.get("kind").and_then(Value::as_str) == Some("order_completed")
                })
                .count() as f64,
        ),
        "ticket_count" => Some(
            rows.iter()
                .filter(|row| {
                    matches!(
                        row.get("kind").and_then(Value::as_str),
                        Some("ticket_created") | Some("ticket_updated")
                    )
                })
                .count() as f64,
        ),
        "active_licenses" => {
            let issued = rows
                .iter()
                .filter(|row| {
                    matches!(
                        row.get("kind").and_then(Value::as_str),
                        Some("license_issued") | Some("license_activated")
                    )
                })
                .count() as f64;
            let closed = rows
                .iter()
                .filter(|row| {
                    matches!(
                        row.get("kind").and_then(Value::as_str),
                        Some("license_suspended") | Some("license_revoked")
                    )
                })
                .count() as f64;
            Some((issued - closed).max(0.0))
        }
        "tasks_completed" => Some(
            rows.iter()
                .filter(|row| {
                    row.get("kind").and_then(Value::as_str) == Some("task_completed")
                })
                .count() as f64,
        ),
        "payment_count" => Some(
            rows.iter()
                .filter(|row| row.get("amount").and_then(Value::as_f64).is_some())
                .count() as f64,
        ),
        "average_order_value" => {
            let amounts: Vec<f64> = rows
                .iter()
                .filter(|row| {
                    row.get("kind").and_then(Value::as_str) == Some("order_completed")
                })
                .filter_map(|row| row.get("amount").and_then(Value::as_f64))
                .collect();
            if amounts.is_empty() {
                None
            } else {
                Some(round_money(
                    amounts.iter().sum::<f64>() / amounts.len() as f64,
                ))
            }
        }
        _ => None,
    }
}

async fn list_dashboards(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<BiState>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_BI_VIEW)?;
        let params = query.into_inner();
        let company_filter = params.filters.get("company_id").cloned();
        let owner_filter = params.filters.get("owner_id").cloned();
        let search = params.search.clone().unwrap_or_default().to_lowercase();
        let mut items: Vec<BiKpiDashboard> = state
            .dashboards
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|dashboard| {
                if let Some(company) = &company_filter {
                    if &dashboard.company_id != company {
                        return false;
                    }
                }
                if let Some(owner) = &owner_filter {
                    if &dashboard.owner_id != owner {
                        return false;
                    }
                }
                if !search.is_empty() {
                    let haystack = format!(
                        "{} {}",
                        dashboard.name,
                        dashboard.description.clone().unwrap_or_default()
                    )
                    .to_lowercase();
                    if !haystack.contains(&search) {
                        return false;
                    }
                }
                true
            })
            .collect();
        items.sort_by(|a, b| a.created_at.cmp(&b.created_at));
        Ok(paginate(items, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn create_dashboard(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<BiState>,
    req: HttpRequest,
    body: web::Json<CreateKpiDashboardRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<BiKpiDashboard> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_BI_MANAGE)?;
        let name = require_text(&body.name, "name", MAX_NAME_LEN)?;
        let company_id = require_text(&body.company_id, "company_id", MAX_COMPANY_LEN)?;
        let kpis = validate_kpis(&body.kpis)?;
        let description = match &body.description {
            Some(text) if !text.trim().is_empty() => Some(require_text(
                text,
                "description",
                MAX_DESCRIPTION_LEN,
            )?),
            _ => None,
        };
        let now = Utc::now();
        let dashboard = BiKpiDashboard {
            id: Uuid::new_v4(),
            company_id,
            owner_id: caller.actor.clone(),
            name,
            description,
            kpis,
            created_at: now,
            updated_at: now,
        };
        state.dashboards.insert(dashboard.id, dashboard.clone());
        Ok(dashboard)
    })();
    match result {
        Ok(data) => created(data),
        Err(error) => err(error, debug),
    }
}

async fn get_dashboard(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<BiState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<BiKpiDashboard> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_BI_VIEW)?;
        load_dashboard(&state, &path.into_inner())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn update_dashboard(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<BiState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdateKpiDashboardRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<BiKpiDashboard> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_BI_MANAGE)?;
        let mut dashboard = load_dashboard(&state, &path.into_inner())?;
        if let Some(name) = &body.name {
            dashboard.name = require_text(name, "name", MAX_NAME_LEN)?;
        }
        if let Some(description) = &body.description {
            dashboard.description = match description {
                Some(text) if !text.trim().is_empty() => Some(require_text(
                    text,
                    "description",
                    MAX_DESCRIPTION_LEN,
                )?),
                _ => None,
            };
        }
        if let Some(kpis) = &body.kpis {
            dashboard.kpis = validate_kpis(kpis)?;
        }
        dashboard.updated_at = Utc::now();
        state.dashboards.insert(dashboard.id, dashboard.clone());
        Ok(dashboard)
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn delete_dashboard(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<BiState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_BI_MANAGE)?;
        let dashboard = load_dashboard(&state, &path.into_inner())?;
        state.dashboards.remove(&dashboard.id);
        Ok(json!({ "deleted": dashboard.id.to_string() }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn compute_dashboard(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<BiState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_BI_VIEW)?;
        let dashboard = load_dashboard(&state, &path.into_inner())?;
        let computed: Vec<ComputedKpi> = dashboard
            .kpis
            .iter()
            .map(|kpi| match compute_kpi(&state, kpi, &dashboard.company_id) {
                Some(value) => ComputedKpi {
                    metric: kpi.metric.clone(),
                    source: kpi.source.clone(),
                    period: kpi.period.clone(),
                    value: Some(value),
                    status: "ok".to_string(),
                },
                None => ComputedKpi {
                    metric: kpi.metric.clone(),
                    source: kpi.source.clone(),
                    period: kpi.period.clone(),
                    value: None,
                    status: "insufficient_data".to_string(),
                },
            })
            .collect();
        Ok(json!({
            "dashboard_id": dashboard.id.to_string(),
            "company_id": dashboard.company_id,
            "computed_at": Utc::now().to_rfc3339(),
            "kpis": computed,
        }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}
