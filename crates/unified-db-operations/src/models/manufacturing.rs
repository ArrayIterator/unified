// Diesel models for the manufacturing domain of the unified-db-operations bounded context.
//
// Split out of `src/models.rs` (see `src/lib.rs`); the structs below are
// byte-identical to the originals, only regrouped.

use crate::schema::*;
use chrono::{DateTime, Utc};
use diesel::data_types::PgNumeric;
use diesel::prelude::*;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

// ---------------------------------------------------------------------------
// Manufacturing / MRP (migration 000086_manufacturing).
// Structs carrying NUMERIC columns skip serde derives (PgNumeric
// has no serde implementation), per the repo header rule.
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = mfg_boms)]
pub struct MfgBom {
    pub id: Uuid,
    pub product_id: String,
    pub name: String,
    pub code: String,
    pub description: Option<String>,
    pub status: String,
    pub active_version_id: Option<Uuid>,
    pub latest_version: i32,
    pub created_by: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = mfg_boms)]
pub struct NewMfgBom {
    pub id: Uuid,
    pub product_id: String,
    pub name: String,
    pub code: String,
    pub description: Option<String>,
    pub active_version_id: Option<Uuid>,
    pub created_by: String,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = mfg_bom_versions)]
pub struct MfgBomVersion {
    pub id: Uuid,
    pub bom_id: Uuid,
    pub version: i32,
    pub created_by: String,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = mfg_bom_versions)]
pub struct NewMfgBomVersion {
    pub id: Uuid,
    pub bom_id: Uuid,
    pub version: i32,
    pub created_by: String,
}

#[derive(Debug, Clone, Queryable, Selectable)]
#[diesel(table_name = mfg_bom_version_lines)]
pub struct MfgBomVersionLine {
    pub id: Uuid,
    pub bom_version_id: Uuid,
    pub component_product_id: String,
    pub quantity: PgNumeric,
    pub unit: String,
    pub scrap_allowance_percent: PgNumeric,
    pub notes: Option<String>,
}

#[derive(Debug, Clone, Insertable)]
#[diesel(table_name = mfg_bom_version_lines)]
pub struct NewMfgBomVersionLine {
    pub id: Uuid,
    pub bom_version_id: Uuid,
    pub component_product_id: String,
    pub quantity: PgNumeric,
    pub unit: String,
    pub notes: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable)]
#[diesel(table_name = mfg_work_centers)]
pub struct MfgWorkCenter {
    pub id: Uuid,
    pub name: String,
    pub code: String,
    pub capacity_hours_per_day: PgNumeric,
    pub cost_per_hour: Option<PgNumeric>,
    pub currency: Option<String>,
    pub status: String,
    pub description: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable)]
#[diesel(table_name = mfg_work_centers)]
pub struct NewMfgWorkCenter {
    pub id: Uuid,
    pub name: String,
    pub code: String,
    pub cost_per_hour: Option<PgNumeric>,
    pub currency: Option<String>,
    pub description: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable)]
#[diesel(table_name = mfg_bom_routing_steps)]
pub struct MfgBomRoutingStep {
    pub id: Uuid,
    pub bom_version_id: Uuid,
    pub sequence: i32,
    pub name: String,
    pub work_center_id: Uuid,
    pub duration_minutes: PgNumeric,
    pub description: Option<String>,
}

#[derive(Debug, Clone, Insertable)]
#[diesel(table_name = mfg_bom_routing_steps)]
pub struct NewMfgBomRoutingStep {
    pub id: Uuid,
    pub bom_version_id: Uuid,
    pub sequence: i32,
    pub name: String,
    pub work_center_id: Uuid,
    pub duration_minutes: PgNumeric,
    pub description: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable)]
#[diesel(table_name = mfg_work_orders)]
pub struct MfgWorkOrder {
    pub id: Uuid,
    pub number: String,
    pub bom_id: Uuid,
    pub bom_version_id: Uuid,
    pub bom_version: i32,
    pub product_id: String,
    pub quantity_planned: PgNumeric,
    pub quantity_produced: PgNumeric,
    pub quantity_scrapped: PgNumeric,
    pub status: String,
    pub planned_start: Option<DateTime<Utc>>,
    pub planned_end: Option<DateTime<Utc>>,
    pub actual_start: Option<DateTime<Utc>>,
    pub actual_end: Option<DateTime<Utc>>,
    pub company_id: Option<String>,
    pub notes: Option<String>,
    pub inspection_status: Option<String>,
    pub inspection_notes: Option<String>,
    pub created_by: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable)]
#[diesel(table_name = mfg_work_orders)]
pub struct NewMfgWorkOrder {
    pub id: Uuid,
    pub number: String,
    pub bom_id: Uuid,
    pub bom_version_id: Uuid,
    pub bom_version: i32,
    pub product_id: String,
    pub quantity_planned: PgNumeric,
    pub planned_start: Option<DateTime<Utc>>,
    pub planned_end: Option<DateTime<Utc>>,
    pub actual_start: Option<DateTime<Utc>>,
    pub actual_end: Option<DateTime<Utc>>,
    pub company_id: Option<String>,
    pub notes: Option<String>,
    pub inspection_status: Option<String>,
    pub inspection_notes: Option<String>,
    pub created_by: String,
}

#[derive(Debug, Clone, Queryable, Selectable)]
#[diesel(table_name = mfg_work_order_requirements)]
pub struct MfgWorkOrderRequirement {
    pub id: Uuid,
    pub work_order_id: Uuid,
    pub component_product_id: String,
    pub quantity_required: PgNumeric,
    pub unit: String,
    pub quantity_reserved: PgNumeric,
    pub quantity_consumed: PgNumeric,
    pub quantity_short: PgNumeric,
}

#[derive(Debug, Clone, Insertable)]
#[diesel(table_name = mfg_work_order_requirements)]
pub struct NewMfgWorkOrderRequirement {
    pub id: Uuid,
    pub work_order_id: Uuid,
    pub component_product_id: String,
    pub quantity_required: PgNumeric,
    pub unit: String,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = mfg_scrap_reasons)]
pub struct MfgScrapReason {
    pub id: Uuid,
    pub code: String,
    pub label: String,
    pub active: bool,
    pub description: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = mfg_scrap_reasons)]
pub struct NewMfgScrapReason {
    pub id: Uuid,
    pub code: String,
    pub label: String,
    pub description: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = mfg_units)]
pub struct MfgUnit {
    pub id: Uuid,
    pub code: String,
    pub label: String,
    pub active: bool,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = mfg_units)]
pub struct NewMfgUnit {
    pub id: Uuid,
    pub code: String,
    pub label: String,
}

#[derive(Debug, Clone, Queryable, Selectable)]
#[diesel(table_name = mfg_production_runs)]
pub struct MfgProductionRun {
    pub id: Uuid,
    pub work_order_id: Uuid,
    pub quantity_produced: PgNumeric,
    pub quantity_scrapped: PgNumeric,
    pub scrap_reason_id: Option<Uuid>,
    pub work_center_id: Option<Uuid>,
    pub operator_id: String,
    pub status: String,
    pub notes: Option<String>,
    pub started_at: DateTime<Utc>,
    pub completed_at: DateTime<Utc>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable)]
#[diesel(table_name = mfg_production_runs)]
pub struct NewMfgProductionRun {
    pub id: Uuid,
    pub work_order_id: Uuid,
    pub quantity_produced: PgNumeric,
    pub scrap_reason_id: Option<Uuid>,
    pub work_center_id: Option<Uuid>,
    pub operator_id: String,
    pub notes: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable)]
#[diesel(table_name = mfg_reservations)]
pub struct MfgReservation {
    pub id: Uuid,
    pub work_order_id: Uuid,
    pub component_product_id: String,
    pub quantity_reserved: PgNumeric,
    pub quantity_consumed: PgNumeric,
    pub unit: String,
    pub status: String,
    pub stock_unknown: bool,
    pub confirmation_reference: Option<String>,
    pub created_by: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable)]
#[diesel(table_name = mfg_reservations)]
pub struct NewMfgReservation {
    pub id: Uuid,
    pub work_order_id: Uuid,
    pub component_product_id: String,
    pub quantity_reserved: PgNumeric,
    pub unit: String,
    pub confirmation_reference: Option<String>,
    pub created_by: String,
}

#[derive(Debug, Clone, Queryable, Selectable)]
#[diesel(table_name = mfg_stock_notices)]
pub struct MfgStockNotice {
    pub id: Uuid,
    pub kind: String,
    pub work_order_id: Option<Uuid>,
    pub reservation_id: Option<Uuid>,
    pub component_product_id: Option<String>,
    pub quantity: Option<PgNumeric>,
    pub message: Option<String>,
    pub received_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable)]
#[diesel(table_name = mfg_stock_notices)]
pub struct NewMfgStockNotice {
    pub id: Uuid,
    pub kind: String,
    pub work_order_id: Option<Uuid>,
    pub reservation_id: Option<Uuid>,
    pub component_product_id: Option<String>,
    pub quantity: Option<PgNumeric>,
    pub message: Option<String>,
}
