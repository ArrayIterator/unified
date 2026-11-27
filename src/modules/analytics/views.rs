//! analytics - views.

use actix_web::{ http::header };
use chrono::{ DateTime, Utc };
use serde::Serialize;
use std::collections::HashMap;
use super::{events::*, helpers::*, types::*, inputs::*, state::*, hooks::*, reports::*, handlers_dashboard::*, handlers_definitions::*, handlers_runs::*, handlers_scheduled::*, module::*};



#[derive(Debug, Clone, Serialize)]
// Views.
pub(crate) struct DashboardView {
    /// Revenue by currency in major units. The sums are integer
    /// minor units internally and converted per currency at the
    /// boundary (`minor_to_major`), so a zero-decimal currency
    /// keeps its whole-unit value; the map itself stays major
    /// units on the wire, as before.
    pub(crate) revenue_30d: HashMap<String, f64>,
    pub(crate) revenue_30d_orders: u64,
    /// Refunds by currency in major units; see `revenue_30d`.
    pub(crate) refunds_30d: HashMap<String, f64>,
    pub(crate) active_licenses: u64,
    pub(crate) licenses_by_status: HashMap<String, u64>,
    pub(crate) uptime_pct_30d: Option<f64>,
    pub(crate) uptime_checks_30d: u64,
    pub(crate) open_tasks: u64,
    pub(crate) capacity_headroom: CapacityHeadroomView,
    pub(crate) generated_at: DateTime<Utc>,
}


#[derive(Debug, Clone, Serialize)]
pub(crate) struct CapacityHeadroomView {
    pub(crate) pools_tracked: u64,
    pub(crate) lowest_headroom_pct: Option<f64>,
    pub(crate) pools_overcommitted: u64,
    pub(crate) pools: Vec<PoolHeadroomView>,
}


#[derive(Debug, Clone, Serialize)]
pub(crate) struct PoolHeadroomView {
    pub(crate) pool_id: String,
    pub(crate) pool_name: String,
    pub(crate) headroom_pct: Option<f64>,
    pub(crate) overcommitted: bool,
}


#[derive(Debug, Clone, Serialize)]
pub(crate) struct LicenseSummaryView {
    pub(crate) total: u64,
    pub(crate) by_status: HashMap<String, u64>,
    pub(crate) by_product: Vec<ProductLicenseCount>,
}


#[derive(Debug, Clone, Serialize)]
pub(crate) struct ProductLicenseCount {
    pub(crate) product: String,
    pub(crate) total: u64,
    pub(crate) active: u64,
    pub(crate) suspended: u64,
    pub(crate) revoked: u64,
    pub(crate) expired: u64,
}


#[derive(Debug, Clone, Serialize)]
pub(crate) struct UsageSummaryView {
    pub(crate) from: DateTime<Utc>,
    pub(crate) to: DateTime<Utc>,
    pub(crate) total: u64,
    pub(crate) by_kind: HashMap<String, u64>,
    pub(crate) by_day: Vec<DayCount>,
}


#[derive(Debug, Clone, Serialize)]
pub(crate) struct DayCount {
    pub(crate) day: String,
    pub(crate) count: u64,
}


#[derive(Debug, Clone, Serialize)]
pub(crate) struct UptimeSummaryView {
    pub(crate) from: DateTime<Utc>,
    pub(crate) to: DateTime<Utc>,
    pub(crate) checks: u64,
    pub(crate) healthy: u64,
    pub(crate) degraded: u64,
    pub(crate) unhealthy: u64,
    pub(crate) unknown: u64,
    pub(crate) uptime_pct: Option<f64>,
    pub(crate) by_day: Vec<DayUptime>,
}


#[derive(Debug, Clone, Serialize)]
pub(crate) struct DayUptime {
    pub(crate) day: String,
    pub(crate) checks: u64,
    pub(crate) uptime_pct: Option<f64>,
}


#[derive(Debug, Clone, Serialize)]
pub(crate) struct RevenueSummaryView {
    pub(crate) from: DateTime<Utc>,
    pub(crate) to: DateTime<Utc>,
    pub(crate) orders: u64,
    pub(crate) refunds: u64,
    pub(crate) by_currency: Vec<CurrencyRevenue>,
}


#[derive(Debug, Clone, Serialize)]
pub(crate) struct CurrencyRevenue {
    pub(crate) currency: String,
    /// Gross sales in integer minor units; JSON carries major units.
    #[serde(with = "unified_core::money::serde_major")]
    pub(crate) gross: i64,
    /// Refunds in integer minor units; JSON carries major units.
    #[serde(with = "unified_core::money::serde_major")]
    pub(crate) refunds: i64,
    /// Gross minus refunds in integer minor units; JSON carries
    /// major units.
    #[serde(with = "unified_core::money::serde_major")]
    pub(crate) net: i64,
}


#[derive(Debug, Clone, Serialize)]
pub(crate) struct RunNowView {
    pub(crate) run: ReportRun,
    pub(crate) scheduler_queued: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) scheduler_error: Option<String>,
    pub(crate) scheduler_note: String,
}
