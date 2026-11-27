// bi module -- Shared types, records, request bodies, and constants.

use unified_core::json;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use uuid::Uuid;

/// Maximum lengths enforced on user supplied text.
pub(crate) const MAX_NAME_LEN: usize = 200;

pub(crate) const MAX_DESCRIPTION_LEN: usize = 5000;

pub(crate) const MAX_FIELD_LEN: usize = 128;

pub(crate) const MAX_FIELDS: usize = 32;

pub(crate) const MAX_GROUP_LEN: usize = 128;

pub(crate) const MAX_COMPANY_LEN: usize = 128;

pub(crate) const MAX_RECIPIENTS: usize = 50;

pub(crate) const MAX_KPIS: usize = 32;

pub(crate) const MAX_METRIC_LEN: usize = 64;

pub(crate) const MAX_PERIOD_LEN: usize = 16;

pub(crate) const MAX_SCHEDULE_LEN: usize = 64;

/// Hard cap on rows returned by one export, so an export can never
/// stream an unbounded result set.
pub(crate) const MAX_EXPORT_ROWS: usize = 10_000;

/// Maximum rows a report run returns inline (exports may go higher,
/// up to `MAX_EXPORT_ROWS`).
pub(crate) const MAX_RUN_ROWS: usize = 1_000;

/// Minimum distinct days of revenue history a forecast needs.
pub(crate) const MIN_FORECAST_DAYS: usize = 7;

/// Moving-average window in days used by every forecast.
pub(crate) const FORECAST_WINDOW_DAYS: i64 = 7;

/// Capability names checked through the `role.check` hook.
pub(crate) const PERM_BI_VIEW: &str = "bi.view";

pub(crate) const PERM_BI_MANAGE: &str = "bi.manage";

/// Source modules a report may read from. Closed allowlist on
/// purpose: an unknown source string is rejected instead of being
/// passed through to a hook name.
pub(crate) const ALLOWED_SOURCES: &[&str] = &[
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

pub(crate) const ALLOWED_CHART_TYPES: &[&str] = &["table", "bar", "line", "pie"];

/// KPI metrics this module can compute. Every metric names the
/// source module its rows come from.
pub(crate) const ALLOWED_METRICS: &[&str] = &[
    "revenue_total",
    "order_count",
    "ticket_count",
    "active_licenses",
    "tasks_completed",
    "payment_count",
    "average_order_value",
];

/// KPI periods.

pub(crate) const ALLOWED_PERIODS: &[&str] = &["7d", "30d", "90d", "all"];

/// Fields a report may select, per source. A field outside the
/// list for the chosen source is rejected at write time.
pub(crate) fn allowed_fields(source: &str) -> &'static [&'static str] {
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
/// metric's source has no rows in the period -- never a fabricated
/// zero presented as a measurement.
#[derive(Debug, Clone, Serialize)]
pub struct ComputedKpi {
    pub metric: String,
    pub source: String,
    pub period: String,
    pub value: Option<f64>,
    pub status: String,
}

// Request bodies

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

pub(crate) fn default_export_format() -> String {
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
