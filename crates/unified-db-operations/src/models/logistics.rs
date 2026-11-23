// Diesel models for the logistics domain of the unified-db-operations bounded context.
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
// Logistics (migration 000062_logistics; batch 5 notes §d). The notes
// give Queryable structs only for most tables and put `Deserialize`
// on Queryable structs; `New*` structs are added per the file
// convention and Queryable structs are normalised to the file's
// `Queryable, Selectable, Serialize` shape. Money structs carry no
// serde derives.
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = logistics_drivers)]
pub struct LogisticsDriver {
    pub id: Uuid,
    pub user_id: String,
    pub license_number: String,
    pub phone: Option<String>,
    pub status: String,
    pub current_vehicle_id: Option<Uuid>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = logistics_drivers)]
pub struct NewLogisticsDriver {
    pub id: Uuid,
    pub user_id: String,
    pub license_number: String,
    pub phone: Option<String>,
    pub current_vehicle_id: Option<Uuid>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = logistics_vehicles)]
pub struct LogisticsVehicle {
    pub id: Uuid,
    pub plate: String,
    pub vehicle_type: String,
    pub capacity_kg: f64,
    pub status: String,
    pub assigned_driver_id: Option<Uuid>,
    pub company_id: Option<Uuid>,
    pub maintenance: Value,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = logistics_vehicles)]
pub struct NewLogisticsVehicle {
    pub id: Uuid,
    pub plate: String,
    pub vehicle_type: String,
    pub capacity_kg: f64,
    pub assigned_driver_id: Option<Uuid>,
    pub company_id: Option<Uuid>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = logistics_warehouses)]
pub struct LogisticsWarehouse {
    pub id: Uuid,
    pub name: String,
    pub address: String,
    pub capacity: i64,
    pub manager_user_id: Option<String>,
    pub default_low_stock_threshold: i64,
    pub company_id: Option<Uuid>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = logistics_warehouses)]
pub struct NewLogisticsWarehouse {
    pub id: Uuid,
    pub name: String,
    pub address: String,
    pub capacity: i64,
    pub manager_user_id: Option<String>,
    pub company_id: Option<Uuid>,
}

#[derive(Debug, Clone, Queryable, Selectable)]
#[diesel(table_name = logistics_shipments)]
pub struct LogisticsShipment {
    pub id: Uuid,
    pub tracking_number: String,
    pub tracking_token_hash: String,
    pub order_id: Option<String>,
    pub order_link_status: String,
    pub company_id: Option<Uuid>,
    pub status: String,
    pub origin_address: String,
    pub origin_latitude: Option<f64>,
    pub origin_longitude: Option<f64>,
    pub destination_address: String,
    pub destination_latitude: Option<f64>,
    pub destination_longitude: Option<f64>,
    pub weight_kg: f64,
    pub length_cm: f64,
    pub width_cm: f64,
    pub height_cm: f64,
    pub zone_origin: String,
    pub zone_destination: String,
    pub service_level: String,
    pub driver_id: Option<Uuid>,
    pub vehicle_id: Option<Uuid>,
    pub warehouse_id: Option<Uuid>,
    pub scheduled_at: Option<DateTime<Utc>>,
    pub window_start: Option<DateTime<Utc>>,
    pub window_end: Option<DateTime<Utc>>,
    pub estimated_delivery_at: Option<DateTime<Utc>>,
    pub shipping_cost: Option<PgNumeric>,
    pub currency: String,
    pub created_by: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable)]
#[diesel(table_name = logistics_shipments)]
pub struct NewLogisticsShipment {
    pub id: Uuid,
    pub tracking_number: String,
    pub tracking_token_hash: String,
    pub order_id: Option<String>,
    pub company_id: Option<Uuid>,
    pub origin_address: String,
    pub origin_latitude: Option<f64>,
    pub origin_longitude: Option<f64>,
    pub destination_address: String,
    pub destination_latitude: Option<f64>,
    pub destination_longitude: Option<f64>,
    pub weight_kg: f64,
    pub length_cm: f64,
    pub width_cm: f64,
    pub height_cm: f64,
    pub zone_origin: String,
    pub zone_destination: String,
    pub driver_id: Option<Uuid>,
    pub vehicle_id: Option<Uuid>,
    pub warehouse_id: Option<Uuid>,
    pub scheduled_at: Option<DateTime<Utc>>,
    pub window_start: Option<DateTime<Utc>>,
    pub window_end: Option<DateTime<Utc>>,
    pub estimated_delivery_at: Option<DateTime<Utc>>,
    pub shipping_cost: Option<PgNumeric>,
    pub created_by: String,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = logistics_shipment_events)]
pub struct LogisticsShipmentEvent {
    pub id: Uuid,
    pub shipment_id: Uuid,
    pub kind: String,
    pub from_status: Option<String>,
    pub to_status: Option<String>,
    pub actor: String,
    pub location: Option<String>,
    pub note: Option<String>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = logistics_shipment_events)]
pub struct NewLogisticsShipmentEvent {
    pub id: Uuid,
    pub shipment_id: Uuid,
    pub from_status: Option<String>,
    pub to_status: Option<String>,
    pub actor: String,
    pub location: Option<String>,
    pub note: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = logistics_stock_movements)]
pub struct LogisticsStockMovement {
    pub id: Uuid,
    pub warehouse_id: Uuid,
    pub asset_id: Uuid,
    pub direction: String,
    pub quantity: i64,
    pub quantity_after: i64,
    pub reason: String,
    pub shipment_id: Option<Uuid>,
    pub actor: String,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = logistics_stock_movements)]
pub struct NewLogisticsStockMovement {
    pub id: Uuid,
    pub warehouse_id: Uuid,
    pub asset_id: Uuid,
    pub direction: String,
    pub quantity: i64,
    pub quantity_after: i64,
    pub reason: String,
    pub shipment_id: Option<Uuid>,
    pub actor: String,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = logistics_routes)]
pub struct LogisticsRoute {
    pub id: Uuid,
    pub name: String,
    pub driver_id: Option<Uuid>,
    pub vehicle_id: Option<Uuid>,
    pub route_date: DateTime<Utc>,
    pub start_latitude: Option<f64>,
    pub start_longitude: Option<f64>,
    pub status: String,
    pub company_id: Option<Uuid>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = logistics_routes)]
pub struct NewLogisticsRoute {
    pub id: Uuid,
    pub name: String,
    pub driver_id: Option<Uuid>,
    pub vehicle_id: Option<Uuid>,
    pub route_date: DateTime<Utc>,
    pub start_latitude: Option<f64>,
    pub start_longitude: Option<f64>,
    pub company_id: Option<Uuid>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = logistics_route_stops)]
pub struct LogisticsRouteStop {
    pub id: Uuid,
    pub route_id: Uuid,
    pub shipment_id: Option<Uuid>,
    pub sequence: i64,
    pub address: String,
    pub latitude: Option<f64>,
    pub longitude: Option<f64>,
    pub status: String,
    pub note: Option<String>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = logistics_route_stops)]
pub struct NewLogisticsRouteStop {
    pub id: Uuid,
    pub route_id: Uuid,
    pub shipment_id: Option<Uuid>,
    pub sequence: i64,
    pub address: String,
    pub latitude: Option<f64>,
    pub longitude: Option<f64>,
    pub note: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = logistics_proofs)]
pub struct LogisticsProof {
    pub id: Uuid,
    pub shipment_id: Uuid,
    pub storage_target: String,
    pub storage_key: String,
    pub content_sha256: String,
    pub signature_storage_target: Option<String>,
    pub signature_storage_key: Option<String>,
    pub recipient_name: String,
    pub delivered_at: DateTime<Utc>,
    pub actor: String,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = logistics_proofs)]
pub struct NewLogisticsProof {
    pub id: Uuid,
    pub shipment_id: Uuid,
    pub storage_target: String,
    pub storage_key: String,
    pub content_sha256: String,
    pub signature_storage_target: Option<String>,
    pub signature_storage_key: Option<String>,
    pub recipient_name: String,
    pub delivered_at: DateTime<Utc>,
    pub actor: String,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = logistics_exceptions)]
pub struct LogisticsException {
    pub id: Uuid,
    pub shipment_id: Uuid,
    pub exception_type: String,
    pub description: String,
    pub status: String,
    pub previous_status: String,
    pub resolution: Option<String>,
    pub actor: String,
    pub resolved_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = logistics_exceptions)]
pub struct NewLogisticsException {
    pub id: Uuid,
    pub shipment_id: Uuid,
    pub exception_type: String,
    pub description: String,
    pub previous_status: String,
    pub resolution: Option<String>,
    pub actor: String,
    pub resolved_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Queryable, Selectable)]
#[diesel(table_name = logistics_rate_tables)]
pub struct LogisticsRateTable {
    pub id: Uuid,
    pub name: String,
    pub origin_zone: Option<String>,
    pub destination_zone: Option<String>,
    pub base_price: PgNumeric,
    pub per_kg: PgNumeric,
    pub per_km: PgNumeric,
    pub currency: String,
    pub active: bool,
    pub company_id: Option<Uuid>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable)]
#[diesel(table_name = logistics_rate_tables)]
pub struct NewLogisticsRateTable {
    pub id: Uuid,
    pub name: String,
    pub origin_zone: Option<String>,
    pub destination_zone: Option<String>,
    pub base_price: PgNumeric,
    pub per_kg: PgNumeric,
    pub per_km: PgNumeric,
    pub company_id: Option<Uuid>,
}
