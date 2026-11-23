// Diesel models for the analytics domain of the unified-db-platform bounded context.
//
// Split out of `src/models.rs` (see `src/lib.rs`); the structs below are
// byte-identical to the originals, only regrouped.

use crate::schema::*;
use chrono::{DateTime, Utc};
use diesel::data_types::PgNumeric;
use diesel::prelude::*;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

#[derive(Debug, Clone, Queryable, Selectable)]
#[diesel(table_name = analytics_sales)]
pub struct AnalyticsSale {
    pub id: Uuid,
    pub order_id: Option<String>,
    pub kind: String,
    pub total: PgNumeric,
    pub currency: String,
    pub products: Value,
    pub company_id: Option<String>,
    pub recorded_at: DateTime<Utc>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable)]
#[diesel(table_name = analytics_sales)]
pub struct NewAnalyticsSale {
    pub id: Uuid,
    pub order_id: Option<String>,
    pub kind: String,
    pub total: PgNumeric,
    pub currency: String,
    pub company_id: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = analytics_license_snapshots)]
pub struct AnalyticsLicenseSnapshot {
    pub license_id: String,
    #[serde(skip_serializing)]
    pub license_key: Option<String>,
    pub product: String,
    pub status: String,
    pub last_event_at: DateTime<Utc>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = analytics_license_snapshots)]
pub struct NewAnalyticsLicenseSnapshot {
    pub license_id: String,
    pub license_key: Option<String>,
    pub status: String,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = analytics_metric_samples)]
pub struct AnalyticsMetricSample {
    pub id: Uuid,
    pub kind: String,
    pub source: Option<String>,
    pub recorded_at: DateTime<Utc>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = analytics_metric_samples)]
pub struct NewAnalyticsMetricSample {
    pub id: Uuid,
    pub kind: String,
    pub source: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = analytics_health_samples)]
pub struct AnalyticsHealthSample {
    pub id: Uuid,
    pub node_id: String,
    pub cluster_id: Option<String>,
    pub status: String,
    pub latency_ms: Option<i64>,
    pub checked_at: DateTime<Utc>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = analytics_health_samples)]
pub struct NewAnalyticsHealthSample {
    pub id: Uuid,
    pub node_id: String,
    pub cluster_id: Option<String>,
    pub status: String,
    pub latency_ms: Option<i64>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = analytics_reports)]
pub struct AnalyticsReport {
    pub id: Uuid,
    pub name: String,
    pub description: Option<String>,
    pub metrics: Value,
    pub dimensions: Value,
    pub range_preset: Option<String>,
    pub start: Option<DateTime<Utc>>,
    pub end: Option<DateTime<Utc>>,
    pub filters: Value,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = analytics_reports)]
pub struct NewAnalyticsReport {
    pub id: Uuid,
    pub name: String,
    pub description: Option<String>,
    pub range_preset: Option<String>,
    pub start: Option<DateTime<Utc>>,
    pub end: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = analytics_report_runs)]
pub struct AnalyticsReportRun {
    pub id: Uuid,
    pub definition_id: Uuid,
    pub status: String,
    pub started_at: DateTime<Utc>,
    pub finished_at: Option<DateTime<Utc>>,
    pub row_count: i64,
    pub error: Option<String>,
    pub rows: Value,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = analytics_report_runs)]
pub struct NewAnalyticsReportRun {
    pub id: Uuid,
    pub definition_id: Uuid,
    pub status: String,
    pub finished_at: Option<DateTime<Utc>>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = analytics_scheduled_reports)]
pub struct AnalyticsScheduledReport {
    pub id: Uuid,
    pub definition_id: Uuid,
    pub schedule: String,
    pub enabled: bool,
    pub next_run_at: Option<DateTime<Utc>>,
    pub last_run_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = analytics_scheduled_reports)]
pub struct NewAnalyticsScheduledReport {
    pub id: Uuid,
    pub definition_id: Uuid,
    pub schedule: String,
    pub next_run_at: Option<DateTime<Utc>>,
    pub last_run_at: Option<DateTime<Utc>>,
}
