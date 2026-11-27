//! Logistics module - shipments, fleet, warehouses, delivery - Request bodies.
//!
//! Split from `src/modules/logistics.rs`; behaviour unchanged.

use super::constants::*;
use super::delivery_routes::*;
use super::drivers::*;
use super::helpers::*;
use super::hooks::*;
use super::module::*;
use super::rates::*;
use super::shipments::*;
use super::state::*;
use super::tracking::*;
use super::types::*;
use super::vehicles::*;
use super::warehouses::*;

use chrono::{DateTime, Utc};
use serde::Deserialize;
use uuid::Uuid;

#[derive(Debug, Deserialize)]
pub struct CreateShipmentRequest {
    pub origin: ShipmentEndpoint,
    pub destination: ShipmentEndpoint,
    pub weight_kg: f64,
    pub length_cm: f64,
    pub width_cm: f64,
    pub height_cm: f64,
    pub zone_origin: String,
    pub zone_destination: String,
    pub service_level: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub order_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub company_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub driver_id: Option<Uuid>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vehicle_id: Option<Uuid>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub warehouse_id: Option<Uuid>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scheduled_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub window_start: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub window_end: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub estimated_delivery_at: Option<DateTime<Utc>>,
    pub currency: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateShipmentRequest {
    pub origin: Option<ShipmentEndpoint>,
    pub destination: Option<ShipmentEndpoint>,
    pub weight_kg: Option<f64>,
    pub length_cm: Option<f64>,
    pub width_cm: Option<f64>,
    pub height_cm: Option<f64>,
    pub zone_origin: Option<String>,
    pub zone_destination: Option<String>,
    pub service_level: Option<String>,
    pub driver_id: Option<Option<Uuid>>,
    pub vehicle_id: Option<Option<Uuid>>,
    pub warehouse_id: Option<Option<Uuid>>,
    pub scheduled_at: Option<Option<DateTime<Utc>>>,
    pub window_start: Option<Option<DateTime<Utc>>>,
    pub window_end: Option<Option<DateTime<Utc>>>,
    pub estimated_delivery_at: Option<Option<DateTime<Utc>>>,
    pub currency: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct TransitionRequest {
    pub to_status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub location: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct LinkOrderRequest {
    pub order_id: String,
}

#[derive(Debug, Deserialize)]
pub struct ScheduleRequest {
    pub scheduled_at: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub window_start: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub window_end: Option<DateTime<Utc>>,
}

#[derive(Debug, Deserialize)]
pub struct RescheduleRequest {
    pub scheduled_at: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub window_start: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub window_end: Option<DateTime<Utc>>,
    pub reason: String,
}

#[derive(Debug, Deserialize)]
pub struct CreateProofRequest {
    pub storage_target: String,
    pub storage_key: String,
    pub content_sha256: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signature_storage_target: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signature_storage_key: Option<String>,
    pub recipient_name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delivered_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct CreateExceptionRequest {
    #[serde(rename = "type")]
    pub exception_type: String,
    pub description: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub location: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct ResolveExceptionRequest {
    pub resolution: String,
    /// When true the shipment goes back to `in_transit` (the
    /// default); when false it is returned instead.
    #[serde(default = "default_resume")]
    pub resume_delivery: bool,
}

pub(super) fn default_resume() -> bool {
    true
}

#[derive(Debug, Deserialize)]
pub struct ReturnExceptionRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct CreateVehicleRequest {
    pub plate: String,
    pub vehicle_type: String,
    pub capacity_kg: f64,
    pub status: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub assigned_driver_id: Option<Uuid>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub company_id: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateVehicleRequest {
    pub plate: Option<String>,
    pub vehicle_type: Option<String>,
    pub capacity_kg: Option<f64>,
    pub status: Option<String>,
    pub assigned_driver_id: Option<Option<Uuid>>,
    pub company_id: Option<Option<String>>,
}

#[derive(Debug, Deserialize)]
pub struct AssignVehicleRequest {
    pub driver_id: Uuid,
}

#[derive(Debug, Deserialize)]
pub struct CreateMaintenanceRequest {
    pub description: String,
    pub due_date: DateTime<Utc>,
}

#[derive(Debug, Deserialize)]
pub struct CreateDriverRequest {
    pub user_id: String,
    pub license_number: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub phone: Option<String>,
    pub status: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current_vehicle_id: Option<Uuid>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateDriverRequest {
    pub license_number: Option<String>,
    pub phone: Option<Option<String>>,
    pub status: Option<String>,
    pub current_vehicle_id: Option<Option<Uuid>>,
}

#[derive(Debug, Deserialize)]
pub struct CreateWarehouseRequest {
    pub name: String,
    pub address: String,
    pub capacity: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub manager_user_id: Option<String>,
    pub default_low_stock_threshold: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub company_id: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateWarehouseRequest {
    pub name: Option<String>,
    pub address: Option<String>,
    pub capacity: Option<i64>,
    pub manager_user_id: Option<Option<String>>,
    pub default_low_stock_threshold: Option<i64>,
    pub company_id: Option<Option<String>>,
}

#[derive(Debug, Deserialize)]
pub struct CreateStockMovementRequest {
    pub asset_id: Uuid,
    pub direction: String,
    pub quantity: i64,
    pub reason: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shipment_id: Option<Uuid>,
    /// Optional per-item low-stock threshold for this movement's
    /// alert check; falls back to the warehouse default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub low_stock_threshold: Option<i64>,
}

#[derive(Debug, Deserialize)]
pub struct CreateRouteRequest {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub driver_id: Option<Uuid>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vehicle_id: Option<Uuid>,
    pub route_date: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start_latitude: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start_longitude: Option<f64>,
    pub status: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub company_id: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateRouteRequest {
    pub name: Option<String>,
    pub driver_id: Option<Option<Uuid>>,
    pub vehicle_id: Option<Option<Uuid>>,
    pub route_date: Option<DateTime<Utc>>,
    pub start_latitude: Option<Option<f64>>,
    pub start_longitude: Option<Option<f64>>,
    pub status: Option<String>,
    pub company_id: Option<Option<String>>,
}

#[derive(Debug, Deserialize)]
pub struct AssignRouteRequest {
    pub driver_id: Uuid,
    pub vehicle_id: Uuid,
}

#[derive(Debug, Deserialize)]
pub struct CreateRouteStopRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shipment_id: Option<Uuid>,
    pub address: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub latitude: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub longitude: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateRouteStopRequest {
    pub shipment_id: Option<Option<Uuid>>,
    pub address: Option<String>,
    pub latitude: Option<Option<f64>>,
    pub longitude: Option<Option<f64>>,
    pub status: Option<String>,
    pub note: Option<Option<String>>,
}

#[derive(Debug, Deserialize)]
pub struct CreateRateTableRequest {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin_zone: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub destination_zone: Option<String>,
    pub base_price: f64,
    pub per_kg: f64,
    pub per_km: f64,
    pub currency: Option<String>,
    pub active: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub company_id: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateRateTableRequest {
    pub name: Option<String>,
    pub origin_zone: Option<Option<String>>,
    pub destination_zone: Option<Option<String>>,
    pub base_price: Option<f64>,
    pub per_kg: Option<f64>,
    pub per_km: Option<f64>,
    pub currency: Option<String>,
    pub active: Option<bool>,
    pub company_id: Option<Option<String>>,
}

#[derive(Debug, Deserialize)]
pub struct QuoteRequest {
    pub zone_origin: String,
    pub zone_destination: String,
    pub weight_kg: f64,
    pub length_cm: f64,
    pub width_cm: f64,
    pub height_cm: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub distance_km: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin_latitude: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin_longitude: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub destination_latitude: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub destination_longitude: Option<f64>,
    pub currency: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rate_table_id: Option<Uuid>,
}

#[derive(Debug, Deserialize)]
pub struct TrackQuery {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token: Option<String>,
}
