// Logistics module — shipments, fleet, warehouses, delivery
// routes, delivery scheduling, proof of delivery, delivery
// exceptions, and shipping cost quotes.
//
// Routes live under `/api/v1/logistics`:
//
//   Shipments:
//     GET/POST /shipments — list shipments / create a shipment.
//     GET/PATCH/DELETE /shipments/{id}.
//     POST /shipments/{id}/transition — move the shipment through
//          the status state machine (reason required for terminal
//          and exception branches).
//     GET  /shipments/{id}/timeline — every status transition with
//          actor, timestamp, location, and note.
//     POST /shipments/{id}/link-order — link an existing store
//          order (verified through the `store.order.lookup` hook
//          query; see the order-link note below).
//     POST /shipments/{id}/schedule — set the delivery window.
//     POST /shipments/{id}/reschedule — change the delivery window
//          (audited through the timeline).
//     POST /shipments/{id}/proof — record proof of delivery
//          (storage reference + hash only, never raw bytes).
//     GET  /shipments/{id}/proof — list proofs of delivery.
//     POST /shipments/{id}/exceptions — open a delivery exception.
//     GET  /shipments/{id}/exceptions — list exceptions.
//   Fleet:
//     GET/POST /vehicles, GET/PATCH/DELETE /vehicles/{id}.
//     POST /vehicles/{id}/assign — assign a driver to a vehicle.
//     GET/POST /vehicles/{id}/maintenance — list / create a
//          maintenance reminder (queued through the scheduler).
//   Drivers:
//     GET/POST /drivers, GET/PATCH/DELETE /drivers/{id}.
//   Warehouses:
//     GET/POST /warehouses, GET/PATCH/DELETE /warehouses/{id}.
//     GET/POST /warehouses/{id}/stock-movements — list / record a
//          stock movement (quantities only; the assets / inventory
//          module owns the item master data).
//     GET  /warehouses/{id}/stock-movements/{movement_id}.
//   Delivery routes:
//     GET/POST /routes, GET/PATCH/DELETE /routes/{id}.
//     GET/POST /routes/{id}/stops.
//     PATCH/DELETE /routes/{id}/stops/{stop_id}.
//     POST /routes/{id}/assign — assign a driver + vehicle.
//     POST /routes/{id}/reorder — nearest-neighbour stop reorder
//          (heuristic only; see the route note below).
//   Exceptions:
//     POST /exceptions/{exception_id}/resolve.
//     POST /exceptions/{exception_id}/return.
//   Pricing:
//     GET/POST /rate-tables, GET/PATCH/DELETE /rate-tables/{id}.
//     POST /quote — shipping cost quote from a rate table.
//   Public tracking (no auth; see the public-access note below):
//     GET  /track/{tracking_number}?token=<verification token>.
//
// HONESTY — READ THIS FIRST. What is real in this module:
// in-memory shipment, timeline, vehicle, driver, warehouse, stock
// movement, route, stop, proof, exception, and rate-table
// management; the shipment status state machine with illegal
// transitions rejected; tracking numbers with a check digit;
// delivery windows with audited rescheduling; proof of delivery
// as storage references plus a content hash; exception workflow
// with customer notification; shipping cost quotes from
// configurable rate tables; and a token-gated, rate-limited public
// tracking lookup that exposes status and location only. What is
// NOT built here: real route optimisation (only a
// nearest-neighbour reorder heuristic — see the route note); live
// carrier APIs or label printing; actual distance measurement
// (distance comes from the caller or from the Calculator module
// through the `calculator.distance_requested` hook query — see the
// distance note); payment collection or refunds (the store /
// billing modules own those); and actual email / push delivery
// (the notifications module owns delivery — this module only
// emits `notification.requested`).
//
// Tracking number format: `LG-` followed by 8 decimal digits
// followed by `-` and one Luhn check digit computed over the 8
// digits, for example `LG-48291736-5`. The 8 digits are drawn at
// random; a collision with an existing tracking number regenerates
// them. The check digit lets a mistyped number be rejected
// without a database lookup, but it is not a secret: the public
// tracking lookup is protected by the verification token, not by
// the tracking number.
//
// Order-link note: a shipment may link a store order id, but this
// module never duplicates order rows and never reads the store's
// tables directly. Linking asks the store module through the
// `store.order.lookup` hook query with `{"order_id": "..."}` and
// expects a result carrying `{"found": true}` (or `false`). When
// the store answers `found: false` the link is rejected with
// 422. When no handler answers at all — today the store module
// has no `store.order.lookup` answerer — the shipment is still
// created but the link is recorded as `unverified`
// (`order_link_status: "unverified"`) and the response carries a
// warning; the shipment is NOT treated as linked to a real order
// until the store confirms it. The follow-up is for the store
// module to answer `store.order.lookup`.
//
// Distance note: a quote needs a distance in kilometres. The
// caller may supply `distance_km` directly. When it is absent and
// both endpoints carry coordinates, this module emits
// `calculator.distance_requested` with
// `{"origin_latitude", "origin_longitude", "destination_latitude",
// "destination_longitude"}` and expects a result carrying
// `{"distance_km": <number>, "method": "<name>"}`. No handler
// answers that event today, so an unanswered query fails the
// quote with 422 asking for `distance_km`; no distance is ever
// guessed. The follow-up is for the Calculator (or geo) module to
// answer `calculator.distance_requested`. This module performs no
// distance or pricing math of its own beyond applying the rate
// table to a billable weight and a distance it was given.
//
// Route note: route optimisation is out of scope. The reorder
// endpoint applies a nearest-neighbour heuristic starting from
// the route start point (or the first stop when no start point is
// set): repeatedly pick the closest unvisited stop by straight-line
// distance. It is a convenience ordering, not an optimiser — it
// ignores roads, traffic, time windows, and vehicle capacity —
// and it refuses (422) when any stop lacks coordinates instead of
// silently ordering on incomplete data.
//
// Public-access gate: the API Manager module enforces public access
// through its admin-managed `ApiAccessRule` records (default deny:
// an endpoint is public only when a rule exists with
// `public_access: true`). The public tracking route below relies
// on that gate: an administrator must create the matching
// `ApiAccessRule` row for `GET /api/v1/logistics/track/*`.
// Do NOT treat the absence of a capability check on the public
// route as an open door: without the rule the API Manager denies
// the call. On top of the gate, the lookup requires the
// per-shipment verification token (issued once at creation, stored
// only as a SHA-256 hash), is rate-limited to 30 lookups per
// tracking number per client address per 10 minutes (429 beyond
// that), and returns status and location only — never the
// recipient name, addresses, phone data, cost, or order id.
//
// Actor identity: until the auth module provides an extractor, the
// caller is identified by the `X-User-Id` header (required) and
// `X-Role` carries the role slug the capability check runs
// against, exactly like the ai module. Capability checks use the
// `role.check` hook and fail closed (no answer means not allowed).
//
// Events emitted on the hook bus:
//   - `logistics.shipment.created`, `logistics.shipment.updated`,
//     `logistics.shipment.deleted`, `logistics.shipment.cancelled`,
//     `logistics.shipment.delivered`, `logistics.shipment.returned`,
//     `logistics.shipment.status_changed`
//   - `logistics.shipment.timeline_added`
//   - `logistics.vehicle.created`, `logistics.vehicle.updated`,
//     `logistics.vehicle.deleted`, `logistics.vehicle.assigned`,
//     `logistics.vehicle.maintenance_created`,
//     `logistics.vehicle.maintenance_due`
//   - `logistics.driver.created`, `logistics.driver.updated`,
//     `logistics.driver.deleted`
//   - `logistics.warehouse.created`, `logistics.warehouse.updated`,
//     `logistics.warehouse.deleted`
//   - `logistics.stock.moved`, `logistics.stock.low`
//   - `logistics.route.created`, `logistics.route.updated`,
//     `logistics.route.deleted`, `logistics.route.assigned`,
//     `logistics.route.reordered`
//   - `logistics.route_stop.created`, `logistics.route_stop.updated`,
//     `logistics.route_stop.deleted`
//   - `logistics.proof.created`
//   - `logistics.exception.created`, `logistics.exception.resolved`,
//     `logistics.exception.returned`
//   - `logistics.rate_table.created`, `logistics.rate_table.updated`,
//     `logistics.rate_table.deleted`
//   - `logistics.quote.created`
//   - `notification.requested` (delivery exceptions, low-stock
//     alerts, maintenance reminders, delivery completion) with the
//     notifications module's payload shape: `{"kind", "user_id",
//     "title", "body", "module": "logistics", "link", "priority",
//     "channels"}`.
//   - `scheduler.enqueue` with `{"kind":
//     "logistics.maintenance.remind", "payload": {"vehicle_id",
//     "reminder_id", ...}}` when a maintenance reminder is created.
//   - `store.order.lookup` (query, not an emit-and-forget event)
//     with `{"order_id": "..."}` when linking an order.
//   - `calculator.distance_requested` (query) with
//     `{"origin_latitude", "origin_longitude",
//     "destination_latitude", "destination_longitude"}` when a
//     quote needs a distance.
// Events this module listens to:
//   - `order.paid` — records the paid order id so a linked
//     shipment can be matched by the caller; no shipment is
//     auto-created (a paid order may be for digital goods).
//   - `store.order.cancelled` — cancels linked shipments still in
//     `created` status with reason "linked order cancelled" and
//     notifies the shipment creator.
//   - `assets.stock.low` — when the payload names a warehouse this
//     module knows (`warehouse_id`), notifies that warehouse's
//     manager and emits `logistics.stock.low`.
//   - `logistics.maintenance.remind_requested` — the event the
//     scheduler emits when a `logistics.maintenance.remind` job
//     fires, following its naming rule (`backup.run` ->
//     `backup.run_requested`); marks the reminder due and notifies
//     the assigned driver.
//
// Scheduler note: `logistics.maintenance.remind` is NOT in the
// scheduler's `KNOWN_KINDS` list yet, so today the scheduler
// answers `{"queued": false, "error": "Unknown job kind: ..."}` and
// the reminder records `scheduler_status: "declined"` instead of
// pretending it was scheduled. The reminder is still stored and
// stays usable (it can be checked manually through the vehicle
// maintenance list); the follow-up is to add
// `"logistics.maintenance.remind"` to `KNOWN_KINDS` in
// `scheduler.rs` and arm the dispatch so the job emits
// `logistics.maintenance.remind_requested` with the reminder
// payload (see the integrator snippet in the module report).

use crate::core::context::AppContext;
use crate::core::error::{UnifiedError, UnifiedResult};
use crate::core::hooks::{HookBus, HookContext};
use crate::core::json;
use crate::core::modules::{Module, ModuleRegistry, RouteInfo, RouteVersionSupport};
use crate::core::pagination::{PageResult, PaginationParams};
use actix_web::{web, HttpRequest, HttpResponse};
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use rand::Rng;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::sync::{Arc, RwLock};
use uuid::Uuid;

/// Maximum lengths enforced on user supplied text.
const MAX_ADDRESS_LEN: usize = 500;
const MAX_NAME_LEN: usize = 200;
const MAX_NOTE_LEN: usize = 2000;
const MAX_REASON_LEN: usize = 500;
const MAX_PLATE_LEN: usize = 20;
const MAX_LICENSE_LEN: usize = 64;
const MAX_PHONE_LEN: usize = 32;
const MAX_ZONE_LEN: usize = 64;
const MAX_SERVICE_LEVEL_LEN: usize = 64;
const MAX_CURRENCY_LEN: usize = 3;
/// Public tracking rate limit: lookups per tracking number per
/// client address per window.
const TRACK_RATE_LIMIT: u64 = 30;
const TRACK_RATE_WINDOW_SECS: i64 = 600;
/// Volumetric divisor (cubic centimetres per kilogram) used for
/// dimensional weight, matching common carrier practice.
const DIM_DIVISOR: f64 = 5000.0;
/// Maximum weight / dimension accepted on a shipment.
const MAX_WEIGHT_KG: f64 = 100_000.0;
const MAX_DIMENSION_CM: f64 = 10_000.0;

/// Capability names checked through the `role.check` hook.
const PERM_LOGISTICS_VIEW: &str = "logistics.view";
const PERM_LOGISTICS_CREATE: &str = "logistics.create";
const PERM_LOGISTICS_MANAGE: &str = "logistics.manage";
const PERM_LOGISTICS_DRIVER: &str = "logistics.driver";
const PERM_LOGISTICS_QUOTE: &str = "logistics.quote";

/// Hook event names this module listens to (literals repeated here
/// so the module stays decoupled from the store / assets modules).
const ORDER_PAID: &str = "order.paid";
const STORE_ORDER_CANCELLED: &str = "store.order.cancelled";
const ASSETS_STOCK_LOW: &str = "assets.stock.low";
const MAINTENANCE_REMIND_REQUESTED: &str = "logistics.maintenance.remind_requested";

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

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
    fn as_str(&self) -> &'static str {
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

    fn parse(raw: &str) -> Option<Self> {
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
    fn is_terminal(&self) -> bool {
        matches!(self, Self::Delivered | Self::Returned | Self::Cancelled)
    }
}

/// The legal transitions of the shipment state machine. Any
/// transition not listed here is rejected with 409. `exception`
/// is entered through the exception endpoint (which also records
/// the exception row) as well as through `transition`, so both
/// paths share this table.
fn allowed_transitions(from: ShipmentStatus) -> &'static [ShipmentStatus] {
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
    /// `open`, `investigating`, `resolved`, `returned`.
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

// ---------------------------------------------------------------------------
// Request bodies
// ---------------------------------------------------------------------------

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

fn default_resume() -> bool {
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

// ---------------------------------------------------------------------------
// Shared state
// ---------------------------------------------------------------------------

/// In-memory stores for logistics. The hook bus is attached in
/// `init` (the bus lives in `AppContext`); handlers tolerate a
/// missing bus by skipping event emission. Durable rows for these
/// types are defined by migration `2026-11-22-000061_logistics`;
/// until that migration is executed, logistics data does not
/// survive a restart.
#[derive(Clone)]
pub struct LogisticsState {
    pub shipments: Arc<DashMap<Uuid, Shipment>>,
    pub timeline: Arc<DashMap<Uuid, ShipmentTimelineEntry>>,
    pub vehicles: Arc<DashMap<Uuid, Vehicle>>,
    pub drivers: Arc<DashMap<Uuid, Driver>>,
    pub warehouses: Arc<DashMap<Uuid, Warehouse>>,
    pub stock_movements: Arc<DashMap<Uuid, StockMovement>>,
    pub routes: Arc<DashMap<Uuid, DeliveryRoute>>,
    pub route_stops: Arc<DashMap<Uuid, RouteStop>>,
    pub proofs: Arc<DashMap<Uuid, ProofOfDelivery>>,
    pub exceptions: Arc<DashMap<Uuid, DeliveryException>>,
    pub rate_tables: Arc<DashMap<Uuid, RateTable>>,
    /// Public tracking rate-limit buckets, keyed by
    /// `client address + tracking number`: (requests in window,
    /// window start).
    pub track_buckets: Arc<DashMap<String, (u64, DateTime<Utc>)>>,
    pub hooks: Arc<RwLock<Option<Arc<HookBus>>>>,
}

impl Default for LogisticsState {
    fn default() -> Self {
        Self {
            shipments: Arc::new(DashMap::new()),
            timeline: Arc::new(DashMap::new()),
            vehicles: Arc::new(DashMap::new()),
            drivers: Arc::new(DashMap::new()),
            warehouses: Arc::new(DashMap::new()),
            stock_movements: Arc::new(DashMap::new()),
            routes: Arc::new(DashMap::new()),
            route_stops: Arc::new(DashMap::new()),
            proofs: Arc::new(DashMap::new()),
            exceptions: Arc::new(DashMap::new()),
            rate_tables: Arc::new(DashMap::new()),
            track_buckets: Arc::new(DashMap::new()),
            hooks: Arc::new(RwLock::new(None)),
        }
    }
}

impl LogisticsState {
    fn bus(&self) -> Option<Arc<HookBus>> {
        self.hooks.read().expect("hooks lock poisoned").clone()
    }

    fn emit(&self, event: &str, data: Value) {
        if let Some(bus) = self.bus() {
            bus.emit(event, data);
        }
    }

    /// Emit a `notification.requested` event in the notifications
    /// module's payload shape.
    fn notify(&self, kind: &str, user_id: &str, title: &str, body: &str, link: Option<String>) {
        self.emit(
            "notification.requested",
            json!({
                "kind": kind,
                "user_id": user_id,
                "title": title,
                "body": body,
                "module": "logistics",
                "category": "logistics",
                "priority": "normal",
                "link": link,
            }),
        );
    }

    fn add_timeline(
        &self,
        shipment_id: Uuid,
        kind: &str,
        from_status: Option<&str>,
        to_status: Option<&str>,
        actor: &str,
        location: Option<String>,
        note: Option<String>,
    ) {
        let now = Utc::now();
        let entry = ShipmentTimelineEntry {
            id: Uuid::now_v7(),
            shipment_id,
            kind: kind.to_string(),
            from_status: from_status.map(str::to_string),
            to_status: to_status.map(str::to_string),
            actor: actor.to_string(),
            location,
            note,
            created_at: now,
        };
        self.timeline.insert(entry.id, entry.clone());
        self.emit(
            "logistics.shipment.timeline_added",
            json!({
                "shipment_id": shipment_id.to_string(),
                "timeline_id": entry.id.to_string(),
                "kind": kind,
                "from_status": from_status,
                "to_status": to_status,
                "actor": actor,
            }),
        );
    }

    /// On-hand quantity for one (warehouse, asset) pair, derived
    /// from the recorded movements. `in` adds, `out` subtracts.
    fn stock_on_hand(&self, warehouse_id: Uuid, asset_id: Uuid) -> i64 {
        self.stock_movements
            .iter()
            .filter(|entry| {
                entry.value().warehouse_id == warehouse_id && entry.value().asset_id == asset_id
            })
            .map(|entry| {
                if entry.value().direction == "in" {
                    entry.value().quantity
                } else {
                    -entry.value().quantity
                }
            })
            .sum()
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Caller identity: `X-User-Id` (required) plus `X-Role` (required
/// for capability checks), exactly like the ai module.
struct Caller {
    actor: String,
    role: String,
}

fn caller(req: &HttpRequest) -> UnifiedResult<Caller> {
    let actor = req
        .headers()
        .get("x-user-id")
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .ok_or_else(|| UnifiedError::unauthorized("Missing X-User-Id header"))?;
    let role = req
        .headers()
        .get("x-role")
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .ok_or_else(|| UnifiedError::unauthorized("Missing X-Role header"))?;
    Ok(Caller { actor, role })
}

/// Ask the roles module whether `role` holds `permission` through
/// the `role.check` hook. Fails closed: no answer means not allowed.
fn check_capability(ctx: &AppContext, role: &str, permission: &str) -> bool {
    let context = ctx.hooks.emit(
        "role.check",
        json!({ "role": role, "permission": permission, "allowed": false }),
    );
    context
        .get("allowed")
        .and_then(|value| value.as_bool())
        .unwrap_or(false)
}

fn require_permission(ctx: &AppContext, role: &str, permission: &str) -> UnifiedResult<()> {
    if check_capability(ctx, role, permission) {
        Ok(())
    } else {
        Err(UnifiedError::forbidden(format!(
            "Role \"{role}\" does not hold permission \"{permission}\""
        )))
    }
}

/// A driver may operate a shipment (transition, proof) when they
/// hold `logistics.driver` and are the shipment's assigned driver;
/// a manager may always operate it. Anyone else is rejected. The
/// caller's driver row is resolved by their user id, so a caller
/// cannot claim another driver's shipment by guessing a driver id.
fn can_operate_shipment(
    ctx: &AppContext,
    state: &LogisticsState,
    caller: &Caller,
    shipment: &Shipment,
) -> UnifiedResult<()> {
    if check_capability(ctx, &caller.role, PERM_LOGISTICS_MANAGE) {
        return Ok(());
    }
    if check_capability(ctx, &caller.role, PERM_LOGISTICS_DRIVER) {
        let assigned = shipment.driver_id.is_some_and(|driver_id| {
            state
                .drivers
                .get(&driver_id)
                .is_some_and(|entry| entry.value().user_id == caller.actor)
        });
        if assigned {
            return Ok(());
        }
    }
    Err(UnifiedError::forbidden(
        "Caller may not operate this shipment",
    ))
}

fn parse_uuid(raw: &str, what: &str) -> UnifiedResult<Uuid> {
    Uuid::parse_str(raw).map_err(|_| UnifiedError::bad_request(format!("Invalid {what} id: {raw}")))
}

fn parse_optional_company(raw: &Option<String>) -> UnifiedResult<Option<Uuid>> {
    match raw {
        None => Ok(None),
        Some(value) if value.trim().is_empty() => Ok(None),
        Some(value) => Ok(Some(parse_uuid(value, "company")?)),
    }
}

fn require_text(value: &str, field: &str, max: usize) -> UnifiedResult<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err(UnifiedError::bad_request(format!("{field} must not be empty")));
    }
    if trimmed.len() > max {
        return Err(UnifiedError::bad_request(format!(
            "{field} must be at most {max} characters"
        )));
    }
    Ok(trimmed.to_string())
}

fn optional_text(value: &Option<String>, field: &str, max: usize) -> UnifiedResult<Option<String>> {
    match value {
        None => Ok(None),
        Some(raw) => {
            let trimmed = raw.trim();
            if trimmed.is_empty() {
                Ok(None)
            } else if trimmed.len() > max {
                Err(UnifiedError::bad_request(format!(
                    "{field} must be at most {max} characters"
                )))
            } else {
                Ok(Some(trimmed.to_string()))
            }
        }
    }
}

fn ok<T: Serialize>(data: T) -> HttpResponse {
    json::success_response(data, 200)
}

fn created<T: Serialize>(data: T) -> HttpResponse {
    json::success_response(data, 201)
}

fn err(error: UnifiedError, debug: bool) -> HttpResponse {
    json::error_response(&error, debug)
}

fn paginate<T: Serialize + Clone>(items: Vec<T>, params: &PaginationParams) -> PageResult<T> {
    let pagination = params.normalized();
    let total = items.len() as u64;
    let start = ((pagination.page - 1) * pagination.per_page) as usize;
    let page_items: Vec<T> = items
        .into_iter()
        .skip(start)
        .take(pagination.per_page as usize)
        .collect();
    PageResult::new(page_items, total, &pagination)
}

/// Money: finite, non-negative, rounded to 4 decimal places.
fn valid_money(value: f64, field: &str) -> UnifiedResult<f64> {
    if !value.is_finite() || value < 0.0 {
        return Err(UnifiedError::bad_request(format!(
            "{field} must be a finite number >= 0"
        )));
    }
    Ok(round_money(value))
}

fn round_money(value: f64) -> f64 {
    (value * 10_000.0).round() / 10_000.0
}

fn valid_currency(currency: &str) -> UnifiedResult<String> {
    let upper = currency.trim().to_uppercase();
    if upper.len() != MAX_CURRENCY_LEN || !upper.chars().all(|c| c.is_ascii_uppercase()) {
        return Err(UnifiedError::bad_request(
            "currency must be a 3-letter ISO code such as \"USD\"",
        ));
    }
    Ok(upper)
}

fn validate_latitude(value: f64, field: &str) -> UnifiedResult<()> {
    if !value.is_finite() || !(-90.0..=90.0).contains(&value) {
        return Err(UnifiedError::bad_request(format!(
            "{field} must be between -90 and 90"
        )));
    }
    Ok(())
}

fn validate_longitude(value: f64, field: &str) -> UnifiedResult<()> {
    if !value.is_finite() || !(-180.0..=180.0).contains(&value) {
        return Err(UnifiedError::bad_request(format!(
            "{field} must be between -180 and 180"
        )));
    }
    Ok(())
}

fn validate_endpoint(endpoint: &ShipmentEndpoint, field: &str) -> UnifiedResult<ShipmentEndpoint> {
    let address = require_text(&endpoint.address, &format!("{field}.address"), MAX_ADDRESS_LEN)?;
    if let Some(latitude) = endpoint.latitude {
        validate_latitude(latitude, &format!("{field}.latitude"))?;
    }
    if let Some(longitude) = endpoint.longitude {
        validate_longitude(longitude, &format!("{field}.longitude"))?;
    }
    Ok(ShipmentEndpoint {
        address,
        latitude: endpoint.latitude,
        longitude: endpoint.longitude,
    })
}

fn validate_dimensions(weight_kg: f64, length: f64, width: f64, height: f64) -> UnifiedResult<()> {
    if !weight_kg.is_finite() || weight_kg <= 0.0 || weight_kg > MAX_WEIGHT_KG {
        return Err(UnifiedError::bad_request(format!(
            "weight_kg must be > 0 and <= {MAX_WEIGHT_KG}"
        )));
    }
    for (value, name) in [
        (length, "length_cm"),
        (width, "width_cm"),
        (height, "height_cm"),
    ] {
        if !value.is_finite() || value <= 0.0 || value > MAX_DIMENSION_CM {
            return Err(UnifiedError::bad_request(format!(
                "{name} must be > 0 and <= {MAX_DIMENSION_CM}"
            )));
        }
    }
    Ok(())
}

fn validate_window(
    start: Option<DateTime<Utc>>,
    end: Option<DateTime<Utc>>,
) -> UnifiedResult<()> {
    if let (Some(start), Some(end)) = (start, end) {
        if end <= start {
            return Err(UnifiedError::bad_request(
                "window_end must be after window_start",
            ));
        }
    }
    Ok(())
}

fn sha256_hex(value: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(value.as_bytes());
    format!("{:x}", hasher.finalize())
}

/// Luhn check digit over a digit string: double every second digit
/// from the right, sum the digits, and return the digit that makes
/// the total a multiple of 10.
fn luhn_check_digit(digits: &str) -> char {
    let mut sum: u32 = 0;
    let mut double = true;
    for ch in digits.chars().rev() {
        let mut digit = ch.to_digit(10).unwrap_or(0);
        if double {
            digit *= 2;
            if digit > 9 {
                digit -= 9;
            }
        }
        sum += digit;
        double = !double;
    }
    let check = (10 - (sum % 10)) % 10;
    char::from_digit(check, 10).unwrap_or('0')
}

/// Generate a tracking number `LG-<8 digits>-<check digit>`,
/// unique against the shipments already stored. Collisions
/// regenerate the digits; the loop is bounded so a pathological
/// store cannot hang the request.
fn generate_tracking_number(state: &LogisticsState) -> UnifiedResult<String> {
    let mut rng = rand::rng();
    for _ in 0..100 {
        let digits: String = (0..8)
            .map(|_| rng.random_range(0..10).to_string())
            .collect();
        let check = luhn_check_digit(&digits);
        let candidate = format!("LG-{digits}-{check}");
        if !state
            .shipments
            .iter()
            .any(|entry| entry.value().tracking_number == candidate)
        {
            return Ok(candidate);
        }
    }
    Err(UnifiedError::internal(
        "Could not generate a unique tracking number",
    ))
}

/// Generate a 32-character hex verification token for public
/// tracking. The raw token is returned to the caller once; only
/// its SHA-256 hash is stored on the shipment.
fn generate_tracking_token() -> String {
    let mut rng = rand::rng();
    let bytes: [u8; 16] = rng.random();
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn valid_sha256_hex(value: &str) -> bool {
    value.len() == 64
        && value
            .chars()
            .all(|c| c.is_ascii_digit() || ('a'..='f').contains(&c))
}

/// Best-effort client address for the public tracking rate limit:
/// first entry of `X-Forwarded-For`, else the peer address.
fn client_address(req: &HttpRequest) -> String {
    if let Some(forwarded) = req.headers().get("x-forwarded-for") {
        if let Ok(raw) = forwarded.to_str() {
            if let Some(first) = raw.split(',').next().map(str::trim) {
                if !first.is_empty() {
                    return first.to_string();
                }
            }
        }
    }
    req.connection_info()
        .realip_remote_addr()
        .unwrap_or("unknown")
        .to_string()
}

fn load_shipment(state: &LogisticsState, raw_id: &str) -> UnifiedResult<Shipment> {
    let id = parse_uuid(raw_id, "shipment")?;
    state
        .shipments
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Shipment not found: {id}")))
}

fn load_vehicle(state: &LogisticsState, raw_id: &str) -> UnifiedResult<Vehicle> {
    let id = parse_uuid(raw_id, "vehicle")?;
    state
        .vehicles
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Vehicle not found: {id}")))
}

fn load_driver(state: &LogisticsState, raw_id: &str) -> UnifiedResult<Driver> {
    let id = parse_uuid(raw_id, "driver")?;
    state
        .drivers
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Driver not found: {id}")))
}

fn load_warehouse(state: &LogisticsState, raw_id: &str) -> UnifiedResult<Warehouse> {
    let id = parse_uuid(raw_id, "warehouse")?;
    state
        .warehouses
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Warehouse not found: {id}")))
}

fn load_route(state: &LogisticsState, raw_id: &str) -> UnifiedResult<DeliveryRoute> {
    let id = parse_uuid(raw_id, "route")?;
    state
        .routes
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Delivery route not found: {id}")))
}

fn load_route_stop(
    state: &LogisticsState,
    route_id: Uuid,
    raw_id: &str,
) -> UnifiedResult<RouteStop> {
    let id = parse_uuid(raw_id, "route stop")?;
    state
        .route_stops
        .get(&id)
        .map(|entry| entry.value().clone())
        .filter(|stop| stop.route_id == route_id)
        .ok_or_else(|| UnifiedError::not_found(format!("Route stop not found: {id}")))
}

fn load_rate_table(state: &LogisticsState, raw_id: &str) -> UnifiedResult<RateTable> {
    let id = parse_uuid(raw_id, "rate table")?;
    state
        .rate_tables
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Rate table not found: {id}")))
}

fn load_exception(state: &LogisticsState, raw_id: &str) -> UnifiedResult<DeliveryException> {
    let id = parse_uuid(raw_id, "exception")?;
    state
        .exceptions
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Delivery exception not found: {id}")))
}

/// Ask the store module whether an order id exists, through the
/// `store.order.lookup` hook query. Returns `Some(true)` when the
/// store confirms the order, `Some(false)` when it denies it, and
/// `None` when no handler answers (the store has no answerer yet).
fn lookup_order(ctx: &AppContext, order_id: &str) -> Option<bool> {
    let context = ctx
        .hooks
        .emit("store.order.lookup", json!({ "order_id": order_id }));
    for result in &context.results {
        if let Some(found) = result.get("found").and_then(Value::as_bool) {
            return Some(found);
        }
    }
    None
}

/// Ask the Calculator module for a distance in kilometres between
/// two coordinate pairs, through the
/// `calculator.distance_requested` hook query. Returns `Some(km)`
/// when a handler answers with a finite positive distance, else
/// `None` (no handler answers today).
fn request_distance(
    ctx: &AppContext,
    origin_lat: f64,
    origin_lng: f64,
    dest_lat: f64,
    dest_lng: f64,
) -> Option<f64> {
    let context = ctx.hooks.emit(
        "calculator.distance_requested",
        json!({
            "origin_latitude": origin_lat,
            "origin_longitude": origin_lng,
            "destination_latitude": dest_lat,
            "destination_longitude": dest_lng,
        }),
    );
    for result in &context.results {
        if let Some(km) = result.get("distance_km").and_then(Value::as_f64) {
            if km.is_finite() && km > 0.0 {
                return Some(km);
            }
        }
    }
    None
}

/// Dimensional (volumetric) weight in kilograms from centimetre
/// dimensions, using the standard 5000 divisor.
fn dimensional_weight(length_cm: f64, width_cm: f64, height_cm: f64) -> f64 {
    (length_cm * width_cm * height_cm) / DIM_DIVISOR
}

/// Straight-line distance in kilometres between two coordinate
/// pairs (haversine). Used only for the nearest-neighbour route
/// reorder heuristic — never as a billable distance.
fn haversine_km(lat1: f64, lng1: f64, lat2: f64, lng2: f64) -> f64 {
    const EARTH_RADIUS_KM: f64 = 6371.0;
    let d_lat = (lat2 - lat1).to_radians();
    let d_lng = (lng2 - lng1).to_radians();
    let a = (d_lat / 2.0).sin().powi(2)
        + lat1.to_radians().cos() * lat2.to_radians().cos() * (d_lng / 2.0).sin().powi(2);
    2.0 * EARTH_RADIUS_KM * a.sqrt().asin()
}

/// Queue a maintenance reminder through `scheduler.enqueue` with
/// kind `logistics.maintenance.remind`. Records `queued` or
/// `declined` on the reminder (see the scheduler note).
fn schedule_maintenance_reminder(
    state: &LogisticsState,
    vehicle_id: Uuid,
    reminder: &mut MaintenanceReminder,
) {
    let Some(bus) = state.bus() else {
        reminder.scheduler_job_id = None;
        reminder.scheduler_status = "declined".to_string();
        return;
    };
    let answer = bus.emit(
        "scheduler.enqueue",
        json!({
            "kind": "logistics.maintenance.remind",
            "payload": {
                "vehicle_id": vehicle_id.to_string(),
                "reminder_id": reminder.id.to_string(),
                "due_date": reminder.due_date.to_rfc3339(),
            },
        }),
    );
    match answer.results.first() {
        Some(result) if result.get("queued").and_then(Value::as_bool) == Some(true) => {
            reminder.scheduler_job_id = result
                .get("job_id")
                .and_then(|value| value.as_str())
                .map(str::to_string);
            reminder.scheduler_status = "queued".to_string();
        }
        _ => {
            reminder.scheduler_job_id = None;
            reminder.scheduler_status = "declined".to_string();
        }
    }
}

// ---------------------------------------------------------------------------
// Shipment handlers
// ---------------------------------------------------------------------------

async fn list_shipments(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<LogisticsState>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_LOGISTICS_VIEW)?;
        let params = query.into_inner();
        let status_filter = params.filters.get("status").cloned();
        let warehouse_filter = params.filters.get("warehouse_id").cloned();
        let driver_filter = params.filters.get("driver_id").cloned();
        let vehicle_filter = params.filters.get("vehicle_id").cloned();
        let order_filter = params.filters.get("order_id").cloned();
        let company_filter = params.filters.get("company_id").cloned();
        let search = params.search.clone().unwrap_or_default().to_lowercase();
        let mut items: Vec<Shipment> = state
            .shipments
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|shipment| {
                if let Some(status) = &status_filter {
                    if shipment.status.as_str() != status.as_str() {
                        return false;
                    }
                }
                if let Some(warehouse_id) = &warehouse_filter {
                    if shipment.warehouse_id.map(|id| id.to_string()) != Some(warehouse_id.clone()) {
                        return false;
                    }
                }
                if let Some(driver_id) = &driver_filter {
                    if shipment.driver_id.map(|id| id.to_string()) != Some(driver_id.clone()) {
                        return false;
                    }
                }
                if let Some(vehicle_id) = &vehicle_filter {
                    if shipment.vehicle_id.map(|id| id.to_string()) != Some(vehicle_id.clone()) {
                        return false;
                    }
                }
                if let Some(order_id) = &order_filter {
                    if shipment.order_id.as_deref() != Some(order_id.as_str()) {
                        return false;
                    }
                }
                if let Some(company_id) = &company_filter {
                    if shipment.company_id.map(|id| id.to_string()) != Some(company_id.clone()) {
                        return false;
                    }
                }
                if !search.is_empty() {
                    let haystack = format!(
                        "{} {} {}",
                        shipment.tracking_number,
                        shipment.origin.address,
                        shipment.destination.address
                    )
                    .to_lowercase();
                    if !haystack.contains(&search) {
                        return false;
                    }
                }
                true
            })
            .collect();
        items.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        let page = paginate(items, &params);
        Ok(serde_json::to_value(page).unwrap_or(Value::Null))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn create_shipment(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<LogisticsState>,
    req: HttpRequest,
    body: web::Json<CreateShipmentRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_LOGISTICS_CREATE)?;
        let input = body.into_inner();
        let origin = validate_endpoint(&input.origin, "origin")?;
        let destination = validate_endpoint(&input.destination, "destination")?;
        validate_dimensions(input.weight_kg, input.length_cm, input.width_cm, input.height_cm)?;
        validate_window(input.window_start, input.window_end)?;
        let zone_origin = require_text(&input.zone_origin, "zone_origin", MAX_ZONE_LEN)?;
        let zone_destination =
            require_text(&input.zone_destination, "zone_destination", MAX_ZONE_LEN)?;
        let service_level = input
            .service_level
            .as_deref()
            .map(|value| require_text(value, "service_level", MAX_SERVICE_LEVEL_LEN))
            .transpose()?
            .unwrap_or_else(|| "standard".to_string());
        let currency = input
            .currency
            .as_deref()
            .map(valid_currency)
            .transpose()?
            .unwrap_or_else(|| "USD".to_string());
        let company_id = parse_optional_company(&input.company_id)?;
        if let Some(driver_id) = input.driver_id {
            if !state.drivers.contains_key(&driver_id) {
                return Err(UnifiedError::bad_request(format!(
                    "Driver not found: {driver_id}"
                )));
            }
        }
        if let Some(vehicle_id) = input.vehicle_id {
            if !state.vehicles.contains_key(&vehicle_id) {
                return Err(UnifiedError::bad_request(format!(
                    "Vehicle not found: {vehicle_id}"
                )));
            }
        }
        if let Some(warehouse_id) = input.warehouse_id {
            if !state.warehouses.contains_key(&warehouse_id) {
                return Err(UnifiedError::bad_request(format!(
                    "Warehouse not found: {warehouse_id}"
                )));
            }
        }
        // Order link: confirmed by the store, denied by the store,
        // or unverified when no handler answers (see the header).
        let mut order_id: Option<String> = None;
        let mut order_link_status = "none".to_string();
        let mut order_warning: Option<String> = None;
        if let Some(raw_order_id) = &input.order_id {
            let trimmed = raw_order_id.trim();
            if !trimmed.is_empty() {
                match lookup_order(&ctx, trimmed) {
                    Some(true) => {
                        order_id = Some(trimmed.to_string());
                        order_link_status = "linked".to_string();
                    }
                    Some(false) => {
                        return Err(UnifiedError::unprocessable(format!(
                            "Order not found in the store: {trimmed}"
                        )));
                    }
                    None => {
                        order_id = Some(trimmed.to_string());
                        order_link_status = "unverified".to_string();
                        order_warning = Some(
                            "Order link is unverified: the store module did not answer the store.order.lookup query. The shipment is not treated as linked to a real order until the store confirms it.".to_string(),
                        );
                    }
                }
            }
        }
        let tracking_number = generate_tracking_number(&state)?;
        let token = generate_tracking_token();
        let now = Utc::now();
        let shipment = Shipment {
            id: Uuid::now_v7(),
            tracking_number: tracking_number.clone(),
            tracking_token_hash: sha256_hex(&token),
            order_id,
            order_link_status,
            company_id,
            status: ShipmentStatus::Created,
            origin,
            destination,
            weight_kg: input.weight_kg,
            length_cm: input.length_cm,
            width_cm: input.width_cm,
            height_cm: input.height_cm,
            zone_origin,
            zone_destination,
            service_level,
            driver_id: input.driver_id,
            vehicle_id: input.vehicle_id,
            warehouse_id: input.warehouse_id,
            scheduled_at: input.scheduled_at,
            window_start: input.window_start,
            window_end: input.window_end,
            estimated_delivery_at: input.estimated_delivery_at,
            shipping_cost: None,
            currency,
            created_by: caller.actor.clone(),
            created_at: now,
            updated_at: now,
        };
        state.shipments.insert(shipment.id, shipment.clone());
        state.add_timeline(
            shipment.id,
            "status_change",
            None,
            Some("created"),
            &caller.actor,
            None,
            None,
        );
        state.emit(
            "logistics.shipment.created",
            json!({
                "shipment_id": shipment.id.to_string(),
                "tracking_number": shipment.tracking_number,
                "order_id": shipment.order_id,
                "order_link_status": shipment.order_link_status,
                "status": "created",
                "created_by": caller.actor,
            }),
        );
        // The verification token is returned once, inside the
        // creation response only. It is never stored raw and never
        // appears in any later response.
        let mut value = serde_json::to_value(&shipment).unwrap_or(Value::Null);
        if let Value::Object(map) = &mut value {
            map.insert(
                "tracking_token".to_string(),
                Value::String(token),
            );
            if let Some(warning) = order_warning {
                map.insert("order_link_warning".to_string(), Value::String(warning));
            }
        }
        Ok(value)
    })();
    match result {
        Ok(data) => created(data),
        Err(error) => err(error, debug),
    }
}

async fn get_shipment(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<LogisticsState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Shipment> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_LOGISTICS_VIEW)?;
        load_shipment(&state, &path)
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn update_shipment(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<LogisticsState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdateShipmentRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Shipment> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_LOGISTICS_MANAGE)?;
        let mut shipment = load_shipment(&state, &path)?;
        if shipment.status != ShipmentStatus::Created {
            return Err(UnifiedError::conflict(format!(
                "Only a shipment in \"created\" status can be edited; this shipment is \"{}\"",
                shipment.status.as_str()
            )));
        }
        let input = body.into_inner();
        if let Some(origin) = &input.origin {
            shipment.origin = validate_endpoint(origin, "origin")?;
        }
        if let Some(destination) = &input.destination {
            shipment.destination = validate_endpoint(destination, "destination")?;
        }
        let weight = input.weight_kg.unwrap_or(shipment.weight_kg);
        let length = input.length_cm.unwrap_or(shipment.length_cm);
        let width = input.width_cm.unwrap_or(shipment.width_cm);
        let height = input.height_cm.unwrap_or(shipment.height_cm);
        validate_dimensions(weight, length, width, height)?;
        shipment.weight_kg = weight;
        shipment.length_cm = length;
        shipment.width_cm = width;
        shipment.height_cm = height;
        if let Some(zone) = &input.zone_origin {
            shipment.zone_origin = require_text(zone, "zone_origin", MAX_ZONE_LEN)?;
        }
        if let Some(zone) = &input.zone_destination {
            shipment.zone_destination = require_text(zone, "zone_destination", MAX_ZONE_LEN)?;
        }
        if let Some(level) = &input.service_level {
            shipment.service_level = require_text(level, "service_level", MAX_SERVICE_LEVEL_LEN)?;
        }
        if let Some(driver_id) = input.driver_id {
            if let Some(id) = driver_id {
                if !state.drivers.contains_key(&id) {
                    return Err(UnifiedError::bad_request(format!("Driver not found: {id}")));
                }
            }
            shipment.driver_id = driver_id;
        }
        if let Some(vehicle_id) = input.vehicle_id {
            if let Some(id) = vehicle_id {
                if !state.vehicles.contains_key(&id) {
                    return Err(UnifiedError::bad_request(format!("Vehicle not found: {id}")));
                }
            }
            shipment.vehicle_id = vehicle_id;
        }
        if let Some(warehouse_id) = input.warehouse_id {
            if let Some(id) = warehouse_id {
                if !state.warehouses.contains_key(&id) {
                    return Err(UnifiedError::bad_request(format!(
                        "Warehouse not found: {id}"
                    )));
                }
            }
            shipment.warehouse_id = warehouse_id;
        }
        if let Some(scheduled_at) = input.scheduled_at {
            shipment.scheduled_at = scheduled_at;
        }
        if let Some(window_start) = input.window_start {
            shipment.window_start = window_start;
        }
        if let Some(window_end) = input.window_end {
            shipment.window_end = window_end;
        }
        validate_window(shipment.window_start, shipment.window_end)?;
        if let Some(estimated) = input.estimated_delivery_at {
            shipment.estimated_delivery_at = estimated;
        }
        if let Some(currency) = &input.currency {
            shipment.currency = valid_currency(currency)?;
        }
        shipment.updated_at = Utc::now();
        state.shipments.insert(shipment.id, shipment.clone());
        state.emit(
            "logistics.shipment.updated",
            json!({
                "shipment_id": shipment.id.to_string(),
                "tracking_number": shipment.tracking_number,
                "status": shipment.status.as_str(),
                "updated_by": caller.actor,
            }),
        );
        Ok(shipment)
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn delete_shipment(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<LogisticsState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_LOGISTICS_MANAGE)?;
        let shipment = load_shipment(&state, &path)?;
        if shipment.status != ShipmentStatus::Created {
            return Err(UnifiedError::conflict(format!(
                "Only a shipment in \"created\" status can be deleted; this shipment is \"{}\". Cancel it instead.",
                shipment.status.as_str()
            )));
        }
        state.shipments.remove(&shipment.id);
        let stale: Vec<Uuid> = state
            .timeline
            .iter()
            .filter(|entry| entry.value().shipment_id == shipment.id)
            .map(|entry| *entry.key())
            .collect();
        for id in stale {
            state.timeline.remove(&id);
        }
        state.emit(
            "logistics.shipment.deleted",
            json!({
                "shipment_id": shipment.id.to_string(),
                "tracking_number": shipment.tracking_number,
                "deleted_by": caller.actor,
            }),
        );
        Ok(json!({ "deleted": true, "shipment_id": shipment.id.to_string() }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

/// Move a shipment through the state machine. The transition is
/// rejected with 409 when the target is not in
/// `allowed_transitions(from)` — the check is the single gate for
/// every status change, including the exception and proof paths,
/// which call the same table through their own endpoints.
fn apply_transition(
    state: &LogisticsState,
    shipment: &mut Shipment,
    to: ShipmentStatus,
    actor: &str,
    reason: Option<&str>,
    location: Option<String>,
    note: Option<String>,
) -> UnifiedResult<()> {
    let from = shipment.status;
    if from.is_terminal() {
        return Err(UnifiedError::conflict(format!(
            "Shipment is in terminal status \"{}\"; no transition is possible",
            from.as_str()
        )));
    }
    if !allowed_transitions(from).contains(&to) {
        let allowed: Vec<&str> = allowed_transitions(from)
            .iter()
            .map(|status| status.as_str())
            .collect();
        return Err(UnifiedError::conflict(format!(
            "Illegal transition from \"{}\" to \"{}\"; allowed: {}",
            from.as_str(),
            to.as_str(),
            allowed.join(", ")
        )));
    }
    if matches!(to, ShipmentStatus::Cancelled | ShipmentStatus::Exception | ShipmentStatus::Returned)
        && reason.map(str::trim).unwrap_or("").is_empty()
    {
        return Err(UnifiedError::bad_request(
            "A reason is required for this transition",
        ));
    }
    shipment.status = to;
    shipment.updated_at = Utc::now();
    state.shipments.insert(shipment.id, shipment.clone());
    let mut timeline_note = note;
    if let Some(reason) = reason {
        if !reason.trim().is_empty() {
            timeline_note = Some(match timeline_note {
                Some(existing) => format!("{reason} — {existing}"),
                None => reason.to_string(),
            });
        }
    }
    state.add_timeline(
        shipment.id,
        "status_change",
        Some(from.as_str()),
        Some(to.as_str()),
        actor,
        location,
        timeline_note,
    );
    state.emit(
        "logistics.shipment.status_changed",
        json!({
            "shipment_id": shipment.id.to_string(),
            "tracking_number": shipment.tracking_number,
            "from_status": from.as_str(),
            "to_status": to.as_str(),
            "actor": actor,
        }),
    );
    match to {
        ShipmentStatus::Cancelled => state.emit(
            "logistics.shipment.cancelled",
            json!({
                "shipment_id": shipment.id.to_string(),
                "tracking_number": shipment.tracking_number,
                "reason": reason,
            }),
        ),
        ShipmentStatus::Delivered => state.emit(
            "logistics.shipment.delivered",
            json!({
                "shipment_id": shipment.id.to_string(),
                "tracking_number": shipment.tracking_number,
            }),
        ),
        ShipmentStatus::Returned => state.emit(
            "logistics.shipment.returned",
            json!({
                "shipment_id": shipment.id.to_string(),
                "tracking_number": shipment.tracking_number,
                "reason": reason,
            }),
        ),
        _ => {}
    }
    Ok(())
}

async fn transition_shipment(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<LogisticsState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<TransitionRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Shipment> = (|| {
        let caller = caller(&req)?;
        let mut shipment = load_shipment(&state, &path)?;
        can_operate_shipment(&ctx, &state, &caller, &shipment)?;
        let input = body.into_inner();
        let to = ShipmentStatus::parse(&input.to_status).ok_or_else(|| {
            UnifiedError::bad_request(format!("Unknown status: {}", input.to_status))
        })?;
        if to == ShipmentStatus::Delivered {
            return Err(UnifiedError::bad_request(
                "Deliveries are recorded through the proof of delivery endpoint, not through a raw transition",
            ));
        }
        if to == ShipmentStatus::Exception {
            return Err(UnifiedError::bad_request(
                "Exceptions are opened through the exceptions endpoint, which records the exception row",
            ));
        }
        let location = optional_text(&input.location, "location", MAX_ADDRESS_LEN)?;
        let note = optional_text(&input.note, "note", MAX_NOTE_LEN)?;
        let reason = optional_text(&input.reason, "reason", MAX_REASON_LEN)?;
        apply_transition(
            &state,
            &mut shipment,
            to,
            &caller.actor,
            reason.as_deref(),
            location,
            note,
        )?;
        Ok(shipment)
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn list_shipment_timeline(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<LogisticsState>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_LOGISTICS_VIEW)?;
        let shipment = load_shipment(&state, &path)?;
        let params = query.into_inner();
        let mut items: Vec<ShipmentTimelineEntry> = state
            .timeline
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|entry| entry.shipment_id == shipment.id)
            .collect();
        items.sort_by(|a, b| a.created_at.cmp(&b.created_at));
        let page = paginate(items, &params);
        Ok(serde_json::to_value(page).unwrap_or(Value::Null))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn link_order(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<LogisticsState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<LinkOrderRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_LOGISTICS_MANAGE)?;
        let mut shipment = load_shipment(&state, &path)?;
        if shipment.status.is_terminal() {
            return Err(UnifiedError::conflict(format!(
                "Cannot link an order to a shipment in terminal status \"{}\"",
                shipment.status.as_str()
            )));
        }
        let order_id = require_text(&body.order_id, "order_id", MAX_NAME_LEN)?;
        let mut warning: Option<String> = None;
        match lookup_order(&ctx, &order_id) {
            Some(true) => {
                shipment.order_link_status = "linked".to_string();
            }
            Some(false) => {
                return Err(UnifiedError::unprocessable(format!(
                    "Order not found in the store: {order_id}"
                )));
            }
            None => {
                shipment.order_link_status = "unverified".to_string();
                warning = Some(
                    "Order link is unverified: the store module did not answer the store.order.lookup query. The shipment is not treated as linked to a real order until the store confirms it.".to_string(),
                );
            }
        }
        shipment.order_id = Some(order_id.clone());
        shipment.updated_at = Utc::now();
        state.shipments.insert(shipment.id, shipment.clone());
        state.add_timeline(
            shipment.id,
            "order_linked",
            None,
            None,
            &caller.actor,
            None,
            Some(format!(
                "Linked order {order_id} ({})",
                shipment.order_link_status
            )),
        );
        let mut value = serde_json::to_value(&shipment).unwrap_or(Value::Null);
        if let (Value::Object(map), Some(warning)) = (&mut value, warning) {
            map.insert("order_link_warning".to_string(), Value::String(warning));
        }
        Ok(value)
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn schedule_shipment(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<LogisticsState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<ScheduleRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Shipment> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_LOGISTICS_MANAGE)?;
        let mut shipment = load_shipment(&state, &path)?;
        if shipment.status.is_terminal() {
            return Err(UnifiedError::conflict(format!(
                "Cannot schedule a shipment in terminal status \"{}\"",
                shipment.status.as_str()
            )));
        }
        let input = body.into_inner();
        validate_window(input.window_start, input.window_end)?;
        shipment.scheduled_at = Some(input.scheduled_at);
        shipment.window_start = input.window_start;
        shipment.window_end = input.window_end;
        shipment.updated_at = Utc::now();
        state.shipments.insert(shipment.id, shipment.clone());
        state.add_timeline(
            shipment.id,
            "rescheduled",
            None,
            None,
            &caller.actor,
            None,
            Some(format!(
                "Delivery scheduled for {}",
                input.scheduled_at.to_rfc3339()
            )),
        );
        Ok(shipment)
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn reschedule_shipment(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<LogisticsState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<RescheduleRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Shipment> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_LOGISTICS_MANAGE)?;
        let mut shipment = load_shipment(&state, &path)?;
        if shipment.status.is_terminal() {
            return Err(UnifiedError::conflict(format!(
                "Cannot reschedule a shipment in terminal status \"{}\"",
                shipment.status.as_str()
            )));
        }
        let input = body.into_inner();
        let reason = require_text(&input.reason, "reason", MAX_REASON_LEN)?;
        validate_window(input.window_start, input.window_end)?;
        let previous = shipment
            .scheduled_at
            .map(|value| value.to_rfc3339())
            .unwrap_or_else(|| "unscheduled".to_string());
        shipment.scheduled_at = Some(input.scheduled_at);
        shipment.window_start = input.window_start;
        shipment.window_end = input.window_end;
        shipment.updated_at = Utc::now();
        state.shipments.insert(shipment.id, shipment.clone());
        // Rescheduling is audited: the old and new times stay in
        // the timeline, and the customer is notified.
        state.add_timeline(
            shipment.id,
            "rescheduled",
            None,
            None,
            &caller.actor,
            None,
            Some(format!(
                "Rescheduled from {previous} to {}: {reason}",
                input.scheduled_at.to_rfc3339()
            )),
        );
        state.notify(
            "logistics.shipment.rescheduled",
            &shipment.created_by,
            "Delivery rescheduled",
            &format!(
                "Shipment {} was rescheduled to {}. Reason: {reason}",
                shipment.tracking_number,
                input.scheduled_at.to_rfc3339()
            ),
            Some(format!("/api/v1/logistics/shipments/{}", shipment.id)),
        );
        Ok(shipment)
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn create_proof(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<LogisticsState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<CreateProofRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        let mut shipment = load_shipment(&state, &path)?;
        can_operate_shipment(&ctx, &state, &caller, &shipment)?;
        if shipment.status != ShipmentStatus::OutForDelivery {
            return Err(UnifiedError::conflict(format!(
                "Proof of delivery requires status \"out_for_delivery\"; this shipment is \"{}\"",
                shipment.status.as_str()
            )));
        }
        let input = body.into_inner();
        let storage_target = require_text(&input.storage_target, "storage_target", MAX_NAME_LEN)?;
        let storage_key = require_text(&input.storage_key, "storage_key", 1024)?;
        if !valid_sha256_hex(&input.content_sha256) {
            return Err(UnifiedError::bad_request(
                "content_sha256 must be a 64-character lowercase hex SHA-256 digest",
            ));
        }
        let signature_target = optional_text(
            &input.signature_storage_target,
            "signature_storage_target",
            MAX_NAME_LEN,
        )?;
        let signature_key =
            optional_text(&input.signature_storage_key, "signature_storage_key", 1024)?;
        if signature_target.is_some() != signature_key.is_some() {
            return Err(UnifiedError::bad_request(
                "signature_storage_target and signature_storage_key must be provided together",
            ));
        }
        let recipient_name = require_text(&input.recipient_name, "recipient_name", MAX_NAME_LEN)?;
        let delivered_at = input.delivered_at.unwrap_or_else(Utc::now);
        let now = Utc::now();
        let proof = ProofOfDelivery {
            id: Uuid::now_v7(),
            shipment_id: shipment.id,
            storage_target,
            storage_key,
            content_sha256: input.content_sha256,
            signature_storage_target: signature_target,
            signature_storage_key: signature_key,
            recipient_name: recipient_name.clone(),
            delivered_at,
            actor: caller.actor.clone(),
            created_at: now,
        };
        state.proofs.insert(proof.id, proof.clone());
        // Proof closes the delivery: the state machine moves the
        // shipment to `delivered` through the same gate as any
        // other transition (out_for_delivery -> delivered is the
        // only legal entry to delivered).
        apply_transition(
            &state,
            &mut shipment,
            ShipmentStatus::Delivered,
            &caller.actor,
            None,
            None,
            Some(format!("Proof of delivery recorded for {recipient_name}")),
        )?;
        state.add_timeline(
            shipment.id,
            "proof_recorded",
            Some("out_for_delivery"),
            Some("delivered"),
            &caller.actor,
            None,
            input.note.clone(),
        );
        state.emit(
            "logistics.proof.created",
            json!({
                "proof_id": proof.id.to_string(),
                "shipment_id": shipment.id.to_string(),
                "tracking_number": shipment.tracking_number,
                "recipient_name": recipient_name,
                "delivered_at": delivered_at.to_rfc3339(),
            }),
        );
        state.notify(
            "logistics.shipment.delivered",
            &shipment.created_by,
            "Shipment delivered",
            &format!(
                "Shipment {} was delivered to {recipient_name}.",
                shipment.tracking_number
            ),
            Some(format!("/api/v1/logistics/shipments/{}", shipment.id)),
        );
        Ok(serde_json::to_value(&proof).unwrap_or(Value::Null))
    })();
    match result {
        Ok(data) => created(data),
        Err(error) => err(error, debug),
    }
}

async fn list_proofs(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<LogisticsState>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_LOGISTICS_VIEW)?;
        let shipment = load_shipment(&state, &path)?;
        let params = query.into_inner();
        let mut items: Vec<ProofOfDelivery> = state
            .proofs
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|proof| proof.shipment_id == shipment.id)
            .collect();
        items.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        let page = paginate(items, &params);
        Ok(serde_json::to_value(page).unwrap_or(Value::Null))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn create_exception(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<LogisticsState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<CreateExceptionRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        let mut shipment = load_shipment(&state, &path)?;
        can_operate_shipment(&ctx, &state, &caller, &shipment)?;
        let input = body.into_inner();
        let exception_type = match input.exception_type.as_str() {
            "damaged" | "refused" | "address_issue" | "weather" | "lost" | "other" => {
                input.exception_type.clone()
            }
            other => {
                return Err(UnifiedError::bad_request(format!(
                    "Unknown exception type: {other} (expected damaged, refused, address_issue, weather, lost, or other)"
                )));
            }
        };
        let description = require_text(&input.description, "description", MAX_NOTE_LEN)?;
        let location = optional_text(&input.location, "location", MAX_ADDRESS_LEN)?;
        // The exception opens only from a live, non-terminal status
        // and only when no exception is already open for this
        // shipment. The state-machine gate is checked BEFORE the
        // exception row is stored, so a rejected transition can
        // never leave an orphan exception behind.
        if shipment.status.is_terminal() {
            return Err(UnifiedError::conflict(format!(
                "Cannot open an exception on a shipment in terminal status \"{}\"",
                shipment.status.as_str()
            )));
        }
        if !allowed_transitions(shipment.status).contains(&ShipmentStatus::Exception) {
            return Err(UnifiedError::conflict(format!(
                "Cannot open an exception while the shipment is \"{}\"; exceptions open from picked_up, in_transit, or out_for_delivery",
                shipment.status.as_str()
            )));
        }
        if state.exceptions.iter().any(|entry| {
            entry.value().shipment_id == shipment.id
                && matches!(entry.value().status.as_str(), "open" | "investigating")
        }) {
            return Err(UnifiedError::conflict(
                "This shipment already has an open exception",
            ));
        }
        let previous_status = shipment.status.as_str().to_string();
        let now = Utc::now();
        let exception = DeliveryException {
            id: Uuid::now_v7(),
            shipment_id: shipment.id,
            exception_type: exception_type.clone(),
            description: description.clone(),
            status: "open".to_string(),
            previous_status: previous_status.clone(),
            resolution: None,
            actor: caller.actor.clone(),
            resolved_at: None,
            created_at: now,
            updated_at: now,
        };
        state.exceptions.insert(exception.id, exception.clone());
        apply_transition(
            &state,
            &mut shipment,
            ShipmentStatus::Exception,
            &caller.actor,
            Some(&description),
            location,
            Some(format!("Exception opened: {exception_type}")),
        )?;
        state.add_timeline(
            shipment.id,
            "exception_opened",
            Some(&previous_status),
            Some("exception"),
            &caller.actor,
            None,
            Some(format!("{exception_type}: {description}")),
        );
        state.emit(
            "logistics.exception.created",
            json!({
                "exception_id": exception.id.to_string(),
                "shipment_id": shipment.id.to_string(),
                "tracking_number": shipment.tracking_number,
                "type": exception_type,
                "previous_status": previous_status,
            }),
        );
        state.notify(
            "logistics.exception.created",
            &shipment.created_by,
            "Delivery exception",
            &format!(
                "Shipment {} hit a delivery exception ({exception_type}): {description}",
                shipment.tracking_number
            ),
            Some(format!("/api/v1/logistics/shipments/{}", shipment.id)),
        );
        Ok(serde_json::to_value(&exception).unwrap_or(Value::Null))
    })();
    match result {
        Ok(data) => created(data),
        Err(error) => err(error, debug),
    }
}

async fn list_exceptions(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<LogisticsState>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_LOGISTICS_VIEW)?;
        let shipment = load_shipment(&state, &path)?;
        let params = query.into_inner();
        let status_filter = params.filters.get("status").cloned();
        let mut items: Vec<DeliveryException> = state
            .exceptions
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|exception| {
                if exception.shipment_id != shipment.id {
                    return false;
                }
                if let Some(status) = &status_filter {
                    if &exception.status != status {
                        return false;
                    }
                }
                true
            })
            .collect();
        items.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        let page = paginate(items, &params);
        Ok(serde_json::to_value(page).unwrap_or(Value::Null))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn resolve_exception(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<LogisticsState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<ResolveExceptionRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<DeliveryException> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_LOGISTICS_MANAGE)?;
        let mut exception = load_exception(&state, &path)?;
        if !matches!(exception.status.as_str(), "open" | "investigating") {
            return Err(UnifiedError::conflict(format!(
                "Exception is already \"{}\"; only open or investigating exceptions can be resolved",
                exception.status
            )));
        }
        let input = body.into_inner();
        let resolution = require_text(&input.resolution, "resolution", MAX_NOTE_LEN)?;
        let mut shipment = load_shipment(&state, &exception.shipment_id.to_string())?;
        if shipment.status != ShipmentStatus::Exception {
            return Err(UnifiedError::conflict(format!(
                "Shipment is \"{}\", not in exception status; the exception cannot be resolved",
                shipment.status.as_str()
            )));
        }
        let now = Utc::now();
        exception.status = "resolved".to_string();
        exception.resolution = Some(resolution.clone());
        exception.resolved_at = Some(now);
        exception.updated_at = now;
        state.exceptions.insert(exception.id, exception.clone());
        if input.resume_delivery {
            apply_transition(
                &state,
                &mut shipment,
                ShipmentStatus::InTransit,
                &caller.actor,
                Some(&resolution),
                None,
                Some("Exception resolved; delivery resumed".to_string()),
            )?;
        } else {
            apply_transition(
                &state,
                &mut shipment,
                ShipmentStatus::Returned,
                &caller.actor,
                Some(&resolution),
                None,
                Some("Exception resolved; shipment returned".to_string()),
            )?;
            if let Some(mut entry) = state.exceptions.get_mut(&exception.id) {
                entry.status = "returned".to_string();
            }
            exception.status = "returned".to_string();
        }
        state.add_timeline(
            shipment.id,
            "exception_resolved",
            Some("exception"),
            Some(shipment.status.as_str()),
            &caller.actor,
            None,
            Some(resolution.clone()),
        );
        state.emit(
            "logistics.exception.resolved",
            json!({
                "exception_id": exception.id.to_string(),
                "shipment_id": shipment.id.to_string(),
                "tracking_number": shipment.tracking_number,
                "resolution": resolution,
                "resumed": input.resume_delivery,
            }),
        );
        Ok(exception)
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn return_exception(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<LogisticsState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<ReturnExceptionRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<DeliveryException> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_LOGISTICS_MANAGE)?;
        let mut exception = load_exception(&state, &path)?;
        if !matches!(exception.status.as_str(), "open" | "investigating") {
            return Err(UnifiedError::conflict(format!(
                "Exception is already \"{}\"; only open or investigating exceptions can be returned",
                exception.status
            )));
        }
        let input = body.into_inner();
        let reason = input
            .reason
            .as_deref()
            .map(|value| require_text(value, "reason", MAX_REASON_LEN))
            .transpose()?
            .unwrap_or_else(|| "Returned to sender".to_string());
        let mut shipment = load_shipment(&state, &exception.shipment_id.to_string())?;
        if shipment.status != ShipmentStatus::Exception {
            return Err(UnifiedError::conflict(format!(
                "Shipment is \"{}\", not in exception status; the exception cannot be returned",
                shipment.status.as_str()
            )));
        }
        let now = Utc::now();
        exception.status = "returned".to_string();
        exception.resolution = Some(reason.clone());
        exception.resolved_at = Some(now);
        exception.updated_at = now;
        state.exceptions.insert(exception.id, exception.clone());
        apply_transition(
            &state,
            &mut shipment,
            ShipmentStatus::Returned,
            &caller.actor,
            Some(&reason),
            None,
            Some("Shipment returned to sender".to_string()),
        )?;
        state.emit(
            "logistics.exception.returned",
            json!({
                "exception_id": exception.id.to_string(),
                "shipment_id": shipment.id.to_string(),
                "tracking_number": shipment.tracking_number,
                "reason": reason,
            }),
        );
        Ok(exception)
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

// ---------------------------------------------------------------------------
// Vehicle handlers
// ---------------------------------------------------------------------------

fn valid_vehicle_status(raw: &str) -> UnifiedResult<String> {
    match raw {
        "available" | "in_service" | "maintenance" | "retired" => Ok(raw.to_string()),
        other => Err(UnifiedError::bad_request(format!(
            "Unknown vehicle status: {other} (expected available, in_service, maintenance, or retired)"
        ))),
    }
}

fn valid_vehicle_type(raw: &str) -> UnifiedResult<String> {
    match raw {
        "van" | "truck" | "motorcycle" | "car" | "other" => Ok(raw.to_string()),
        other => Err(UnifiedError::bad_request(format!(
            "Unknown vehicle type: {other} (expected van, truck, motorcycle, car, or other)"
        ))),
    }
}

async fn list_vehicles(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<LogisticsState>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_LOGISTICS_VIEW)?;
        let params = query.into_inner();
        let status_filter = params.filters.get("status").cloned();
        let type_filter = params.filters.get("vehicle_type").cloned();
        let driver_filter = params.filters.get("assigned_driver_id").cloned();
        let company_filter = params.filters.get("company_id").cloned();
        let search = params.search.clone().unwrap_or_default().to_lowercase();
        let mut items: Vec<Vehicle> = state
            .vehicles
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|vehicle| {
                if let Some(status) = &status_filter {
                    if &vehicle.status != status {
                        return false;
                    }
                }
                if let Some(vehicle_type) = &type_filter {
                    if &vehicle.vehicle_type != vehicle_type {
                        return false;
                    }
                }
                if let Some(driver_id) = &driver_filter {
                    if vehicle.assigned_driver_id.map(|id| id.to_string()) != Some(driver_id.clone()) {
                        return false;
                    }
                }
                if let Some(company_id) = &company_filter {
                    if vehicle.company_id.map(|id| id.to_string()) != Some(company_id.clone()) {
                        return false;
                    }
                }
                if !search.is_empty() && !vehicle.plate.to_lowercase().contains(&search) {
                    return false;
                }
                true
            })
            .collect();
        items.sort_by(|a, b| a.plate.cmp(&b.plate));
        let page = paginate(items, &params);
        Ok(serde_json::to_value(page).unwrap_or(Value::Null))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn create_vehicle(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<LogisticsState>,
    req: HttpRequest,
    body: web::Json<CreateVehicleRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Vehicle> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_LOGISTICS_CREATE)?;
        let input = body.into_inner();
        let plate = require_text(&input.plate, "plate", MAX_PLATE_LEN)?.to_uppercase();
        if state
            .vehicles
            .iter()
            .any(|entry| entry.value().plate.eq_ignore_ascii_case(&plate))
        {
            return Err(UnifiedError::conflict(format!(
                "A vehicle with plate \"{plate}\" already exists"
            )));
        }
        let vehicle_type = valid_vehicle_type(&input.vehicle_type)?;
        if !input.capacity_kg.is_finite() || input.capacity_kg <= 0.0 || input.capacity_kg > MAX_WEIGHT_KG {
            return Err(UnifiedError::bad_request(format!(
                "capacity_kg must be > 0 and <= {MAX_WEIGHT_KG}"
            )));
        }
        let status = input
            .status
            .as_deref()
            .map(valid_vehicle_status)
            .transpose()?
            .unwrap_or_else(|| "available".to_string());
        if let Some(driver_id) = input.assigned_driver_id {
            if !state.drivers.contains_key(&driver_id) {
                return Err(UnifiedError::bad_request(format!("Driver not found: {driver_id}")));
            }
        }
        let company_id = parse_optional_company(&input.company_id)?;
        let now = Utc::now();
        let vehicle = Vehicle {
            id: Uuid::now_v7(),
            plate,
            vehicle_type,
            capacity_kg: input.capacity_kg,
            status,
            assigned_driver_id: input.assigned_driver_id,
            company_id,
            maintenance: Vec::new(),
            created_at: now,
            updated_at: now,
        };
        state.vehicles.insert(vehicle.id, vehicle.clone());
        state.emit(
            "logistics.vehicle.created",
            json!({
                "vehicle_id": vehicle.id.to_string(),
                "plate": vehicle.plate,
                "created_by": caller.actor,
            }),
        );
        Ok(vehicle)
    })();
    match result {
        Ok(data) => created(data),
        Err(error) => err(error, debug),
    }
}

async fn get_vehicle(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<LogisticsState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Vehicle> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_LOGISTICS_VIEW)?;
        load_vehicle(&state, &path)
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn update_vehicle(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<LogisticsState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdateVehicleRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Vehicle> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_LOGISTICS_MANAGE)?;
        let mut vehicle = load_vehicle(&state, &path)?;
        let input = body.into_inner();
        if let Some(plate) = &input.plate {
            let plate = require_text(plate, "plate", MAX_PLATE_LEN)?.to_uppercase();
            if state.vehicles.iter().any(|entry| {
                entry.value().id != vehicle.id && entry.value().plate.eq_ignore_ascii_case(&plate)
            }) {
                return Err(UnifiedError::conflict(format!(
                    "A vehicle with plate \"{plate}\" already exists"
                )));
            }
            vehicle.plate = plate;
        }
        if let Some(vehicle_type) = &input.vehicle_type {
            vehicle.vehicle_type = valid_vehicle_type(vehicle_type)?;
        }
        if let Some(capacity) = input.capacity_kg {
            if !capacity.is_finite() || capacity <= 0.0 || capacity > MAX_WEIGHT_KG {
                return Err(UnifiedError::bad_request(format!(
                    "capacity_kg must be > 0 and <= {MAX_WEIGHT_KG}"
                )));
            }
            vehicle.capacity_kg = capacity;
        }
        if let Some(status) = &input.status {
            vehicle.status = valid_vehicle_status(status)?;
        }
        if let Some(driver_id) = input.assigned_driver_id {
            if let Some(id) = driver_id {
                if !state.drivers.contains_key(&id) {
                    return Err(UnifiedError::bad_request(format!("Driver not found: {id}")));
                }
            }
            vehicle.assigned_driver_id = driver_id;
        }
        if let Some(company_id) = &input.company_id {
            vehicle.company_id = match company_id.as_deref() {
                None => None,
                Some(raw) if raw.trim().is_empty() => None,
                Some(raw) => Some(parse_uuid(raw, "company")?),
            };
        }
        vehicle.updated_at = Utc::now();
        state.vehicles.insert(vehicle.id, vehicle.clone());
        state.emit(
            "logistics.vehicle.updated",
            json!({
                "vehicle_id": vehicle.id.to_string(),
                "plate": vehicle.plate,
                "updated_by": caller.actor,
            }),
        );
        Ok(vehicle)
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn delete_vehicle(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<LogisticsState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_LOGISTICS_MANAGE)?;
        let vehicle = load_vehicle(&state, &path)?;
        if state.shipments.iter().any(|entry| {
            entry.value().vehicle_id == Some(vehicle.id)
                && !entry.value().status.is_terminal()
        }) {
            return Err(UnifiedError::conflict(
                "Vehicle is assigned to a live shipment and cannot be deleted",
            ));
        }
        state.vehicles.remove(&vehicle.id);
        state.emit(
            "logistics.vehicle.deleted",
            json!({
                "vehicle_id": vehicle.id.to_string(),
                "plate": vehicle.plate,
                "deleted_by": caller.actor,
            }),
        );
        Ok(json!({ "deleted": true, "vehicle_id": vehicle.id.to_string() }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn assign_vehicle(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<LogisticsState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<AssignVehicleRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Vehicle> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_LOGISTICS_MANAGE)?;
        let mut vehicle = load_vehicle(&state, &path)?;
        let mut driver = load_driver(&state, &body.driver_id.to_string())?;
        if vehicle.status == "retired" {
            return Err(UnifiedError::conflict(
                "A retired vehicle cannot take a driver assignment",
            ));
        }
        if driver.status != "active" {
            return Err(UnifiedError::conflict(format!(
                "Driver is \"{}\"; only an active driver can be assigned",
                driver.status
            )));
        }
        // One driver drives one vehicle at a time: clear any other
        // vehicle that still points at this driver.
        for mut entry in state.vehicles.iter_mut() {
            if entry.value().assigned_driver_id == Some(driver.id) && entry.key() != &vehicle.id {
                entry.value_mut().assigned_driver_id = None;
                entry.value_mut().updated_at = Utc::now();
            }
        }
        vehicle.assigned_driver_id = Some(driver.id);
        vehicle.updated_at = Utc::now();
        state.vehicles.insert(vehicle.id, vehicle.clone());
        driver.current_vehicle_id = Some(vehicle.id);
        driver.updated_at = Utc::now();
        state.drivers.insert(driver.id, driver.clone());
        state.emit(
            "logistics.vehicle.assigned",
            json!({
                "vehicle_id": vehicle.id.to_string(),
                "plate": vehicle.plate,
                "driver_id": driver.id.to_string(),
                "user_id": driver.user_id,
                "assigned_by": caller.actor,
            }),
        );
        Ok(vehicle)
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn list_maintenance(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<LogisticsState>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_LOGISTICS_VIEW)?;
        let vehicle = load_vehicle(&state, &path)?;
        let params = query.into_inner();
        let status_filter = params.filters.get("status").cloned();
        let mut items: Vec<MaintenanceReminder> = vehicle
            .maintenance
            .iter()
            .cloned()
            .filter(|reminder| {
                if let Some(status) = &status_filter {
                    if &reminder.status != status {
                        return false;
                    }
                }
                true
            })
            .collect();
        items.sort_by(|a, b| a.due_date.cmp(&b.due_date));
        let page = paginate(items, &params);
        Ok(serde_json::to_value(page).unwrap_or(Value::Null))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn create_maintenance(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<LogisticsState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<CreateMaintenanceRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<MaintenanceReminder> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_LOGISTICS_MANAGE)?;
        let mut vehicle = load_vehicle(&state, &path)?;
        let description = require_text(&body.description, "description", MAX_NOTE_LEN)?;
        let now = Utc::now();
        let mut reminder = MaintenanceReminder {
            id: Uuid::now_v7(),
            description: description.clone(),
            due_date: body.due_date,
            status: "pending".to_string(),
            scheduler_job_id: None,
            scheduler_status: "not_scheduled".to_string(),
            completed_at: None,
            created_at: now,
            updated_at: now,
        };
        schedule_maintenance_reminder(&state, vehicle.id, &mut reminder);
        vehicle.maintenance.push(reminder.clone());
        vehicle.updated_at = now;
        state.vehicles.insert(vehicle.id, vehicle.clone());
        state.emit(
            "logistics.vehicle.maintenance_created",
            json!({
                "vehicle_id": vehicle.id.to_string(),
                "plate": vehicle.plate,
                "reminder_id": reminder.id.to_string(),
                "due_date": reminder.due_date.to_rfc3339(),
                "scheduler_status": reminder.scheduler_status,
                "created_by": caller.actor,
            }),
        );
        Ok(reminder)
    })();
    match result {
        Ok(data) => created(data),
        Err(error) => err(error, debug),
    }
}

// ---------------------------------------------------------------------------
// Driver handlers
// ---------------------------------------------------------------------------

fn valid_driver_status(raw: &str) -> UnifiedResult<String> {
    match raw {
        "active" | "inactive" | "on_leave" => Ok(raw.to_string()),
        other => Err(UnifiedError::bad_request(format!(
            "Unknown driver status: {other} (expected active, inactive, or on_leave)"
        ))),
    }
}

async fn list_drivers(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<LogisticsState>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_LOGISTICS_VIEW)?;
        let params = query.into_inner();
        let status_filter = params.filters.get("status").cloned();
        let user_filter = params.filters.get("user_id").cloned();
        let search = params.search.clone().unwrap_or_default().to_lowercase();
        let mut items: Vec<Driver> = state
            .drivers
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|driver| {
                if let Some(status) = &status_filter {
                    if &driver.status != status {
                        return false;
                    }
                }
                if let Some(user_id) = &user_filter {
                    if &driver.user_id != user_id {
                        return false;
                    }
                }
                if !search.is_empty()
                    && !driver.license_number.to_lowercase().contains(&search)
                    && !driver.user_id.to_lowercase().contains(&search)
                {
                    return false;
                }
                true
            })
            .collect();
        items.sort_by(|a, b| a.created_at.cmp(&b.created_at));
        let page = paginate(items, &params);
        Ok(serde_json::to_value(page).unwrap_or(Value::Null))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn create_driver(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<LogisticsState>,
    req: HttpRequest,
    body: web::Json<CreateDriverRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Driver> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_LOGISTICS_CREATE)?;
        let input = body.into_inner();
        let user_id = require_text(&input.user_id, "user_id", MAX_NAME_LEN)?;
        if state
            .drivers
            .iter()
            .any(|entry| entry.value().user_id == user_id)
        {
            return Err(UnifiedError::conflict(format!(
                "A driver record already exists for user \"{user_id}\""
            )));
        }
        let license_number = require_text(&input.license_number, "license_number", MAX_LICENSE_LEN)?;
        let phone = optional_text(&input.phone, "phone", MAX_PHONE_LEN)?;
        let status = input
            .status
            .as_deref()
            .map(valid_driver_status)
            .transpose()?
            .unwrap_or_else(|| "active".to_string());
        if let Some(vehicle_id) = input.current_vehicle_id {
            if !state.vehicles.contains_key(&vehicle_id) {
                return Err(UnifiedError::bad_request(format!(
                    "Vehicle not found: {vehicle_id}"
                )));
            }
        }
        let now = Utc::now();
        let driver = Driver {
            id: Uuid::now_v7(),
            user_id,
            license_number,
            phone,
            status,
            current_vehicle_id: input.current_vehicle_id,
            created_at: now,
            updated_at: now,
        };
        state.drivers.insert(driver.id, driver.clone());
        state.emit(
            "logistics.driver.created",
            json!({
                "driver_id": driver.id.to_string(),
                "user_id": driver.user_id,
                "created_by": caller.actor,
            }),
        );
        Ok(driver)
    })();
    match result {
        Ok(data) => created(data),
        Err(error) => err(error, debug),
    }
}

async fn get_driver(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<LogisticsState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Driver> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_LOGISTICS_VIEW)?;
        load_driver(&state, &path)
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn update_driver(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<LogisticsState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdateDriverRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Driver> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_LOGISTICS_MANAGE)?;
        let mut driver = load_driver(&state, &path)?;
        let input = body.into_inner();
        if let Some(license) = &input.license_number {
            driver.license_number = require_text(license, "license_number", MAX_LICENSE_LEN)?;
        }
        if let Some(phone) = &input.phone {
            driver.phone = match phone.as_deref() {
                None => None,
                Some(raw) if raw.trim().is_empty() => None,
                Some(raw) => Some(require_text(raw, "phone", MAX_PHONE_LEN)?),
            };
        }
        if let Some(status) = &input.status {
            driver.status = valid_driver_status(status)?;
        }
        if let Some(vehicle_id) = input.current_vehicle_id {
            if let Some(id) = vehicle_id {
                if !state.vehicles.contains_key(&id) {
                    return Err(UnifiedError::bad_request(format!("Vehicle not found: {id}")));
                }
            }
            driver.current_vehicle_id = vehicle_id;
        }
        driver.updated_at = Utc::now();
        state.drivers.insert(driver.id, driver.clone());
        state.emit(
            "logistics.driver.updated",
            json!({
                "driver_id": driver.id.to_string(),
                "user_id": driver.user_id,
                "updated_by": caller.actor,
            }),
        );
        Ok(driver)
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn delete_driver(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<LogisticsState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_LOGISTICS_MANAGE)?;
        let driver = load_driver(&state, &path)?;
        if state.shipments.iter().any(|entry| {
            entry.value().driver_id == Some(driver.id) && !entry.value().status.is_terminal()
        }) {
            return Err(UnifiedError::conflict(
                "Driver is assigned to a live shipment and cannot be deleted",
            ));
        }
        state.drivers.remove(&driver.id);
        for mut entry in state.vehicles.iter_mut() {
            if entry.value().assigned_driver_id == Some(driver.id) {
                entry.value_mut().assigned_driver_id = None;
                entry.value_mut().updated_at = Utc::now();
            }
        }
        state.emit(
            "logistics.driver.deleted",
            json!({
                "driver_id": driver.id.to_string(),
                "user_id": driver.user_id,
                "deleted_by": caller.actor,
            }),
        );
        Ok(json!({ "deleted": true, "driver_id": driver.id.to_string() }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

// ---------------------------------------------------------------------------
// Warehouse + stock movement handlers
// ---------------------------------------------------------------------------

async fn list_warehouses(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<LogisticsState>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_LOGISTICS_VIEW)?;
        let params = query.into_inner();
        let company_filter = params.filters.get("company_id").cloned();
        let search = params.search.clone().unwrap_or_default().to_lowercase();
        let mut items: Vec<Warehouse> = state
            .warehouses
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|warehouse| {
                if let Some(company_id) = &company_filter {
                    if warehouse.company_id.map(|id| id.to_string()) != Some(company_id.clone()) {
                        return false;
                    }
                }
                if !search.is_empty() && !warehouse.name.to_lowercase().contains(&search) {
                    return false;
                }
                true
            })
            .collect();
        items.sort_by(|a, b| a.name.cmp(&b.name));
        let page = paginate(items, &params);
        Ok(serde_json::to_value(page).unwrap_or(Value::Null))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn create_warehouse(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<LogisticsState>,
    req: HttpRequest,
    body: web::Json<CreateWarehouseRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Warehouse> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_LOGISTICS_CREATE)?;
        let input = body.into_inner();
        let name = require_text(&input.name, "name", MAX_NAME_LEN)?;
        let address = require_text(&input.address, "address", MAX_ADDRESS_LEN)?;
        if input.capacity < 0 {
            return Err(UnifiedError::bad_request("capacity must be >= 0"));
        }
        let threshold = input.default_low_stock_threshold.unwrap_or(0);
        if threshold < 0 {
            return Err(UnifiedError::bad_request(
                "default_low_stock_threshold must be >= 0",
            ));
        }
        let manager_user_id = optional_text(&input.manager_user_id, "manager_user_id", MAX_NAME_LEN)?;
        let company_id = parse_optional_company(&input.company_id)?;
        let now = Utc::now();
        let warehouse = Warehouse {
            id: Uuid::now_v7(),
            name,
            address,
            capacity: input.capacity,
            manager_user_id,
            default_low_stock_threshold: threshold,
            company_id,
            created_at: now,
            updated_at: now,
        };
        state.warehouses.insert(warehouse.id, warehouse.clone());
        state.emit(
            "logistics.warehouse.created",
            json!({
                "warehouse_id": warehouse.id.to_string(),
                "name": warehouse.name,
                "created_by": caller.actor,
            }),
        );
        Ok(warehouse)
    })();
    match result {
        Ok(data) => created(data),
        Err(error) => err(error, debug),
    }
}

async fn get_warehouse(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<LogisticsState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Warehouse> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_LOGISTICS_VIEW)?;
        load_warehouse(&state, &path)
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn update_warehouse(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<LogisticsState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdateWarehouseRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Warehouse> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_LOGISTICS_MANAGE)?;
        let mut warehouse = load_warehouse(&state, &path)?;
        let input = body.into_inner();
        if let Some(name) = &input.name {
            warehouse.name = require_text(name, "name", MAX_NAME_LEN)?;
        }
        if let Some(address) = &input.address {
            warehouse.address = require_text(address, "address", MAX_ADDRESS_LEN)?;
        }
        if let Some(capacity) = input.capacity {
            if capacity < 0 {
                return Err(UnifiedError::bad_request("capacity must be >= 0"));
            }
            warehouse.capacity = capacity;
        }
        if let Some(manager) = &input.manager_user_id {
            warehouse.manager_user_id = match manager.as_deref() {
                None => None,
                Some(raw) if raw.trim().is_empty() => None,
                Some(raw) => Some(require_text(raw, "manager_user_id", MAX_NAME_LEN)?),
            };
        }
        if let Some(threshold) = input.default_low_stock_threshold {
            if threshold < 0 {
                return Err(UnifiedError::bad_request(
                    "default_low_stock_threshold must be >= 0",
                ));
            }
            warehouse.default_low_stock_threshold = threshold;
        }
        if let Some(company_id) = &input.company_id {
            warehouse.company_id = match company_id.as_deref() {
                None => None,
                Some(raw) if raw.trim().is_empty() => None,
                Some(raw) => Some(parse_uuid(raw, "company")?),
            };
        }
        warehouse.updated_at = Utc::now();
        state.warehouses.insert(warehouse.id, warehouse.clone());
        state.emit(
            "logistics.warehouse.updated",
            json!({
                "warehouse_id": warehouse.id.to_string(),
                "name": warehouse.name,
                "updated_by": caller.actor,
            }),
        );
        Ok(warehouse)
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn delete_warehouse(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<LogisticsState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_LOGISTICS_MANAGE)?;
        let warehouse = load_warehouse(&state, &path)?;
        if state
            .stock_movements
            .iter()
            .any(|entry| entry.value().warehouse_id == warehouse.id)
        {
            return Err(UnifiedError::conflict(
                "Warehouse has stock movements and cannot be deleted",
            ));
        }
        state.warehouses.remove(&warehouse.id);
        state.emit(
            "logistics.warehouse.deleted",
            json!({
                "warehouse_id": warehouse.id.to_string(),
                "name": warehouse.name,
                "deleted_by": caller.actor,
            }),
        );
        Ok(json!({ "deleted": true, "warehouse_id": warehouse.id.to_string() }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn list_stock_movements(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<LogisticsState>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_LOGISTICS_VIEW)?;
        let warehouse = load_warehouse(&state, &path)?;
        let params = query.into_inner();
        let direction_filter = params.filters.get("direction").cloned();
        let asset_filter = params.filters.get("asset_id").cloned();
        let mut items: Vec<StockMovement> = state
            .stock_movements
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|movement| {
                if movement.warehouse_id != warehouse.id {
                    return false;
                }
                if let Some(direction) = &direction_filter {
                    if &movement.direction != direction {
                        return false;
                    }
                }
                if let Some(asset_id) = &asset_filter {
                    if movement.asset_id.to_string() != *asset_id {
                        return false;
                    }
                }
                true
            })
            .collect();
        items.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        let page = paginate(items, &params);
        Ok(serde_json::to_value(page).unwrap_or(Value::Null))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn create_stock_movement(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<LogisticsState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<CreateStockMovementRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<StockMovement> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_LOGISTICS_MANAGE)?;
        let warehouse = load_warehouse(&state, &path)?;
        let input = body.into_inner();
        let direction = match input.direction.as_str() {
            "in" | "out" => input.direction.clone(),
            other => {
                return Err(UnifiedError::bad_request(format!(
                    "Unknown direction: {other} (expected in or out)"
                )));
            }
        };
        if input.quantity <= 0 {
            return Err(UnifiedError::bad_request("quantity must be > 0"));
        }
        let reason = require_text(&input.reason, "reason", MAX_REASON_LEN)?;
        if let Some(shipment_id) = input.shipment_id {
            if !state.shipments.contains_key(&shipment_id) {
                return Err(UnifiedError::bad_request(format!(
                    "Shipment not found: {shipment_id}"
                )));
            }
        }
        let on_hand = state.stock_on_hand(warehouse.id, input.asset_id);
        if direction == "out" && input.quantity > on_hand {
            return Err(UnifiedError::conflict(format!(
                "Cannot move out {} units: only {on_hand} on hand in this warehouse for this item",
                input.quantity
            )));
        }
        let quantity_after = if direction == "in" {
            on_hand + input.quantity
        } else {
            on_hand - input.quantity
        };
        let movement = StockMovement {
            id: Uuid::now_v7(),
            warehouse_id: warehouse.id,
            asset_id: input.asset_id,
            direction: direction.clone(),
            quantity: input.quantity,
            quantity_after,
            reason,
            shipment_id: input.shipment_id,
            actor: caller.actor.clone(),
            created_at: Utc::now(),
        };
        state.stock_movements.insert(movement.id, movement.clone());
        state.emit(
            "logistics.stock.moved",
            json!({
                "movement_id": movement.id.to_string(),
                "warehouse_id": warehouse.id.to_string(),
                "asset_id": input.asset_id.to_string(),
                "direction": direction,
                "quantity": input.quantity,
                "quantity_after": quantity_after,
                "actor": caller.actor,
            }),
        );
        // Low-stock alert: when the balance after the move is at or
        // below the threshold, notify the warehouse manager and
        // emit the low-stock event. The assets module owns the item
        // master data, so this alert carries ids only, no item
        // names or prices.
        let threshold = input
            .low_stock_threshold
            .unwrap_or(warehouse.default_low_stock_threshold);
        if quantity_after <= threshold {
            if let Some(manager) = &warehouse.manager_user_id {
                state.notify(
                    "logistics.stock.low",
                    manager,
                    "Low stock in warehouse",
                    &format!(
                        "Item {} in warehouse \"{}\" is at {quantity_after} units (threshold {threshold}).",
                        input.asset_id, warehouse.name
                    ),
                    Some(format!("/api/v1/logistics/warehouses/{}", warehouse.id)),
                );
            }
            state.emit(
                "logistics.stock.low",
                json!({
                    "warehouse_id": warehouse.id.to_string(),
                    "asset_id": input.asset_id.to_string(),
                    "quantity_after": quantity_after,
                    "threshold": threshold,
                }),
            );
        }
        Ok(movement)
    })();
    match result {
        Ok(data) => created(data),
        Err(error) => err(error, debug),
    }
}

async fn get_stock_movement(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<LogisticsState>,
    req: HttpRequest,
    path: web::Path<(String, String)>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<StockMovement> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_LOGISTICS_VIEW)?;
        let (warehouse_raw, movement_raw) = path.into_inner();
        let warehouse = load_warehouse(&state, &warehouse_raw)?;
        let movement_id = parse_uuid(&movement_raw, "stock movement")?;
        state
            .stock_movements
            .get(&movement_id)
            .map(|entry| entry.value().clone())
            .filter(|movement| movement.warehouse_id == warehouse.id)
            .ok_or_else(|| {
                UnifiedError::not_found(format!("Stock movement not found: {movement_id}"))
            })
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

// ---------------------------------------------------------------------------
// Delivery route handlers
// ---------------------------------------------------------------------------

fn valid_route_status(raw: &str) -> UnifiedResult<String> {
    match raw {
        "planned" | "active" | "completed" | "cancelled" => Ok(raw.to_string()),
        other => Err(UnifiedError::bad_request(format!(
            "Unknown route status: {other} (expected planned, active, completed, or cancelled)"
        ))),
    }
}

async fn list_routes(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<LogisticsState>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_LOGISTICS_VIEW)?;
        let params = query.into_inner();
        let status_filter = params.filters.get("status").cloned();
        let driver_filter = params.filters.get("driver_id").cloned();
        let vehicle_filter = params.filters.get("vehicle_id").cloned();
        let company_filter = params.filters.get("company_id").cloned();
        let search = params.search.clone().unwrap_or_default().to_lowercase();
        let mut items: Vec<DeliveryRoute> = state
            .routes
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|route| {
                if let Some(status) = &status_filter {
                    if &route.status != status {
                        return false;
                    }
                }
                if let Some(driver_id) = &driver_filter {
                    if route.driver_id.map(|id| id.to_string()) != Some(driver_id.clone()) {
                        return false;
                    }
                }
                if let Some(vehicle_id) = &vehicle_filter {
                    if route.vehicle_id.map(|id| id.to_string()) != Some(vehicle_id.clone()) {
                        return false;
                    }
                }
                if let Some(company_id) = &company_filter {
                    if route.company_id.map(|id| id.to_string()) != Some(company_id.clone()) {
                        return false;
                    }
                }
                if !search.is_empty() && !route.name.to_lowercase().contains(&search) {
                    return false;
                }
                true
            })
            .collect();
        items.sort_by(|a, b| b.route_date.cmp(&a.route_date));
        let page = paginate(items, &params);
        Ok(serde_json::to_value(page).unwrap_or(Value::Null))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn create_route(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<LogisticsState>,
    req: HttpRequest,
    body: web::Json<CreateRouteRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<DeliveryRoute> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_LOGISTICS_CREATE)?;
        let input = body.into_inner();
        let name = require_text(&input.name, "name", MAX_NAME_LEN)?;
        if let Some(latitude) = input.start_latitude {
            validate_latitude(latitude, "start_latitude")?;
        }
        if let Some(longitude) = input.start_longitude {
            validate_longitude(longitude, "start_longitude")?;
        }
        if input.start_latitude.is_some() != input.start_longitude.is_some() {
            return Err(UnifiedError::bad_request(
                "start_latitude and start_longitude must be provided together",
            ));
        }
        if let Some(driver_id) = input.driver_id {
            if !state.drivers.contains_key(&driver_id) {
                return Err(UnifiedError::bad_request(format!("Driver not found: {driver_id}")));
            }
        }
        if let Some(vehicle_id) = input.vehicle_id {
            if !state.vehicles.contains_key(&vehicle_id) {
                return Err(UnifiedError::bad_request(format!(
                    "Vehicle not found: {vehicle_id}"
                )));
            }
        }
        let status = input
            .status
            .as_deref()
            .map(valid_route_status)
            .transpose()?
            .unwrap_or_else(|| "planned".to_string());
        let company_id = parse_optional_company(&input.company_id)?;
        let now = Utc::now();
        let route = DeliveryRoute {
            id: Uuid::now_v7(),
            name,
            driver_id: input.driver_id,
            vehicle_id: input.vehicle_id,
            route_date: input.route_date,
            start_latitude: input.start_latitude,
            start_longitude: input.start_longitude,
            status,
            company_id,
            created_at: now,
            updated_at: now,
        };
        state.routes.insert(route.id, route.clone());
        state.emit(
            "logistics.route.created",
            json!({
                "route_id": route.id.to_string(),
                "name": route.name,
                "created_by": caller.actor,
            }),
        );
        Ok(route)
    })();
    match result {
        Ok(data) => created(data),
        Err(error) => err(error, debug),
    }
}

async fn get_route(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<LogisticsState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<DeliveryRoute> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_LOGISTICS_VIEW)?;
        load_route(&state, &path)
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn update_route(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<LogisticsState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdateRouteRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<DeliveryRoute> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_LOGISTICS_MANAGE)?;
        let mut route = load_route(&state, &path)?;
        if route.status == "completed" || route.status == "cancelled" {
            return Err(UnifiedError::conflict(format!(
                "A {} route cannot be edited",
                route.status
            )));
        }
        let input = body.into_inner();
        if let Some(name) = &input.name {
            route.name = require_text(name, "name", MAX_NAME_LEN)?;
        }
        if let Some(driver_id) = input.driver_id {
            if let Some(id) = driver_id {
                if !state.drivers.contains_key(&id) {
                    return Err(UnifiedError::bad_request(format!("Driver not found: {id}")));
                }
            }
            route.driver_id = driver_id;
        }
        if let Some(vehicle_id) = input.vehicle_id {
            if let Some(id) = vehicle_id {
                if !state.vehicles.contains_key(&id) {
                    return Err(UnifiedError::bad_request(format!("Vehicle not found: {id}")));
                }
            }
            route.vehicle_id = vehicle_id;
        }
        if let Some(route_date) = input.route_date {
            route.route_date = route_date;
        }
        if let Some(latitude) = input.start_latitude {
            if let Some(value) = latitude {
                validate_latitude(value, "start_latitude")?;
            }
            route.start_latitude = latitude;
        }
        if let Some(longitude) = input.start_longitude {
            if let Some(value) = longitude {
                validate_longitude(value, "start_longitude")?;
            }
            route.start_longitude = longitude;
        }
        if route.start_latitude.is_some() != route.start_longitude.is_some() {
            return Err(UnifiedError::bad_request(
                "start_latitude and start_longitude must be provided together",
            ));
        }
        if let Some(status) = &input.status {
            route.status = valid_route_status(status)?;
        }
        if let Some(company_id) = &input.company_id {
            route.company_id = match company_id.as_deref() {
                None => None,
                Some(raw) if raw.trim().is_empty() => None,
                Some(raw) => Some(parse_uuid(raw, "company")?),
            };
        }
        route.updated_at = Utc::now();
        state.routes.insert(route.id, route.clone());
        state.emit(
            "logistics.route.updated",
            json!({
                "route_id": route.id.to_string(),
                "name": route.name,
                "updated_by": caller.actor,
            }),
        );
        Ok(route)
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn delete_route(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<LogisticsState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_LOGISTICS_MANAGE)?;
        let route = load_route(&state, &path)?;
        if route.status == "active" {
            return Err(UnifiedError::conflict(
                "An active route cannot be deleted; complete or cancel it first",
            ));
        }
        state.routes.remove(&route.id);
        let stale: Vec<Uuid> = state
            .route_stops
            .iter()
            .filter(|entry| entry.value().route_id == route.id)
            .map(|entry| *entry.key())
            .collect();
        for id in stale {
            state.route_stops.remove(&id);
        }
        state.emit(
            "logistics.route.deleted",
            json!({
                "route_id": route.id.to_string(),
                "name": route.name,
                "deleted_by": caller.actor,
            }),
        );
        Ok(json!({ "deleted": true, "route_id": route.id.to_string() }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn assign_route(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<LogisticsState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<AssignRouteRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<DeliveryRoute> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_LOGISTICS_MANAGE)?;
        let mut route = load_route(&state, &path)?;
        if route.status == "completed" || route.status == "cancelled" {
            return Err(UnifiedError::conflict(format!(
                "A {} route cannot be assigned",
                route.status
            )));
        }
        let driver = load_driver(&state, &body.driver_id.to_string())?;
        let vehicle = load_vehicle(&state, &body.vehicle_id.to_string())?;
        if driver.status != "active" {
            return Err(UnifiedError::conflict(format!(
                "Driver is \"{}\"; only an active driver can be assigned",
                driver.status
            )));
        }
        if vehicle.status == "retired" {
            return Err(UnifiedError::conflict(
                "A retired vehicle cannot be assigned to a route",
            ));
        }
        route.driver_id = Some(driver.id);
        route.vehicle_id = Some(vehicle.id);
        route.updated_at = Utc::now();
        state.routes.insert(route.id, route.clone());
        state.emit(
            "logistics.route.assigned",
            json!({
                "route_id": route.id.to_string(),
                "driver_id": driver.id.to_string(),
                "vehicle_id": vehicle.id.to_string(),
                "assigned_by": caller.actor,
            }),
        );
        Ok(route)
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn list_route_stops(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<LogisticsState>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_LOGISTICS_VIEW)?;
        let route = load_route(&state, &path)?;
        let params = query.into_inner();
        let status_filter = params.filters.get("status").cloned();
        let mut items: Vec<RouteStop> = state
            .route_stops
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|stop| {
                if stop.route_id != route.id {
                    return false;
                }
                if let Some(status) = &status_filter {
                    if &stop.status != status {
                        return false;
                    }
                }
                true
            })
            .collect();
        items.sort_by_key(|stop| stop.sequence);
        let page = paginate(items, &params);
        Ok(serde_json::to_value(page).unwrap_or(Value::Null))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn create_route_stop(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<LogisticsState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<CreateRouteStopRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<RouteStop> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_LOGISTICS_MANAGE)?;
        let route = load_route(&state, &path)?;
        if route.status == "completed" || route.status == "cancelled" {
            return Err(UnifiedError::conflict(format!(
                "Stops cannot be added to a {} route",
                route.status
            )));
        }
        let input = body.into_inner();
        let address = require_text(&input.address, "address", MAX_ADDRESS_LEN)?;
        if let Some(latitude) = input.latitude {
            validate_latitude(latitude, "latitude")?;
        }
        if let Some(longitude) = input.longitude {
            validate_longitude(longitude, "longitude")?;
        }
        if input.latitude.is_some() != input.longitude.is_some() {
            return Err(UnifiedError::bad_request(
                "latitude and longitude must be provided together",
            ));
        }
        if let Some(shipment_id) = input.shipment_id {
            if !state.shipments.contains_key(&shipment_id) {
                return Err(UnifiedError::bad_request(format!(
                    "Shipment not found: {shipment_id}"
                )));
            }
        }
        let note = optional_text(&input.note, "note", MAX_NOTE_LEN)?;
        let sequence = state
            .route_stops
            .iter()
            .filter(|entry| entry.value().route_id == route.id)
            .map(|entry| entry.value().sequence)
            .max()
            .unwrap_or(0)
            + 1;
        let now = Utc::now();
        let stop = RouteStop {
            id: Uuid::now_v7(),
            route_id: route.id,
            shipment_id: input.shipment_id,
            sequence,
            address,
            latitude: input.latitude,
            longitude: input.longitude,
            status: "pending".to_string(),
            note,
            created_at: now,
            updated_at: now,
        };
        state.route_stops.insert(stop.id, stop.clone());
        state.emit(
            "logistics.route_stop.created",
            json!({
                "stop_id": stop.id.to_string(),
                "route_id": route.id.to_string(),
                "sequence": sequence,
                "created_by": caller.actor,
            }),
        );
        Ok(stop)
    })();
    match result {
        Ok(data) => created(data),
        Err(error) => err(error, debug),
    }
}

async fn update_route_stop(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<LogisticsState>,
    req: HttpRequest,
    path: web::Path<(String, String)>,
    body: web::Json<UpdateRouteStopRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<RouteStop> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_LOGISTICS_MANAGE)?;
        let (route_raw, stop_raw) = path.into_inner();
        let route = load_route(&state, &route_raw)?;
        let mut stop = load_route_stop(&state, route.id, &stop_raw)?;
        let input = body.into_inner();
        if let Some(shipment_id) = input.shipment_id {
            if let Some(id) = shipment_id {
                if !state.shipments.contains_key(&id) {
                    return Err(UnifiedError::bad_request(format!("Shipment not found: {id}")));
                }
            }
            stop.shipment_id = shipment_id;
        }
        if let Some(address) = &input.address {
            stop.address = require_text(address, "address", MAX_ADDRESS_LEN)?;
        }
        if let Some(latitude) = input.latitude {
            if let Some(value) = latitude {
                validate_latitude(value, "latitude")?;
            }
            stop.latitude = latitude;
        }
        if let Some(longitude) = input.longitude {
            if let Some(value) = longitude {
                validate_longitude(value, "longitude")?;
            }
            stop.longitude = longitude;
        }
        if stop.latitude.is_some() != stop.longitude.is_some() {
            return Err(UnifiedError::bad_request(
                "latitude and longitude must be provided together",
            ));
        }
        if let Some(status) = &input.status {
            match status.as_str() {
                "pending" | "visited" | "skipped" => stop.status = status.clone(),
                other => {
                    return Err(UnifiedError::bad_request(format!(
                        "Unknown stop status: {other} (expected pending, visited, or skipped)"
                    )));
                }
            }
        }
        if let Some(note) = &input.note {
            stop.note = match note.as_deref() {
                None => None,
                Some(raw) if raw.trim().is_empty() => None,
                Some(raw) => Some(require_text(raw, "note", MAX_NOTE_LEN)?),
            };
        }
        stop.updated_at = Utc::now();
        state.route_stops.insert(stop.id, stop.clone());
        state.emit(
            "logistics.route_stop.updated",
            json!({
                "stop_id": stop.id.to_string(),
                "route_id": route.id.to_string(),
                "updated_by": caller.actor,
            }),
        );
        Ok(stop)
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn delete_route_stop(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<LogisticsState>,
    req: HttpRequest,
    path: web::Path<(String, String)>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_LOGISTICS_MANAGE)?;
        let (route_raw, stop_raw) = path.into_inner();
        let route = load_route(&state, &route_raw)?;
        let stop = load_route_stop(&state, route.id, &stop_raw)?;
        state.route_stops.remove(&stop.id);
        // Renumber the remaining stops so sequences stay contiguous.
        let mut remaining: Vec<RouteStop> = state
            .route_stops
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|entry| entry.route_id == route.id)
            .collect();
        remaining.sort_by_key(|entry| entry.sequence);
        for (index, mut entry) in remaining.into_iter().enumerate() {
            entry.sequence = (index + 1) as i64;
            entry.updated_at = Utc::now();
            state.route_stops.insert(entry.id, entry);
        }
        state.emit(
            "logistics.route_stop.deleted",
            json!({
                "stop_id": stop.id.to_string(),
                "route_id": route.id.to_string(),
                "deleted_by": caller.actor,
            }),
        );
        Ok(json!({ "deleted": true, "stop_id": stop.id.to_string() }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn reorder_route(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<LogisticsState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_LOGISTICS_MANAGE)?;
        let route = load_route(&state, &path)?;
        if route.status == "completed" || route.status == "cancelled" {
            return Err(UnifiedError::conflict(format!(
                "A {} route cannot be reordered",
                route.status
            )));
        }
        let mut stops: Vec<RouteStop> = state
            .route_stops
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|stop| stop.route_id == route.id)
            .collect();
        stops.sort_by_key(|stop| stop.sequence);
        if stops.len() < 2 {
            return Ok(json!({
                "route_id": route.id.to_string(),
                "method": "nearest_neighbour",
                "reordered": false,
                "reason": "Fewer than two stops; nothing to reorder.",
            }));
        }
        // Nearest-neighbour heuristic only (see the route note in
        // the module header). Every stop must carry coordinates;
        // otherwise the reorder is refused rather than guessed.
        for stop in &stops {
            if stop.latitude.is_none() || stop.longitude.is_none() {
                return Err(UnifiedError::unprocessable(
                    "All stops need latitude and longitude before a reorder; at least one stop has no coordinates",
                ));
            }
        }
        let (mut current_lat, mut current_lng) = match (route.start_latitude, route.start_longitude) {
            (Some(lat), Some(lng)) => (lat, lng),
            _ => (
                stops[0].latitude.expect("coordinates checked above"),
                stops[0].longitude.expect("coordinates checked above"),
            ),
        };
        let mut remaining = stops.clone();
        let mut ordered: Vec<RouteStop> = Vec::with_capacity(stops.len());
        while !remaining.is_empty() {
            let mut best_index = 0;
            let mut best_distance = f64::MAX;
            for (index, stop) in remaining.iter().enumerate() {
                let distance = haversine_km(
                    current_lat,
                    current_lng,
                    stop.latitude.expect("coordinates checked above"),
                    stop.longitude.expect("coordinates checked above"),
                );
                if distance < best_distance {
                    best_distance = distance;
                    best_index = index;
                }
            }
            let next = remaining.remove(best_index);
            current_lat = next.latitude.expect("coordinates checked above");
            current_lng = next.longitude.expect("coordinates checked above");
            ordered.push(next);
        }
        for (index, mut stop) in ordered.clone().into_iter().enumerate() {
            stop.sequence = (index + 1) as i64;
            stop.updated_at = Utc::now();
            state.route_stops.insert(stop.id, stop);
        }
        state.emit(
            "logistics.route.reordered",
            json!({
                "route_id": route.id.to_string(),
                "method": "nearest_neighbour",
                "stop_count": ordered.len(),
                "reordered_by": caller.actor,
            }),
        );
        Ok(json!({
            "route_id": route.id.to_string(),
            "method": "nearest_neighbour",
            "reordered": true,
            "stop_count": ordered.len(),
            "stops": ordered,
        }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

// ---------------------------------------------------------------------------
// Rate table + quote handlers
// ---------------------------------------------------------------------------

async fn list_rate_tables(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<LogisticsState>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_LOGISTICS_VIEW)?;
        let params = query.into_inner();
        let origin_filter = params.filters.get("origin_zone").cloned();
        let dest_filter = params.filters.get("destination_zone").cloned();
        let active_filter = params.filters.get("active").cloned();
        let search = params.search.clone().unwrap_or_default().to_lowercase();
        let mut items: Vec<RateTable> = state
            .rate_tables
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|table| {
                if let Some(origin) = &origin_filter {
                    if table.origin_zone.as_deref() != Some(origin.as_str()) {
                        return false;
                    }
                }
                if let Some(dest) = &dest_filter {
                    if table.destination_zone.as_deref() != Some(dest.as_str()) {
                        return false;
                    }
                }
                if let Some(active) = &active_filter {
                    let wanted = active == "true";
                    if table.active != wanted {
                        return false;
                    }
                }
                if !search.is_empty() && !table.name.to_lowercase().contains(&search) {
                    return false;
                }
                true
            })
            .collect();
        items.sort_by(|a, b| a.name.cmp(&b.name));
        let page = paginate(items, &params);
        Ok(serde_json::to_value(page).unwrap_or(Value::Null))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn create_rate_table(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<LogisticsState>,
    req: HttpRequest,
    body: web::Json<CreateRateTableRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<RateTable> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_LOGISTICS_CREATE)?;
        let input = body.into_inner();
        let name = require_text(&input.name, "name", MAX_NAME_LEN)?;
        let origin_zone = optional_text(&input.origin_zone, "origin_zone", MAX_ZONE_LEN)?;
        let destination_zone =
            optional_text(&input.destination_zone, "destination_zone", MAX_ZONE_LEN)?;
        let base_price = valid_money(input.base_price, "base_price")?;
        let per_kg = valid_money(input.per_kg, "per_kg")?;
        let per_km = valid_money(input.per_km, "per_km")?;
        let currency = input
            .currency
            .as_deref()
            .map(valid_currency)
            .transpose()?
            .unwrap_or_else(|| "USD".to_string());
        let company_id = parse_optional_company(&input.company_id)?;
        let now = Utc::now();
        let table = RateTable {
            id: Uuid::now_v7(),
            name,
            origin_zone,
            destination_zone,
            base_price,
            per_kg,
            per_km,
            currency,
            active: input.active.unwrap_or(true),
            company_id,
            created_at: now,
            updated_at: now,
        };
        state.rate_tables.insert(table.id, table.clone());
        state.emit(
            "logistics.rate_table.created",
            json!({
                "rate_table_id": table.id.to_string(),
                "name": table.name,
                "created_by": caller.actor,
            }),
        );
        Ok(table)
    })();
    match result {
        Ok(data) => created(data),
        Err(error) => err(error, debug),
    }
}

async fn get_rate_table(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<LogisticsState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<RateTable> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_LOGISTICS_VIEW)?;
        load_rate_table(&state, &path)
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn update_rate_table(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<LogisticsState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdateRateTableRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<RateTable> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_LOGISTICS_MANAGE)?;
        let mut table = load_rate_table(&state, &path)?;
        let input = body.into_inner();
        if let Some(name) = &input.name {
            table.name = require_text(name, "name", MAX_NAME_LEN)?;
        }
        if let Some(origin) = &input.origin_zone {
            table.origin_zone = match origin.as_deref() {
                None => None,
                Some(raw) if raw.trim().is_empty() => None,
                Some(raw) => Some(require_text(raw, "origin_zone", MAX_ZONE_LEN)?),
            };
        }
        if let Some(destination) = &input.destination_zone {
            table.destination_zone = match destination.as_deref() {
                None => None,
                Some(raw) if raw.trim().is_empty() => None,
                Some(raw) => Some(require_text(raw, "destination_zone", MAX_ZONE_LEN)?),
            };
        }
        if let Some(base_price) = input.base_price {
            table.base_price = valid_money(base_price, "base_price")?;
        }
        if let Some(per_kg) = input.per_kg {
            table.per_kg = valid_money(per_kg, "per_kg")?;
        }
        if let Some(per_km) = input.per_km {
            table.per_km = valid_money(per_km, "per_km")?;
        }
        if let Some(currency) = &input.currency {
            table.currency = valid_currency(currency)?;
        }
        if let Some(active) = input.active {
            table.active = active;
        }
        if let Some(company_id) = &input.company_id {
            table.company_id = match company_id.as_deref() {
                None => None,
                Some(raw) if raw.trim().is_empty() => None,
                Some(raw) => Some(parse_uuid(raw, "company")?),
            };
        }
        table.updated_at = Utc::now();
        state.rate_tables.insert(table.id, table.clone());
        state.emit(
            "logistics.rate_table.updated",
            json!({
                "rate_table_id": table.id.to_string(),
                "name": table.name,
                "updated_by": caller.actor,
            }),
        );
        Ok(table)
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn delete_rate_table(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<LogisticsState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_LOGISTICS_MANAGE)?;
        let table = load_rate_table(&state, &path)?;
        state.rate_tables.remove(&table.id);
        state.emit(
            "logistics.rate_table.deleted",
            json!({
                "rate_table_id": table.id.to_string(),
                "name": table.name,
                "deleted_by": caller.actor,
            }),
        );
        Ok(json!({ "deleted": true, "rate_table_id": table.id.to_string() }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

/// Pick the best matching active rate table: an exact zone pair
/// first, then a table matching one zone, then the wildcard table
/// (both zones unset). Returns `None` when no table matches.
fn find_rate_table(state: &LogisticsState, origin: &str, destination: &str) -> Option<RateTable> {
    let mut tables: Vec<RateTable> = state
        .rate_tables
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|table| table.active)
        .collect();
    tables.sort_by(|a, b| a.created_at.cmp(&b.created_at));
    let score = |table: &RateTable| -> i64 {
        let origin_match = table.origin_zone.as_deref() == Some(origin);
        let dest_match = table.destination_zone.as_deref() == Some(destination);
        match (origin_match, dest_match) {
            (true, true) => 3,
            (true, false) | (false, true) => {
                if table.origin_zone.is_none() || table.destination_zone.is_none() {
                    2
                } else {
                    -1
                }
            }
            (false, false) => {
                if table.origin_zone.is_none() && table.destination_zone.is_none() {
                    1
                } else {
                    -1
                }
            }
        }
    };
    tables
        .into_iter()
        .filter(|table| score(table) >= 0)
        .max_by_key(score)
}

async fn quote_shipping(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<LogisticsState>,
    req: HttpRequest,
    body: web::Json<QuoteRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_LOGISTICS_QUOTE)?;
        let input = body.into_inner();
        let zone_origin = require_text(&input.zone_origin, "zone_origin", MAX_ZONE_LEN)?;
        let zone_destination =
            require_text(&input.zone_destination, "zone_destination", MAX_ZONE_LEN)?;
        validate_dimensions(input.weight_kg, input.length_cm, input.width_cm, input.height_cm)?;
        if let Some(latitude) = input.origin_latitude {
            validate_latitude(latitude, "origin_latitude")?;
        }
        if let Some(longitude) = input.origin_longitude {
            validate_longitude(longitude, "origin_longitude")?;
        }
        if let Some(latitude) = input.destination_latitude {
            validate_latitude(latitude, "destination_latitude")?;
        }
        if let Some(longitude) = input.destination_longitude {
            validate_longitude(longitude, "destination_longitude")?;
        }
        let currency = input
            .currency
            .as_deref()
            .map(valid_currency)
            .transpose()?
            .unwrap_or_else(|| "USD".to_string());
        let table = match input.rate_table_id {
            Some(id) => {
                let table = load_rate_table(&state, &id.to_string())?;
                if !table.active {
                    return Err(UnifiedError::conflict(
                        "The requested rate table is inactive",
                    ));
                }
                table
            }
            None => find_rate_table(&state, &zone_origin, &zone_destination).ok_or_else(|| {
                UnifiedError::not_found(format!(
                    "No active rate table matches zones \"{zone_origin}\" -> \"{zone_destination}\""
                ))
            })?,
        };
        // Distance: caller-supplied wins; otherwise ask the
        // Calculator module through the distance query (see the
        // distance note in the module header). No distance is ever
        // guessed — an unanswered query fails the quote.
        let mut distance_km = input.distance_km;
        let mut distance_source = "caller";
        if let Some(km) = distance_km {
            if !km.is_finite() || km < 0.0 {
                return Err(UnifiedError::bad_request("distance_km must be >= 0"));
            }
        } else if let (Some(o_lat), Some(o_lng), Some(d_lat), Some(d_lng)) = (
            input.origin_latitude,
            input.origin_longitude,
            input.destination_latitude,
            input.destination_longitude,
        ) {
            match request_distance(&ctx, o_lat, o_lng, d_lat, d_lng) {
                Some(km) => {
                    distance_km = Some(km);
                    distance_source = "calculator.distance_requested";
                }
                None => {
                    return Err(UnifiedError::unprocessable(
                        "No distance_km was supplied and no module answered the calculator.distance_requested query; supply distance_km or enable a distance provider",
                    ));
                }
            }
        } else {
            return Err(UnifiedError::bad_request(
                "distance_km is required when origin/destination coordinates are not both supplied",
            ));
        }
        let distance_km = distance_km.expect("distance resolved above");
        let dimensional = dimensional_weight(input.length_cm, input.width_cm, input.height_cm);
        let billable_kg = input.weight_kg.max(dimensional);
        let total = round_money(
            table.base_price + table.per_kg * billable_kg + table.per_km * distance_km,
        );
        state.emit(
            "logistics.quote.created",
            json!({
                "rate_table_id": table.id.to_string(),
                "zone_origin": zone_origin,
                "zone_destination": zone_destination,
                "billable_kg": billable_kg,
                "distance_km": distance_km,
                "distance_source": distance_source,
                "total": total,
                "currency": currency,
                "quoted_by": caller.actor,
            }),
        );
        Ok(json!({
            "rate_table_id": table.id.to_string(),
            "rate_table_name": table.name,
            "zone_origin": zone_origin,
            "zone_destination": zone_destination,
            "actual_weight_kg": input.weight_kg,
            "dimensional_weight_kg": round_money(dimensional),
            "billable_weight_kg": round_money(billable_kg),
            "distance_km": distance_km,
            "distance_source": distance_source,
            "base_price": table.base_price,
            "per_kg": table.per_kg,
            "per_km": table.per_km,
            "total": total,
            "currency": currency,
        }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

// ---------------------------------------------------------------------------
// Public tracking (no auth — token-gated, rate-limited; see header)
// ---------------------------------------------------------------------------

async fn track_shipment(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<LogisticsState>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<TrackQuery>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let tracking_number = path.into_inner();
        let token = query
            .token
            .clone()
            .unwrap_or_default();
        if token.trim().is_empty() {
            return Err(UnifiedError::unauthorized(
                "A verification token is required (?token=...)",
            ));
        }
        // Rate limit before any token check, so a wrong token cannot
        // be probed without bound. The bucket key binds the client
        // address to the tracking number.
        let address = client_address(&req);
        let bucket_key = format!("{address}|{tracking_number}");
        let now = Utc::now();
        let mut allowed = true;
        if let Some(mut bucket) = state.track_buckets.get_mut(&bucket_key) {
            let (count, window_start) = bucket.value_mut();
            if now.signed_duration_since(*window_start).num_seconds() >= TRACK_RATE_WINDOW_SECS {
                *count = 1;
                *window_start = now;
            } else if *count >= TRACK_RATE_LIMIT {
                allowed = false;
            } else {
                *count += 1;
            }
        } else {
            state
                .track_buckets
                .insert(bucket_key, (1, now));
        }
        if !allowed {
            return Err(UnifiedError::new(
                format!(
                    "Too many tracking lookups for this shipment; try again in {TRACK_RATE_WINDOW_SECS} seconds"
                ),
                429,
            ));
        }
        let shipment = state
            .shipments
            .iter()
            .map(|entry| entry.value().clone())
            .find(|shipment| shipment.tracking_number == tracking_number)
            .ok_or_else(|| {
                UnifiedError::not_found(format!("No shipment with tracking number {tracking_number}"))
            })?;
        if sha256_hex(token.trim()) != shipment.tracking_token_hash {
            return Err(UnifiedError::unauthorized(
                "The verification token does not match this tracking number",
            ));
        }
        // Public view: status and location history only. No
        // recipient name, addresses, phone data, cost, order id,
        // driver, or vehicle is exposed here.
        let mut timeline: Vec<ShipmentTimelineEntry> = state
            .timeline
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|entry| entry.shipment_id == shipment.id)
            .collect();
        timeline.sort_by(|a, b| a.created_at.cmp(&b.created_at));
        let locations: Vec<Value> = timeline
            .iter()
            .map(|entry| {
                json!({
                    "at": entry.created_at.to_rfc3339(),
                    "status": entry.to_status,
                    "location": entry.location,
                })
            })
            .collect();
        let _ = ctx;
        Ok(json!({
            "tracking_number": shipment.tracking_number,
            "status": shipment.status.as_str(),
            "estimated_delivery_at": shipment.estimated_delivery_at.map(|value| value.to_rfc3339()),
            "scheduled_at": shipment.scheduled_at.map(|value| value.to_rfc3339()),
            "locations": locations,
        }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

// ---------------------------------------------------------------------------
// Hook handlers
// ---------------------------------------------------------------------------

/// `order.paid` — no shipment is auto-created (a paid order may be
/// for digital goods). The handler only reports which linked
/// shipments exist for the order, so callers can see the linkage.
fn on_order_paid(state: Arc<LogisticsState>, ctx: &mut HookContext) {
    let order_id = ctx
        .get("id")
        .and_then(|value| value.as_str())
        .map(str::to_string)
        .or_else(|| {
            ctx.get("order_id")
                .and_then(|value| value.as_str())
                .map(str::to_string)
        })
        .or_else(|| {
            ctx.get("order")
                .and_then(|order| order.get("id"))
                .and_then(|id| id.as_str())
                .map(str::to_string)
        });
    let Some(order_id) = order_id else {
        ctx.add_result(json!({
            "logistics": "skipped",
            "reason": "order.paid carries no order id",
        }));
        return;
    };
    let linked: Vec<String> = state
        .shipments
        .iter()
        .filter(|entry| entry.value().order_id.as_deref() == Some(order_id.as_str()))
        .map(|entry| entry.value().tracking_number.clone())
        .collect();
    ctx.add_result(json!({
        "logistics": "seen",
        "order_id": order_id,
        "linked_shipments": linked,
    }));
}

/// `store.order.cancelled` — cancel every linked shipment still in
/// `created` status (reason "linked order cancelled") and notify
/// the shipment creator. Shipments already moving are left alone:
/// cancelling a truck already on the road is an operator decision,
/// not an automatic one.
fn on_store_order_cancelled(state: Arc<LogisticsState>, ctx: &mut HookContext) {
    let order_id = ctx
        .get("id")
        .and_then(|value| value.as_str())
        .map(str::to_string)
        .or_else(|| {
            ctx.get("order_id")
                .and_then(|value| value.as_str())
                .map(str::to_string)
        })
        .or_else(|| {
            ctx.get("order")
                .and_then(|order| order.get("id"))
                .and_then(|id| id.as_str())
                .map(str::to_string)
        });
    let Some(order_id) = order_id else {
        ctx.add_result(json!({
            "logistics": "skipped",
            "reason": "store.order.cancelled carries no order id",
        }));
        return;
    };
    let mut cancelled: Vec<String> = Vec::new();
    let mut left_alone: Vec<String> = Vec::new();
    let linked: Vec<Shipment> = state
        .shipments
        .iter()
        .filter(|entry| entry.value().order_id.as_deref() == Some(order_id.as_str()))
        .map(|entry| entry.value().clone())
        .collect();
    for mut shipment in linked {
        if shipment.status == ShipmentStatus::Created {
            if apply_transition(
                &state,
                &mut shipment,
                ShipmentStatus::Cancelled,
                "system:store.order.cancelled",
                Some("linked order cancelled"),
                None,
                None,
            )
            .is_ok()
            {
                state.notify(
                    "logistics.shipment.cancelled",
                    &shipment.created_by,
                    "Shipment cancelled",
                    &format!(
                        "Shipment {} was cancelled because its linked order was cancelled.",
                        shipment.tracking_number
                    ),
                    Some(format!("/api/v1/logistics/shipments/{}", shipment.id)),
                );
                cancelled.push(shipment.tracking_number);
            }
        } else {
            left_alone.push(shipment.tracking_number);
        }
    }
    ctx.add_result(json!({
        "logistics": "handled",
        "order_id": order_id,
        "cancelled_shipments": cancelled,
        "left_alone": left_alone,
    }));
}

/// `assets.stock.low` — when the payload names a warehouse this
/// module knows (`warehouse_id`), notify that warehouse's manager
/// and emit `logistics.stock.low`. Payloads without a warehouse
/// this module knows are ignored (the assets module owns the item
/// master data; this module only alerts for its own warehouses).
fn on_assets_stock_low(state: Arc<LogisticsState>, ctx: &mut HookContext) {
    let warehouse_id = ctx
        .get("warehouse_id")
        .and_then(|value| value.as_str())
        .and_then(|raw| Uuid::parse_str(raw).ok());
    let Some(warehouse_id) = warehouse_id else {
        ctx.add_result(json!({
            "logistics": "skipped",
            "reason": "assets.stock.low carries no warehouse_id",
        }));
        return;
    };
    let Some(warehouse) = state
        .warehouses
        .get(&warehouse_id)
        .map(|entry| entry.value().clone())
    else {
        ctx.add_result(json!({
            "logistics": "skipped",
            "reason": "warehouse is not a logistics warehouse",
        }));
        return;
    };
    let asset_id = ctx
        .get("asset_id")
        .and_then(|value| value.as_str())
        .unwrap_or("")
        .to_string();
    let quantity = ctx
        .get("quantity_on_hand")
        .and_then(Value::as_i64)
        .or_else(|| ctx.get("quantity_after").and_then(Value::as_i64));
    if let Some(manager) = &warehouse.manager_user_id {
        state.notify(
            "logistics.stock.low",
            manager,
            "Low stock in warehouse",
            &format!(
                "Item {asset_id} in warehouse \"{}\" is low (on hand: {}).",
                warehouse.name,
                quantity.map(|value| value.to_string()).unwrap_or_else(|| "unknown".to_string())
            ),
            Some(format!("/api/v1/logistics/warehouses/{}", warehouse.id)),
        );
    }
    state.emit(
        "logistics.stock.low",
        json!({
            "warehouse_id": warehouse.id.to_string(),
            "asset_id": asset_id,
            "quantity_after": quantity,
        }),
    );
    ctx.add_result(json!({
        "logistics": "handled",
        "warehouse_id": warehouse.id.to_string(),
        "asset_id": asset_id,
    }));
}

/// `logistics.maintenance.remind_requested` — the scheduler's
/// dispatch for a `logistics.maintenance.remind` job. Marks the
/// reminder due and notifies the assigned driver (or the vehicle
/// record's assignee when the driver row cannot be found).
fn on_maintenance_remind_requested(state: Arc<LogisticsState>, ctx: &mut HookContext) {
    let vehicle_id = ctx
        .get("vehicle_id")
        .and_then(|value| value.as_str())
        .and_then(|raw| Uuid::parse_str(raw).ok());
    let reminder_id = ctx
        .get("reminder_id")
        .and_then(|value| value.as_str())
        .and_then(|raw| Uuid::parse_str(raw).ok());
    let (Some(vehicle_id), Some(reminder_id)) = (vehicle_id, reminder_id) else {
        ctx.add_result(json!({
            "logistics": "skipped",
            "reason": "maintenance reminder payload is missing vehicle_id or reminder_id",
        }));
        return;
    };
    let Some(mut vehicle) = state
        .vehicles
        .get(&vehicle_id)
        .map(|entry| entry.value().clone())
    else {
        ctx.add_result(json!({
            "logistics": "skipped",
            "reason": "vehicle not found for maintenance reminder",
        }));
        return;
    };
    let mut found = false;
    for reminder in vehicle.maintenance.iter_mut() {
        if reminder.id == reminder_id && reminder.status == "pending" {
            reminder.status = "due".to_string();
            reminder.updated_at = Utc::now();
            found = true;
        }
    }
    if !found {
        ctx.add_result(json!({
            "logistics": "skipped",
            "reason": "maintenance reminder not found or already handled",
        }));
        return;
    }
    state.vehicles.insert(vehicle.id, vehicle.clone());
    let due_description = vehicle
        .maintenance
        .iter()
        .find(|reminder| reminder.id == reminder_id)
        .map(|reminder| reminder.description.clone())
        .unwrap_or_default();
    if let Some(driver_id) = vehicle.assigned_driver_id {
        if let Some(driver) = state.drivers.get(&driver_id) {
            state.notify(
                "logistics.vehicle.maintenance_due",
                &driver.value().user_id,
                "Vehicle maintenance due",
                &format!(
                    "Maintenance is due for vehicle {}: {due_description}",
                    vehicle.plate
                ),
                Some(format!("/api/v1/logistics/vehicles/{}", vehicle.id)),
            );
        }
    }
    state.emit(
        "logistics.vehicle.maintenance_due",
        json!({
            "vehicle_id": vehicle.id.to_string(),
            "plate": vehicle.plate,
            "reminder_id": reminder_id.to_string(),
            "description": due_description,
        }),
    );
    ctx.add_result(json!({
        "logistics": "handled",
        "vehicle_id": vehicle.id.to_string(),
        "reminder_id": reminder_id.to_string(),
    }));
}

// ---------------------------------------------------------------------------
// Module
// ---------------------------------------------------------------------------

pub struct LogisticsModule {
    state: LogisticsState,
}

impl LogisticsModule {
    pub fn new() -> Self {
        Self {
            state: LogisticsState::default(),
        }
    }
}

impl Default for LogisticsModule {
    fn default() -> Self {
        Self::new()
    }
}

impl Module for LogisticsModule {
    fn id(&self) -> &'static str {
        "logistics"
    }

    fn name(&self) -> &'static str {
        "Logistics"
    }

    fn description(&self) -> &'static str {
        "Shipments with a checked state machine and timeline, fleet vehicles and drivers, warehouses with stock movements and low-stock alerts, delivery routes with a nearest-neighbour reorder heuristic, delivery scheduling, proof of delivery, delivery exceptions, rate-table shipping quotes, and token-gated public tracking."
    }

    fn requires(&self) -> &[&'static str] {
        &["users", "roles", "store", "notifications", "scheduler", "assets"]
    }

    fn dependencies(&self) -> Vec<&'static str> {
        vec!["users", "roles", "store", "notifications", "scheduler", "assets"]
    }

    fn available_as_product(&self) -> bool {
        true
    }

    fn product_category(&self) -> Option<&'static str> {
        Some("commerce")
    }

    fn init(&self, ctx: &AppContext) -> UnifiedResult<()> {
        *self
            .state
            .hooks
            .write()
            .expect("hooks lock poisoned") = Some(Arc::clone(&ctx.hooks));
        Ok(())
    }

    fn register_hooks(&self, bus: &HookBus) {
        let state = Arc::new(self.state.clone());
        bus.register(
            ORDER_PAID,
            "logistics",
            Arc::new(move |ctx: &mut HookContext| on_order_paid(Arc::clone(&state), ctx)),
        );
        let state = Arc::new(self.state.clone());
        bus.register(
            STORE_ORDER_CANCELLED,
            "logistics",
            Arc::new(move |ctx: &mut HookContext| {
                on_store_order_cancelled(Arc::clone(&state), ctx)
            }),
        );
        let state = Arc::new(self.state.clone());
        bus.register(
            ASSETS_STOCK_LOW,
            "logistics",
            Arc::new(move |ctx: &mut HookContext| {
                on_assets_stock_low(Arc::clone(&state), ctx)
            }),
        );
        let state = self.state.clone();
        bus.register(
            MAINTENANCE_REMIND_REQUESTED,
            "logistics",
            Arc::new(move |ctx: &mut HookContext| {
                on_maintenance_remind_requested(state.clone(), ctx)
            }),
        );
    }

    fn configure_routes(&self, cfg: &mut web::ServiceConfig) {
        let state = web::Data::new(self.state.clone());
        cfg.service(
            web::scope("/api/v1/logistics")
                .app_data(state)
                // Literal segments first: they must win over `/{id}`.
                .route("/track/{tracking_number}", web::get().to(track_shipment))
                .route("/quote", web::post().to(quote_shipping))
                .route("/shipments", web::get().to(list_shipments))
                .route("/shipments", web::post().to(create_shipment))
                .route("/shipments/{id}", web::get().to(get_shipment))
                .route("/shipments/{id}", web::patch().to(update_shipment))
                .route("/shipments/{id}", web::delete().to(delete_shipment))
                .route("/shipments/{id}/transition", web::post().to(transition_shipment))
                .route("/shipments/{id}/timeline", web::get().to(list_shipment_timeline))
                .route("/shipments/{id}/link-order", web::post().to(link_order))
                .route("/shipments/{id}/schedule", web::post().to(schedule_shipment))
                .route("/shipments/{id}/reschedule", web::post().to(reschedule_shipment))
                .route("/shipments/{id}/proof", web::post().to(create_proof))
                .route("/shipments/{id}/proof", web::get().to(list_proofs))
                .route("/shipments/{id}/exceptions", web::post().to(create_exception))
                .route("/shipments/{id}/exceptions", web::get().to(list_exceptions))
                .route("/exceptions/{exception_id}/resolve", web::post().to(resolve_exception))
                .route("/exceptions/{exception_id}/return", web::post().to(return_exception))
                .route("/vehicles", web::get().to(list_vehicles))
                .route("/vehicles", web::post().to(create_vehicle))
                .route("/vehicles/{id}", web::get().to(get_vehicle))
                .route("/vehicles/{id}", web::patch().to(update_vehicle))
                .route("/vehicles/{id}", web::delete().to(delete_vehicle))
                .route("/vehicles/{id}/assign", web::post().to(assign_vehicle))
                .route("/vehicles/{id}/maintenance", web::get().to(list_maintenance))
                .route("/vehicles/{id}/maintenance", web::post().to(create_maintenance))
                .route("/drivers", web::get().to(list_drivers))
                .route("/drivers", web::post().to(create_driver))
                .route("/drivers/{id}", web::get().to(get_driver))
                .route("/drivers/{id}", web::patch().to(update_driver))
                .route("/drivers/{id}", web::delete().to(delete_driver))
                .route("/warehouses", web::get().to(list_warehouses))
                .route("/warehouses", web::post().to(create_warehouse))
                .route("/warehouses/{id}", web::get().to(get_warehouse))
                .route("/warehouses/{id}", web::patch().to(update_warehouse))
                .route("/warehouses/{id}", web::delete().to(delete_warehouse))
                .route(
                    "/warehouses/{id}/stock-movements",
                    web::get().to(list_stock_movements),
                )
                .route(
                    "/warehouses/{id}/stock-movements",
                    web::post().to(create_stock_movement),
                )
                .route(
                    "/warehouses/{id}/stock-movements/{movement_id}",
                    web::get().to(get_stock_movement),
                )
                .route("/routes", web::get().to(list_routes))
                .route("/routes", web::post().to(create_route))
                .route("/routes/{id}", web::get().to(get_route))
                .route("/routes/{id}", web::patch().to(update_route))
                .route("/routes/{id}", web::delete().to(delete_route))
                .route("/routes/{id}/assign", web::post().to(assign_route))
                .route("/routes/{id}/stops", web::get().to(list_route_stops))
                .route("/routes/{id}/stops", web::post().to(create_route_stop))
                .route("/routes/{id}/stops/{stop_id}", web::patch().to(update_route_stop))
                .route("/routes/{id}/stops/{stop_id}", web::delete().to(delete_route_stop))
                .route("/routes/{id}/reorder", web::post().to(reorder_route))
                .route("/rate-tables", web::get().to(list_rate_tables))
                .route("/rate-tables", web::post().to(create_rate_table))
                .route("/rate-tables/{id}", web::get().to(get_rate_table))
                .route("/rate-tables/{id}", web::patch().to(update_rate_table))
                .route("/rate-tables/{id}", web::delete().to(delete_rate_table)),
        );
    }

    fn route_info(&self) -> Vec<RouteInfo> {
        vec![
            RouteInfo { method: "GET", path: "/api/v1/logistics/track/{tracking_number}", owner: "logistics", description: "PUBLIC: track a shipment by tracking number + verification token (?token=...); rate-limited, status and location only (no auth; gated by the API Manager ApiAccessRule)." },
            RouteInfo { method: "POST", path: "/api/v1/logistics/quote", owner: "logistics", description: "Shipping cost quote from a rate table (billable weight = max(actual, dimensional); distance from caller or calculator.distance_requested)." },
            RouteInfo { method: "GET", path: "/api/v1/logistics/shipments", owner: "logistics", description: "List shipments (paginated, filter by status, warehouse_id, driver_id, vehicle_id, order_id, company_id)." },
            RouteInfo { method: "POST", path: "/api/v1/logistics/shipments", owner: "logistics", description: "Create a shipment (tracking number + one-time verification token returned; order link verified via store.order.lookup, unverified when unanswered)." },
            RouteInfo { method: "GET", path: "/api/v1/logistics/shipments/{id}", owner: "logistics", description: "Get one shipment." },
            RouteInfo { method: "PATCH", path: "/api/v1/logistics/shipments/{id}", owner: "logistics", description: "Update a shipment in created status only." },
            RouteInfo { method: "DELETE", path: "/api/v1/logistics/shipments/{id}", owner: "logistics", description: "Delete a shipment in created status only." },
            RouteInfo { method: "POST", path: "/api/v1/logistics/shipments/{id}/transition", owner: "logistics", description: "Move a shipment through the state machine (illegal transitions rejected with 409; delivered/exception go through proof/exception endpoints)." },
            RouteInfo { method: "GET", path: "/api/v1/logistics/shipments/{id}/timeline", owner: "logistics", description: "List the shipment status timeline (actor, timestamp, location, note)." },
            RouteInfo { method: "POST", path: "/api/v1/logistics/shipments/{id}/link-order", owner: "logistics", description: "Link a store order to a shipment (verified via store.order.lookup; rejected when the store denies it)." },
            RouteInfo { method: "POST", path: "/api/v1/logistics/shipments/{id}/schedule", owner: "logistics", description: "Set the delivery window of a shipment." },
            RouteInfo { method: "POST", path: "/api/v1/logistics/shipments/{id}/reschedule", owner: "logistics", description: "Reschedule a delivery (reason required; audited in the timeline; customer notified)." },
            RouteInfo { method: "POST", path: "/api/v1/logistics/shipments/{id}/proof", owner: "logistics", description: "Record proof of delivery (storage reference + SHA-256 hash; moves the shipment to delivered)." },
            RouteInfo { method: "GET", path: "/api/v1/logistics/shipments/{id}/proof", owner: "logistics", description: "List proofs of delivery of a shipment." },
            RouteInfo { method: "POST", path: "/api/v1/logistics/shipments/{id}/exceptions", owner: "logistics", description: "Open a delivery exception (moves the shipment to exception; customer notified)." },
            RouteInfo { method: "GET", path: "/api/v1/logistics/shipments/{id}/exceptions", owner: "logistics", description: "List exceptions of a shipment (filter by status)." },
            RouteInfo { method: "POST", path: "/api/v1/logistics/exceptions/{exception_id}/resolve", owner: "logistics", description: "Resolve an exception (shipment resumes to in_transit, or returns when resume_delivery is false)." },
            RouteInfo { method: "POST", path: "/api/v1/logistics/exceptions/{exception_id}/return", owner: "logistics", description: "Return a shipment from an open exception to the sender." },
            RouteInfo { method: "GET", path: "/api/v1/logistics/vehicles", owner: "logistics", description: "List vehicles (paginated, filter by status, vehicle_type, assigned_driver_id, company_id)." },
            RouteInfo { method: "POST", path: "/api/v1/logistics/vehicles", owner: "logistics", description: "Create a vehicle (plate must be unique)." },
            RouteInfo { method: "GET", path: "/api/v1/logistics/vehicles/{id}", owner: "logistics", description: "Get one vehicle." },
            RouteInfo { method: "PATCH", path: "/api/v1/logistics/vehicles/{id}", owner: "logistics", description: "Update a vehicle." },
            RouteInfo { method: "DELETE", path: "/api/v1/logistics/vehicles/{id}", owner: "logistics", description: "Delete a vehicle with no live shipments." },
            RouteInfo { method: "POST", path: "/api/v1/logistics/vehicles/{id}/assign", owner: "logistics", description: "Assign an active driver to a vehicle (one driver, one vehicle)." },
            RouteInfo { method: "GET", path: "/api/v1/logistics/vehicles/{id}/maintenance", owner: "logistics", description: "List maintenance reminders of a vehicle." },
            RouteInfo { method: "POST", path: "/api/v1/logistics/vehicles/{id}/maintenance", owner: "logistics", description: "Create a maintenance reminder (queued as logistics.maintenance.remind; scheduler_status declined until the kind is known)." },
            RouteInfo { method: "GET", path: "/api/v1/logistics/drivers", owner: "logistics", description: "List drivers (paginated, filter by status, user_id)." },
            RouteInfo { method: "POST", path: "/api/v1/logistics/drivers", owner: "logistics", description: "Create a driver (links a user id; never duplicates user rows)." },
            RouteInfo { method: "GET", path: "/api/v1/logistics/drivers/{id}", owner: "logistics", description: "Get one driver." },
            RouteInfo { method: "PATCH", path: "/api/v1/logistics/drivers/{id}", owner: "logistics", description: "Update a driver." },
            RouteInfo { method: "DELETE", path: "/api/v1/logistics/drivers/{id}", owner: "logistics", description: "Delete a driver with no live shipments (clears vehicle assignments)." },
            RouteInfo { method: "GET", path: "/api/v1/logistics/warehouses", owner: "logistics", description: "List warehouses (paginated, filter by company_id)." },
            RouteInfo { method: "POST", path: "/api/v1/logistics/warehouses", owner: "logistics", description: "Create a warehouse." },
            RouteInfo { method: "GET", path: "/api/v1/logistics/warehouses/{id}", owner: "logistics", description: "Get one warehouse." },
            RouteInfo { method: "PATCH", path: "/api/v1/logistics/warehouses/{id}", owner: "logistics", description: "Update a warehouse." },
            RouteInfo { method: "DELETE", path: "/api/v1/logistics/warehouses/{id}", owner: "logistics", description: "Delete a warehouse with no stock movements." },
            RouteInfo { method: "GET", path: "/api/v1/logistics/warehouses/{id}/stock-movements", owner: "logistics", description: "List stock movements of a warehouse (filter by direction, asset_id)." },
            RouteInfo { method: "POST", path: "/api/v1/logistics/warehouses/{id}/stock-movements", owner: "logistics", description: "Record a stock movement in/out (quantities only; out cannot exceed on-hand; low-stock alert at/below threshold)." },
            RouteInfo { method: "GET", path: "/api/v1/logistics/warehouses/{id}/stock-movements/{movement_id}", owner: "logistics", description: "Get one stock movement." },
            RouteInfo { method: "GET", path: "/api/v1/logistics/routes", owner: "logistics", description: "List delivery routes (paginated, filter by status, driver_id, vehicle_id, company_id)." },
            RouteInfo { method: "POST", path: "/api/v1/logistics/routes", owner: "logistics", description: "Create a delivery route." },
            RouteInfo { method: "GET", path: "/api/v1/logistics/routes/{id}", owner: "logistics", description: "Get one delivery route." },
            RouteInfo { method: "PATCH", path: "/api/v1/logistics/routes/{id}", owner: "logistics", description: "Update a delivery route (not when completed or cancelled)." },
            RouteInfo { method: "DELETE", path: "/api/v1/logistics/routes/{id}", owner: "logistics", description: "Delete a delivery route (not when active; stops are removed with it)." },
            RouteInfo { method: "POST", path: "/api/v1/logistics/routes/{id}/assign", owner: "logistics", description: "Assign a driver + vehicle to a delivery route." },
            RouteInfo { method: "GET", path: "/api/v1/logistics/routes/{id}/stops", owner: "logistics", description: "List stops of a delivery route in visit order." },
            RouteInfo { method: "POST", path: "/api/v1/logistics/routes/{id}/stops", owner: "logistics", description: "Add a stop to a delivery route (appended at the end)." },
            RouteInfo { method: "PATCH", path: "/api/v1/logistics/routes/{id}/stops/{stop_id}", owner: "logistics", description: "Update a route stop (address, coordinates, status, note)." },
            RouteInfo { method: "DELETE", path: "/api/v1/logistics/routes/{id}/stops/{stop_id}", owner: "logistics", description: "Delete a route stop (remaining stops are renumbered)." },
            RouteInfo { method: "POST", path: "/api/v1/logistics/routes/{id}/reorder", owner: "logistics", description: "Reorder stops by nearest-neighbour heuristic (requires coordinates on every stop; not a real optimiser)." },
            RouteInfo { method: "GET", path: "/api/v1/logistics/rate-tables", owner: "logistics", description: "List shipping rate tables (filter by origin_zone, destination_zone, active)." },
            RouteInfo { method: "POST", path: "/api/v1/logistics/rate-tables", owner: "logistics", description: "Create a shipping rate table." },
            RouteInfo { method: "GET", path: "/api/v1/logistics/rate-tables/{id}", owner: "logistics", description: "Get one rate table." },
            RouteInfo { method: "PATCH", path: "/api/v1/logistics/rate-tables/{id}", owner: "logistics", description: "Update a rate table." },
            RouteInfo { method: "DELETE", path: "/api/v1/logistics/rate-tables/{id}", owner: "logistics", description: "Delete a rate table." },
        ]
    }

    fn route_version_support(&self) -> RouteVersionSupport {
        RouteVersionSupport::All
    }
}

pub fn register(registry: &ModuleRegistry) {
    registry.register(Arc::new(LogisticsModule::new()));
}
