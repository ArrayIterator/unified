//! manufacturing - inputs.

use chrono::{ DateTime, Utc };
use serde::Deserialize;
use std::collections::HashMap;



#[derive(Debug, Deserialize)]
// ---------------------------------------------------------------------------
// Request DTOs
// ---------------------------------------------------------------------------
pub struct CreateBomRequest {
    pub product_id: String,
    pub name: String,
    #[serde(default)]
    pub code: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    pub lines: Vec<BomLineInput>,
    #[serde(default)]
    pub routing: Vec<RoutingStepInput>,
}


#[derive(Debug, Clone, Deserialize)]
pub struct BomLineInput {
    pub component_product_id: String,
    pub quantity: f64,
    pub unit: String,
    #[serde(default)]
    pub scrap_allowance_percent: Option<f64>,
    #[serde(default)]
    pub notes: Option<String>,
}


#[derive(Debug, Clone, Deserialize)]
pub struct RoutingStepInput {
    pub sequence: u32,
    pub name: String,
    pub work_center_id: String,
    pub duration_minutes: f64,
    #[serde(default)]
    pub description: Option<String>,
}


#[derive(Debug, Deserialize)]
pub struct UpdateBomRequest {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub status: Option<String>,
    /// When present, a new BOM version is created from these
    /// lines (and routing when given) and becomes active.
    #[serde(default)]
    pub lines: Option<Vec<BomLineInput>>,
    #[serde(default)]
    pub routing: Option<Vec<RoutingStepInput>>,
}


#[derive(Debug, Deserialize)]
pub struct CreateBomVersionRequest {
    pub lines: Vec<BomLineInput>,
    #[serde(default)]
    pub routing: Vec<RoutingStepInput>,
}


#[derive(Debug, Deserialize)]
pub struct CreateWorkCenterRequest {
    pub name: String,
    #[serde(default)]
    pub code: Option<String>,
    #[serde(default)]
    pub capacity_hours_per_day: Option<f64>,
    #[serde(default)]
    pub cost_per_hour: Option<f64>,
    #[serde(default)]
    pub currency: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
}


#[derive(Debug, Deserialize)]
pub struct UpdateWorkCenterRequest {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub capacity_hours_per_day: Option<f64>,
    #[serde(default)]
    pub cost_per_hour: Option<f64>,
    #[serde(default)]
    pub currency: Option<String>,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
}


#[derive(Debug, Deserialize)]
pub struct CreateWorkOrderRequest {
    pub bom_id: String,
    #[serde(default)]
    pub bom_version_id: Option<String>,
    pub quantity: f64,
    #[serde(default)]
    pub planned_start: Option<DateTime<Utc>>,
    #[serde(default)]
    pub planned_end: Option<DateTime<Utc>>,
    #[serde(default)]
    pub company_id: Option<String>,
    #[serde(default)]
    pub notes: Option<String>,
}


#[derive(Debug, Deserialize)]
pub struct UpdateWorkOrderRequest {
    #[serde(default)]
    pub quantity: Option<f64>,
    #[serde(default)]
    pub planned_start: Option<DateTime<Utc>>,
    #[serde(default)]
    pub planned_end: Option<DateTime<Utc>>,
    #[serde(default)]
    pub notes: Option<String>,
}


#[derive(Debug, Deserialize)]
pub struct TransitionRequest {
    pub to_status: String,
    #[serde(default)]
    pub notes: Option<String>,
}


#[derive(Debug, Deserialize)]
pub struct ConsumeRequest {
    pub items: Vec<ConsumeItemInput>,
}


#[derive(Debug, Deserialize)]
pub struct ConsumeItemInput {
    pub component_product_id: String,
    pub quantity: f64,
    #[serde(default)]
    pub unit: Option<String>,
}


#[derive(Debug, Deserialize)]
pub struct ProduceRequest {
    pub quantity_produced: f64,
    #[serde(default)]
    pub quantity_scrapped: f64,
    #[serde(default)]
    pub scrap_reason_code_id: Option<String>,
    #[serde(default)]
    pub work_center_id: Option<String>,
    #[serde(default)]
    pub notes: Option<String>,
    #[serde(default)]
    pub started_at: Option<DateTime<Utc>>,
}


#[derive(Debug, Deserialize)]
pub struct CreateScrapReasonRequest {
    pub code: String,
    pub label: String,
    #[serde(default)]
    pub description: Option<String>,
}


#[derive(Debug, Deserialize)]
pub struct UpdateScrapReasonRequest {
    #[serde(default)]
    pub label: Option<String>,
    #[serde(default)]
    pub active: Option<bool>,
    #[serde(default)]
    pub description: Option<String>,
}


#[derive(Debug, Deserialize)]
pub struct CreateUnitRequest {
    pub code: String,
    pub label: String,
}


#[derive(Debug, Deserialize)]
pub struct UpdateSettingsRequest {
    #[serde(default)]
    pub currency: Option<String>,
    #[serde(default)]
    pub default_capacity_hours_per_day: Option<f64>,
    #[serde(default)]
    pub max_bom_depth: Option<u32>,
    #[serde(default)]
    pub default_scrap_allowance_percent: Option<f64>,
    #[serde(default)]
    pub enforce_capacity_on_confirm: Option<bool>,
    #[serde(default)]
    pub units: Option<Vec<String>>,
    #[serde(default)]
    pub work_order_transitions: Option<HashMap<String, Vec<String>>>,
    #[serde(default)]
    pub work_order_statuses: Option<Vec<String>>,
}
