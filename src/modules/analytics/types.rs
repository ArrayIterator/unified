//! analytics - types.

use actix_web::{ http::header };
use chrono::{ DateTime, Utc };
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use super::{events::*, helpers::*, views::*, inputs::*, state::*, hooks::*, reports::*, handlers_dashboard::*, handlers_definitions::*, handlers_runs::*, handlers_scheduled::*, module::*};



/// Capability checked for the analytics read routes (dashboard,
/// summaries, lists, single reads, export).
pub(crate) const PERM_ANALYTICS_VIEW: &str = "analytics.view";
/// Capability checked for the analytics write routes (record a sale
/// / metric / health sample, report and scheduled-report CRUD, runs).
pub(crate) const PERM_ANALYTICS_MANAGE: &str = "analytics.manage";

#[derive(Debug, Clone, Serialize, Deserialize)]
// ---------------------------------------------------------------------------
// Types.
// ---------------------------------------------------------------------------
/// One recorded sale or refund. Sales come from `store.order.completed`
/// events; refunds from `store.order.refunded` events; both can also be
/// recorded manually through POST /sales.
pub struct SaleRecord {
    pub id: String,
    /// Store order id, when the record came from a store event.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub order_id: Option<String>,
    /// `sale` or `refund`.
    pub kind: String,
    /// Sale total in integer minor units (cents for two-decimal
    /// currencies). JSON still carries major units.
    #[serde(with = "unified_core::money::serde_major")]
    pub total: i64,
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
    /// Extra filters, for example {"currency": "USD", "product": "..."}.
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
pub(crate) struct TaskMirror {
    pub(crate) task_id: String,
    pub(crate) status: String,
    pub(crate) updated_at: DateTime<Utc>,
}


/// Dashboard mirror of one capacity pool, fed by planner events.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct PoolMirror {
    pub(crate) pool_id: String,
    pub(crate) pool_name: String,
    pub(crate) capacity: PoolResources,
    /// Reservations per plan id, so approve/activate/expire updates
    /// stay idempotent.
    #[serde(skip)]
    pub(crate) allocations: Arc<Mutex<HashMap<String, PoolResources>>>,
}


/// Planner resource bundle, mirrored locally so the dashboard can
/// report headroom without calling the planner module.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
pub(crate) struct PoolResources {
    #[serde(default)]
    pub(crate) cpu_cores: f64,
    #[serde(default)]
    pub(crate) ram_gb: f64,
    #[serde(default)]
    pub(crate) disk_gb: f64,
    #[serde(default)]
    pub(crate) bandwidth_tb: f64,
}


impl PoolResources {
    pub(crate) fn from_value(value: &Value) -> Self {
        let number = |key: &str| value.get(key).and_then(|item| item.as_f64()).unwrap_or(0.0);
        Self {
            cpu_cores: number("cpu_cores"),
            ram_gb: number("ram_gb"),
            disk_gb: number("disk_gb"),
            bandwidth_tb: number("bandwidth_tb"),
        }
    }

    pub(crate) fn total_allocated(&self, allocations: &HashMap<String, PoolResources>) -> PoolResources {
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
