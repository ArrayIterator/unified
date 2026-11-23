//! Logistics module — shipments, fleet, warehouses, delivery — Types.
//!
//! Split from `src/modules/logistics.rs`; behaviour unchanged.

use super::constants::*;
use super::delivery_routes::*;
use super::drivers::*;
use super::helpers::*;
use super::hooks::*;
use super::module::*;
use super::rates::*;
use super::requests::*;
use super::shipments::*;
use super::state::*;
use super::tracking::*;
use super::vehicles::*;
use super::warehouses::*;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Lifecycle of a shipment. The happy path is `created` ->
/// `picked_up` -> `in_transit` -> `out_for_delivery` ->
/// `delivered`. `exception` is reachable from `picked_up`,
/// `in_transit`, and `out_for_delivery`; an exception resolves
/// back to `in_transit` or ends in `returned`. `cancelled` is
/// reachable from `created`, `picked_up`, and `in_transit`.
/// `delivered`, `returned`, and `cancelled` are terminal.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ShipmentStatus {
    Created,
    PickedUp,
    InTransit,
    OutForDelivery,
    Delivered,
    Exception,
    Returned,
    Cancelled,
}

impl Default for ShipmentStatus {
    fn default() -> Self {
        Self::Created
    }
}

impl ShipmentStatus {
    pub(super) fn as_str(&self) -> &'static str {
        match self {
            Self::Created => "created",
            Self::PickedUp => "picked_up",
            Self::InTransit => "in_transit",
            Self::OutForDelivery => "out_for_delivery",
            Self::Delivered => "delivered",
            Self::Exception => "exception",
            Self::Returned => "returned",
            Self::Cancelled => "cancelled",
        }
    }

    pub(super) fn parse(raw: &str) -> Option<Self> {
        Some(match raw {
            "created" => Self::Created,
            "picked_up" => Self::PickedUp,
            "in_transit" => Self::InTransit,
            "out_for_delivery" => Self::OutForDelivery,
            "delivered" => Self::Delivered,
            "exception" => Self::Exception,
            "returned" => Self::Returned,
            "cancelled" => Self::Cancelled,
            _ => return None,
        })
    }

    /// Terminal states: no transition leaves them.
    pub(super) fn is_terminal(&self) -> bool {
        matches!(self, Self::Delivered | Self::Returned | Self::Cancelled)
    }
}

/// The legal transitions of the shipment state machine. Any
/// transition not listed here is rejected with 409. `exception`
/// is entered only through the exceptions endpoint (which also
/// records the exception row): the raw `transition` endpoint
/// rejects `exception` as a target. Likewise `delivered` is
/// entered only through the proof of delivery endpoint, which
/// moves the shipment through this same table.
pub(super) fn allowed_transitions(from: ShipmentStatus) -> &'static [ShipmentStatus] {
    match from {
        ShipmentStatus::Created => &[
            ShipmentStatus::PickedUp,
            ShipmentStatus::Cancelled,
        ],
        ShipmentStatus::PickedUp => &[
            ShipmentStatus::InTransit,
            ShipmentStatus::Exception,
            ShipmentStatus::Cancelled,
        ],
        ShipmentStatus::InTransit => &[
            ShipmentStatus::OutForDelivery,
            ShipmentStatus::Exception,
            ShipmentStatus::Cancelled,
        ],
        ShipmentStatus::OutForDelivery => &[
            ShipmentStatus::Delivered,
            ShipmentStatus::Exception,
        ],
        ShipmentStatus::Exception => &[
            ShipmentStatus::InTransit,
            ShipmentStatus::Returned,
        ],
        ShipmentStatus::Delivered
        | ShipmentStatus::Returned
        | ShipmentStatus::Cancelled => &[],
    }
}

/// One endpoint of a shipment: address text plus optional
/// coordinates. Coordinates are validated ranges only; no
/// geocoding is performed here.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShipmentEndpoint {
    pub address: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub latitude: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub longitude: Option<f64>,
}

/// One shipment.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Shipment {
    pub id: Uuid,
    pub tracking_number: String,
    /// SHA-256 hex of the public tracking verification token.
    /// The raw token is shown once at creation and never stored.
    /// Never serialised: the token hash must not leave the server.
    #[serde(skip_serializing)]
    pub tracking_token_hash: String,
    /// Store order id, when linked. The store module owns orders;
    /// this is a plain string reference, never a duplicated row.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub order_id: Option<String>,
    /// `linked` (store confirmed the order), `unverified` (no
    /// store answer yet — see the order-link note), or `none`.
    pub order_link_status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub company_id: Option<Uuid>,
    pub status: ShipmentStatus,
    pub origin: ShipmentEndpoint,
    pub destination: ShipmentEndpoint,
    pub weight_kg: f64,
    pub length_cm: f64,
    pub width_cm: f64,
    pub height_cm: f64,
    pub zone_origin: String,
    pub zone_destination: String,
    pub service_level: String,
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shipping_cost: Option<f64>,
    pub currency: String,
    pub created_by: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// One timeline entry: every status transition, reschedule, and
/// note on a shipment, with the actor and an optional location.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShipmentTimelineEntry {
    pub id: Uuid,
    pub shipment_id: Uuid,
    /// `status_change`, `rescheduled`, `note`, `exception_opened`,
    /// `exception_resolved`, `proof_recorded`, `order_linked`.
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from_status: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub to_status: Option<String>,
    pub actor: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub location: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    pub created_at: DateTime<Utc>,
}

/// One vehicle in the fleet.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Vehicle {
    pub id: Uuid,
    pub plate: String,
    /// `van`, `truck`, `motorcycle`, `car`, `other`.
    pub vehicle_type: String,
    pub capacity_kg: f64,
    /// `available`, `in_service`, `maintenance`, `retired`.
    pub status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub assigned_driver_id: Option<Uuid>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub company_id: Option<Uuid>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub maintenance: Vec<MaintenanceReminder>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// One maintenance reminder on a vehicle. `scheduler_status` is
/// `not_scheduled`, `queued`, or `declined` (see the scheduler
/// note in the module header).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MaintenanceReminder {
    pub id: Uuid,
    pub description: String,
    pub due_date: DateTime<Utc>,
    /// `pending`, `due`, `done`.
    pub status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scheduler_job_id: Option<String>,
    pub scheduler_status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completed_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// One driver. `user_id` is the opaque user id from the
/// `X-User-Id` header: the users module owns user rows, this
/// module only references them and never duplicates them.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Driver {
    pub id: Uuid,
    pub user_id: String,
    pub license_number: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub phone: Option<String>,
    /// `active`, `inactive`, `on_leave`.
    pub status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current_vehicle_id: Option<Uuid>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// One warehouse.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Warehouse {
    pub id: Uuid,
    pub name: String,
    pub address: String,
    /// Storage capacity in the warehouse's own unit count.
    pub capacity: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub manager_user_id: Option<String>,
    pub default_low_stock_threshold: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub company_id: Option<Uuid>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// One stock movement in or out of a warehouse. Quantities only:
/// the assets / inventory module owns the item master data, so
/// `asset_id` is the assets stock item id (plain UUID reference,
/// no foreign key) and no item name or price is stored here.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StockMovement {
    pub id: Uuid,
    pub warehouse_id: Uuid,
    pub asset_id: Uuid,
    /// `in` or `out`.
    pub direction: String,
    pub quantity: i64,
    /// On-hand quantity for this (warehouse, asset) after the move.
    pub quantity_after: i64,
    pub reason: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shipment_id: Option<Uuid>,
    pub actor: String,
    pub created_at: DateTime<Utc>,
}

/// One delivery route.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeliveryRoute {
    pub id: Uuid,
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
    /// `planned`, `active`, `completed`, `cancelled`.
    pub status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub company_id: Option<Uuid>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// One stop on a delivery route, in visit order (`sequence` is
/// 1-based and contiguous after reorder).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RouteStop {
    pub id: Uuid,
    pub route_id: Uuid,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shipment_id: Option<Uuid>,
    pub sequence: i64,
    pub address: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub latitude: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub longitude: Option<f64>,
    /// `pending`, `visited`, `skipped`.
    pub status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Proof of delivery. Only storage references and a content hash
/// are stored: the photo / signature bytes live in the storage
/// module, referenced by target + key.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProofOfDelivery {
    pub id: Uuid,
    pub shipment_id: Uuid,
    pub storage_target: String,
    pub storage_key: String,
    pub content_sha256: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signature_storage_target: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signature_storage_key: Option<String>,
    pub recipient_name: String,
    pub delivered_at: DateTime<Utc>,
    pub actor: String,
    pub created_at: DateTime<Utc>,
}

/// One delivery exception.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeliveryException {
    pub id: Uuid,
    pub shipment_id: Uuid,
    /// `damaged`, `refused`, `address_issue`, `weather`, `lost`,
    /// `other`.
    pub exception_type: String,
    pub description: String,
    /// `open`, `resolved`, `returned`. Only these three statuses
    /// exist: no code path ever sets any other status, so the
    /// resolve / return guards accept `open` only.
    pub status: String,
    /// Shipment status before the exception, so a resolution can
    /// restore it (back to `in_transit`).
    pub previous_status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolution: Option<String>,
    pub actor: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolved_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// One configurable shipping rate table. `origin_zone` /
/// `destination_zone` of `None` mean "any zone" (the fallback
/// table). Cost = `base_price` + `per_kg` * billable weight +
/// `per_km` * distance.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RateTable {
    pub id: Uuid,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin_zone: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub destination_zone: Option<String>,
    pub base_price: f64,
    pub per_kg: f64,
    pub per_km: f64,
    pub currency: String,
    pub active: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub company_id: Option<Uuid>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_as_str_round_trip() {
        let cases = [
            (ShipmentStatus::Created, "created"),
            (ShipmentStatus::PickedUp, "picked_up"),
            (ShipmentStatus::InTransit, "in_transit"),
            (ShipmentStatus::OutForDelivery, "out_for_delivery"),
            (ShipmentStatus::Delivered, "delivered"),
            (ShipmentStatus::Exception, "exception"),
            (ShipmentStatus::Returned, "returned"),
            (ShipmentStatus::Cancelled, "cancelled"),
        ];
        for (status, text) in cases {
            assert_eq!(status.as_str(), text);
            assert_eq!(ShipmentStatus::parse(text), Some(status));
        }
    }

    #[test]
    fn status_parse_unknown_returns_none() {
        assert_eq!(ShipmentStatus::parse(""), None);
        assert_eq!(ShipmentStatus::parse("shipped"), None);
        assert_eq!(ShipmentStatus::parse("Created"), None);
    }

    #[test]
    fn status_is_terminal() {
        assert!(ShipmentStatus::Delivered.is_terminal());
        assert!(ShipmentStatus::Returned.is_terminal());
        assert!(ShipmentStatus::Cancelled.is_terminal());
        assert!(!ShipmentStatus::Created.is_terminal());
        assert!(!ShipmentStatus::InTransit.is_terminal());
        assert!(!ShipmentStatus::Exception.is_terminal());
    }

    #[test]
    fn allowed_transitions_from_created() {
        assert_eq!(
            allowed_transitions(ShipmentStatus::Created),
            &[ShipmentStatus::PickedUp, ShipmentStatus::Cancelled]
        );
    }

    #[test]
    fn allowed_transitions_from_picked_up() {
        let allowed = allowed_transitions(ShipmentStatus::PickedUp);
        assert!(allowed.contains(&ShipmentStatus::InTransit));
        assert!(allowed.contains(&ShipmentStatus::Exception));
        assert!(allowed.contains(&ShipmentStatus::Cancelled));
        assert!(!allowed.contains(&ShipmentStatus::Delivered));
    }

    #[test]
    fn allowed_transitions_from_exception() {
        assert_eq!(
            allowed_transitions(ShipmentStatus::Exception),
            &[ShipmentStatus::InTransit, ShipmentStatus::Returned]
        );
    }

    #[test]
    fn allowed_transitions_from_out_for_delivery() {
        let allowed = allowed_transitions(ShipmentStatus::OutForDelivery);
        assert!(allowed.contains(&ShipmentStatus::Delivered));
        assert!(allowed.contains(&ShipmentStatus::Exception));
        assert!(!allowed.contains(&ShipmentStatus::Cancelled));
        assert!(!allowed.contains(&ShipmentStatus::Returned));
    }

    #[test]
    fn allowed_transitions_terminal_states_are_empty() {
        assert!(allowed_transitions(ShipmentStatus::Delivered).is_empty());
        assert!(allowed_transitions(ShipmentStatus::Returned).is_empty());
        assert!(allowed_transitions(ShipmentStatus::Cancelled).is_empty());
    }
}
