// Analytics and reports — sales, licenses, usage, uptime and revenue.
//
// Routes under `/api/v1/analytics`:
//
//   Dashboard and summaries:
//     GET    /dashboard              key numbers: revenue 30d, active
//                                    licenses, uptime %, open tasks,
//                                    capacity headroom
//     GET    /sales                  recorded sales and refunds (paginated)
//     POST   /sales                  record a sale or refund (fallback
//                                    when no store event arrives)
//     GET    /sales/summary          revenue by day / product / currency
//     GET    /licenses/summary       license counts by status and product
//     GET    /usage/summary          API calls, downloads, logins by kind
//     GET    /usage                  recorded metric samples (paginated)
//     POST   /metrics                record one metric sample
//     GET    /uptime/summary         uptime % from health samples
//     GET    /health                 recorded health samples (paginated)
//     POST   /health                 record one health sample
//     GET    /revenue/summary        totals by currency, refunds deducted
//   Report definitions:
//     GET    /reports                list report definitions (paginated)
//     POST   /reports                create a report definition
//     GET    /reports/{id}           one definition
//     PUT    /reports/{id}           update a definition
//     DELETE /reports/{id}           delete a definition (and its runs)
//     POST   /reports/{id}/run       generate a report run now
//     GET    /reports/{id}/runs      runs of a definition (paginated)
//     GET    /reports/{id}/export    download the latest run as CSV or JSON
//   Report runs:
//     GET    /runs/{id}              one run, rows paginated
//   Scheduled reports:
//     GET    /scheduled-reports      list scheduled reports (paginated)
//     POST   /scheduled-reports      create a scheduled report
//     GET    /scheduled-reports/{id}  one scheduled report
//     PUT    /scheduled-reports/{id}  update a scheduled report
//     DELETE /scheduled-reports/{id}  delete a scheduled report
//     POST   /scheduled-reports/{id}/run-now  run now and enqueue the
//                                    scheduler job (see note below)
//
// Events consumed (listeners registered in `register_hooks`):
//
//   store.order.completed   -> sale record (gross revenue)
//   store.order.refunded    -> refund record (deducted from revenue)
//   license.issued          -> license snapshot, status active
//   license.activated       -> license snapshot, status active
//   license.suspended       -> license snapshot, status suspended
//   license.revoked         -> license snapshot, status revoked
//   download.finished       -> metric sample, kind `download`
//   auth.login              -> metric sample, kind `login`
//   cluster.health.changed  -> health sample for the node
//   task.created / task.status.changed / task.completed
//                           -> open-task mirror for the dashboard
//   planner.pool.created / planner.plan.approved /
//   planner.plan.activated / planner.plan.expired
//                           -> capacity mirror for dashboard headroom
//   analytics.report.run_requested
//                           -> run a report (integration point for the
//                             scheduler once `report.generate` is a
//                             known scheduler job kind)
//
// Notes on the sources, stated honestly:
//
//   - The store module swaps its private hook bus for the global bus in
//     its `init`, so `store.order.*` events do reach the global bus and
//     these listeners see them. When no store event arrives (for
//     example sales imported from elsewhere), POST /sales records the
//     same shape manually.
//   - `license.deactivated` is deliberately NOT mapped to a license
//     status: the licensing module emits it when one activation (one
//     device) is removed, not when the license itself changes state.
//   - The service monitor emits no health events today, so uptime comes
//     from cluster health events plus POST /health samples. With no
//     samples at all the dashboard reports `uptime_pct: null` instead
//     of inventing a number.
//   - Scheduled reports: the scheduler only accepts the job kinds
//     `mail.send`, `geo.update`, `ssl.renew` and `cleanup` today, so
//     `scheduler.enqueue` with kind `report.generate` is refused until
//     the scheduler gains that kind (snippet in the coordinator
//     report). Run-now still runs the report synchronously, so it
//     always produces a run; the enqueue result is reported in the
//     response instead of being hidden.
//
// Events emitted on the global hook bus:
//
//   analytics.report.generated   a report run finished
//   analytics.report.exported    a report export was downloaded
//
// Storage is in memory (DashMap), the same pattern as the store and
// scheduler modules. Durable persistence is a follow-up for the
// integrator: the migration sketch is in the coordinator report.

use crate::core::error::{UnifiedError, UnifiedResult};
use crate::core::hooks::{events, HookBus, HookContext};
use crate::core::json;
use crate::core::modules::{Module, ModuleRegistry, RouteInfo};
use crate::core::pagination::{PageResult, PaginationParams};
use actix_web::{http::header, web, HttpResponse};
use chrono::{DateTime, Datelike, Timelike, Utc};
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use std::collections::HashMap;
use std::sync::{Arc, Mutex, RwLock};
use uuid::Uuid;

// ---------------------------------------------------------------------------
// Event names.
// ---------------------------------------------------------------------------

pub const ANALYTICS_REPORT_GENERATED: &str = "analytics.report.generated";
pub const ANALYTICS_REPORT_EXPORTED: &str = "analytics.report.exported";
pub const ANALYTICS_REPORT_RUN_REQUESTED: &str = "analytics.report.run_requested";

const STORE_ORDER_COMPLETED: &str = "store.order.completed";
const STORE_ORDER_REFUNDED: &str = "store.order.refunded";
const LICENSE_ACTIVATED: &str = "license.activated";
const LICENSE_SUSPENDED: &str = "license.suspended";
const CLUSTER_HEALTH_CHANGED: &str = "cluster.health.changed";
const TASK_CREATED: &str = "task.created";
const TASK_STATUS_CHANGED: &str = "task.status.changed";
const TASK_COMPLETED: &str = "task.completed";
const PLANNER_POOL_CREATED: &str = "planner.pool.created";
const PLANNER_PLAN_APPROVED: &str = "planner.plan.approved";
const PLANNER_PLAN_ACTIVATED: &str = "planner.plan.activated";
const PLANNER_PLAN_EXPIRED: &str = "planner.plan.expired";
const SCHEDULER_ENQUEUE: &str = "scheduler.enqueue";

// ---------------------------------------------------------------------------
// Helpers.
// ---------------------------------------------------------------------------

fn new_id() -> String {
    Uuid::new_v4().to_string()
}

fn now() -> DateTime<Utc> {
    Utc::now()
}

fn respond_created<T: Serialize>(result: UnifiedResult<T>, debug: bool) -> HttpResponse {
    match result {
        Ok(data) => json::success_response(data, 201),
        Err(error) => json::error_response(&error, debug),
    }
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

fn round_money(value: f64) -> f64 {
    (value * 100.0).round() / 100.0
}

fn require_name(name: &str) -> UnifiedResult<String> {
    let name = name.trim().to_string();
    if name.is_empty() {
        return Err(UnifiedError::bad_request("name must not be empty"));
    }
    if name.chars().count() > 200 {
        return Err(UnifiedError::bad_request("name must be at most 200 characters"));
    }
    Ok(name)
}

fn valid_currency(currency: &str) -> bool {
    currency.len() == 3 && currency.chars().all(|c| c.is_ascii_alphabetic())
}

fn parse_datetime(value: &str, field: &str) -> UnifiedResult<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(value)
        .map(|parsed| parsed.with_timezone(&Utc))
        .map_err(|_| UnifiedError::bad_request(format!("{field} must be an RFC 3339 date-time")))
}

/// Resolve a date range from `from`/`to` (RFC 3339) or a `range` preset
/// (`7d`, `30d`, `90d`, `365d`). Defaults to the last 30 days.
fn resolve_range(from: Option<&str>, to: Option<&str>, range: Option<&str>) -> UnifiedResult<(DateTime<Utc>, DateTime<Utc>)> {
    let end = match to {
        Some(value) => parse_datetime(value, "to")?,
        None => now(),
    };
    let start = match (from, range) {
        (Some(value), _) => parse_datetime(value, "from")?,
        (None, Some(preset)) => {
            let days: i64 = match preset {
                "7d" => 7,
                "30d" => 30,
                "90d" => 90,
                "365d" => 365,
                _ => {
                    return Err(UnifiedError::bad_request(
                        "range must be one of: 7d, 30d, 90d, 365d",
                    ))
                }
            };
            end - chrono::Duration::days(days)
        }
        (None, None) => end - chrono::Duration::days(30),
    };
    if start >= end {
        return Err(UnifiedError::bad_request("from must be before to"));
    }
    Ok((start, end))
}

fn day_key(moment: DateTime<Utc>) -> String {
    moment.format("%Y-%m-%d").to_string()
}

/// A schedule is either an interval in seconds (`"300"`) or five
/// cron-like fields (`"*/5 * * * *"`), where each field is `*`, `*/N`,
/// a plain number, or a comma list of plain numbers. Same rule as the
/// scheduler module's `valid_schedule`.
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
        field
            .split(',')
            .all(|part| part.parse::<u32>().is_ok())
    })
}

fn field_matches(field: &str, value: u32) -> bool {
    if field == "*" {
        return true;
    }
    if let Some(step) = field.strip_prefix("*/") {
        if let Ok(step) = step.parse::<u32>() {
            return step > 0 && value % step == 0;
        }
        return false;
    }
    field
        .split(',')
        .any(|part| part.parse::<u32>().is_ok_and(|number| number == value))
}

/// Next run time for a schedule, searching minute by minute up to 8 days
/// ahead. Interval schedules (`"300"`) resolve exactly; cron-like
/// schedules resolve to the next matching minute. Returns `None` when
/// no match is found inside the search window.
fn next_run_at(schedule: &str, from: DateTime<Utc>) -> Option<DateTime<Utc>> {
    let schedule = schedule.trim();
    if let Ok(seconds) = schedule.parse::<i64>() {
        if seconds > 0 {
            return Some(from + chrono::Duration::seconds(seconds));
        }
        return None;
    }
    let fields: Vec<&str> = schedule.split_whitespace().collect();
    if fields.len() != 5 {
        return None;
    }
    let mut candidate = from + chrono::Duration::minutes(1);
    candidate = candidate
        .with_second(0)
        .and_then(|moment| moment.with_nanosecond(0))
        .unwrap_or(candidate);
    for _ in 0..(8 * 24 * 60) {
        let minute = candidate.minute();
        let hour = candidate.hour();
        let day = candidate.day();
        let month = candidate.month();
        // chrono weekday: Monday = 1 .. Sunday = 7; cron uses 0/7 = Sunday.
        let weekday = candidate.weekday().number_from_sunday();
        if field_matches(fields[0], minute)
            && field_matches(fields[1], hour)
            && field_matches(fields[2], day)
            && field_matches(fields[3], month)
            && field_matches(fields[4], weekday)
        {
            return Some(candidate);
        }
        candidate += chrono::Duration::minutes(1);
    }
    None
}

// ---------------------------------------------------------------------------
// Types.
// ---------------------------------------------------------------------------

/// One recorded sale or refund. Sales come from `store.order.completed`
/// events; refunds from `store.order.refunded` events; both can also be
/// recorded manually through POST /sales.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SaleRecord {
    pub id: String,
    /// Store order id, when the record came from a store event.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub order_id: Option<String>,
    /// `sale` or `refund`.
    pub kind: String,
    pub total: f64,
    pub currency: String,
    #[serde(default)]
    pub products: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub company_id: Option<String>,
    pub recorded_at: DateTime<Utc>,
    pub created_at: DateTime<Utc>,
}

/// Mirror of one license, maintained from license events. The dashboard
/// and the license summary count from this mirror, so they stay correct
/// without calling the licensing module directly.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LicenseSnapshot {
    pub license_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub license_key: Option<String>,
    #[serde(default)]
    pub product: String,
    /// `active`, `suspended`, `revoked`, `expired`.
    pub status: String,
    pub last_event_at: DateTime<Utc>,
    pub created_at: DateTime<Utc>,
}

/// One recorded usage metric: an API call, a download, a login, or an
/// upload.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MetricSample {
    pub id: String,
    /// `api_call`, `download`, `login`, `upload`.
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    pub recorded_at: DateTime<Utc>,
    pub created_at: DateTime<Utc>,
}

/// One recorded health observation for a node or service.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HealthSample {
    pub id: String,
    pub node_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cluster_id: Option<String>,
    /// `healthy`, `degraded`, `unhealthy`, `unknown`.
    pub status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub latency_ms: Option<u64>,
    pub checked_at: DateTime<Utc>,
    pub created_at: DateTime<Utc>,
}

/// A report definition: which metrics, over which range, grouped by
/// which dimensions.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReportDefinition {
    pub id: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Subset of: revenue, refunds, licenses, usage, uptime,
    /// sales_by_product.
    pub metrics: Vec<String>,
    /// Subset of: day, product, currency, status, kind.
    #[serde(default)]
    pub dimensions: Vec<String>,
    /// Preset range: `7d`, `30d`, `90d`, `365d`. Ignored when explicit
    /// start/end are set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub range_preset: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub end: Option<DateTime<Utc>>,
    /// Extra filters, for example {"currency": "USD", "product": "…"}.
    #[serde(default)]
    pub filters: Value,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// One generated report run with its result rows.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReportRun {
    pub id: String,
    pub definition_id: String,
    /// `completed` or `failed`.
    pub status: String,
    pub started_at: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finished_at: Option<DateTime<Utc>>,
    pub row_count: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    pub rows: Vec<Value>,
}

/// A report definition on a schedule.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScheduledReport {
    pub id: String,
    pub definition_id: String,
    /// Interval in seconds (`"3600"`) or cron-like (`"0 8 * * *"`).
    pub schedule: String,
    pub enabled: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_run_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_run_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Dashboard mirror of one task, fed by task events.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct TaskMirror {
    task_id: String,
    status: String,
    updated_at: DateTime<Utc>,
}

/// Dashboard mirror of one capacity pool, fed by planner events.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct PoolMirror {
    pool_id: String,
    pool_name: String,
    capacity: PoolResources,
    /// Reservations per plan id, so approve/activate/expire updates
    /// stay idempotent.
    #[serde(skip)]
    allocations: Mutex<HashMap<String, PoolResources>>,
}

/// Planner resource bundle, mirrored locally so the dashboard can
/// report headroom without calling the planner module.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
struct PoolResources {
    #[serde(default)]
    cpu_cores: f64,
    #[serde(default)]
    ram_gb: f64,
    #[serde(default)]
    disk_gb: f64,
    #[serde(default)]
    bandwidth_tb: f64,
}

impl PoolResources {
    fn from_value(value: &Value) -> Self {
        let number = |key: &str| value.get(key).and_then(|item| item.as_f64()).unwrap_or(0.0);
        Self {
            cpu_cores: number("cpu_cores"),
            ram_gb: number("ram_gb"),
            disk_gb: number("disk_gb"),
            bandwidth_tb: number("bandwidth_tb"),
        }
    }

    fn total_allocated(&self, allocations: &HashMap<String, PoolResources>) -> PoolResources {
        let mut total = PoolResources::default();
        for reserved in allocations.values() {
            total.cpu_cores += reserved.cpu_cores;
            total.ram_gb += reserved.ram_gb;
            total.disk_gb += reserved.disk_gb;
            total.bandwidth_tb += reserved.bandwidth_tb;
        }
        total
    }
}

// ---------------------------------------------------------------------------
// Views.
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
struct DashboardView {
    revenue_30d: HashMap<String, f64>,
    revenue_30d_orders: u64,
    refunds_30d: HashMap<String, f64>,
    active_licenses: u64,
    licenses_by_status: HashMap<String, u64>,
    uptime_pct_30d: Option<f64>,
    uptime_checks_30d: u64,
    open_tasks: u64,
    capacity_headroom: CapacityHeadroomView,
    generated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize)]
struct CapacityHeadroomView {
    pools_tracked: u64,
    lowest_headroom_pct: Option<f64>,
    pools_overcommitted: u64,
    pools: Vec<PoolHeadroomView>,
}

#[derive(Debug, Clone, Serialize)]
struct PoolHeadroomView {
    pool_id: String,
    pool_name: String,
    headroom_pct: Option<f64>,
    overcommitted: bool,
}

#[derive(Debug, Clone, Serialize)]
struct LicenseSummaryView {
    total: u64,
    by_status: HashMap<String, u64>,
    by_product: Vec<ProductLicenseCount>,
}

#[derive(Debug, Clone, Serialize)]
struct ProductLicenseCount {
    product: String,
    total: u64,
    active: u64,
    suspended: u64,
    revoked: u64,
    expired: u64,
}

#[derive(Debug, Clone, Serialize)]
struct UsageSummaryView {
    from: DateTime<Utc>,
    to: DateTime<Utc>,
    total: u64,
    by_kind: HashMap<String, u64>,
    by_day: Vec<DayCount>,
}

#[derive(Debug, Clone, Serialize)]
struct DayCount {
    day: String,
    count: u64,
}

#[derive(Debug, Clone, Serialize)]
struct UptimeSummaryView {
    from: DateTime<Utc>,
    to: DateTime<Utc>,
    checks: u64,
    healthy: u64,
    degraded: u64,
    unhealthy: u64,
    unknown: u64,
    uptime_pct: Option<f64>,
    by_day: Vec<DayUptime>,
}

#[derive(Debug, Clone, Serialize)]
struct DayUptime {
    day: String,
    checks: u64,
    uptime_pct: Option<f64>,
}

#[derive(Debug, Clone, Serialize)]
struct RevenueSummaryView {
    from: DateTime<Utc>,
    to: DateTime<Utc>,
    orders: u64,
    refunds: u64,
    by_currency: Vec<CurrencyRevenue>,
}

#[derive(Debug, Clone, Serialize)]
struct CurrencyRevenue {
    currency: String,
    gross: f64,
    refunds: f64,
    net: f64,
}

#[derive(Debug, Clone, Serialize)]
struct RunNowView {
    run: ReportRun,
    scheduler_queued: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    scheduler_error: Option<String>,
    scheduler_note: String,
}

// ---------------------------------------------------------------------------
// Request types.
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct RecordSaleRequest {
    #[serde(default)]
    order_id: Option<String>,
    kind: String,
    total: f64,
    #[serde(default)]
    currency: Option<String>,
    #[serde(default)]
    products: Option<Vec<String>>,
    #[serde(default)]
    company_id: Option<String>,
    #[serde(default)]
    recorded_at: Option<String>,
}

#[derive(Debug, Deserialize)]
struct RecordMetricRequest {
    kind: String,
    #[serde(default)]
    source: Option<String>,
    #[serde(default)]
    recorded_at: Option<String>,
}

#[derive(Debug, Deserialize)]
struct RecordHealthRequest {
    node_id: String,
    #[serde(default)]
    cluster_id: Option<String>,
    status: String,
    #[serde(default)]
    latency_ms: Option<u64>,
    #[serde(default)]
    checked_at: Option<String>,
}

#[derive(Debug, Deserialize)]
struct SummaryQuery {
    #[serde(default)]
    from: Option<String>,
    #[serde(default)]
    to: Option<String>,
    #[serde(default)]
    range: Option<String>,
    #[serde(default)]
    kind: Option<String>,
    #[serde(default)]
    node_id: Option<String>,
    #[serde(default)]
    cluster_id: Option<String>,
}

#[derive(Debug, Deserialize)]
struct CreateReportRequest {
    name: String,
    #[serde(default)]
    description: Option<String>,
    metrics: Vec<String>,
    #[serde(default)]
    dimensions: Option<Vec<String>>,
    #[serde(default)]
    range_preset: Option<String>,
    #[serde(default)]
    start: Option<String>,
    #[serde(default)]
    end: Option<String>,
    #[serde(default)]
    filters: Option<Value>,
}

#[derive(Debug, Deserialize)]
struct UpdateReportRequest {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    description: Option<Option<String>>,
    #[serde(default)]
    metrics: Option<Vec<String>>,
    #[serde(default)]
    dimensions: Option<Vec<String>>,
    #[serde(default)]
    range_preset: Option<Option<String>>,
    #[serde(default)]
    start: Option<Option<String>>,
    #[serde(default)]
    end: Option<Option<String>>,
    #[serde(default)]
    filters: Option<Value>,
}

#[derive(Debug, Deserialize)]
struct CreateScheduledRequest {
    definition_id: String,
    schedule: String,
    #[serde(default)]
    enabled: Option<bool>,
}

#[derive(Debug, Deserialize)]
struct UpdateScheduledRequest {
    #[serde(default)]
    schedule: Option<String>,
    #[serde(default)]
    enabled: Option<bool>,
}

#[derive(Debug, Deserialize)]
struct ExportQuery {
    #[serde(default)]
    format: Option<String>,
    #[serde(default)]
    run_id: Option<String>,
}

// ---------------------------------------------------------------------------
// State.
// ---------------------------------------------------------------------------

pub struct AnalyticsState {
    /// Hook bus, replaced with the global bus in `init` (same pattern
    /// as the store and scheduler modules) so hook listeners can emit
    /// events of their own.
    hooks: RwLock<Arc<HookBus>>,
    sales: DashMap<String, SaleRecord>,
    licenses: DashMap<String, LicenseSnapshot>,
    metrics: DashMap<String, MetricSample>,
    health: DashMap<String, HealthSample>,
    definitions: DashMap<String, ReportDefinition>,
    runs: DashMap<String, ReportRun>,
    scheduled: DashMap<String, ScheduledReport>,
    tasks: DashMap<String, TaskMirror>,
    pools: DashMap<String, PoolMirror>,
}

impl AnalyticsState {
    fn new() -> Self {
        Self {
            hooks: RwLock::new(Arc::new(HookBus::new())),
            sales: DashMap::new(),
            licenses: DashMap::new(),
            metrics: DashMap::new(),
            health: DashMap::new(),
            definitions: DashMap::new(),
            runs: DashMap::new(),
            scheduled: DashMap::new(),
            tasks: DashMap::new(),
            pools: DashMap::new(),
        }
    }

    fn bus(&self) -> Arc<HookBus> {
        self.hooks.read().expect("hooks lock poisoned").clone()
    }

    fn definition(&self, id: &str) -> UnifiedResult<ReportDefinition> {
        self.definitions
            .get(id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found(format!("Report definition \"{id}\" not found")))
    }
}

// ---------------------------------------------------------------------------
// Hook listeners: ingest events from other modules into local records.
// ---------------------------------------------------------------------------

fn record_sale_from_order(state: &Arc<AnalyticsState>, order: &Value, kind: &str) {
    let Some(order_id) = order.get("id").and_then(|value| value.as_str()) else {
        return;
    };
    if state.sales.iter().any(|entry| {
        entry.value().order_id.as_deref() == Some(order_id) && entry.value().kind == kind
    }) {
        return;
    }
    let total = order.get("total").and_then(|value| value.as_f64()).unwrap_or(0.0);
    let currency = order
        .get("currency")
        .and_then(|value| value.as_str())
        .unwrap_or("USD")
        .to_uppercase();
    let products: Vec<String> = order
        .get("items")
        .and_then(|value| value.as_array())
        .map(|items| {
            items
                .iter()
                .filter_map(|item| item.get("name").and_then(|value| value.as_str()).map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    let company_id = order
        .get("company_id")
        .and_then(|value| value.as_str())
        .map(str::to_string);
    let recorded_at = order
        .get("updated_at")
        .and_then(|value| value.as_str())
        .and_then(|value| DateTime::parse_from_rfc3339(value).ok())
        .map(|parsed| parsed.with_timezone(&Utc))
        .unwrap_or_else(now);
    let record = SaleRecord {
        id: new_id(),
        order_id: Some(order_id.to_string()),
        kind: kind.to_string(),
        total: round_money(total),
        currency,
        products,
        company_id,
        recorded_at,
        created_at: now(),
    };
    state.sales.insert(record.id.clone(), record);
}

fn upsert_license(state: &Arc<AnalyticsState>, data: &Value, status: &str) {
    let Some(license_id) = data
        .get("license_id")
        .and_then(|value| value.as_str())
        .map(str::to_string)
        .or_else(|| {
            data.get("id")
                .and_then(|value| value.as_str())
                .map(str::to_string)
        })
    else {
        return;
    };
    let product = data
        .get("product")
        .and_then(|value| value.as_str())
        .unwrap_or("")
        .to_string();
    let license_key = data
        .get("license_key")
        .and_then(|value| value.as_str())
        .map(str::to_string);
    match state.licenses.get_mut(&license_id) {
        Some(mut entry) => {
            entry.status = status.to_string();
            if !product.is_empty() {
                entry.product = product;
            }
            if license_key.is_some() {
                entry.license_key = license_key;
            }
            entry.last_event_at = now();
        }
        None => {
            let snapshot = LicenseSnapshot {
                license_id: license_id.clone(),
                license_key,
                product,
                status: status.to_string(),
                last_event_at: now(),
                created_at: now(),
            };
            state.licenses.insert(license_id, snapshot);
        }
    }
}

fn record_metric(state: &Arc<AnalyticsState>, kind: &str, source: Option<String>) {
    let sample = MetricSample {
        id: new_id(),
        kind: kind.to_string(),
        source,
        recorded_at: now(),
        created_at: now(),
    };
    state.metrics.insert(sample.id.clone(), sample);
}

fn on_store_order_completed(state: Arc<AnalyticsState>, data: &Value) {
    record_sale_from_order(&state, data, "sale");
}

fn on_store_order_refunded(state: Arc<AnalyticsState>, data: &Value) {
    if let Some(order) = data.get("order") {
        record_sale_from_order(&state, order, "refund");
    }
}

fn on_cluster_health_changed(state: Arc<AnalyticsState>, data: &Value) {
    let Some(node_id) = data.get("node_id").and_then(|value| value.as_str()) else {
        return;
    };
    let status = data
        .get("health")
        .and_then(|value| value.as_str())
        .unwrap_or("unknown")
        .to_string();
    if !matches!(status.as_str(), "healthy" | "degraded" | "unhealthy" | "unknown") {
        return;
    }
    let sample = HealthSample {
        id: new_id(),
        node_id: node_id.to_string(),
        cluster_id: data
            .get("cluster_id")
            .and_then(|value| value.as_str())
            .map(str::to_string),
        status,
        latency_ms: data.get("latency_ms").and_then(|value| value.as_u64()),
        checked_at: now(),
        created_at: now(),
    };
    state.health.insert(sample.id.clone(), sample);
}

fn on_task_event(state: Arc<AnalyticsState>, data: &Value) {
    let Some(task_id) = data
        .get("task_id")
        .and_then(|value| value.as_str())
        .or_else(|| data.get("id").and_then(|value| value.as_str()))
    else {
        return;
    };
    let status = data
        .get("status")
        .and_then(|value| value.as_str())
        .unwrap_or("todo")
        .to_string();
    state.tasks.insert(
        task_id.to_string(),
        TaskMirror {
            task_id: task_id.to_string(),
            status,
            updated_at: now(),
        },
    );
}

fn on_planner_pool_created(state: Arc<AnalyticsState>, data: &Value) {
    let Some(pool_id) = data.get("id").and_then(|value| value.as_str()) else {
        return;
    };
    let capacity = data
        .get("capacity")
        .map(PoolResources::from_value)
        .unwrap_or_default();
    let pool_name = data
        .get("name")
        .and_then(|value| value.as_str())
        .unwrap_or("")
        .to_string();
    match state.pools.get_mut(pool_id) {
        Some(mut entry) => {
            entry.capacity = capacity;
            entry.pool_name = pool_name;
        }
        None => {
            state.pools.insert(
                pool_id.to_string(),
                PoolMirror {
                    pool_id: pool_id.to_string(),
                    pool_name,
                    capacity,
                    allocations: Mutex::new(HashMap::new()),
                },
            );
        }
    }
}

fn on_planner_plan_event(state: Arc<AnalyticsState>, data: &Value, active: bool) {
    let Some(pool_id) = data.get("pool_id").and_then(|value| value.as_str()) else {
        return;
    };
    let Some(plan_id) = data.get("id").and_then(|value| value.as_str()) else {
        return;
    };
    let Some(pool) = state.pools.get(pool_id) else {
        return;
    };
    let mut allocations = pool.allocations.lock().expect("pool allocations lock poisoned");
    if active {
        let reserved = data
            .get("reserved")
            .map(PoolResources::from_value)
            .unwrap_or_default();
        allocations.insert(plan_id.to_string(), reserved);
    } else {
        allocations.remove(plan_id);
    }
}

/// Run a report when `analytics.report.run_requested` arrives (the hook
/// the scheduler dispatches `report.generate` jobs through). Emits
/// `analytics.report.generated` with the finished run.
fn on_report_run_requested(state: Arc<AnalyticsState>, data: &Value) {
    let Some(definition_id) = data.get("definition_id").and_then(|value| value.as_str()) else {
        return;
    };
    if let Ok(run) = run_report(&state, definition_id) {
        state.bus().emit(
            ANALYTICS_REPORT_GENERATED,
            serde_json::to_value(&run).unwrap_or(Value::Null),
        );
    }
}

// ---------------------------------------------------------------------------
// Report computation.
// ---------------------------------------------------------------------------

const VALID_METRICS: [&str; 6] = [
    "revenue",
    "refunds",
    "licenses",
    "usage",
    "uptime",
    "sales_by_product",
];

const VALID_DIMENSIONS: [&str; 5] = ["day", "product", "currency", "status", "kind"];

fn validate_metrics(metrics: &[String]) -> UnifiedResult<()> {
    if metrics.is_empty() {
        return Err(UnifiedError::bad_request("metrics must not be empty"));
    }
    for metric in metrics {
        if !VALID_METRICS.contains(&metric.as_str()) {
            return Err(UnifiedError::bad_request(format!(
                "Unknown metric \"{metric}\". Known metrics: {}",
                VALID_METRICS.join(", ")
            )));
        }
    }
    Ok(())
}

fn validate_dimensions(dimensions: &[String]) -> UnifiedResult<()> {
    for dimension in dimensions {
        if !VALID_DIMENSIONS.contains(&dimension.as_str()) {
            return Err(UnifiedError::bad_request(format!(
                "Unknown dimension \"{dimension}\". Known dimensions: {}",
                VALID_DIMENSIONS.join(", ")
            )));
        }
    }
    Ok(())
}

fn resolve_definition_range(definition: &ReportDefinition) -> UnifiedResult<(DateTime<Utc>, DateTime<Utc>)> {
    match (definition.start, definition.end) {
        (Some(start), Some(end)) => {
            if start >= end {
                return Err(UnifiedError::bad_request("start must be before end"));
            }
            Ok((start, end))
        }
        _ => {
            let preset = definition.range_preset.as_deref().unwrap_or("30d");
            let end = now();
            let days: i64 = match preset {
                "7d" => 7,
                "30d" => 30,
                "90d" => 90,
                "365d" => 365,
                _ => {
                    return Err(UnifiedError::bad_request(
                        "range_preset must be one of: 7d, 30d, 90d, 365d",
                    ))
                }
            };
            Ok((end - chrono::Duration::days(days), end))
        }
    }
}

fn filter_currency(definition: &ReportDefinition) -> Option<String> {
    definition
        .filters
        .get("currency")
        .and_then(|value| value.as_str())
        .map(str::to_string)
}

fn filter_product(definition: &ReportDefinition) -> Option<String> {
    definition
        .filters
        .get("product")
        .and_then(|value| value.as_str())
        .map(str::to_string)
}

fn revenue_rows(
    state: &AnalyticsState,
    definition: &ReportDefinition,
    start: DateTime<Utc>,
    end: DateTime<Utc>,
    refunds_only: bool,
) -> Vec<Value> {
    let by_day = definition.dimensions.iter().any(|dimension| dimension == "day");
    let by_currency = definition.dimensions.iter().any(|dimension| dimension == "currency");
    let by_product = definition.dimensions.iter().any(|dimension| dimension == "product");
    let currency_filter = filter_currency(definition);
    let product_filter = filter_product(definition);
    // Group key -> (gross, refunds, orders, refund_count).
    let mut groups: HashMap<String, (f64, f64, u64, u64)> = HashMap::new();
    let mut labels: HashMap<String, Value> = HashMap::new();
    for entry in state.sales.iter() {
        let sale = entry.value();
        if sale.recorded_at < start || sale.recorded_at > end {
            continue;
        }
        if let Some(currency) = &currency_filter {
            if sale.currency != *currency {
                continue;
            }
        }
        if let Some(product) = &product_filter {
            if !sale.products.iter().any(|name| name == product) {
                continue;
            }
        }
        if refunds_only && sale.kind != "refund" {
            continue;
        }
        if !refunds_only && sale.kind != "sale" && sale.kind != "refund" {
            continue;
        }
        let product = sale.products.first().cloned().unwrap_or_default();
        let mut key_parts: Vec<String> = Vec::new();
        let mut label = Map::new();
        if by_day {
            key_parts.push(day_key(sale.recorded_at));
            label.insert("day".to_string(), Value::String(day_key(sale.recorded_at)));
        }
        if by_currency {
            key_parts.push(sale.currency.clone());
            label.insert("currency".to_string(), Value::String(sale.currency.clone()));
        }
        if by_product {
            key_parts.push(product.clone());
            label.insert("product".to_string(), Value::String(product));
        }
        if key_parts.is_empty() {
            key_parts.push("all".to_string());
        }
        let key = key_parts.join("|");
        labels.entry(key.clone()).or_insert_with(|| Value::Object(label));
        let group = groups.entry(key).or_insert((0.0, 0.0, 0, 0));
        if sale.kind == "sale" {
            group.0 += sale.total;
            group.2 += 1;
        } else {
            group.1 += sale.total;
            group.3 += 1;
        }
    }
    let mut rows: Vec<Value> = Vec::new();
    for (key, (gross, refunds, orders, refund_count)) in groups {
        let mut row = labels.remove(&key).unwrap_or(Value::Null);
        if let Value::Object(map) = &mut row {
            map.insert("metric".to_string(), Value::String(if refunds_only { "refunds".to_string() } else { "revenue".to_string() }));
            map.insert("gross".to_string(), Value::from(round_money(gross)));
            map.insert("refunds".to_string(), Value::from(round_money(refunds)));
            map.insert("net".to_string(), Value::from(round_money(gross - refunds)));
            map.insert("orders".to_string(), Value::from(orders));
            map.insert("refund_count".to_string(), Value::from(refund_count));
        }
        rows.push(row);
    }
    rows.sort_by(|a, b| {
        a.get("day")
            .and_then(|value| value.as_str())
            .cmp(&b.get("day").and_then(|value| value.as_str()))
    });
    rows
}

fn license_rows(state: &AnalyticsState, definition: &ReportDefinition) -> Vec<Value> {
    let by_status = definition.dimensions.iter().any(|dimension| dimension == "status");
    let product_filter = filter_product(definition);
    let status_filter = definition
        .filters
        .get("status")
        .and_then(|value| value.as_str())
        .map(str::to_string);
    let mut groups: HashMap<String, (String, String, u64)> = HashMap::new();
    for entry in state.licenses.iter() {
        let license = entry.value();
        if let Some(product) = &product_filter {
            if license.product != *product {
                continue;
            }
        }
        if let Some(status) = &status_filter {
            if license.status != *status {
                continue;
            }
        }
        let key = if by_status {
            format!("{}|{}", license.product, license.status)
        } else {
            license.product.clone()
        };
        let group = groups
            .entry(key)
            .or_insert_with(|| (license.product.clone(), license.status.clone(), 0));
        group.2 += 1;
    }
    let mut rows: Vec<Value> = groups
        .into_values()
        .map(|(product, status, count)| {
            let mut row = json!({
                "metric": "licenses",
                "product": product,
                "count": count,
            });
            if by_status {
                row["status"] = Value::String(status);
            }
            row
        })
        .collect();
    rows.sort_by(|a, b| {
        a.get("product")
            .and_then(|value| value.as_str())
            .cmp(&b.get("product").and_then(|value| value.as_str()))
    });
    rows
}

fn usage_rows(
    state: &AnalyticsState,
    definition: &ReportDefinition,
    start: DateTime<Utc>,
    end: DateTime<Utc>,
) -> Vec<Value> {
    let by_day = definition.dimensions.iter().any(|dimension| dimension == "day");
    let by_kind = definition.dimensions.iter().any(|dimension| dimension == "kind");
    let kind_filter = definition
        .filters
        .get("kind")
        .and_then(|value| value.as_str())
        .map(str::to_string);
    let mut groups: HashMap<String, (Option<String>, Option<String>, u64)> = HashMap::new();
    for entry in state.metrics.iter() {
        let sample = entry.value();
        if sample.recorded_at < start || sample.recorded_at > end {
            continue;
        }
        if let Some(kind) = &kind_filter {
            if sample.kind != *kind {
                continue;
            }
        }
        let mut key_parts: Vec<String> = Vec::new();
        let mut day: Option<String> = None;
        let mut kind: Option<String> = None;
        if by_day {
            day = Some(day_key(sample.recorded_at));
            key_parts.push(day.clone().unwrap_or_default());
        }
        if by_kind {
            kind = Some(sample.kind.clone());
            key_parts.push(sample.kind.clone());
        }
        if key_parts.is_empty() {
            key_parts.push("all".to_string());
        }
        let group = groups
            .entry(key_parts.join("|"))
            .or_insert((day, kind, 0));
        group.2 += 1;
    }
    let mut rows: Vec<Value> = groups
        .into_values()
        .map(|(day, kind, count)| {
            let mut row = json!({ "metric": "usage", "count": count });
            if let Some(day) = day {
                row["day"] = Value::String(day);
            }
            if let Some(kind) = kind {
                row["kind"] = Value::String(kind);
            }
            row
        })
        .collect();
    rows.sort_by(|a, b| {
        a.get("day")
            .and_then(|value| value.as_str())
            .cmp(&b.get("day").and_then(|value| value.as_str()))
    });
    rows
}

fn uptime_rows(
    state: &AnalyticsState,
    definition: &ReportDefinition,
    start: DateTime<Utc>,
    end: DateTime<Utc>,
) -> Vec<Value> {
    let node_filter = definition
        .filters
        .get("node_id")
        .and_then(|value| value.as_str())
        .map(str::to_string);
    let cluster_filter = definition
        .filters
        .get("cluster_id")
        .and_then(|value| value.as_str())
        .map(str::to_string);
    let mut groups: HashMap<String, (u64, u64)> = HashMap::new();
    for entry in state.health.iter() {
        let sample = entry.value();
        if sample.checked_at < start || sample.checked_at > end {
            continue;
        }
        if let Some(node_id) = &node_filter {
            if sample.node_id != *node_id {
                continue;
            }
        }
        if let Some(cluster_id) = &cluster_filter {
            if sample.cluster_id.as_deref() != Some(cluster_id.as_str()) {
                continue;
            }
        }
        let group = groups.entry(day_key(sample.checked_at)).or_insert((0, 0));
        group.0 += 1;
        if sample.status == "healthy" {
            group.1 += 1;
        }
    }
    let mut rows: Vec<Value> = groups
        .into_iter()
        .map(|(day, (checks, healthy))| {
            let uptime = if checks > 0 {
                Some((healthy as f64 / checks as f64 * 10000.0).round() / 100.0)
            } else {
                None
            };
            json!({
                "metric": "uptime",
                "day": day,
                "checks": checks,
                "healthy": healthy,
                "uptime_pct": uptime,
            })
        })
        .collect();
    rows.sort_by(|a, b| {
        a.get("day")
            .and_then(|value| value.as_str())
            .cmp(&b.get("day").and_then(|value| value.as_str()))
    });
    rows
}

fn sales_by_product_rows(
    state: &AnalyticsState,
    definition: &ReportDefinition,
    start: DateTime<Utc>,
    end: DateTime<Utc>,
) -> Vec<Value> {
    let currency_filter = filter_currency(definition);
    let mut groups: HashMap<String, (f64, u64)> = HashMap::new();
    for entry in state.sales.iter() {
        let sale = entry.value();
        if sale.kind != "sale" || sale.recorded_at < start || sale.recorded_at > end {
            continue;
        }
        if let Some(currency) = &currency_filter {
            if sale.currency != *currency {
                continue;
            }
        }
        for product in &sale.products {
            let group = groups.entry(product.clone()).or_insert((0.0, 0));
            group.0 += sale.total;
            group.1 += 1;
        }
    }
    let mut rows: Vec<Value> = groups
        .into_iter()
        .map(|(product, (gross, orders))| {
            json!({
                "metric": "sales_by_product",
                "product": product,
                "gross": round_money(gross),
                "orders": orders,
            })
        })
        .collect();
    rows.sort_by(|a, b| {
        b.get("gross")
            .and_then(|value| value.as_f64())
            .partial_cmp(&a.get("gross").and_then(|value| value.as_f64()).unwrap_or(0.0))
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    rows
}

fn compute_rows(state: &AnalyticsState, definition: &ReportDefinition) -> UnifiedResult<Vec<Value>> {
    let (start, end) = resolve_definition_range(definition)?;
    let mut rows: Vec<Value> = Vec::new();
    for metric in &definition.metrics {
        match metric.as_str() {
            "revenue" => rows.extend(revenue_rows(state, definition, start, end, false)),
            "refunds" => rows.extend(revenue_rows(state, definition, start, end, true)),
            "licenses" => rows.extend(license_rows(state, definition)),
            "usage" => rows.extend(usage_rows(state, definition, start, end)),
            "uptime" => rows.extend(uptime_rows(state, definition, start, end)),
            "sales_by_product" => rows.extend(sales_by_product_rows(state, definition, start, end)),
            other => {
                return Err(UnifiedError::bad_request(format!("Unknown metric \"{other}\"")));
            }
        }
    }
    Ok(rows)
}

fn run_report(state: &AnalyticsState, definition_id: &str) -> UnifiedResult<ReportRun> {
    let definition = state.definition(definition_id)?;
    let started_at = now();
    match compute_rows(state, &definition) {
        Ok(rows) => {
            let run = ReportRun {
                id: new_id(),
                definition_id: definition.id.clone(),
                status: "completed".to_string(),
                started_at,
                finished_at: Some(now()),
                row_count: rows.len(),
                error: None,
                rows,
            };
            state.runs.insert(run.id.clone(), run.clone());
            Ok(run)
        }
        Err(error) => {
            let run = ReportRun {
                id: new_id(),
                definition_id: definition.id.clone(),
                status: "failed".to_string(),
                started_at,
                finished_at: Some(now()),
                row_count: 0,
                error: Some(error.message.clone()),
                rows: Vec::new(),
            };
            state.runs.insert(run.id.clone(), run.clone());
            Err(error)
        }
    }
}

// ---------------------------------------------------------------------------
// Dashboard and summary handlers.
// ---------------------------------------------------------------------------

async fn get_dashboard(state: web::Data<Arc<AnalyticsState>>) -> HttpResponse {
    let end = now();
    let start = end - chrono::Duration::days(30);
    let mut revenue_30d: HashMap<String, f64> = HashMap::new();
    let mut refunds_30d: HashMap<String, f64> = HashMap::new();
    let mut orders = 0u64;
    for entry in state.sales.iter() {
        let sale = entry.value();
        if sale.recorded_at < start || sale.recorded_at > end {
            continue;
        }
        if sale.kind == "sale" {
            *revenue_30d.entry(sale.currency.clone()).or_insert(0.0) += sale.total;
            orders += 1;
        } else if sale.kind == "refund" {
            *refunds_30d.entry(sale.currency.clone()).or_insert(0.0) += sale.total;
        }
    }
    for value in revenue_30d.values_mut() {
        *value = round_money(*value);
    }
    for value in refunds_30d.values_mut() {
        *value = round_money(*value);
    }
    let mut licenses_by_status: HashMap<String, u64> = HashMap::new();
    let mut active_licenses = 0u64;
    for entry in state.licenses.iter() {
        let license = entry.value();
        *licenses_by_status.entry(license.status.clone()).or_insert(0) += 1;
        if license.status == "active" {
            active_licenses += 1;
        }
    }
    let mut checks = 0u64;
    let mut healthy = 0u64;
    for entry in state.health.iter() {
        let sample = entry.value();
        if sample.checked_at < start || sample.checked_at > end {
            continue;
        }
        checks += 1;
        if sample.status == "healthy" {
            healthy += 1;
        }
    }
    let uptime_pct_30d = if checks > 0 {
        Some((healthy as f64 / checks as f64 * 10000.0).round() / 100.0)
    } else {
        None
    };
    let open_tasks = state
        .tasks
        .iter()
        .filter(|entry| {
            matches!(
                entry.value().status.as_str(),
                "todo" | "in_progress" | "review" | "blocked"
            )
        })
        .count() as u64;
    let mut pool_views: Vec<PoolHeadroomView> = Vec::new();
    let mut pools_overcommitted = 0u64;
    let mut lowest: Option<f64> = None;
    for entry in state.pools.iter() {
        let pool = entry.value();
        let allocations = pool.allocations.lock().expect("pool allocations lock poisoned");
        let allocated = PoolResources::default().total_allocated(&allocations);
        let mut worst: Option<f64> = None;
        let mut overcommitted = false;
        for (used, capacity) in [
            (allocated.cpu_cores, pool.capacity.cpu_cores),
            (allocated.ram_gb, pool.capacity.ram_gb),
            (allocated.disk_gb, pool.capacity.disk_gb),
            (allocated.bandwidth_tb, pool.capacity.bandwidth_tb),
        ] {
            if capacity > 0.0 {
                let headroom = ((capacity - used) / capacity * 10000.0).round() / 100.0;
                worst = Some(worst.map_or(headroom, |current: f64| current.min(headroom)));
                if used > capacity {
                    overcommitted = true;
                }
            }
        }
        if overcommitted {
            pools_overcommitted += 1;
        }
        if let Some(headroom) = worst {
            lowest = Some(lowest.map_or(headroom, |current: f64| current.min(headroom)));
        }
        pool_views.push(PoolHeadroomView {
            pool_id: pool.pool_id.clone(),
            pool_name: pool.pool_name.clone(),
            headroom_pct: worst,
            overcommitted,
        });
    }
    pool_views.sort_by(|a, b| a.pool_name.cmp(&b.pool_name));
    let dashboard = DashboardView {
        revenue_30d,
        revenue_30d_orders: orders,
        refunds_30d,
        active_licenses,
        licenses_by_status,
        uptime_pct_30d,
        uptime_checks_30d: checks,
        open_tasks,
        capacity_headroom: CapacityHeadroomView {
            pools_tracked: pool_views.len() as u64,
            lowest_headroom_pct: lowest,
            pools_overcommitted,
            pools: pool_views,
        },
        generated_at: now(),
    };
    json::success_response(dashboard, 200)
}

async fn list_sales(
    state: web::Data<Arc<AnalyticsState>>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let params = query.into_inner();
    let mut items: Vec<SaleRecord> = state
        .sales
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|sale| {
            matches_search(
                &format!("{} {}", sale.products.join(" "), sale.order_id.clone().unwrap_or_default()),
                &params,
            ) && params
                .filters
                .get("kind")
                .is_none_or(|kind| sale.kind == *kind)
                && params
                    .filters
                    .get("currency")
                    .is_none_or(|currency| sale.currency.eq_ignore_ascii_case(currency))
                && params.filters.get("product").is_none_or(|product| {
                    sale.products.iter().any(|name| name == product)
                })
        })
        .collect();
    items.sort_by(|a, b| b.recorded_at.cmp(&a.recorded_at));
    if params.normalized().order == "asc" {
        items.reverse();
    }
    let page = paginate(items, &params);
    json::success_response(page, 200)
}

async fn record_sale(
    state: web::Data<Arc<AnalyticsState>>,
    body: web::Json<RecordSaleRequest>,
) -> HttpResponse {
    let result: UnifiedResult<SaleRecord> = (|| {
        let input = body.into_inner();
        if !matches!(input.kind.as_str(), "sale" | "refund") {
            return Err(UnifiedError::bad_request("kind must be sale or refund"));
        }
        if !input.total.is_finite() || input.total < 0.0 {
            return Err(UnifiedError::bad_request("total must be a finite number >= 0"));
        }
        let currency = input
            .currency
            .unwrap_or_else(|| "USD".to_string())
            .trim()
            .to_uppercase();
        if !valid_currency(&currency) {
            return Err(UnifiedError::bad_request("currency must be a 3-letter code"));
        }
        let order_id = input
            .order_id
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty());
        if let Some(order_id) = &order_id {
            if state.sales.iter().any(|entry| {
                entry.value().order_id.as_deref() == Some(order_id.as_str())
                    && entry.value().kind == input.kind
            }) {
                return Err(UnifiedError::conflict(format!(
                    "A {} record for order \"{order_id}\" already exists",
                    input.kind
                )));
            }
        }
        let recorded_at = match input.recorded_at {
            Some(value) => parse_datetime(&value, "recorded_at")?,
            None => now(),
        };
        if recorded_at > now() {
            return Err(UnifiedError::bad_request("recorded_at must not be in the future"));
        }
        let record = SaleRecord {
            id: new_id(),
            order_id,
            kind: input.kind,
            total: round_money(input.total),
            currency,
            products: input.products.unwrap_or_default(),
            company_id: input
                .company_id
                .map(|value| value.trim().to_string())
                .filter(|value| !value.is_empty()),
            recorded_at,
            created_at: now(),
        };
        state.sales.insert(record.id.clone(), record.clone());
        Ok(record)
    })();
    respond_created(result, false)
}

async fn get_sales_summary(
    state: web::Data<Arc<AnalyticsState>>,
    query: web::Query<SummaryQuery>,
) -> HttpResponse {
    let query = query.into_inner();
    let result: UnifiedResult<Value> = (|| {
        let (start, end) = resolve_range(
            query.from.as_deref(),
            query.to.as_deref(),
            query.range.as_deref(),
        )?;
        let definition = ReportDefinition {
            id: String::new(),
            name: String::new(),
            description: None,
            metrics: vec!["revenue".to_string()],
            dimensions: vec!["day".to_string()],
            range_preset: None,
            start: Some(start),
            end: Some(end),
            filters: Value::Null,
            created_at: now(),
            updated_at: now(),
        };
        let rows = revenue_rows(&state, &definition, start, end, false);
        Ok(json!({
            "from": start,
            "to": end,
            "rows": rows,
        }))
    })();
    json::respond(result, false)
}

async fn get_license_summary(state: web::Data<Arc<AnalyticsState>>) -> HttpResponse {
    let mut by_status: HashMap<String, u64> = HashMap::new();
    let mut by_product: HashMap<String, ProductLicenseCount> = HashMap::new();
    for entry in state.licenses.iter() {
        let license = entry.value();
        *by_status.entry(license.status.clone()).or_insert(0) += 1;
        let group = by_product
            .entry(license.product.clone())
            .or_insert_with(|| ProductLicenseCount {
                product: license.product.clone(),
                total: 0,
                active: 0,
                suspended: 0,
                revoked: 0,
                expired: 0,
            });
        group.total += 1;
        match license.status.as_str() {
            "active" => group.active += 1,
            "suspended" => group.suspended += 1,
            "revoked" => group.revoked += 1,
            "expired" => group.expired += 1,
            _ => {}
        }
    }
    let mut products: Vec<ProductLicenseCount> = by_product.into_values().collect();
    products.sort_by(|a, b| a.product.cmp(&b.product));
    let total = state.licenses.len() as u64;
    json::success_response(
        LicenseSummaryView {
            total,
            by_status,
            by_product: products,
        },
        200,
    )
}

async fn get_revenue_summary(
    state: web::Data<Arc<AnalyticsState>>,
    query: web::Query<SummaryQuery>,
) -> HttpResponse {
    let query = query.into_inner();
    let result: UnifiedResult<RevenueSummaryView> = (|| {
        let (start, end) = resolve_range(
            query.from.as_deref(),
            query.to.as_deref(),
            query.range.as_deref(),
        )?;
        let mut by_currency: HashMap<String, CurrencyRevenue> = HashMap::new();
        let mut orders = 0u64;
        let mut refunds = 0u64;
        for entry in state.sales.iter() {
            let sale = entry.value();
            if sale.recorded_at < start || sale.recorded_at > end {
                continue;
            }
            let group = by_currency
                .entry(sale.currency.clone())
                .or_insert_with(|| CurrencyRevenue {
                    currency: sale.currency.clone(),
                    gross: 0.0,
                    refunds: 0.0,
                    net: 0.0,
                });
            if sale.kind == "sale" {
                group.gross += sale.total;
                orders += 1;
            } else if sale.kind == "refund" {
                group.refunds += sale.total;
                refunds += 1;
            }
        }
        let mut currencies: Vec<CurrencyRevenue> = by_currency
            .into_values()
            .map(|mut group| {
                group.gross = round_money(group.gross);
                group.refunds = round_money(group.refunds);
                group.net = round_money(group.gross - group.refunds);
                group
            })
            .collect();
        currencies.sort_by(|a, b| a.currency.cmp(&b.currency));
        Ok(RevenueSummaryView {
            from: start,
            to: end,
            orders,
            refunds,
            by_currency: currencies,
        })
    })();
    json::respond(result, false)
}

async fn list_metrics(
    state: web::Data<Arc<AnalyticsState>>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let params = query.into_inner();
    let mut items: Vec<MetricSample> = state
        .metrics
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|sample| {
            params
                .filters
                .get("kind")
                .is_none_or(|kind| sample.kind == *kind)
                && params.filters.get("source").is_none_or(|source| {
                    sample.source.as_deref() == Some(source.as_str())
                })
        })
        .collect();
    items.sort_by(|a, b| b.recorded_at.cmp(&a.recorded_at));
    if params.normalized().order == "asc" {
        items.reverse();
    }
    let page = paginate(items, &params);
    json::success_response(page, 200)
}

async fn record_metric(
    state: web::Data<Arc<AnalyticsState>>,
    body: web::Json<RecordMetricRequest>,
) -> HttpResponse {
    let result: UnifiedResult<MetricSample> = (|| {
        let input = body.into_inner();
        if !matches!(input.kind.as_str(), "api_call" | "download" | "login" | "upload") {
            return Err(UnifiedError::bad_request(
                "kind must be one of: api_call, download, login, upload",
            ));
        }
        let recorded_at = match input.recorded_at {
            Some(value) => parse_datetime(&value, "recorded_at")?,
            None => now(),
        };
        if recorded_at > now() {
            return Err(UnifiedError::bad_request("recorded_at must not be in the future"));
        }
        let sample = MetricSample {
            id: new_id(),
            kind: input.kind,
            source: input
                .source
                .map(|value| value.trim().to_string())
                .filter(|value| !value.is_empty()),
            recorded_at,
            created_at: now(),
        };
        state.metrics.insert(sample.id.clone(), sample.clone());
        Ok(sample)
    })();
    respond_created(result, false)
}

async fn get_usage_summary(
    state: web::Data<Arc<AnalyticsState>>,
    query: web::Query<SummaryQuery>,
) -> HttpResponse {
    let query = query.into_inner();
    let result: UnifiedResult<UsageSummaryView> = (|| {
        let (start, end) = resolve_range(
            query.from.as_deref(),
            query.to.as_deref(),
            query.range.as_deref(),
        )?;
        let mut by_kind: HashMap<String, u64> = HashMap::new();
        let mut by_day_map: HashMap<String, u64> = HashMap::new();
        let mut total = 0u64;
        for entry in state.metrics.iter() {
            let sample = entry.value();
            if sample.recorded_at < start || sample.recorded_at > end {
                continue;
            }
            if let Some(kind) = &query.kind {
                if sample.kind != *kind {
                    continue;
                }
            }
            *by_kind.entry(sample.kind.clone()).or_insert(0) += 1;
            *by_day_map.entry(day_key(sample.recorded_at)).or_insert(0) += 1;
            total += 1;
        }
        let mut by_day: Vec<DayCount> = by_day_map
            .into_iter()
            .map(|(day, count)| DayCount { day, count })
            .collect();
        by_day.sort_by(|a, b| a.day.cmp(&b.day));
        Ok(UsageSummaryView {
            from: start,
            to: end,
            total,
            by_kind,
            by_day,
        })
    })();
    json::respond(result, false)
}

async fn list_health(
    state: web::Data<Arc<AnalyticsState>>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let params = query.into_inner();
    let mut items: Vec<HealthSample> = state
        .health
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|sample| {
            params
                .filters
                .get("status")
                .is_none_or(|status| sample.status == *status)
                && params.filters.get("node_id").is_none_or(|node_id| sample.node_id == *node_id)
                && params.filters.get("cluster_id").is_none_or(|cluster_id| {
                    sample.cluster_id.as_deref() == Some(cluster_id.as_str())
                })
        })
        .collect();
    items.sort_by(|a, b| b.checked_at.cmp(&a.checked_at));
    if params.normalized().order == "asc" {
        items.reverse();
    }
    let page = paginate(items, &params);
    json::success_response(page, 200)
}

async fn record_health(
    state: web::Data<Arc<AnalyticsState>>,
    body: web::Json<RecordHealthRequest>,
) -> HttpResponse {
    let result: UnifiedResult<HealthSample> = (|| {
        let input = body.into_inner();
        let node_id = input.node_id.trim().to_string();
        if node_id.is_empty() {
            return Err(UnifiedError::bad_request("node_id must not be empty"));
        }
        if !matches!(input.status.as_str(), "healthy" | "degraded" | "unhealthy" | "unknown") {
            return Err(UnifiedError::bad_request(
                "status must be one of: healthy, degraded, unhealthy, unknown",
            ));
        }
        let checked_at = match input.checked_at {
            Some(value) => parse_datetime(&value, "checked_at")?,
            None => now(),
        };
        if checked_at > now() {
            return Err(UnifiedError::bad_request("checked_at must not be in the future"));
        }
        let sample = HealthSample {
            id: new_id(),
            node_id,
            cluster_id: input
                .cluster_id
                .map(|value| value.trim().to_string())
                .filter(|value| !value.is_empty()),
            status: input.status,
            latency_ms: input.latency_ms,
            checked_at,
            created_at: now(),
        };
        state.health.insert(sample.id.clone(), sample.clone());
        Ok(sample)
    })();
    respond_created(result, false)
}

async fn get_uptime_summary(
    state: web::Data<Arc<AnalyticsState>>,
    query: web::Query<SummaryQuery>,
) -> HttpResponse {
    let query = query.into_inner();
    let result: UnifiedResult<UptimeSummaryView> = (|| {
        let (start, end) = resolve_range(
            query.from.as_deref(),
            query.to.as_deref(),
            query.range.as_deref(),
        )?;
        let mut checks = 0u64;
        let mut healthy = 0u64;
        let mut degraded = 0u64;
        let mut unhealthy = 0u64;
        let mut unknown = 0u64;
        let mut by_day_map: HashMap<String, (u64, u64)> = HashMap::new();
        for entry in state.health.iter() {
            let sample = entry.value();
            if sample.checked_at < start || sample.checked_at > end {
                continue;
            }
            if let Some(node_id) = &query.node_id {
                if sample.node_id != *node_id {
                    continue;
                }
            }
            if let Some(cluster_id) = &query.cluster_id {
                if sample.cluster_id.as_deref() != Some(cluster_id.as_str()) {
                    continue;
                }
            }
            checks += 1;
            match sample.status.as_str() {
                "healthy" => healthy += 1,
                "degraded" => degraded += 1,
                "unhealthy" => unhealthy += 1,
                _ => unknown += 1,
            }
            let group = by_day_map
                .entry(day_key(sample.checked_at))
                .or_insert((0, 0));
            group.0 += 1;
            if sample.status == "healthy" {
                group.1 += 1;
            }
        }
        let uptime_pct = if checks > 0 {
            Some((healthy as f64 / checks as f64 * 10000.0).round() / 100.0)
        } else {
            None
        };
        let mut by_day: Vec<DayUptime> = by_day_map
            .into_iter()
            .map(|(day, (day_checks, day_healthy))| DayUptime {
                day,
                checks: day_checks,
                uptime_pct: if day_checks > 0 {
                    Some((day_healthy as f64 / day_checks as f64 * 10000.0).round() / 100.0)
                } else {
                    None
                },
            })
            .collect();
        by_day.sort_by(|a, b| a.day.cmp(&b.day));
        Ok(UptimeSummaryView {
            from: start,
            to: end,
            checks,
            healthy,
            degraded,
            unhealthy,
            unknown,
            uptime_pct,
            by_day,
        })
    })();
    json::respond(result, false)
}

// ---------------------------------------------------------------------------
// Report definition handlers.
// ---------------------------------------------------------------------------

fn parse_optional_datetime(value: Option<String>, field: &str) -> UnifiedResult<Option<DateTime<Utc>>> {
    match value {
        Some(value) => Ok(Some(parse_datetime(&value, field)?)),
        None => Ok(None),
    }
}

async fn list_reports(
    state: web::Data<Arc<AnalyticsState>>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let params = query.into_inner();
    let mut items: Vec<ReportDefinition> = state
        .definitions
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|definition| {
            matches_search(&format!("{} {}", definition.name, definition.description.clone().unwrap_or_default()), &params)
        })
        .collect();
    items.sort_by(|a, b| a.created_at.cmp(&b.created_at));
    if params.normalized().order == "desc" {
        items.reverse();
    }
    let page = paginate(items, &params);
    json::success_response(page, 200)
}

async fn create_report(
    state: web::Data<Arc<AnalyticsState>>,
    body: web::Json<CreateReportRequest>,
) -> HttpResponse {
    let result: UnifiedResult<ReportDefinition> = (|| {
        let input = body.into_inner();
        let name = require_name(&input.name)?;
        validate_metrics(&input.metrics)?;
        let dimensions = input.dimensions.unwrap_or_default();
        validate_dimensions(&dimensions)?;
        let range_preset = match input.range_preset {
            Some(preset) => {
                if !matches!(preset.as_str(), "7d" | "30d" | "90d" | "365d") {
                    return Err(UnifiedError::bad_request(
                        "range_preset must be one of: 7d, 30d, 90d, 365d",
                    ));
                }
                Some(preset)
            }
            None => None,
        };
        let start = parse_optional_datetime(input.start, "start")?;
        let end = parse_optional_datetime(input.end, "end")?;
        if let (Some(start), Some(end)) = (start, end) {
            if start >= end {
                return Err(UnifiedError::bad_request("start must be before end"));
            }
        }
        let timestamp = now();
        let definition = ReportDefinition {
            id: new_id(),
            name,
            description: input
                .description
                .map(|value| value.trim().to_string())
                .filter(|value| !value.is_empty()),
            metrics: input.metrics,
            dimensions,
            range_preset,
            start,
            end,
            filters: input.filters.unwrap_or(Value::Null),
            created_at: timestamp,
            updated_at: timestamp,
        };
        state
            .definitions
            .insert(definition.id.clone(), definition.clone());
        Ok(definition)
    })();
    respond_created(result, false)
}

async fn get_report(
    state: web::Data<Arc<AnalyticsState>>,
    path: web::Path<String>,
) -> HttpResponse {
    let result = state.definition(path.as_str());
    json::respond(result, false)
}

async fn update_report(
    state: web::Data<Arc<AnalyticsState>>,
    path: web::Path<String>,
    body: web::Json<UpdateReportRequest>,
) -> HttpResponse {
    let result: UnifiedResult<ReportDefinition> = (|| {
        let input = body.into_inner();
        let mut definition = state.definition(path.as_str())?;
        if let Some(name) = input.name {
            definition.name = require_name(&name)?;
        }
        if let Some(description) = input.description {
            definition.description = description
                .map(|value| value.trim().to_string())
                .filter(|value| !value.is_empty());
        }
        if let Some(metrics) = input.metrics {
            validate_metrics(&metrics)?;
            definition.metrics = metrics;
        }
        if let Some(dimensions) = input.dimensions {
            validate_dimensions(&dimensions)?;
            definition.dimensions = dimensions;
        }
        if let Some(range_preset) = input.range_preset {
            definition.range_preset = match range_preset {
                Some(preset) => {
                    if !matches!(preset.as_str(), "7d" | "30d" | "90d" | "365d") {
                        return Err(UnifiedError::bad_request(
                            "range_preset must be one of: 7d, 30d, 90d, 365d",
                        ));
                    }
                    Some(preset)
                }
                None => None,
            };
        }
        if let Some(start) = input.start {
            definition.start = parse_optional_datetime(start, "start")?;
        }
        if let Some(end) = input.end {
            definition.end = parse_optional_datetime(end, "end")?;
        }
        if let (Some(start), Some(end)) = (definition.start, definition.end) {
            if start >= end {
                return Err(UnifiedError::bad_request("start must be before end"));
            }
        }
        if let Some(filters) = input.filters {
            definition.filters = filters;
        }
        definition.updated_at = now();
        state
            .definitions
            .insert(definition.id.clone(), definition.clone());
        Ok(definition)
    })();
    json::respond(result, false)
}

async fn delete_report(
    state: web::Data<Arc<AnalyticsState>>,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<Value> = (|| {
        let definition = state.definition(path.as_str())?;
        state.definitions.remove(&definition.id);
        state
            .runs
            .retain(|_, run| run.definition_id != definition.id);
        state
            .scheduled
            .retain(|_, scheduled| scheduled.definition_id != definition.id);
        Ok(json!({ "deleted": true, "id": definition.id }))
    })();
    json::respond(result, false)
}

// ---------------------------------------------------------------------------
// Report runs and export.
// ---------------------------------------------------------------------------

async fn run_report_now(
    ctx: web::Data<Arc<crate::core::context::AppContext>>,
    state: web::Data<Arc<AnalyticsState>>,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<ReportRun> = (|| {
        let definition = state.definition(path.as_str())?;
        let run = run_report(&state, &definition.id)?;
        ctx.hooks.emit(
            ANALYTICS_REPORT_GENERATED,
            serde_json::to_value(&run).unwrap_or(Value::Null),
        );
        Ok(run)
    })();
    respond_created(result, false)
}

async fn list_runs(
    state: web::Data<Arc<AnalyticsState>>,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let params = query.into_inner();
    let result: UnifiedResult<PageResult<ReportRun>> = (|| {
        let definition = state.definition(path.as_str())?;
        let mut items: Vec<ReportRun> = state
            .runs
            .iter()
            .filter(|entry| entry.value().definition_id == definition.id)
            .map(|entry| entry.value().clone())
            .collect();
        items.sort_by(|a, b| b.started_at.cmp(&a.started_at));
        if params.normalized().order == "asc" {
            items.reverse();
        }
        Ok(paginate(items, &params))
    })();
    json::respond(result, false)
}

async fn get_run(
    state: web::Data<Arc<AnalyticsState>>,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let params = query.into_inner();
    let result: UnifiedResult<Value> = (|| {
        let run = state
            .runs
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found(format!("Report run \"{}\" not found", path.as_str())))?;
        let page = paginate(run.rows.clone(), &params);
        Ok(json!({
            "id": run.id,
            "definition_id": run.definition_id,
            "status": run.status,
            "started_at": run.started_at,
            "finished_at": run.finished_at,
            "row_count": run.row_count,
            "error": run.error,
            "rows": page,
        }))
    })();
    json::respond(result, false)
}

fn escape_csv_cell(cell: &str) -> String {
    if cell.contains(',') || cell.contains('"') || cell.contains('\n') || cell.contains('\r') {
        format!("\"{}\"", cell.replace('"', "\"\""))
    } else {
        cell.to_string()
    }
}

fn rows_to_csv(rows: &[Value]) -> String {
    let mut headers: Vec<String> = Vec::new();
    for row in rows {
        if let Value::Object(map) = row {
            for key in map.keys() {
                if !headers.contains(key) {
                    headers.push(key.clone());
                }
            }
        }
    }
    headers.sort();
    let mut output = String::new();
    output.push_str(
        &headers
            .iter()
            .map(|header| escape_csv_cell(header))
            .collect::<Vec<String>>()
            .join(","),
    );
    output.push('\n');
    for row in rows {
        let cells: Vec<String> = headers
            .iter()
            .map(|header| {
                let cell = row.get(header).map(|value| match value {
                    Value::Null => String::new(),
                    Value::String(text) => text.clone(),
                    other => other.to_string(),
                });
                escape_csv_cell(&cell.unwrap_or_default())
            })
            .collect();
        output.push_str(&cells.join(","));
        output.push('\n');
    }
    output
}

async fn export_report(
    ctx: web::Data<Arc<crate::core::context::AppContext>>,
    state: web::Data<Arc<AnalyticsState>>,
    path: web::Path<String>,
    query: web::Query<ExportQuery>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let query = query.into_inner();
    let format = query.format.unwrap_or_else(|| "json".to_string()).to_lowercase();
    if format != "json" && format != "csv" {
        return json::error_response(
            &UnifiedError::bad_request("format must be json or csv"),
            debug,
        );
    }
    let run = match query.run_id {
        Some(run_id) => state
            .runs
            .get(run_id.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found(format!("Report run \"{run_id}\" not found"))),
        None => {
            let definition = match state.definition(path.as_str()) {
                Ok(definition) => definition,
                Err(error) => return json::error_response(&error, debug),
            };
            state
                .runs
                .iter()
                .filter(|entry| entry.value().definition_id == definition.id)
                .map(|entry| entry.value().clone())
                .max_by_key(|run| run.started_at)
                .ok_or_else(|| {
                    UnifiedError::not_found(format!(
                        "No runs exist for report \"{}\"; run it first",
                        definition.id
                    ))
                })
        }
    };
    let run = match run {
        Ok(run) => run,
        Err(error) => return json::error_response(&error, debug),
    };
    if run.definition_id != path.as_str() {
        return json::error_response(
            &UnifiedError::not_found(format!(
                "Report run \"{}\" does not belong to report \"{}\"",
                run.id,
                path.as_str()
            )),
            debug,
        );
    }
    if run.status != "completed" {
        return json::error_response(
            &UnifiedError::conflict(format!("Report run \"{}\" did not complete", run.id)),
            debug,
        );
    }
    // A download, not an API payload, so the body is the raw export
    // rather than the {"data": ...} envelope (same pattern as the logs
    // export).
    let date = now().format("%Y%m%d");
    let (body, content_type, filename) = if format == "csv" {
        (
            rows_to_csv(&run.rows),
            "text/csv",
            format!("analytics-report-{}-{date}.csv", run.definition_id),
        )
    } else {
        (
            serde_json::to_string_pretty(&run.rows).unwrap_or_else(|_| "[]".to_string()),
            "application/json",
            format!("analytics-report-{}-{date}.json", run.definition_id),
        )
    };
    ctx.hooks.emit(
        ANALYTICS_REPORT_EXPORTED,
        json!({
            "definition_id": run.definition_id,
            "run_id": run.id,
            "format": format,
            "row_count": run.row_count,
            "filename": filename,
        }),
    );
    HttpResponse::Ok()
        .content_type(content_type)
        .insert_header((
            header::CONTENT_DISPOSITION,
            format!("attachment; filename=\"{filename}\""),
        ))
        .body(body)
}

// ---------------------------------------------------------------------------
// Scheduled report handlers.
// ---------------------------------------------------------------------------

async fn list_scheduled(
    state: web::Data<Arc<AnalyticsState>>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let params = query.into_inner();
    let mut items: Vec<ScheduledReport> = state
        .scheduled
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|scheduled| {
            params
                .filters
                .get("definition_id")
                .is_none_or(|id| scheduled.definition_id == *id)
                && params.filters.get("enabled").is_none_or(|flag| {
                    scheduled.enabled == (flag == "true" || flag == "1")
                })
        })
        .collect();
    items.sort_by(|a, b| a.created_at.cmp(&b.created_at));
    if params.normalized().order == "desc" {
        items.reverse();
    }
    let page = paginate(items, &params);
    json::success_response(page, 200)
}

async fn create_scheduled(
    state: web::Data<Arc<AnalyticsState>>,
    body: web::Json<CreateScheduledRequest>,
) -> HttpResponse {
    let result: UnifiedResult<ScheduledReport> = (|| {
        let input = body.into_inner();
        let definition = state.definition(&input.definition_id)?;
        let schedule = input.schedule.trim().to_string();
        if !valid_schedule(&schedule) {
            return Err(UnifiedError::bad_request(
                "schedule must be an interval in seconds or a cron-like expression such as \"*/5 * * * *\"",
            ));
        }
        let enabled = input.enabled.unwrap_or(true);
        let timestamp = now();
        let scheduled = ScheduledReport {
            id: new_id(),
            definition_id: definition.id,
            schedule,
            enabled,
            next_run_at: if enabled { next_run_at(&input.schedule, timestamp) } else { None },
            last_run_at: None,
            created_at: timestamp,
            updated_at: timestamp,
        };
        state
            .scheduled
            .insert(scheduled.id.clone(), scheduled.clone());
        Ok(scheduled)
    })();
    respond_created(result, false)
}

async fn get_scheduled(
    state: web::Data<Arc<AnalyticsState>>,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<ScheduledReport> = (|| {
        state
            .scheduled
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found(format!("Scheduled report \"{}\" not found", path.as_str())))
    })();
    json::respond(result, false)
}

async fn update_scheduled(
    state: web::Data<Arc<AnalyticsState>>,
    path: web::Path<String>,
    body: web::Json<UpdateScheduledRequest>,
) -> HttpResponse {
    let result: UnifiedResult<ScheduledReport> = (|| {
        let input = body.into_inner();
        let mut scheduled = state
            .scheduled
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found(format!("Scheduled report \"{}\" not found", path.as_str())))?;
        if let Some(schedule) = input.schedule {
            let schedule = schedule.trim().to_string();
            if !valid_schedule(&schedule) {
                return Err(UnifiedError::bad_request(
                    "schedule must be an interval in seconds or a cron-like expression such as \"*/5 * * * *\"",
                ));
            }
            scheduled.schedule = schedule;
        }
        if let Some(enabled) = input.enabled {
            scheduled.enabled = enabled;
        }
        scheduled.updated_at = now();
        scheduled.next_run_at = if scheduled.enabled {
            next_run_at(&scheduled.schedule, now())
        } else {
            None
        };
        state
            .scheduled
            .insert(scheduled.id.clone(), scheduled.clone());
        Ok(scheduled)
    })();
    json::respond(result, false)
}

async fn delete_scheduled(
    state: web::Data<Arc<AnalyticsState>>,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<Value> = (|| {
        state
            .scheduled
            .remove(path.as_str())
            .map(|_| json!({ "deleted": true, "id": path.as_str() }))
            .ok_or_else(|| UnifiedError::not_found(format!("Scheduled report \"{}\" not found", path.as_str())))
    })();
    json::respond(result, false)
}

async fn run_scheduled_now(
    ctx: web::Data<Arc<crate::core::context::AppContext>>,
    state: web::Data<Arc<AnalyticsState>>,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<RunNowView> = (|| {
        let mut scheduled = state
            .scheduled
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found(format!("Scheduled report \"{}\" not found", path.as_str())))?;
        let run = run_report(&state, &scheduled.definition_id)?;
        ctx.hooks.emit(
            ANALYTICS_REPORT_GENERATED,
            serde_json::to_value(&run).unwrap_or(Value::Null),
        );
        // Ask the scheduler to queue the same work. The scheduler only
        // knows mail.send, geo.update, ssl.renew and cleanup today, so
        // it refuses kind "report.generate" until it gains that kind;
        // the refusal is reported, not hidden, and the report above
        // already ran synchronously either way.
        let hook_context = ctx.hooks.emit(
            SCHEDULER_ENQUEUE,
            json!({
                "kind": "report.generate",
                "payload": {
                    "definition_id": scheduled.definition_id,
                    "scheduled_report_id": scheduled.id,
                },
            }),
        );
        let mut scheduler_queued = false;
        let mut scheduler_error: Option<String> = None;
        for outcome in &hook_context.results {
            if outcome.get("queued").and_then(|value| value.as_bool()) == Some(true) {
                scheduler_queued = true;
            }
            if let Some(error) = outcome.get("error").and_then(|value| value.as_str()) {
                scheduler_error = Some(error.to_string());
            }
        }
        scheduled.last_run_at = Some(now());
        if scheduled.enabled {
            scheduled.next_run_at = next_run_at(&scheduled.schedule, now());
        }
        scheduled.updated_at = now();
        state
            .scheduled
            .insert(scheduled.id.clone(), scheduled.clone());
        Ok(RunNowView {
            run,
            scheduler_queued,
            scheduler_error,
            scheduler_note: "Scheduler queueing needs the scheduler to accept job kind \"report.generate\"; see the coordinator report for the snippet. The report itself ran synchronously regardless.",
        })
    })();
    json::respond(result, false)
}

// ---------------------------------------------------------------------------
// Module definition.
// ---------------------------------------------------------------------------

pub struct AnalyticsModule {
    state: Arc<AnalyticsState>,
}

impl AnalyticsModule {
    pub fn new() -> Self {
        Self {
            state: Arc::new(AnalyticsState::new()),
        }
    }
}

impl Default for AnalyticsModule {
    fn default() -> Self {
        Self::new()
    }
}

impl Module for AnalyticsModule {
    fn id(&self) -> &'static str {
        "analytics"
    }

    fn name(&self) -> &'static str {
        "Analytics & Reports"
    }

    fn description(&self) -> &'static str {
        "Sales, license, usage, uptime and revenue analytics with report definitions, runs, CSV/JSON export and scheduled reports."
    }

    fn register_hooks(&self, bus: &HookBus) {
        let state = self.state.clone();
        bus.register(
            STORE_ORDER_COMPLETED,
            "analytics",
            Arc::new(move |ctx: &mut HookContext| {
                on_store_order_completed(state.clone(), &ctx.data.clone());
            }),
        );
        let state = self.state.clone();
        bus.register(
            STORE_ORDER_REFUNDED,
            "analytics",
            Arc::new(move |ctx: &mut HookContext| {
                on_store_order_refunded(state.clone(), &ctx.data.clone());
            }),
        );
        let state = self.state.clone();
        bus.register(
            events::LICENSE_ISSUED,
            "analytics",
            Arc::new(move |ctx: &mut HookContext| {
                upsert_license(&state, &ctx.data.clone(), "active");
            }),
        );
        let state = self.state.clone();
        bus.register(
            LICENSE_ACTIVATED,
            "analytics",
            Arc::new(move |ctx: &mut HookContext| {
                upsert_license(&state, &ctx.data.clone(), "active");
            }),
        );
        let state = self.state.clone();
        bus.register(
            LICENSE_SUSPENDED,
            "analytics",
            Arc::new(move |ctx: &mut HookContext| {
                upsert_license(&state, &ctx.data.clone(), "suspended");
            }),
        );
        let state = self.state.clone();
        bus.register(
            events::LICENSE_REVOKED,
            "analytics",
            Arc::new(move |ctx: &mut HookContext| {
                upsert_license(&state, &ctx.data.clone(), "revoked");
            }),
        );
        let state = self.state.clone();
        bus.register(
            events::DOWNLOAD_FINISHED,
            "analytics",
            Arc::new(move |_ctx: &mut HookContext| {
                record_metric(&state, "download", Some("store".to_string()));
            }),
        );
        let state = self.state.clone();
        bus.register(
            events::AUTH_LOGIN,
            "analytics",
            Arc::new(move |_ctx: &mut HookContext| {
                record_metric(&state, "login", Some("auth".to_string()));
            }),
        );
        let state = self.state.clone();
        bus.register(
            CLUSTER_HEALTH_CHANGED,
            "analytics",
            Arc::new(move |ctx: &mut HookContext| {
                on_cluster_health_changed(state.clone(), &ctx.data.clone());
            }),
        );
        for event in [TASK_CREATED, TASK_STATUS_CHANGED, TASK_COMPLETED] {
            let state = self.state.clone();
            bus.register(
                event,
                "analytics",
                Arc::new(move |ctx: &mut HookContext| {
                    on_task_event(state.clone(), &ctx.data.clone());
                }),
            );
        }
        let state = self.state.clone();
        bus.register(
            PLANNER_POOL_CREATED,
            "analytics",
            Arc::new(move |ctx: &mut HookContext| {
                on_planner_pool_created(state.clone(), &ctx.data.clone());
            }),
        );
        for (event, active) in [
            (PLANNER_PLAN_APPROVED, true),
            (PLANNER_PLAN_ACTIVATED, true),
            (PLANNER_PLAN_EXPIRED, false),
        ] {
            let state = self.state.clone();
            bus.register(
                event,
                "analytics",
                Arc::new(move |ctx: &mut HookContext| {
                    on_planner_plan_event(state.clone(), &ctx.data.clone(), active);
                }),
            );
        }
        let state = self.state.clone();
        bus.register(
            ANALYTICS_REPORT_RUN_REQUESTED,
            "analytics",
            Arc::new(move |ctx: &mut HookContext| {
                on_report_run_requested(state.clone(), &ctx.data.clone());
            }),
        );
    }

    fn init(&self, ctx: &crate::core::context::AppContext) -> UnifiedResult<()> {
        *self
            .state
            .hooks
            .write()
            .expect("hooks lock poisoned") = ctx.hooks.clone();
        Ok(())
    }

    fn configure_routes(&self, cfg: &mut web::ServiceConfig) {
        cfg.app_data(web::Data::new(self.state.clone()));
        cfg.service(
            web::scope("/api/v1/analytics")
                .route("/dashboard", web::get().to(get_dashboard))
                .route("/sales", web::get().to(list_sales))
                .route("/sales", web::post().to(record_sale))
                .route("/sales/summary", web::get().to(get_sales_summary))
                .route("/licenses/summary", web::get().to(get_license_summary))
                .route("/revenue/summary", web::get().to(get_revenue_summary))
                .route("/usage", web::get().to(list_metrics))
                .route("/usage/summary", web::get().to(get_usage_summary))
                .route("/metrics", web::post().to(record_metric))
                .route("/health", web::get().to(list_health))
                .route("/health", web::post().to(record_health))
                .route("/uptime/summary", web::get().to(get_uptime_summary))
                .route("/reports", web::get().to(list_reports))
                .route("/reports", web::post().to(create_report))
                .route("/reports/{id}", web::get().to(get_report))
                .route("/reports/{id}", web::put().to(update_report))
                .route("/reports/{id}", web::delete().to(delete_report))
                .route("/reports/{id}/run", web::post().to(run_report_now))
                .route("/reports/{id}/runs", web::get().to(list_runs))
                .route("/reports/{id}/export", web::get().to(export_report))
                .route("/runs/{id}", web::get().to(get_run))
                .route("/scheduled-reports", web::get().to(list_scheduled))
                .route("/scheduled-reports", web::post().to(create_scheduled))
                .route("/scheduled-reports/{id}", web::get().to(get_scheduled))
                .route("/scheduled-reports/{id}", web::put().to(update_scheduled))
                .route("/scheduled-reports/{id}", web::delete().to(delete_scheduled))
                .route("/scheduled-reports/{id}/run-now", web::post().to(run_scheduled_now)),
        );
    }

    fn route_info(&self) -> Vec<RouteInfo> {
        vec![
            RouteInfo { method: "GET", path: "/api/v1/analytics/dashboard", owner: "analytics", description: "Dashboard summary: revenue 30d, active licenses, uptime %, open tasks, capacity headroom." },
            RouteInfo { method: "GET", path: "/api/v1/analytics/sales", owner: "analytics", description: "List recorded sales and refunds." },
            RouteInfo { method: "POST", path: "/api/v1/analytics/sales", owner: "analytics", description: "Record a sale or refund manually." },
            RouteInfo { method: "GET", path: "/api/v1/analytics/sales/summary", owner: "analytics", description: "Revenue by day for a date range." },
            RouteInfo { method: "GET", path: "/api/v1/analytics/licenses/summary", owner: "analytics", description: "License counts by status and product." },
            RouteInfo { method: "GET", path: "/api/v1/analytics/revenue/summary", owner: "analytics", description: "Revenue totals by currency, refunds deducted." },
            RouteInfo { method: "GET", path: "/api/v1/analytics/usage", owner: "analytics", description: "List recorded metric samples." },
            RouteInfo { method: "GET", path: "/api/v1/analytics/usage/summary", owner: "analytics", description: "Usage counts by kind and day." },
            RouteInfo { method: "POST", path: "/api/v1/analytics/metrics", owner: "analytics", description: "Record one metric sample (api_call, download, login, upload)." },
            RouteInfo { method: "GET", path: "/api/v1/analytics/health", owner: "analytics", description: "List recorded health samples." },
            RouteInfo { method: "POST", path: "/api/v1/analytics/health", owner: "analytics", description: "Record one health sample." },
            RouteInfo { method: "GET", path: "/api/v1/analytics/uptime/summary", owner: "analytics", description: "Uptime percentage from health samples." },
            RouteInfo { method: "GET", path: "/api/v1/analytics/reports", owner: "analytics", description: "List report definitions." },
            RouteInfo { method: "POST", path: "/api/v1/analytics/reports", owner: "analytics", description: "Create a report definition." },
            RouteInfo { method: "GET", path: "/api/v1/analytics/reports/{id}", owner: "analytics", description: "Get one report definition." },
            RouteInfo { method: "PUT", path: "/api/v1/analytics/reports/{id}", owner: "analytics", description: "Update a report definition." },
            RouteInfo { method: "DELETE", path: "/api/v1/analytics/reports/{id}", owner: "analytics", description: "Delete a report definition and its runs." },
            RouteInfo { method: "POST", path: "/api/v1/analytics/reports/{id}/run", owner: "analytics", description: "Generate a report run now." },
            RouteInfo { method: "GET", path: "/api/v1/analytics/reports/{id}/runs", owner: "analytics", description: "List runs of a report definition." },
            RouteInfo { method: "GET", path: "/api/v1/analytics/reports/{id}/export", owner: "analytics", description: "Download the latest run as CSV or JSON (raw file)." },
            RouteInfo { method: "GET", path: "/api/v1/analytics/runs/{id}", owner: "analytics", description: "Get one report run with paginated rows." },
            RouteInfo { method: "GET", path: "/api/v1/analytics/scheduled-reports", owner: "analytics", description: "List scheduled reports." },
            RouteInfo { method: "POST", path: "/api/v1/analytics/scheduled-reports", owner: "analytics", description: "Create a scheduled report." },
            RouteInfo { method: "GET", path: "/api/v1/analytics/scheduled-reports/{id}", owner: "analytics", description: "Get one scheduled report." },
            RouteInfo { method: "PUT", path: "/api/v1/analytics/scheduled-reports/{id}", owner: "analytics", description: "Update a scheduled report." },
            RouteInfo { method: "DELETE", path: "/api/v1/analytics/scheduled-reports/{id}", owner: "analytics", description: "Delete a scheduled report." },
            RouteInfo { method: "POST", path: "/api/v1/analytics/scheduled-reports/{id}/run-now", owner: "analytics", description: "Run a scheduled report now and enqueue the scheduler job." },
        ]
    }
}

pub fn register(registry: &ModuleRegistry) {
    registry.register(Arc::new(AnalyticsModule::new()));
}
