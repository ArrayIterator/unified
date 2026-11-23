// Diesel models for the properties domain of the unified-db-operations bounded context.
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

// ---------------------------------------------------------------------------
// Properties (migration 000061_properties; batch 5 notes §d). The notes
// give Queryable structs only; `New*` structs are added per the file
// convention (id carried, created_at/updated_at and DB-defaulted
// columns omitted). Money structs carry no serde derives.
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Queryable, Selectable)]
#[diesel(table_name = properties)]
pub struct PropertyRow {
    pub id: Uuid,
    pub title: String,
    pub description: Option<String>,
    pub slug: String,
    pub latitude: Option<f64>,
    pub longitude: Option<f64>,
    pub address: Option<String>,
    pub total_area_sqm: Option<PgNumeric>,
    pub company_id: Option<Uuid>,
    pub status: String,
    pub cover_storage_target: Option<String>,
    pub cover_storage_key: Option<String>,
    pub created_by: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable)]
#[diesel(table_name = properties)]
pub struct NewPropertyRow {
    pub id: Uuid,
    pub title: String,
    pub description: Option<String>,
    pub slug: String,
    pub latitude: Option<f64>,
    pub longitude: Option<f64>,
    pub address: Option<String>,
    pub total_area_sqm: Option<PgNumeric>,
    pub company_id: Option<Uuid>,
    pub cover_storage_target: Option<String>,
    pub cover_storage_key: Option<String>,
    pub created_by: String,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = property_plans)]
pub struct PropertyPlanRow {
    pub id: Uuid,
    pub property_id: Uuid,
    pub name: String,
    pub description: Option<String>,
    pub geometry: Option<Value>,
    pub geometry_hash: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = property_plans)]
pub struct NewPropertyPlanRow {
    pub id: Uuid,
    pub property_id: Uuid,
    pub name: String,
    pub description: Option<String>,
    pub geometry: Option<Value>,
    pub geometry_hash: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable)]
#[diesel(table_name = property_blocks)]
pub struct PropertyBlockRow {
    pub id: Uuid,
    pub plan_id: Uuid,
    pub property_id: Uuid,
    pub title: String,
    pub notes: Option<String>,
    pub geometry: Value,
    pub geometry_hash: String,
    pub area_sqm: Option<PgNumeric>,
    pub length_m: Option<PgNumeric>,
    pub price: PgNumeric,
    pub currency: String,
    pub status: String,
    pub assigned_customer_id: Option<String>,
    pub click_behavior: String,
    pub click_link_kind: Option<String>,
    pub click_link_target: Option<String>,
    pub order_id: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable)]
#[diesel(table_name = property_blocks)]
pub struct NewPropertyBlockRow {
    pub id: Uuid,
    pub plan_id: Uuid,
    pub property_id: Uuid,
    pub title: String,
    pub notes: Option<String>,
    pub geometry: Value,
    pub geometry_hash: String,
    pub area_sqm: Option<PgNumeric>,
    pub length_m: Option<PgNumeric>,
    pub assigned_customer_id: Option<String>,
    pub click_link_kind: Option<String>,
    pub click_link_target: Option<String>,
    pub order_id: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = property_block_history)]
pub struct PropertyBlockHistoryRow {
    pub id: Uuid,
    pub block_id: Uuid,
    pub property_id: Uuid,
    pub plan_id: Uuid,
    pub from_status: String,
    pub to_status: String,
    pub actor_id: String,
    pub reason: Option<String>,
    pub order_id: Option<String>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = property_block_history)]
pub struct NewPropertyBlockHistoryRow {
    pub id: Uuid,
    pub block_id: Uuid,
    pub property_id: Uuid,
    pub plan_id: Uuid,
    pub from_status: String,
    pub to_status: String,
    pub actor_id: String,
    pub reason: Option<String>,
    pub order_id: Option<String>,
}
