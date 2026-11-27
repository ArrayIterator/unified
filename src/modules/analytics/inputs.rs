//! analytics - inputs.

use unified_core::hooks::events;
use actix_web::{ http::header };
use serde::Deserialize;
use serde_json::Value;



#[derive(Debug, Deserialize)]
// Request types.
pub(crate) struct RecordSaleRequest {
    #[serde(default)]
    pub(crate) order_id: Option<String>,
    pub(crate) kind: String,
    /// Sale total in integer minor units; JSON carries major units.
    #[serde(with = "unified_core::money::serde_major")]
    pub(crate) total: i64,
    #[serde(default)]
    pub(crate) currency: Option<String>,
    #[serde(default)]
    pub(crate) products: Option<Vec<String>>,
    #[serde(default)]
    pub(crate) company_id: Option<String>,
    #[serde(default)]
    pub(crate) recorded_at: Option<String>,
}


#[derive(Debug, Deserialize)]
pub(crate) struct RecordMetricRequest {
    pub(crate) kind: String,
    #[serde(default)]
    pub(crate) source: Option<String>,
    #[serde(default)]
    pub(crate) recorded_at: Option<String>,
}


#[derive(Debug, Deserialize)]
pub(crate) struct RecordHealthRequest {
    pub(crate) node_id: String,
    #[serde(default)]
    pub(crate) cluster_id: Option<String>,
    pub(crate) status: String,
    #[serde(default)]
    pub(crate) latency_ms: Option<u64>,
    #[serde(default)]
    pub(crate) checked_at: Option<String>,
}


#[derive(Debug, Deserialize)]
pub(crate) struct SummaryQuery {
    #[serde(default)]
    pub(crate) from: Option<String>,
    #[serde(default)]
    pub(crate) to: Option<String>,
    #[serde(default)]
    pub(crate) range: Option<String>,
    #[serde(default)]
    pub(crate) kind: Option<String>,
    #[serde(default)]
    pub(crate) node_id: Option<String>,
    #[serde(default)]
    pub(crate) cluster_id: Option<String>,
}


#[derive(Debug, Deserialize)]
pub(crate) struct CreateReportRequest {
    pub(crate) name: String,
    #[serde(default)]
    pub(crate) description: Option<String>,
    pub(crate) metrics: Vec<String>,
    #[serde(default)]
    pub(crate) dimensions: Option<Vec<String>>,
    #[serde(default)]
    pub(crate) range_preset: Option<String>,
    #[serde(default)]
    pub(crate) start: Option<String>,
    #[serde(default)]
    pub(crate) end: Option<String>,
    #[serde(default)]
    pub(crate) filters: Option<Value>,
}


#[derive(Debug, Deserialize)]
pub(crate) struct UpdateReportRequest {
    #[serde(default)]
    pub(crate) name: Option<String>,
    #[serde(default)]
    pub(crate) description: Option<Option<String>>,
    #[serde(default)]
    pub(crate) metrics: Option<Vec<String>>,
    #[serde(default)]
    pub(crate) dimensions: Option<Vec<String>>,
    #[serde(default)]
    pub(crate) range_preset: Option<Option<String>>,
    #[serde(default)]
    pub(crate) start: Option<Option<String>>,
    #[serde(default)]
    pub(crate) end: Option<Option<String>>,
    #[serde(default)]
    pub(crate) filters: Option<Value>,
}


#[derive(Debug, Deserialize)]
pub(crate) struct CreateScheduledRequest {
    pub(crate) definition_id: String,
    pub(crate) schedule: String,
    #[serde(default)]
    pub(crate) enabled: Option<bool>,
}


#[derive(Debug, Deserialize)]
pub(crate) struct UpdateScheduledRequest {
    #[serde(default)]
    pub(crate) schedule: Option<String>,
    #[serde(default)]
    pub(crate) enabled: Option<bool>,
}


#[derive(Debug, Deserialize)]
pub(crate) struct ExportQuery {
    #[serde(default)]
    pub(crate) format: Option<String>,
    #[serde(default)]
    pub(crate) run_id: Option<String>,
}
