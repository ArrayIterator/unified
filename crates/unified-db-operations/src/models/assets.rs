// Diesel models for the assets domain of the unified-db-operations bounded context.
//
// Split out of `src/models.rs` (see `src/lib.rs`); the structs below are
// byte-identical to the originals, only regrouped.

use crate::schema::*;
use chrono::{DateTime, NaiveDate, Utc};
use diesel::data_types::PgNumeric;
use diesel::prelude::*;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

// ---------------------------------------------------------------------------
// Assets (migration 000050_assets).
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Queryable, Selectable)]
#[diesel(table_name = assets)]
pub struct Asset {
    pub id: Uuid,
    pub name: String,
    pub description: Option<String>,
    pub asset_type: String,
    pub serial_number: Option<String>,
    pub purchase_date: Option<NaiveDate>,
    pub purchase_price: PgNumeric,
    pub currency: String,
    pub vendor: Option<String>,
    pub location: Option<String>,
    pub status: String,
    pub warranty_end: Option<NaiveDate>,
    pub license_expires_at: Option<NaiveDate>,
    pub license_seats: Option<i64>,
    pub depreciation_method: Option<String>,
    pub useful_life_years: Option<i64>,
    pub salvage_value: PgNumeric,
    pub depreciation_status: String,
    pub depreciation_schedule: Option<Value>,
    pub depreciation_error: Option<String>,
    pub created_by: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable)]
#[diesel(table_name = assets)]
pub struct NewAsset {
    pub id: Uuid,
    pub name: String,
    pub description: Option<String>,
    pub serial_number: Option<String>,
    pub purchase_date: Option<NaiveDate>,
    pub vendor: Option<String>,
    pub location: Option<String>,
    pub warranty_end: Option<NaiveDate>,
    pub license_expires_at: Option<NaiveDate>,
    pub license_seats: Option<i64>,
    pub depreciation_method: Option<String>,
    pub useful_life_years: Option<i64>,
    pub depreciation_schedule: Option<Value>,
    pub depreciation_error: Option<String>,
    pub created_by: String,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = asset_assignments)]
pub struct AssetAssignment {
    pub id: Uuid,
    pub asset_id: Uuid,
    pub user_id: String,
    pub assigned_by: String,
    pub assigned_at: DateTime<Utc>,
    pub returned_at: Option<DateTime<Utc>>,
    pub return_reason: Option<String>,
    pub status: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = asset_assignments)]
pub struct NewAssetAssignment {
    pub id: Uuid,
    pub asset_id: Uuid,
    pub user_id: String,
    pub assigned_by: String,
    pub returned_at: Option<DateTime<Utc>>,
    pub return_reason: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = asset_stock)]
pub struct AssetStock {
    pub id: Uuid,
    pub asset_id: Uuid,
    pub quantity_on_hand: i64,
    pub reorder_threshold: i64,
    pub location: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = asset_stock)]
pub struct NewAssetStock {
    pub id: Uuid,
    pub asset_id: Uuid,
    pub location: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = asset_stock_adjustments)]
pub struct AssetStockAdjustment {
    pub id: Uuid,
    pub stock_id: Uuid,
    pub asset_id: Uuid,
    pub delta: i64,
    pub quantity_after: i64,
    pub reason: String,
    pub adjusted_by: String,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = asset_stock_adjustments)]
pub struct NewAssetStockAdjustment {
    pub id: Uuid,
    pub stock_id: Uuid,
    pub asset_id: Uuid,
    pub delta: i64,
    pub quantity_after: i64,
    pub reason: String,
    pub adjusted_by: String,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = asset_stock_takes)]
pub struct AssetStockTake {
    pub id: Uuid,
    pub title: String,
    pub notes: Option<String>,
    pub status: String,
    pub started_by: String,
    pub started_at: DateTime<Utc>,
    pub closed_at: Option<DateTime<Utc>>,
    pub closed_by: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = asset_stock_takes)]
pub struct NewAssetStockTake {
    pub id: Uuid,
    pub title: String,
    pub notes: Option<String>,
    pub started_by: String,
    pub closed_at: Option<DateTime<Utc>>,
    pub closed_by: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = asset_stock_take_lines)]
pub struct AssetStockTakeLine {
    pub id: Uuid,
    pub stock_take_id: Uuid,
    pub asset_id: Uuid,
    pub stock_id: Uuid,
    pub expected_quantity: i64,
    pub counted_quantity: Option<i64>,
    pub counted_by: Option<String>,
    pub note: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = asset_stock_take_lines)]
pub struct NewAssetStockTakeLine {
    pub id: Uuid,
    pub stock_take_id: Uuid,
    pub asset_id: Uuid,
    pub stock_id: Uuid,
    pub expected_quantity: i64,
    pub counted_quantity: Option<i64>,
    pub counted_by: Option<String>,
    pub note: Option<String>,
}
