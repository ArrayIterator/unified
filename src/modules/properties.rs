// Properties module — land parcels and housing developments with
// 2D site plans (master plans), blocks / plots for sale, location
// search, SVG rendering, and print support.
//
// Routes live under `/api/v1/properties`:
//
//   Properties:
//     GET/POST / — list properties / create a property.
//     GET/PATCH/DELETE /{id}.
//     POST /{id}/publish, /{id}/archive, /{id}/mark-sold-out.
//   Plans:
//     GET/POST /{id}/plans — list plans / create a plan.
//     GET/PATCH/DELETE /{id}/plans/{plan_id}.
//     GET  /{id}/plans/{plan_id}/svg — the plan as SVG.
//     GET  /{id}/plans/{plan_id}/print — print-friendly HTML page.
//   Blocks:
//     GET/POST /{id}/plans/{plan_id}/blocks — list blocks / create a block.
//     GET/PATCH/DELETE /{id}/plans/{plan_id}/blocks/{block_id}.
//     POST /{id}/plans/{plan_id}/blocks/{block_id}/reserve,
//          /release, /purchase, /assign, /block, /unblock,
//          /correct-status.
//     GET  /{id}/plans/{plan_id}/blocks/{block_id}/history.
//     GET  /{id}/plans/{plan_id}/blocks/export?format=csv|json.
//   Location:
//     GET  /location/search?q=... — geocoder search.
//     GET  /location/reverse?lat=...&lng=... — reverse geocoding.
//     GET/PATCH /location/settings — geocoder provider settings.
//   Public (no auth; see the public-access note below):
//     GET  /public — published properties only.
//     GET  /public/{slug} — one published property with plans and blocks.
//     GET  /public/{slug}/plans/{plan_id}/svg.
//     GET  /public/{slug}/plans/{plan_id}/print.
//
// HONESTY — READ THIS FIRST. What is real in this module:
// in-memory property, plan, block, history, and purchase
// management; GeoJSON geometry validated server-side (only
// Polygon / MultiPolygon / LineString, bounded vertices, closed
// rings, coordinates in range) with area and length computed
// server-side from an equirectangular projection (an approximation
// suited to site plans, not a survey-grade geodesic measurement);
// a block status machine (available -> reserved -> sold / assigned,
// reserved -> available, blocked admin-only, sold terminal except
// audited admin correction) with a history row per transition;
// server-generated SVG from stored geometry (status colours,
// labels, viewBox fitted to the plan bounds) and a print page
// (HTML + inline SVG + per-block table + print stylesheet); a
// pluggable geocoder provider interface with one built-in HTTP
// adapter for a Nominatim-compatible endpoint plus a `none`
// provider; and a buy flow that emits an event for the store
// module and reacts to the payment events (see the buy-flow
// contract below). What is NOT built here: TRUE 3D VISUALIZATION
// — this is a 2D SVG site-plan editor only; a 3D view (extruded
// plots, terrain, a WebGL viewer) is a future item, recorded in
// the builder report, and nothing in this module pretends it
// exists. There is also no built-in geocoding database: location
// search needs a configured provider (see the location note).
// No payment collection lives here (payments, store, and billing
// own it — this module only reacts to their hook events), and no
// image bytes live here (media is referenced through the storage
// module by target + key, exactly like the events module's cover
// image). The map picker itself is client-side: the server
// accepts and validates coordinates and serves stored geometry.
//
// Location note: the geo module in this codebase resolves IP
// addresses (MaxMind database / settings overrides) and has NO
// address geocoder, so this module does not pretend one exists.
// `LocationProvider` is the pluggable interface
// (`search(query) -> Vec<PlaceCandidate>`,
// `reverse_geocode(lat, lng) -> Option<PlaceCandidate>`). The
// built-in `NominatimProvider` calls a Nominatim-compatible HTTP
// endpoint whose base URL is admin-configured; the host must match
// the configured allowlisted host exactly, the scheme must be
// HTTPS, requests carry a fixed User-Agent and a bounded timeout,
// and no API key is ever sent or logged (Nominatim needs none).
// The `none` provider (the default) accepts manual coordinates
// only and answers search / reverse with a clear "not configured"
// error instead of a fake result.
//
// Buy-flow contract (the store module owns checkout):
//   1. `POST .../blocks/{block_id}/purchase` requires the block
//      to be `available`. It reserves the block (status `reserved`,
//      buyer recorded), stores a purchase record, and emits
//      `properties.block.purchase_requested` with
//      `{purchase_id, property_id, plan_id, block_id, buyer_id,
//      price, currency}`. It does NOT create an order and does NOT
//      charge anything.
//   2. The store side (see the integrator snippet) listens to
//      `properties.block.purchase_requested`, creates the order,
//      and emits `properties.order.created` with
//      `{purchase_id, order_id, block_id, property_id, buyer_id,
//      total, currency}`. This module listens to that event and
//      links the order id to the purchase record.
//   3. On `payment.captured` for that order (gateway captures must
//      cover the block price; bank-transfer captures skip the
//      amount check, mirroring the store module), the block moves
//      to `sold` and `properties.block.sold` is emitted.
//   4. On `bank_transfer.verification.created` for that order the
//      purchase moves to `pending_verification` and
//      `properties.block.pending_verification` is emitted (the
//      block stays `reserved` while verification is pending).
//   5. On `bank_transfer.rejected` for that order the block is
//      released back to `available` and
//      `properties.block.released` is emitted.
//   A purchase whose order is never created leaves the block
//   `reserved`; releasing such a stale reservation is a manual
//   admin action today (a scheduler sweep is a future item).
//
// Public-access gate: exactly like the events module, the API
// Manager module enforces public access through its
// admin-managed `ApiAccessRule` records (default deny). The
// public routes below rely on that gate: an administrator must
// create the matching `ApiAccessRule` rows for
// `GET /api/v1/properties/public/*`. Do NOT treat the absence of
// a capability check on a public route as an open door: without
// the rule the API Manager denies the call. Public responses
// only ever contain `published` properties and never expose the
// buyer id or order id of a block.
//
// Actor identity: until the auth module provides an extractor, the
// caller is identified by the `X-User-Id` header (required) and
// `X-Role` carries the role slug the capability check runs
// against, exactly like the events module. Capability checks use
// the `role.check` hook and fail closed (no answer means not
// allowed).
//
// Events emitted on the hook bus:
//   - `properties.created`, `properties.updated`,
//     `properties.deleted`, `properties.published`,
//     `properties.archived`, `properties.sold_out`
//   - `properties.plan.created`, `properties.plan.updated`,
//     `properties.plan.deleted`
//   - `properties.block.created`, `properties.block.updated`,
//     `properties.block.deleted`
//   - `properties.block.reserved`, `properties.block.released`,
//     `properties.block.sold`, `properties.block.assigned`,
//     `properties.block.blocked`, `properties.block.unblocked`,
//     `properties.block.status_corrected`,
//     `properties.block.pending_verification`
//   - `properties.block.purchase_requested`
//   - `properties.location.settings_updated`
//   - `notification.requested` (reservation, sale, assignment)
//     with the notifications module's payload shape.
// Events this module listens to:
//   - `properties.order.created` — links a store order id to a
//     purchase record (emitted by the store-side purchase
//     handler; see the buy-flow contract above).
//   - `payment.captured` — marks the purchased block `sold`
//     (gateway captures must cover the block price; bank-transfer
//     captures skip the amount check; only purchases in
//     `requested` / `order_created` / `pending_verification` may
//     move to sold; already-sold is a safe no-op).
//   - `bank_transfer.verification.created` — moves the purchase
//     to `pending_verification` (the block stays `reserved`).
//   - `bank_transfer.rejected` — releases the block back to
//     `available`.

use crate::core::context::AppContext;
use crate::core::error::{UnifiedError, UnifiedResult};
use crate::core::hooks::{HookBus, HookContext};
use crate::core::json;
use crate::core::modules::{Module, ModuleRegistry, RouteInfo, RouteVersionSupport};
use crate::core::pagination::{PageResult, PaginationParams};
use actix_web::{web, HttpRequest, HttpResponse};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::sync::{Arc, RwLock};
use std::time::Duration;
use uuid::Uuid;

/// Maximum lengths enforced on user supplied text.
const MAX_TITLE_LEN: usize = 200;
const MAX_DESCRIPTION_LEN: usize = 5000;
const MAX_SLUG_LEN: usize = 128;
const MAX_ADDRESS_LEN: usize = 500;
const MAX_NOTES_LEN: usize = 2000;
const MAX_REASON_LEN: usize = 500;
const MAX_NAME_LEN: usize = 200;
const MAX_STORAGE_KEY_LEN: usize = 512;
const MAX_SEARCH_QUERY_LEN: usize = 200;
/// Maximum positions (vertices) in one geometry, and maximum
/// rings in one Polygon / MultiPolygon, so a hostile geometry
/// can never allocate unbounded work.
const MAX_GEOMETRY_POSITIONS: usize = 10_000;
const MAX_GEOMETRY_RINGS: usize = 100;
/// Maximum search results returned by a geocoder provider.
const MAX_SEARCH_RESULTS: usize = 5;
/// Default and maximum geocoder HTTP timeout, in seconds.
const DEFAULT_GEOCODER_TIMEOUT_SECS: u64 = 10;
const MAX_GEOCODER_TIMEOUT_SECS: u64 = 30;
/// Fixed User-Agent sent to the geocoder provider (Nominatim
/// usage policy requires an identifying User-Agent).
const GEOCODER_USER_AGENT: &str = "Unified-Properties/1.0 (geocoder; contact: admin)";
/// SVG canvas: the plan is fitted into this box with padding.
const SVG_WIDTH: f64 = 1000.0;
const SVG_HEIGHT: f64 = 700.0;
const SVG_PADDING: f64 = 40.0;
/// Equirectangular projection constants (metres per degree).
const METRES_PER_DEGREE_LAT: f64 = 110_540.0;
const METRES_PER_DEGREE_LNG_AT_EQUATOR: f64 = 111_320.0;

/// Capability names checked through the `role.check` hook.
const PERM_PROPERTIES_VIEW: &str = "properties.view";
const PERM_PROPERTIES_CREATE: &str = "properties.create";
const PERM_PROPERTIES_MANAGE: &str = "properties.manage";

/// Hook event names this module listens to (literals repeated here
/// so the module stays decoupled from the payments / store modules,
/// exactly like the store module repeats the payments constants).
const PAYMENT_CAPTURED: &str = "payment.captured";
const BANK_TRANSFER_VERIFICATION_CREATED: &str = "bank_transfer.verification.created";
const BANK_TRANSFER_REJECTED: &str = "bank_transfer.rejected";
const PROPERTIES_ORDER_CREATED: &str = "properties.order.created";

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

/// Lifecycle of a property: `draft` -> `published` -> `sold_out`,
/// with `archived` reachable from draft / published / sold_out.
/// `archived` is terminal (a property is deleted, not reopened).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PropertyStatus {
    Draft,
    Published,
    SoldOut,
    Archived,
}

impl Default for PropertyStatus {
    fn default() -> Self {
        Self::Draft
    }
}

impl PropertyStatus {
    fn as_str(&self) -> &'static str {
        match self {
            Self::Draft => "draft",
            Self::Published => "published",
            Self::SoldOut => "sold_out",
            Self::Archived => "archived",
        }
    }
}

/// One property: a land parcel or a housing development. The
/// developer / owner company is referenced by id only — company
/// rows are never duplicated here (the company module owns them).
/// Media is referenced through the storage module (target + key);
/// no image bytes live in this module.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Property {
    pub id: Uuid,
    pub title: String,
    pub description: Option<String>,
    pub slug: String,
    /// Latitude / longitude of the property marker (-90..90 /
    /// -180..180). The map picker is client-side; the server only
    /// accepts and validates coordinates.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub latitude: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub longitude: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub address: Option<String>,
    /// Total area in square metres (declared by the seller; the
    /// per-block areas computed from geometry do not have to sum
    /// to this value — roads and common areas are not blocks).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total_area_sqm: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub company_id: Option<Uuid>,
    pub status: PropertyStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cover_storage_target: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cover_storage_key: Option<String>,
    pub created_by: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// One site plan (master plan) of a property. `geometry` is the
/// plan boundary as GeoJSON (Polygon / MultiPolygon only — a plan
/// boundary is an area, never a line). Blocks carry their own
/// geometry; a plan with no boundary still renders its blocks.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PropertyPlan {
    pub id: Uuid,
    pub property_id: Uuid,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// GeoJSON geometry object (or null when the plan has no
    /// boundary yet). Validated on write; see `validate_geometry`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub geometry: Option<Value>,
    /// SHA-256 hex of the canonical geometry JSON, for change
    /// detection. Null when there is no geometry.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub geometry_hash: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Lifecycle of a block (plot): `available` -> `reserved` ->
/// `sold` / `assigned`; `reserved` -> `available` (release);
/// `blocked` is admin-only and reachable from any non-sold
/// status; `sold` is terminal except for an audited admin
/// correction (see `correct_block_status`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BlockStatus {
    Available,
    Reserved,
    Sold,
    Assigned,
    Blocked,
}

impl Default for BlockStatus {
    fn default() -> Self {
        Self::Available
    }
}

impl BlockStatus {
    fn as_str(&self) -> &'static str {
        match self {
            Self::Available => "available",
            Self::Reserved => "reserved",
            Self::Sold => "sold",
            Self::Assigned => "assigned",
            Self::Blocked => "blocked",
        }
    }

    /// Fill colour used for the block in the SVG rendering.
    fn svg_fill(&self) -> &'static str {
        match self {
            Self::Available => "#22c55e",
            Self::Reserved => "#f59e0b",
            Self::Sold => "#ef4444",
            Self::Assigned => "#3b82f6",
            Self::Blocked => "#6b7280",
        }
    }
}

/// What happens when a visitor clicks a block on the public plan.
/// `popup` shows the block title, notes, price, and status;
/// `link` navigates to the configured target instead.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClickBehavior {
    Popup,
    Link,
}

impl Default for ClickBehavior {
    fn default() -> Self {
        Self::Popup
    }
}

/// The target of a `link` click. `product` and `order` targets
/// are UUIDs (a store product id / order id); a `page` target is
/// a relative path ("/...") or an absolute URL whose host is on
/// the configured link allowlist.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClickLinkKind {
    Product,
    Order,
    Page,
}

/// One block (plot) inside a site plan. Geometry is GeoJSON
/// (Polygon / MultiPolygon for a plot, LineString for a drawn
/// line such as a road or boundary marker). `area_sqm` is
/// computed server-side from the geometry for polygons;
/// `length_m` is computed server-side for lines. The buyer is
/// referenced by customer id only — customer rows are never
/// duplicated here (the CRM module owns them).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PropertyBlock {
    pub id: Uuid,
    pub plan_id: Uuid,
    pub property_id: Uuid,
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,
    /// GeoJSON geometry object. Validated on write.
    pub geometry: Value,
    /// SHA-256 hex of the canonical geometry JSON.
    pub geometry_hash: String,
    /// Computed area in square metres (polygons only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub area_sqm: Option<f64>,
    /// Computed length in metres (lines only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub length_m: Option<f64>,
    /// Sale price in `currency` (0 means not priced yet).
    pub price: f64,
    pub currency: String,
    pub status: BlockStatus,
    /// Customer id of the buyer / assignee (CRM reference only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub assigned_customer_id: Option<String>,
    pub click_behavior: ClickBehavior,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub click_link_kind: Option<ClickLinkKind>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub click_link_target: Option<String>,
    /// Store order id of the purchase that sold this block.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub order_id: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// One audited block status change: who moved the block, when,
/// from which status to which, and why. Written on every status
/// transition, including admin corrections.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PropertyBlockHistory {
    pub id: Uuid,
    pub block_id: Uuid,
    pub property_id: Uuid,
    pub plan_id: Uuid,
    pub from_status: String,
    pub to_status: String,
    pub actor_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub order_id: Option<String>,
    pub created_at: DateTime<Utc>,
}

/// One block purchase: the record the buy flow revolves around.
/// Created by the purchase endpoint (block becomes `reserved`),
/// linked to a store order id by the `properties.order.created`
/// listener, and resolved by the payment listeners.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BlockPurchase {
    pub id: Uuid,
    pub property_id: Uuid,
    pub plan_id: Uuid,
    pub block_id: Uuid,
    pub buyer_id: String,
    pub price: f64,
    pub currency: String,
    /// Store order id, once the store side has created the order.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub order_id: Option<String>,
    /// `requested`, `order_created`, `pending_verification`,
    /// `paid`, `rejected`, `cancelled`.
    pub status: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// One location search result from a geocoder provider.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlaceCandidate {
    pub display_name: String,
    pub latitude: f64,
    pub longitude: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub place_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub importance: Option<f64>,
}

/// Geocoder provider settings, held in module state and editable
/// through the admin endpoint. `provider` is `none` (manual
/// coordinates only) or `nominatim` (HTTP adapter). `allowed_host`
/// is the only host the adapter may call; `link_allowlist_hosts`
/// are the only hosts an absolute page link target may point to
/// (relative paths need no allowlist entry).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LocationSettings {
    pub provider: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
    pub allowed_host: String,
    pub timeout_secs: u64,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub link_allowlist_hosts: Vec<String>,
}

impl Default for LocationSettings {
    fn default() -> Self {
        Self {
            provider: "none".to_string(),
            base_url: None,
            allowed_host: "nominatim.openstreetmap.org".to_string(),
            timeout_secs: DEFAULT_GEOCODER_TIMEOUT_SECS,
            link_allowlist_hosts: Vec::new(),
        }
    }
}

// ---------------------------------------------------------------------------
// Request bodies
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct CreatePropertyRequest {
    pub title: String,
    pub description: Option<String>,
    pub slug: Option<String>,
    pub latitude: Option<f64>,
    pub longitude: Option<f64>,
    pub address: Option<String>,
    pub total_area_sqm: Option<f64>,
    pub company_id: Option<Uuid>,
    pub cover_storage_target: Option<String>,
    pub cover_storage_key: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct UpdatePropertyRequest {
    pub title: Option<String>,
    pub description: Option<Option<String>>,
    pub slug: Option<String>,
    pub latitude: Option<Option<f64>>,
    pub longitude: Option<Option<f64>>,
    pub address: Option<Option<String>>,
    pub total_area_sqm: Option<Option<f64>>,
    pub company_id: Option<Option<Uuid>>,
    pub cover_storage_target: Option<Option<String>>,
    pub cover_storage_key: Option<Option<String>>,
}

#[derive(Debug, Deserialize)]
pub struct CreatePlanRequest {
    pub name: String,
    pub description: Option<String>,
    pub geometry: Option<Value>,
}

#[derive(Debug, Deserialize)]
pub struct UpdatePlanRequest {
    pub name: Option<String>,
    pub description: Option<Option<String>>,
    pub geometry: Option<Option<Value>>,
}

#[derive(Debug, Deserialize)]
pub struct CreateBlockRequest {
    pub title: String,
    pub notes: Option<String>,
    pub geometry: Value,
    pub price: Option<f64>,
    pub currency: Option<String>,
    pub click_behavior: Option<ClickBehavior>,
    pub click_link_kind: Option<ClickLinkKind>,
    pub click_link_target: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateBlockRequest {
    pub title: Option<String>,
    pub notes: Option<Option<String>>,
    pub geometry: Option<Value>,
    pub price: Option<f64>,
    pub currency: Option<String>,
    pub click_behavior: Option<ClickBehavior>,
    pub click_link_kind: Option<Option<ClickLinkKind>>,
    pub click_link_target: Option<Option<String>>,
}

#[derive(Debug, Deserialize)]
pub struct BlockActionRequest {
    /// The buyer / assignee. Defaults to the caller for reserve
    /// and purchase; required for assign. Acting for another user
    /// requires the `properties.manage` capability.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub buyer_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct CorrectStatusRequest {
    /// Target status after the correction: `available`,
    /// `reserved`, or `assigned`.
    pub target_status: String,
    pub reason: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub buyer_id: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateLocationSettingsRequest {
    pub provider: Option<String>,
    pub base_url: Option<Option<String>>,
    pub allowed_host: Option<String>,
    pub timeout_secs: Option<u64>,
    pub link_allowlist_hosts: Option<Vec<String>>,
}

#[derive(Debug, Deserialize)]
pub struct LocationSearchQuery {
    pub q: String,
}

#[derive(Debug, Deserialize)]
pub struct LocationReverseQuery {
    pub lat: f64,
    pub lng: f64,
}

#[derive(Debug, Deserialize)]
pub struct ExportQuery {
    #[serde(default = "default_export_format")]
    pub format: String,
}

fn default_export_format() -> String {
    "json".to_string()
}

// ---------------------------------------------------------------------------
// GeoJSON validation, measurement, and hashing
// ---------------------------------------------------------------------------

/// What `validate_geometry` returns for a geometry that passed
/// every check: its kind, the server-computed measurements, and
/// the centroid (projected back to longitude / latitude) used for
/// the SVG label position.
#[derive(Debug, Clone)]
pub struct ValidatedGeometry {
    pub kind: String,
    pub area_sqm: Option<f64>,
    pub length_m: Option<f64>,
    pub centroid: Option<(f64, f64)>,
}

/// Validate a GeoJSON geometry object. Accepted types are
/// `Polygon`, `MultiPolygon`, and `LineString` only — no `Point`,
/// no `GeometryCollection`, no `Feature` wrapper (the geometry
/// object itself is what is stored). Every position must be
/// exactly `[longitude, latitude]` with finite numbers in range.
/// Polygons need at least one ring, each ring at least four
/// positions, and every ring must be closed (first position
/// exactly equal to the last). Lines need at least two positions.
/// The total position count is bounded so one geometry can never
/// allocate unbounded work.
pub fn validate_geometry(value: &Value) -> UnifiedResult<ValidatedGeometry> {
    let object = value.as_object().ok_or_else(|| {
        UnifiedError::bad_request("geometry must be a GeoJSON geometry object")
    })?;
    let kind = object
        .get("type")
        .and_then(|value| value.as_str())
        .ok_or_else(|| UnifiedError::bad_request("geometry.type is required"))?;
    let coordinates = object.get("coordinates").ok_or_else(|| {
        UnifiedError::bad_request("geometry.coordinates is required")
    })?;
    match kind {
        "Polygon" => validate_polygon(coordinates),
        "MultiPolygon" => validate_multi_polygon(coordinates),
        "LineString" => validate_line_string(coordinates),
        other => Err(UnifiedError::bad_request(format!(
            "unsupported geometry type \"{other}\": only Polygon, MultiPolygon, and LineString are accepted"
        ))),
    }
}

/// Read one position: exactly two finite numbers
/// `[longitude, latitude]` in range.
fn read_position(value: &Value) -> UnifiedResult<(f64, f64)> {
    let array = value.as_array().ok_or_else(|| {
        UnifiedError::bad_request("geometry positions must be arrays [longitude, latitude]")
    })?;
    if array.len() != 2 {
        return Err(UnifiedError::bad_request(
            "geometry positions must have exactly two numbers [longitude, latitude]",
        ));
    }
    let longitude = array[0].as_f64().ok_or_else(|| {
        UnifiedError::bad_request("geometry longitude must be a number")
    })?;
    let latitude = array[1].as_f64().ok_or_else(|| {
        UnifiedError::bad_request("geometry latitude must be a number")
    })?;
    if !longitude.is_finite() || !(-180.0..=180.0).contains(&longitude) {
        return Err(UnifiedError::bad_request(
            "geometry longitude must be between -180 and 180",
        ));
    }
    if !latitude.is_finite() || !(-90.0..=90.0).contains(&latitude) {
        return Err(UnifiedError::bad_request(
            "geometry latitude must be between -90 and 90",
        ));
    }
    Ok((longitude, latitude))
}

/// Read one ring: an array of positions, at least four, closed
/// (first position exactly equal to the last).
fn read_ring(value: &Value, position_count: &mut usize) -> UnifiedResult<Vec<(f64, f64)>> {
    let array = value.as_array().ok_or_else(|| {
        UnifiedError::bad_request("polygon rings must be arrays of positions")
    })?;
    if array.len() < 4 {
        return Err(UnifiedError::bad_request(
            "polygon rings must have at least 4 positions (closed ring)",
        ));
    }
    let mut positions = Vec::with_capacity(array.len());
    for raw in array {
        *position_count += 1;
        if *position_count > MAX_GEOMETRY_POSITIONS {
            return Err(UnifiedError::bad_request(format!(
                "geometry has more than {MAX_GEOMETRY_POSITIONS} positions"
            )));
        }
        positions.push(read_position(raw)?);
    }
    if positions.first() != positions.last() {
        return Err(UnifiedError::bad_request(
            "polygon rings must be closed (first position must equal last position)",
        ));
    }
    Ok(positions)
}

fn validate_polygon(coordinates: &Value) -> UnifiedResult<ValidatedGeometry> {
    let rings = coordinates.as_array().ok_or_else(|| {
        UnifiedError::bad_request("Polygon coordinates must be an array of rings")
    })?;
    if rings.is_empty() {
        return Err(UnifiedError::bad_request(
            "Polygon must have at least one ring",
        ));
    }
    if rings.len() > MAX_GEOMETRY_RINGS {
        return Err(UnifiedError::bad_request(format!(
            "Polygon has more than {MAX_GEOMETRY_RINGS} rings"
        )));
    }
    let mut count = 0usize;
    let mut parsed: Vec<Vec<(f64, f64)>> = Vec::with_capacity(rings.len());
    for ring in rings {
        parsed.push(read_ring(ring, &mut count)?);
    }
    let (area_sqm, centroid) = polygon_measurements(&parsed);
    Ok(ValidatedGeometry {
        kind: "Polygon".to_string(),
        area_sqm: Some(area_sqm),
        length_m: None,
        centroid,
    })
}

fn validate_multi_polygon(coordinates: &Value) -> UnifiedResult<ValidatedGeometry> {
    let polygons = coordinates.as_array().ok_or_else(|| {
        UnifiedError::bad_request("MultiPolygon coordinates must be an array of polygons")
    })?;
    if polygons.is_empty() {
        return Err(UnifiedError::bad_request(
            "MultiPolygon must have at least one polygon",
        ));
    }
    let mut count = 0usize;
    let mut total_area = 0.0f64;
    let mut centroid: Option<(f64, f64)> = None;
    let mut largest_area = 0.0f64;
    for polygon in polygons {
        let rings = polygon.as_array().ok_or_else(|| {
            UnifiedError::bad_request("MultiPolygon polygons must be arrays of rings")
        })?;
        if rings.is_empty() {
            return Err(UnifiedError::bad_request(
                "MultiPolygon polygons must have at least one ring",
            ));
        }
        if rings.len() > MAX_GEOMETRY_RINGS {
            return Err(UnifiedError::bad_request(format!(
                "MultiPolygon polygon has more than {MAX_GEOMETRY_RINGS} rings"
            )));
        }
        let mut parsed: Vec<Vec<(f64, f64)>> = Vec::with_capacity(rings.len());
        for ring in rings {
            parsed.push(read_ring(ring, &mut count)?);
        }
        let (area, ring_centroid) = polygon_measurements(&parsed);
        total_area += area;
        if area > largest_area {
            largest_area = area;
            centroid = ring_centroid;
        }
    }
    Ok(ValidatedGeometry {
        kind: "MultiPolygon".to_string(),
        area_sqm: Some(total_area),
        length_m: None,
        centroid,
    })
}

fn validate_line_string(coordinates: &Value) -> UnifiedResult<ValidatedGeometry> {
    let array = coordinates.as_array().ok_or_else(|| {
        UnifiedError::bad_request("LineString coordinates must be an array of positions")
    })?;
    if array.len() < 2 {
        return Err(UnifiedError::bad_request(
            "LineString must have at least 2 positions",
        ));
    }
    if array.len() > MAX_GEOMETRY_POSITIONS {
        return Err(UnifiedError::bad_request(format!(
            "geometry has more than {MAX_GEOMETRY_POSITIONS} positions"
        )));
    }
    let mut positions = Vec::with_capacity(array.len());
    for raw in array {
        positions.push(read_position(raw)?);
    }
    let length_m = line_length_m(&positions);
    let midpoint = positions.get(positions.len() / 2).copied();
    Ok(ValidatedGeometry {
        kind: "LineString".to_string(),
        area_sqm: None,
        length_m: Some(length_m),
        centroid: midpoint,
    })
}

/// Project longitude / latitude to local metres with an
/// equirectangular projection around `reference_latitude`.
/// This is an approximation suited to site plans (a few hundred
/// metres across); it is not a survey-grade geodesic measurement.
fn project(longitude: f64, latitude: f64, reference_latitude: f64) -> (f64, f64) {
    let x = longitude * METRES_PER_DEGREE_LNG_AT_EQUATOR * reference_latitude.to_radians().cos();
    let y = latitude * METRES_PER_DEGREE_LAT;
    (x, y)
}

fn mean_latitude(rings: &[Vec<(f64, f64)>]) -> f64 {
    let mut sum = 0.0f64;
    let mut count = 0usize;
    for ring in rings {
        for (_, latitude) in ring {
            sum += latitude;
            count += 1;
        }
    }
    if count == 0 {
        0.0
    } else {
        sum / count as f64
    }
}

/// Shoelace area and centroid of one ring, in projected metres.
/// Returns (signed area, centroid in projected metres).
fn ring_area_centroid(ring: &[(f64, f64)], reference_latitude: f64) -> (f64, (f64, f64)) {
    let mut cross_sum = 0.0f64;
    let mut cx = 0.0f64;
    let mut cy = 0.0f64;
    for window in ring.windows(2) {
        let (x1, y1) = project(window[0].0, window[0].1, reference_latitude);
        let (x2, y2) = project(window[1].0, window[1].1, reference_latitude);
        let cross = x1 * y2 - x2 * y1;
        cross_sum += cross;
        cx += (x1 + x2) * cross;
        cy += (y1 + y2) * cross;
    }
    let signed_area = cross_sum / 2.0;
    if signed_area.abs() < f64::EPSILON {
        // Degenerate ring (zero area): fall back to the first vertex.
        let (x, y) = project(ring[0].0, ring[0].1, reference_latitude);
        return (0.0, (x, y));
    }
    (signed_area, (cx / (6.0 * signed_area), cy / (6.0 * signed_area)))
}

/// Area of a polygon (outer ring minus holes) and the centroid of
/// its outer ring, converted back to longitude / latitude.
fn polygon_measurements(rings: &[Vec<(f64, f64)>]) -> (f64, Option<(f64, f64)>) {
    let reference_latitude = mean_latitude(rings);
    let (outer_area, outer_centroid) = ring_area_centroid(&rings[0], reference_latitude);
    let mut area = outer_area.abs();
    for hole in &rings[1..] {
        let (hole_area, _) = ring_area_centroid(hole, reference_latitude);
        area -= hole_area.abs();
    }
    let scale_x = METRES_PER_DEGREE_LNG_AT_EQUATOR * reference_latitude.to_radians().cos();
    let centroid = if scale_x.abs() < f64::EPSILON {
        None
    } else {
        Some((
            outer_centroid.0 / scale_x,
            outer_centroid.1 / METRES_PER_DEGREE_LAT,
        ))
    };
    (area.max(0.0), centroid)
}

fn line_length_m(positions: &[(f64, f64)]) -> f64 {
    let reference_latitude = positions.iter().map(|(_, lat)| lat).sum::<f64>() / positions.len() as f64;
    let mut total = 0.0f64;
    for window in positions.windows(2) {
        let (x1, y1) = project(window[0].0, window[0].1, reference_latitude);
        let (x2, y2) = project(window[1].0, window[1].1, reference_latitude);
        total += ((x2 - x1).powi(2) + (y2 - y1).powi(2)).sqrt();
    }
    total
}

/// SHA-256 hex of the canonical JSON of a geometry value.
/// `serde_json::Value` serializes object keys in sorted order
/// (the default `Map` is a `BTreeMap`), so the same geometry
/// always hashes the same way regardless of key order in the
/// request body.
fn geometry_hash(value: &Value) -> String {
    let canonical = serde_json::to_string(value).unwrap_or_default();
    let mut hasher = Sha256::new();
    hasher.update(canonical.as_bytes());
    format!("{:x}", hasher.finalize())
}

/// Extract every position of a geometry (any accepted type) as a
/// flat list, for bounds computation. Returns an empty list for a
/// malformed value (callers validate first).
fn geometry_positions(value: &Value) -> Vec<(f64, f64)> {
    let mut out = Vec::new();
    let Some(kind) = value.get("type").and_then(|v| v.as_str()) else {
        return out;
    };
    let Some(coordinates) = value.get("coordinates") else {
        return out;
    };
    match kind {
        "LineString" => {
            if let Some(array) = coordinates.as_array() {
                for raw in array {
                    if let (Some(lng), Some(lat)) = (raw.get(0).and_then(Value::as_f64), raw.get(1).and_then(Value::as_f64)) {
                        out.push((lng, lat));
                    }
                }
            }
        }
        "Polygon" => {
            if let Some(rings) = coordinates.as_array() {
                for ring in rings {
                    if let Some(array) = ring.as_array() {
                        for raw in array {
                            if let (Some(lng), Some(lat)) = (raw.get(0).and_then(Value::as_f64), raw.get(1).and_then(Value::as_f64)) {
                                out.push((lng, lat));
                            }
                        }
                    }
                }
            }
        }
        "MultiPolygon" => {
            if let Some(polygons) = coordinates.as_array() {
                for polygon in polygons {
                    if let Some(rings) = polygon.as_array() {
                        for ring in rings {
                            if let Some(array) = ring.as_array() {
                                for raw in array {
                                    if let (Some(lng), Some(lat)) = (raw.get(0).and_then(Value::as_f64), raw.get(1).and_then(Value::as_f64)) {
                                        out.push((lng, lat));
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
        _ => {}
    }
    out
}

// ---------------------------------------------------------------------------
// XML / HTML / CSV escaping (all output text is escaped)
// ---------------------------------------------------------------------------

fn xml_escape(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for ch in value.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            other => out.push(other),
        }
    }
    out
}

fn csv_escape(value: &str) -> String {
    if value.contains(',') || value.contains('"') || value.contains('\n') || value.contains('\r') {
        format!("\"{}\"", value.replace('"', "\"\""))
    } else {
        value.to_string()
    }
}

// ---------------------------------------------------------------------------
// SVG rendering
// ---------------------------------------------------------------------------

/// A projected plan: bounds in local metres plus the scale that
/// maps metres to SVG units.
struct PlanProjection {
    reference_latitude: f64,
    min_x: f64,
    min_y: f64,
    scale: f64,
    /// Height of the fitted plan in SVG units (y span * scale).
    fitted_height_svg: f64,
}

impl PlanProjection {
    fn fit(positions: &[(f64, f64)]) -> Option<Self> {
        if positions.is_empty() {
            return None;
        }
        let reference_latitude =
            positions.iter().map(|(_, lat)| lat).sum::<f64>() / positions.len() as f64;
        let mut min_x = f64::MAX;
        let mut max_x = f64::MIN;
        let mut min_y = f64::MAX;
        let mut max_y = f64::MIN;
        for (lng, lat) in positions {
            let (x, y) = project(*lng, *lat, reference_latitude);
            min_x = min_x.min(x);
            max_x = max_x.max(x);
            min_y = min_y.min(y);
            max_y = max_y.max(y);
        }
        let width_m = (max_x - min_x).max(1.0);
        let height_m = (max_y - min_y).max(1.0);
        let scale = ((SVG_WIDTH - 2.0 * SVG_PADDING) / width_m)
            .min((SVG_HEIGHT - 2.0 * SVG_PADDING) / height_m);
        Some(Self {
            reference_latitude,
            min_x,
            min_y,
            scale,
            fitted_height_svg: height_m * scale,
        })
    }

    /// Project a lon/lat point to SVG coordinates (y flipped, so
    /// north is up).
    fn to_svg(&self, longitude: f64, latitude: f64) -> (f64, f64) {
        let (x, y) = project(longitude, latitude, self.reference_latitude);
        let svg_x = SVG_PADDING + (x - self.min_x) * self.scale;
        let svg_y = SVG_PADDING + self.fitted_height_svg - (y - self.min_y) * self.scale;
        (svg_x, svg_y)
    }
}

/// Build an SVG path `d` attribute from a ring of positions.
fn ring_path(projection: &PlanProjection, ring: &[(f64, f64)], close: bool) -> String {
    let mut out = String::new();
    for (index, (lng, lat)) in ring.iter().enumerate() {
        let (x, y) = projection.to_svg(*lng, *lat);
        if index == 0 {
            out.push_str(&format!("M{x:.2} {y:.2}"));
        } else {
            out.push_str(&format!(" L{x:.2} {y:.2}"));
        }
    }
    if close {
        out.push('Z');
    }
    out
}

/// Render one plan (boundary + blocks) as an SVG document.
/// Blocks are filled with their status colour and labelled with
/// their title; the plan boundary (when present) is a dashed
/// outline. All text is XML-escaped. Class hooks
/// (`site-plan`, `plan-boundary`, `block`, `block-<status>`,
/// `block-label`) let a print stylesheet restyle the output.
pub fn render_plan_svg(
    plan: &PropertyPlan,
    blocks: &[PropertyBlock],
    title: &str,
) -> UnifiedResult<String> {
    let mut positions: Vec<(f64, f64)> = Vec::new();
    if let Some(geometry) = &plan.geometry {
        positions.extend(geometry_positions(geometry));
    }
    for block in blocks {
        positions.extend(geometry_positions(&block.geometry));
    }
    let Some(projection) = PlanProjection::fit(&positions) else {
        return Err(UnifiedError::bad_request(
            "plan has no geometry to render: add a plan boundary or blocks first",
        ));
    };
    let mut out = String::new();
    out.push_str(&format!(
        "<svg xmlns=\"http://www.w3.org/2000/svg\" class=\"site-plan\" viewBox=\"0 0 {SVG_WIDTH:.0} {SVG_HEIGHT:.0}\" role=\"img\" aria-label=\"{}\">\n",
        xml_escape(title)
    ));
    out.push_str(&format!(
        "<title>{}</title>\n",
        xml_escape(title)
    ));
    if let Some(geometry) = &plan.geometry {
        let kind = geometry
            .get("type")
            .and_then(|value| value.as_str())
            .unwrap_or("");
        let mut boundary_d = String::new();
        if kind == "Polygon" {
            if let Some(rings) = geometry.get("coordinates").and_then(Value::as_array) {
                for ring_value in rings {
                    if let Some(array) = ring_value.as_array() {
                        let ring: Vec<(f64, f64)> = array
                            .iter()
                            .filter_map(|raw| {
                                match (raw.get(0).and_then(Value::as_f64), raw.get(1).and_then(Value::as_f64)) {
                                    (Some(lng), Some(lat)) => Some((lng, lat)),
                                    _ => None,
                                }
                            })
                            .collect();
                        boundary_d.push_str(&ring_path(&projection, &ring, true));
                    }
                }
            }
        } else if kind == "MultiPolygon" {
            if let Some(polygons) = geometry.get("coordinates").and_then(Value::as_array) {
                for polygon in polygons {
                    if let Some(rings) = polygon.as_array() {
                        for ring_value in rings {
                            if let Some(array) = ring_value.as_array() {
                                let ring: Vec<(f64, f64)> = array
                                    .iter()
                                    .filter_map(|raw| {
                                        match (raw.get(0).and_then(Value::as_f64), raw.get(1).and_then(Value::as_f64)) {
                                            (Some(lng), Some(lat)) => Some((lng, lat)),
                                            _ => None,
                                        }
                                    })
                                    .collect();
                                boundary_d.push_str(&ring_path(&projection, &ring, true));
                            }
                        }
                    }
                }
            }
        }
        if !boundary_d.is_empty() {
            out.push_str(&format!(
                "<path class=\"plan-boundary\" d=\"{boundary_d}\" fill=\"none\" stroke=\"#111827\" stroke-width=\"2\" stroke-dasharray=\"8 4\" />\n"
            ));
        }
    }
    for block in blocks {
        let kind = block
            .geometry
            .get("type")
            .and_then(|value| value.as_str())
            .unwrap_or("");
        let status = block.status.as_str();
        let fill = block.status.svg_fill();
        let label = xml_escape(&block.title);
        let detail = xml_escape(&format!(
            "{} — {} — {} {}",
            block.title,
            status,
            format_money(block.price),
            block.currency
        ));
        match kind {
            "Polygon" | "MultiPolygon" => {
                let mut d = String::new();
                let mut label_at: Option<(f64, f64)> = None;
                if kind == "Polygon" {
                    if let Some(rings) = block.geometry.get("coordinates").and_then(Value::as_array) {
                        for ring_value in rings {
                            if let Some(array) = ring_value.as_array() {
                                let ring: Vec<(f64, f64)> = array
                                    .iter()
                                    .filter_map(|raw| {
                                        match (raw.get(0).and_then(Value::as_f64), raw.get(1).and_then(Value::as_f64)) {
                                            (Some(lng), Some(lat)) => Some((lng, lat)),
                                            _ => None,
                                        }
                                    })
                                    .collect();
                                d.push_str(&ring_path(&projection, &ring, true));
                            }
                        }
                    }
                } else if let Some(polygons) = block.geometry.get("coordinates").and_then(Value::as_array) {
                    for polygon in polygons {
                        if let Some(rings) = polygon.as_array() {
                            for ring_value in rings {
                                if let Some(array) = ring_value.as_array() {
                                    let ring: Vec<(f64, f64)> = array
                                        .iter()
                                        .filter_map(|raw| {
                                            match (raw.get(0).and_then(Value::as_f64), raw.get(1).and_then(Value::as_f64)) {
                                                (Some(lng), Some(lat)) => Some((lng, lat)),
                                                _ => None,
                                            }
                                        })
                                        .collect();
                                    d.push_str(&ring_path(&projection, &ring, true));
                                }
                            }
                        }
                    }
                }
                // Label at the stored centroid (computed at write
                // time); fall back to the first position.
                if let Some(validated) = validate_geometry(&block.geometry).ok().and_then(|v| v.centroid) {
                    label_at = Some(projection.to_svg(validated.0, validated.1));
                } else if let Some((lng, lat)) = geometry_positions(&block.geometry).into_iter().next() {
                    label_at = Some(projection.to_svg(lng, lat));
                }
                out.push_str(&format!(
                    "<path class=\"block block-{status}\" d=\"{d}\" fill=\"{fill}\" fill-opacity=\"0.55\" stroke=\"#111827\" stroke-width=\"1.5\"><title>{detail}</title></path>\n"
                ));
                if let Some((x, y)) = label_at {
                    out.push_str(&format!(
                        "<text class=\"block-label\" x=\"{x:.2}\" y=\"{y:.2}\" text-anchor=\"middle\" font-size=\"14\" fill=\"#111827\">{label}</text>\n"
                    ));
                }
            }
            "LineString" => {
                let positions = geometry_positions(&block.geometry);
                let d = ring_path(&projection, &positions, false);
                out.push_str(&format!(
                    "<path class=\"block block-{status} block-line\" d=\"{d}\" fill=\"none\" stroke=\"{fill}\" stroke-width=\"3\"><title>{detail}</title></path>\n"
                ));
                if let Some((lng, lat)) = positions.get(positions.len() / 2) {
                    let (x, y) = projection.to_svg(*lng, *lat);
                    out.push_str(&format!(
                        "<text class=\"block-label\" x=\"{x:.2}\" y=\"{y:.2}\" text-anchor=\"middle\" font-size=\"14\" fill=\"#111827\">{label}</text>\n"
                    ));
                }
            }
            _ => {}
        }
    }
    out.push_str("</svg>\n");
    Ok(out)
}

fn format_money(value: f64) -> String {
    format!("{value:.2}")
}

/// Render the print page: HTML with the inline SVG, a per-block
/// list table, and a print stylesheet. `show_buyer` controls
/// whether the buyer column is included (admin print only; the
/// public print never shows buyer ids).
pub fn render_print_html(
    property: &Property,
    plan: &PropertyPlan,
    blocks: &[PropertyBlock],
    svg: &str,
    show_buyer: bool,
) -> String {
    let mut rows = String::new();
    for block in blocks {
        let area = block
            .area_sqm
            .map(|value| format!("{value:.2} m²"))
            .unwrap_or_else(|| "-".to_string());
        let length = block
            .length_m
            .map(|value| format!("{value:.2} m"))
            .unwrap_or_else(|| "-".to_string());
        rows.push_str("<tr>");
        rows.push_str(&format!("<td>{}</td>", xml_escape(&block.title)));
        rows.push_str(&format!("<td>{}</td>", xml_escape(block.status.as_str())));
        rows.push_str(&format!("<td>{area}</td>"));
        rows.push_str(&format!("<td>{length}</td>"));
        rows.push_str(&format!(
            "<td>{} {}</td>",
            format_money(block.price),
            xml_escape(&block.currency)
        ));
        if show_buyer {
            rows.push_str(&format!(
                "<td>{}</td>",
                xml_escape(block.assigned_customer_id.as_deref().unwrap_or("-"))
            ));
        }
        rows.push_str("</tr>\n");
    }
    let buyer_header = if show_buyer { "<th>Buyer</th>" } else { "" };
    format!(
        "<!DOCTYPE html>\n<html lang=\"en\">\n<head>\n<meta charset=\"utf-8\" />\n<title>{} — {}</title>\n<style>\nbody {{ font-family: sans-serif; color: #111827; margin: 24px; }}\nh1 {{ font-size: 22px; margin-bottom: 4px; }}\nh2 {{ font-size: 16px; margin: 24px 0 8px; }}\n.meta {{ color: #4b5563; font-size: 13px; margin-bottom: 16px; }}\n.site-plan {{ width: 100%; height: auto; border: 1px solid #d1d5db; }}\ntable {{ border-collapse: collapse; width: 100%; font-size: 13px; }}\nth, td {{ border: 1px solid #d1d5db; padding: 6px 8px; text-align: left; }}\nth {{ background: #f3f4f6; }}\n@media print {{\n  body {{ margin: 0; }}\n  .no-print {{ display: none; }}\n  .site-plan {{ border: none; }}\n}}\n</style>\n</head>\n<body>\n<h1>{} — {}</h1>\n<p class=\"meta\">Plan: {} &middot; {} block(s) &middot; Printed site plan</p>\n<div class=\"plan-svg\">{}</div>\n<h2>Blocks</h2>\n<table>\n<thead><tr><th>Title</th><th>Status</th><th>Area</th><th>Length</th><th>Price</th>{}</tr></thead>\n<tbody>\n{}\n</tbody>\n</table>\n<p class=\"no-print\"><button onclick=\"window.print()\">Print</button></p>\n</body>\n</html>\n",
        xml_escape(&property.title),
        xml_escape(&plan.name),
        xml_escape(&property.title),
        xml_escape(&plan.name),
        xml_escape(&plan.name),
        blocks.len(),
        svg,
        buyer_header,
        rows
    )
}

/// Export one plan's blocks as CSV (RFC 4180 escaping).
pub fn blocks_to_csv(blocks: &[PropertyBlock]) -> String {
    let mut out = String::from("id,title,status,area_sqm,length_m,price,currency,assigned_customer_id,order_id\n");
    for block in blocks {
        out.push_str(&format!(
            "{},{},{},{},{},{},{},{},{}\n",
            block.id,
            csv_escape(&block.title),
            block.status.as_str(),
            block.area_sqm.map(|v| format!("{v:.4}")).unwrap_or_default(),
            block.length_m.map(|v| format!("{v:.4}")).unwrap_or_default(),
            format!("{:.4}", block.price),
            csv_escape(&block.currency),
            csv_escape(block.assigned_customer_id.as_deref().unwrap_or("")),
            csv_escape(block.order_id.as_deref().unwrap_or("")),
        ));
    }
    out
}

// ---------------------------------------------------------------------------
// Location providers (pluggable geocoder)
// ---------------------------------------------------------------------------

/// Pluggable geocoder provider. Implementations must never log a
/// query or a configured URL that could contain credentials; this
/// codebase's providers need no API key at all.
#[async_trait]
pub trait LocationProvider: Send + Sync {
    fn name(&self) -> &'static str;
    async fn search(&self, query: &str) -> UnifiedResult<Vec<PlaceCandidate>>;
    async fn reverse_geocode(
        &self,
        latitude: f64,
        longitude: f64,
    ) -> UnifiedResult<Option<PlaceCandidate>>;
}

/// The `none` provider: manual coordinates only. Search and
/// reverse geocoding answer with a clear "not configured" error
/// instead of a fabricated result.
pub struct NoneProvider;

#[async_trait]
impl LocationProvider for NoneProvider {
    fn name(&self) -> &'static str {
        "none"
    }

    async fn search(&self, _query: &str) -> UnifiedResult<Vec<PlaceCandidate>> {
        Err(UnifiedError::bad_request(
            "No location provider is configured: set provider to \"nominatim\" in the location settings, or enter coordinates manually",
        ))
    }

    async fn reverse_geocode(
        &self,
        _latitude: f64,
        _longitude: f64,
    ) -> UnifiedResult<Option<PlaceCandidate>> {
        Err(UnifiedError::bad_request(
            "No location provider is configured: set provider to \"nominatim\" in the location settings, or enter coordinates manually",
        ))
    }
}

/// One result row of a Nominatim-compatible `/search` response.
/// Nominatim returns latitude / longitude as strings.
#[derive(Debug, Deserialize)]
struct NominatimSearchItem {
    display_name: String,
    lat: String,
    lon: String,
    #[serde(default)]
    #[serde(rename = "type")]
    place_type: Option<String>,
    #[serde(default)]
    importance: Option<f64>,
}

/// One result row of a Nominatim-compatible `/reverse` response.
#[derive(Debug, Deserialize)]
struct NominatimReverseItem {
    display_name: String,
    lat: String,
    lon: String,
    #[serde(default)]
    #[serde(rename = "type")]
    place_type: Option<String>,
}

/// HTTP adapter for a Nominatim-compatible geocoding endpoint.
/// The base URL is validated at construction: HTTPS only, host
/// must equal the configured allowlisted host exactly, no
/// non-standard port. Requests carry a fixed User-Agent and a
/// bounded timeout. No API key is sent or logged.
pub struct NominatimProvider {
    base_url: String,
    client: reqwest::Client,
}

impl NominatimProvider {
    pub fn new(base_url: &str, timeout_secs: u64) -> UnifiedResult<Self> {
        let parsed = reqwest::Url::parse(base_url).map_err(|_| {
            UnifiedError::bad_request(format!("Invalid geocoder base URL: \"{base_url}\""))
        })?;
        if parsed.scheme() != "https" {
            return Err(UnifiedError::bad_request(
                "Geocoder base URL must use https",
            ));
        }
        if parsed.host_str().is_none() {
            return Err(UnifiedError::bad_request(
                "Geocoder base URL must have a host",
            ));
        }
        if parsed.port().is_some_and(|port| port != 443) {
            return Err(UnifiedError::bad_request(
                "Geocoder base URL must not use a non-standard port",
            ));
        }
        // The host allowlist is enforced by the settings layer
        // (`build_provider` compares the host against
        // `allowed_host` before constructing the adapter).
        let client = reqwest::Client::builder()
            .user_agent(GEOCODER_USER_AGENT)
            .timeout(Duration::from_secs(timeout_secs))
            .connect_timeout(Duration::from_secs(timeout_secs.min(5)))
            .build()
            .map_err(|error| {
                UnifiedError::internal(format!("Cannot build geocoder HTTP client: {error}"))
            })?;
        Ok(Self {
            base_url: base_url.trim_end_matches('/').to_string(),
            client,
        })
    }

    fn parse_candidate(item: NominatimSearchItem) -> Option<PlaceCandidate> {
        let latitude: f64 = item.lat.parse().ok()?;
        let longitude: f64 = item.lon.parse().ok()?;
        if !(-90.0..=90.0).contains(&latitude) || !(-180.0..=180.0).contains(&longitude) {
            return None;
        }
        Some(PlaceCandidate {
            display_name: item.display_name,
            latitude,
            longitude,
            place_type: item.place_type,
            importance: item.importance,
        })
    }
}

#[async_trait]
impl LocationProvider for NominatimProvider {
    fn name(&self) -> &'static str {
        "nominatim"
    }

    async fn search(&self, query: &str) -> UnifiedResult<Vec<PlaceCandidate>> {
        let url = format!("{}/search", self.base_url);
        let response = self
            .client
            .get(url)
            .query(&[
                ("format", "jsonv2"),
                ("q", query),
                ("limit", "5"),
                ("addressdetails", "1"),
            ])
            .send()
            .await
            .map_err(|error| {
                UnifiedError::internal(format!("Geocoder request failed: {error}"))
            })?;
        if !response.status().is_success() {
            return Err(UnifiedError::internal(format!(
                "Geocoder answered with HTTP {}",
                response.status()
            )));
        }
        let items: Vec<NominatimSearchItem> = response.json().await.map_err(|error| {
            UnifiedError::internal(format!("Geocoder answered with an unreadable body: {error}"))
        })?;
        let mut out: Vec<PlaceCandidate> = items
            .into_iter()
            .filter_map(Self::parse_candidate)
            .take(MAX_SEARCH_RESULTS)
            .collect();
        out.truncate(MAX_SEARCH_RESULTS);
        Ok(out)
    }

    async fn reverse_geocode(
        &self,
        latitude: f64,
        longitude: f64,
    ) -> UnifiedResult<Option<PlaceCandidate>> {
        let url = format!("{}/reverse", self.base_url);
        let response = self
            .client
            .get(url)
            .query(&[
                ("format", "jsonv2"),
                ("lat", latitude.to_string()),
                ("lon", longitude.to_string()),
                ("zoom", "18".to_string()),
                ("addressdetails", "1".to_string()),
            ])
            .send()
            .await
            .map_err(|error| {
                UnifiedError::internal(format!("Geocoder request failed: {error}"))
            })?;
        if response.status().as_u16() == 404 {
            return Ok(None);
        }
        if !response.status().is_success() {
            return Err(UnifiedError::internal(format!(
                "Geocoder answered with HTTP {}",
                response.status()
            )));
        }
        let item: NominatimReverseItem = response.json().await.map_err(|error| {
            UnifiedError::internal(format!("Geocoder answered with an unreadable body: {error}"))
        })?;
        let latitude: f64 = item.lat.parse().map_err(|_| {
            UnifiedError::internal("Geocoder answered with an invalid latitude")
        })?;
        let longitude: f64 = item.lon.parse().map_err(|_| {
            UnifiedError::internal("Geocoder answered with an invalid longitude")
        })?;
        Ok(Some(PlaceCandidate {
            display_name: item.display_name,
            latitude,
            longitude,
            place_type: item.place_type,
            importance: None,
        }))
    }
}

/// Build the provider for a settings snapshot. Returns an error
/// when the settings name a provider that cannot be constructed
/// (for example a Nominatim base URL whose host does not match
/// the allowlisted host).
fn build_provider(settings: &LocationSettings) -> UnifiedResult<Arc<dyn LocationProvider>> {
    match settings.provider.as_str() {
        "none" => Ok(Arc::new(NoneProvider)),
        "nominatim" => {
            let Some(base_url) = &settings.base_url else {
                return Err(UnifiedError::bad_request(
                    "Nominatim provider needs a base_url in the location settings",
                ));
            };
            let parsed = reqwest::Url::parse(base_url).map_err(|_| {
                UnifiedError::bad_request(format!("Invalid geocoder base URL: \"{base_url}\""))
            })?;
            let host = parsed.host_str().unwrap_or("");
            if host != settings.allowed_host {
                return Err(UnifiedError::bad_request(format!(
                    "Geocoder host \"{host}\" is not the allowlisted host \"{}\"",
                    settings.allowed_host
                )));
            }
            Ok(Arc::new(NominatimProvider::new(
                base_url,
                settings.timeout_secs,
            )?))
        }
        other => Err(UnifiedError::bad_request(format!(
            "Unknown location provider \"{other}\": expected \"none\" or \"nominatim\""
        ))),
    }
}

/// A hostname is labels of letters, digits, and hyphens separated
/// by dots, never empty, never starting or ending with a hyphen
/// or a dot, at most 253 characters.
fn valid_hostname(host: &str) -> bool {
    if host.is_empty() || host.len() > 253 {
        return false;
    }
    if host.starts_with('-') || host.ends_with('-') || host.starts_with('.') || host.ends_with('.') {
        return false;
    }
    host.split('.').all(|label| {
        !label.is_empty()
            && label.len() <= 63
            && !label.starts_with('-')
            && !label.ends_with('-')
            && label
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-')
    })
}

// ---------------------------------------------------------------------------
// Shared state
// ---------------------------------------------------------------------------

/// In-memory stores for properties. The hook bus is attached in
/// `init` (the bus lives in `AppContext`); handlers tolerate a
/// missing bus by skipping event emission. Durable rows for these
/// types are defined by migration
/// `2026-11-22-000060_properties`; until that migration is
/// executed, properties do not survive a restart.
#[derive(Clone)]
pub struct PropertiesState {
    pub properties: Arc<DashMap<Uuid, Property>>,
    pub plans: Arc<DashMap<Uuid, PropertyPlan>>,
    pub blocks: Arc<DashMap<Uuid, PropertyBlock>>,
    pub history: Arc<DashMap<Uuid, PropertyBlockHistory>>,
    pub purchases: Arc<DashMap<Uuid, BlockPurchase>>,
    pub location_settings: Arc<RwLock<LocationSettings>>,
    pub location_provider: Arc<RwLock<Arc<dyn LocationProvider>>>,
    pub hooks: Arc<RwLock<Option<Arc<HookBus>>>>,
}

impl Default for PropertiesState {
    fn default() -> Self {
        Self {
            properties: Arc::new(DashMap::new()),
            plans: Arc::new(DashMap::new()),
            blocks: Arc::new(DashMap::new()),
            history: Arc::new(DashMap::new()),
            purchases: Arc::new(DashMap::new()),
            location_settings: Arc::new(RwLock::new(LocationSettings::default())),
            location_provider: Arc::new(RwLock::new(Arc::new(NoneProvider))),
            hooks: Arc::new(RwLock::new(None)),
        }
    }
}

impl PropertiesState {
    fn bus(&self) -> Option<Arc<HookBus>> {
        self.hooks.read().expect("hooks lock poisoned").clone()
    }

    fn emit(&self, event: &str, data: Value) {
        if let Some(bus) = self.bus() {
            bus.emit(event, data);
        }
    }

    /// Emit a `notification.requested` event in the notifications
    /// module's payload shape (see the events module's `notify`).
    fn notify(&self, kind: &str, user_id: &str, title: &str, body: &str, link: Option<String>) {
        self.emit(
            "notification.requested",
            json!({
                "kind": kind,
                "user_id": user_id,
                "title": title,
                "body": body,
                "module": "properties",
                "category": "properties",
                "priority": "normal",
                "link": link,
            }),
        );
    }

    fn provider(&self) -> Arc<dyn LocationProvider> {
        self.location_provider
            .read()
            .expect("provider lock poisoned")
            .clone()
    }

    fn blocks_of_plan(&self, plan_id: Uuid) -> Vec<PropertyBlock> {
        let mut items: Vec<PropertyBlock> = self
            .blocks
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|block| block.plan_id == plan_id)
            .collect();
        items.sort_by(|a, b| a.title.cmp(&b.title).then(a.created_at.cmp(&b.created_at)));
        items
    }

    fn plans_of_property(&self, property_id: Uuid) -> Vec<PropertyPlan> {
        let mut items: Vec<PropertyPlan> = self
            .plans
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|plan| plan.property_id == property_id)
            .collect();
        items.sort_by(|a, b| a.name.cmp(&b.name).then(a.created_at.cmp(&b.created_at)));
        items
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Caller identity: `X-User-Id` (required) plus `X-Role` (required
/// for capability checks), exactly like the events module.
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
/// the `role.check` hook. Fails closed: no answer means not allowed
/// (same fail-closed shape as the events module's capability check).
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

fn parse_uuid(raw: &str, what: &str) -> UnifiedResult<Uuid> {
    Uuid::parse_str(raw).map_err(|_| UnifiedError::bad_request(format!("Invalid {what} id: {raw}")))
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

/// A slug is lowercase letters, digits, and single hyphens only,
/// never leading or trailing with a hyphen, at most 128 chars.
fn valid_slug(slug: &str) -> bool {
    if slug.is_empty() || slug.len() > MAX_SLUG_LEN {
        return false;
    }
    if slug.starts_with('-') || slug.ends_with('-') || slug.contains("--") {
        return false;
    }
    slug.chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

fn slugify(title: &str) -> String {
    let mut out = String::new();
    let mut last_dash = true;
    for ch in title.to_lowercase().chars() {
        if ch.is_ascii_lowercase() || ch.is_ascii_digit() {
            out.push(ch);
            last_dash = false;
        } else if !last_dash {
            out.push('-');
            last_dash = true;
        }
    }
    while out.ends_with('-') {
        out.pop();
    }
    if out.is_empty() {
        out.push_str("property");
    }
    out.truncate(MAX_SLUG_LEN);
    while out.ends_with('-') {
        out.pop();
    }
    out
}

/// Money: finite, non-negative, rounded to 4 decimal places
/// (NUMERIC(19,4) in the migration).
fn valid_price(price: f64) -> UnifiedResult<f64> {
    if !price.is_finite() || price < 0.0 {
        return Err(UnifiedError::bad_request("price must be a finite number >= 0"));
    }
    Ok((price * 10_000.0).round() / 10_000.0)
}

fn round_money(value: f64) -> f64 {
    (value * 10_000.0).round() / 10_000.0
}

/// Currency: 3-letter ISO code, uppercased.
fn valid_currency(currency: &str) -> UnifiedResult<String> {
    let upper = currency.trim().to_uppercase();
    if upper.len() != 3 || !upper.chars().all(|c| c.is_ascii_uppercase()) {
        return Err(UnifiedError::bad_request(
            "currency must be a 3-letter ISO code such as \"USD\"",
        ));
    }
    Ok(upper)
}

fn valid_latitude(latitude: f64) -> UnifiedResult<f64> {
    if !latitude.is_finite() || !(-90.0..=90.0).contains(&latitude) {
        return Err(UnifiedError::bad_request("latitude must be between -90 and 90"));
    }
    Ok(latitude)
}

fn valid_longitude(longitude: f64) -> UnifiedResult<f64> {
    if !longitude.is_finite() || !(-180.0..=180.0).contains(&longitude) {
        return Err(UnifiedError::bad_request(
            "longitude must be between -180 and 180",
        ));
    }
    Ok(longitude)
}

/// Validate a click target against the click behaviour and the
/// link kind. `popup` ignores any target. `product` / `order`
/// targets must be UUIDs; `page` targets must be a relative path
/// ("/...") or an absolute http(s) URL whose host is on the
/// configured link allowlist.
fn validate_click_target(
    behavior: ClickBehavior,
    link_kind: Option<ClickLinkKind>,
    link_target: Option<&str>,
    link_allowlist_hosts: &[String],
) -> UnifiedResult<(Option<ClickLinkKind>, Option<String>)> {
    match behavior {
        ClickBehavior::Popup => Ok((None, None)),
        ClickBehavior::Link => {
            let kind = link_kind.ok_or_else(|| {
                UnifiedError::bad_request("click_link_kind is required when click_behavior is \"link\"")
            })?;
            let target = link_target
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .ok_or_else(|| {
                    UnifiedError::bad_request("click_link_target is required when click_behavior is \"link\"")
                })?;
            match kind {
                ClickLinkKind::Product | ClickLinkKind::Order => {
                    Uuid::parse_str(target).map_err(|_| {
                        UnifiedError::bad_request(
                            "click_link_target must be a UUID for product / order links",
                        )
                    })?;
                    Ok((Some(kind), Some(target.to_string())))
                }
                ClickLinkKind::Page => {
                    if target.starts_with('/') && !target.starts_with("//") {
                        return Ok((Some(kind), Some(target.to_string())));
                    }
                    let parsed = reqwest::Url::parse(target).map_err(|_| {
                        UnifiedError::bad_request(
                            "click_link_target must be a relative path or an absolute URL",
                        )
                    })?;
                    if parsed.scheme() != "http" && parsed.scheme() != "https" {
                        return Err(UnifiedError::bad_request(
                            "click_link_target absolute URLs must use http or https",
                        ));
                    }
                    let host = parsed.host_str().unwrap_or("");
                    if !link_allowlist_hosts.iter().any(|allowed| allowed == host) {
                        return Err(UnifiedError::bad_request(format!(
                            "click_link_target host \"{host}\" is not on the link allowlist"
                        )));
                    }
                    Ok((Some(kind), Some(target.to_string())))
                }
            }
        }
    }
}

fn load_property(state: &PropertiesState, raw_id: &str) -> UnifiedResult<Property> {
    let id = parse_uuid(raw_id, "property")?;
    state
        .properties
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Property not found: {id}")))
}

fn load_property_by_slug(state: &PropertiesState, slug: &str) -> UnifiedResult<Property> {
    state
        .properties
        .iter()
        .map(|entry| entry.value().clone())
        .find(|property| property.slug == slug)
        .ok_or_else(|| UnifiedError::not_found(format!("Property not found: {slug}")))
}

fn load_plan(state: &PropertiesState, property_id: Uuid, raw_id: &str) -> UnifiedResult<PropertyPlan> {
    let id = parse_uuid(raw_id, "plan")?;
    state
        .plans
        .get(&id)
        .map(|entry| entry.value().clone())
        .filter(|plan| plan.property_id == property_id)
        .ok_or_else(|| UnifiedError::not_found(format!("Plan not found: {id}")))
}

fn load_block(
    state: &PropertiesState,
    property_id: Uuid,
    plan_id: Uuid,
    raw_id: &str,
) -> UnifiedResult<PropertyBlock> {
    let id = parse_uuid(raw_id, "block")?;
    state
        .blocks
        .get(&id)
        .map(|entry| entry.value().clone())
        .filter(|block| block.property_id == property_id && block.plan_id == plan_id)
        .ok_or_else(|| UnifiedError::not_found(format!("Block not found: {id}")))
}

fn slug_in_use(state: &PropertiesState, slug: &str, except: Option<Uuid>) -> bool {
    state.properties.iter().any(|entry| {
        entry.value().slug == slug && Some(entry.value().id) != except
    })
}

fn validate_storage_reference(target: &str, key: &str) -> UnifiedResult<()> {
    if target.trim().is_empty() || target.len() > MAX_NAME_LEN {
        return Err(UnifiedError::bad_request(
            "cover_storage_target must not be empty and must be at most 200 characters",
        ));
    }
    if key.trim().is_empty() || key.len() > MAX_STORAGE_KEY_LEN {
        return Err(UnifiedError::bad_request(
            "cover_storage_key must not be empty and must be at most 512 characters",
        ));
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Block status machine + audit history
// ---------------------------------------------------------------------------

/// Write one audited history row for a block status change.
fn record_history(
    state: &PropertiesState,
    block: &PropertyBlock,
    from: BlockStatus,
    to: BlockStatus,
    actor_id: &str,
    reason: Option<String>,
    order_id: Option<String>,
) {
    let entry = PropertyBlockHistory {
        id: Uuid::now_v7(),
        block_id: block.id,
        property_id: block.property_id,
        plan_id: block.plan_id,
        from_status: from.as_str().to_string(),
        to_status: to.as_str().to_string(),
        actor_id: actor_id.to_string(),
        reason,
        order_id,
        created_at: Utc::now(),
    };
    state.history.insert(entry.id, entry);
}

/// Move a block to a new status, writing the audit row and
/// emitting the matching event. The caller owns the transition
/// rules; this helper only records and announces the move.
fn move_block_status(
    state: &PropertiesState,
    block: &mut PropertyBlock,
    to: BlockStatus,
    actor_id: &str,
    reason: Option<String>,
    order_id: Option<String>,
    event: &str,
) {
    let from = block.status;
    block.status = to;
    block.updated_at = Utc::now();
    record_history(state, block, from, to, actor_id, reason.clone(), order_id.clone());
    state.emit(
        event,
        json!({
            "property_id": block.property_id.to_string(),
            "plan_id": block.plan_id.to_string(),
            "block_id": block.id.to_string(),
            "from_status": from.as_str(),
            "to_status": to.as_str(),
            "actor_id": actor_id,
            "reason": reason,
            "order_id": order_id,
        }),
    );
}

/// Public view of a property: plans with their blocks, but never
/// the buyer id or order id of a block.
fn public_property_view(state: &PropertiesState, property: &Property) -> Value {
    let plans: Vec<Value> = state
        .plans_of_property(property.id)
        .into_iter()
        .map(|plan| {
            let blocks: Vec<Value> = state
                .blocks_of_plan(plan.id)
                .into_iter()
                .map(|block| public_block_view(&block))
                .collect();
            let mut value = serde_json::to_value(&plan).unwrap_or(Value::Null);
            if let Value::Object(map) = &mut value {
                map.insert("blocks".to_string(), Value::Array(blocks));
            }
            value
        })
        .collect();
    let mut value = serde_json::to_value(property).unwrap_or(Value::Null);
    if let Value::Object(map) = &mut value {
        map.insert("plans".to_string(), Value::Array(plans));
    }
    value
}

/// Public view of a block: everything a visitor needs to see the
/// plan, without the buyer id or the order id.
fn public_block_view(block: &PropertyBlock) -> Value {
    let mut value = serde_json::to_value(block).unwrap_or(Value::Null);
    if let Value::Object(map) = &mut value {
        map.remove("assigned_customer_id");
        map.remove("order_id");
    }
    value
}

/// Admin view of a plan: the plan plus its blocks (full detail).
fn plan_view(state: &PropertiesState, plan: &PropertyPlan) -> Value {
    let blocks: Vec<PropertyBlock> = state.blocks_of_plan(plan.id);
    let mut value = serde_json::to_value(plan).unwrap_or(Value::Null);
    if let Value::Object(map) = &mut value {
        map.insert(
            "blocks".to_string(),
            serde_json::to_value(blocks).unwrap_or(Value::Null),
        );
    }
    value
}

// ---------------------------------------------------------------------------
// Hook listeners (payment / order events)
// ---------------------------------------------------------------------------

/// Link a store order id to a purchase record. The store side
/// emits `properties.order.created` after creating the order for
/// a `properties.block.purchase_requested` (see the buy-flow
/// contract in the module header). Payload:
/// `{purchase_id, order_id, block_id, property_id, buyer_id,
/// total, currency}`.
fn on_properties_order_created(state: Arc<PropertiesState>, ctx: &mut HookContext) {
    let purchase_id = ctx
        .get("purchase_id")
        .and_then(|value| value.as_str())
        .and_then(|raw| Uuid::parse_str(raw).ok());
    let order_id = ctx
        .get("order_id")
        .and_then(|value| value.as_str())
        .map(str::to_string);
    let (Some(purchase_id), Some(order_id)) = (purchase_id, order_id) else {
        ctx.add_result(json!({
            "properties": "skipped",
            "reason": "properties.order.created carries no purchase_id or order_id",
        }));
        return;
    };
    let Some(mut purchase) = state
        .purchases
        .get_mut(&purchase_id)
        .map(|entry| entry.value().clone())
    else {
        ctx.add_result(json!({
            "properties": "skipped",
            "purchase_id": purchase_id.to_string(),
            "reason": "purchase not found",
        }));
        return;
    };
    if purchase.status != "requested" {
        ctx.add_result(json!({
            "properties": "already_linked",
            "purchase_id": purchase_id.to_string(),
            "status": purchase.status,
        }));
        return;
    }
    purchase.order_id = Some(order_id.clone());
    purchase.status = "order_created".to_string();
    purchase.updated_at = Utc::now();
    state.purchases.insert(purchase.id, purchase);
    ctx.add_result(json!({
        "properties": "order_linked",
        "purchase_id": purchase_id.to_string(),
        "order_id": order_id,
    }));
}

/// Find the purchase for a payment event: by `order_id` first
/// (top-level, or nested under `order.id`), then by a top-level
/// `purchase_id` when the emitter forwards it.
fn find_purchase_for_payment(state: &PropertiesState, ctx: &HookContext) -> Option<BlockPurchase> {
    let order_id = ctx
        .get("order_id")
        .and_then(|value| value.as_str())
        .map(str::to_string)
        .or_else(|| {
            ctx.get("order")
                .and_then(|order| order.get("id"))
                .and_then(|id| id.as_str())
                .map(str::to_string)
        });
    if let Some(order_id) = order_id {
        if let Some(purchase) = state
            .purchases
            .iter()
            .map(|entry| entry.value().clone())
            .find(|purchase| purchase.order_id.as_deref() == Some(order_id.as_str()))
        {
            return Some(purchase);
        }
    }
    let purchase_id = ctx
        .get("purchase_id")
        .and_then(|value| value.as_str())
        .and_then(|raw| Uuid::parse_str(raw).ok());
    if let Some(purchase_id) = purchase_id {
        if let Some(purchase) = state
            .purchases
            .get(&purchase_id)
            .map(|entry| entry.value().clone())
        {
            return Some(purchase);
        }
    }
    None
}

/// Zero-decimal currencies (ISO 4217): the minor unit is the
/// major unit, so no division by 100 applies.
fn is_zero_decimal_currency(currency: &str) -> bool {
    matches!(
        currency.to_uppercase().as_str(),
        "JPY" | "KRW" | "VND" | "CLP" | "ISK" | "UGX" | "XAF" | "XOF" | "XPF"
    )
}

fn minor_to_major(amount_minor: i64, currency: &str) -> f64 {
    if is_zero_decimal_currency(currency) {
        amount_minor as f64
    } else {
        amount_minor as f64 / 100.0
    }
}

/// An approved payment (`payment.captured`, gateway or bank
/// transfer) moves the purchased block to `sold`. Only purchases
/// in `requested` / `order_created` / `pending_verification` may
/// move to sold — never `rejected` or `cancelled` — and a block
/// that is already sold is left alone, so redelivered captures
/// are safe. Gateway captures must cover the block price;
/// bank-transfer captures skip the amount check (the
/// administrator already verified the amount when approving the
/// transfer, mirroring the store module).
fn on_payment_captured(state: Arc<PropertiesState>, ctx: &mut HookContext) {
    if let Some(status) = ctx.get("status").and_then(|value| value.as_str()) {
        if status != "captured" {
            ctx.add_result(json!({
                "properties": "skipped",
                "reason": format!("payment.captured carries status \"{status}\", not \"captured\""),
            }));
            return;
        }
    }
    let Some(purchase) = find_purchase_for_payment(&state, ctx) else {
        ctx.add_result(json!({
            "properties": "skipped",
            "reason": "payment.captured carries no order id matching a block purchase",
        }));
        return;
    };
    if purchase.status == "paid" {
        ctx.add_result(json!({
            "properties": "already_paid",
            "purchase_id": purchase.id.to_string(),
        }));
        return;
    }
    if !matches!(
        purchase.status.as_str(),
        "requested" | "order_created" | "pending_verification"
    ) {
        ctx.add_result(json!({
            "properties": "skipped",
            "purchase_id": purchase.id.to_string(),
            "reason": format!("purchase in status {} can never move to paid", purchase.status),
        }));
        return;
    }
    let provider = ctx
        .get("provider")
        .and_then(|value| value.as_str())
        .map(str::to_string);
    if provider.as_deref() != Some("bank_transfer") {
        if let (Some(amount_minor), Some(event_currency)) = (
            ctx.get("amount_minor").and_then(|value| value.as_i64()),
            ctx.get("currency").and_then(|value| value.as_str()),
        ) {
            if !event_currency.eq_ignore_ascii_case(&purchase.currency) {
                ctx.add_result(json!({
                    "properties": "skipped",
                    "purchase_id": purchase.id.to_string(),
                    "reason": format!(
                        "captured currency {event_currency} does not match purchase currency {}",
                        purchase.currency
                    ),
                }));
                return;
            }
            if minor_to_major(amount_minor, event_currency) < purchase.price {
                ctx.add_result(json!({
                    "properties": "skipped",
                    "purchase_id": purchase.id.to_string(),
                    "reason": "captured amount does not cover the block price",
                }));
                return;
            }
        }
    }
    let Some(mut block) = state.blocks.get_mut(&purchase.block_id).map(|entry| entry.value().clone()) else {
        ctx.add_result(json!({
            "properties": "skipped",
            "purchase_id": purchase.id.to_string(),
            "reason": "purchased block not found",
        }));
        return;
    };
    if block.status == BlockStatus::Sold {
        ctx.add_result(json!({
            "properties": "already_sold",
            "block_id": block.id.to_string(),
        }));
        return;
    }
    if block.status != BlockStatus::Reserved && block.status != BlockStatus::Assigned {
        ctx.add_result(json!({
            "properties": "skipped",
            "block_id": block.id.to_string(),
            "reason": format!("block in status {} cannot move to sold", block.status.as_str()),
        }));
        return;
    }
    let mut purchase = purchase;
    purchase.status = "paid".to_string();
    purchase.updated_at = Utc::now();
    state.purchases.insert(purchase.id, purchase.clone());
    block.order_id = purchase.order_id.clone();
    block.assigned_customer_id = Some(purchase.buyer_id.clone());
    move_block_status(
        &state,
        &mut block,
        BlockStatus::Sold,
        "system:payment",
        Some("payment captured".to_string()),
        purchase.order_id.clone(),
        "properties.block.sold",
    );
    state.blocks.insert(block.id, block.clone());
    state.notify(
        "properties.block.sold",
        &purchase.buyer_id,
        &format!("Block \"{}\" sold", block.title),
        &format!(
            "Your purchase of block \"{}\" is confirmed. Price: {} {}.",
            block.title,
            format_money(block.price),
            block.currency
        ),
        Some(format!("/api/v1/properties/{}", block.property_id)),
    );
    ctx.add_result(json!({
        "properties": "sold",
        "purchase_id": purchase.id.to_string(),
        "block_id": block.id.to_string(),
    }));
}

/// A bank-transfer verification moves the purchase to
/// `pending_verification`; the block stays `reserved` while the
/// administrator verifies the transfer.
fn on_bank_transfer_verification_created(state: Arc<PropertiesState>, ctx: &mut HookContext) {
    let Some(purchase) = find_purchase_for_payment(&state, ctx) else {
        ctx.add_result(json!({
            "properties": "skipped",
            "reason": "bank_transfer.verification.created carries no order id matching a block purchase",
        }));
        return;
    };
    if !matches!(purchase.status.as_str(), "requested" | "order_created") {
        ctx.add_result(json!({
            "properties": "skipped",
            "purchase_id": purchase.id.to_string(),
            "reason": format!("purchase in status {} cannot move to pending_verification", purchase.status),
        }));
        return;
    }
    let mut purchase = purchase;
    purchase.status = "pending_verification".to_string();
    purchase.updated_at = Utc::now();
    state.purchases.insert(purchase.id, purchase.clone());
    state.emit(
        "properties.block.pending_verification",
        json!({
            "property_id": purchase.property_id.to_string(),
            "plan_id": purchase.plan_id.to_string(),
            "block_id": purchase.block_id.to_string(),
            "purchase_id": purchase.id.to_string(),
            "order_id": purchase.order_id,
            "buyer_id": purchase.buyer_id,
        }),
    );
    ctx.add_result(json!({
        "properties": "pending_verification",
        "purchase_id": purchase.id.to_string(),
    }));
}

/// A rejected bank transfer releases the block back to
/// `available` so it can be sold again.
fn on_bank_transfer_rejected(state: Arc<PropertiesState>, ctx: &mut HookContext) {
    let Some(purchase) = find_purchase_for_payment(&state, ctx) else {
        ctx.add_result(json!({
            "properties": "skipped",
            "reason": "bank_transfer.rejected carries no order id matching a block purchase",
        }));
        return;
    };
    if !matches!(
        purchase.status.as_str(),
        "requested" | "order_created" | "pending_verification"
    ) {
        ctx.add_result(json!({
            "properties": "skipped",
            "purchase_id": purchase.id.to_string(),
            "reason": format!("purchase in status {} cannot be released", purchase.status),
        }));
        return;
    }
    let Some(mut block) = state.blocks.get_mut(&purchase.block_id).map(|entry| entry.value().clone()) else {
        ctx.add_result(json!({
            "properties": "skipped",
            "purchase_id": purchase.id.to_string(),
            "reason": "purchased block not found",
        }));
        return;
    };
    if block.status == BlockStatus::Reserved {
        let mut purchase = purchase;
        purchase.status = "rejected".to_string();
        purchase.updated_at = Utc::now();
        state.purchases.insert(purchase.id, purchase.clone());
        block.assigned_customer_id = None;
        block.order_id = None;
        move_block_status(
            &state,
            &mut block,
            BlockStatus::Available,
            "system:bank_transfer",
            Some("bank transfer rejected".to_string()),
            purchase.order_id.clone(),
            "properties.block.released",
        );
        state.blocks.insert(block.id, block);
        ctx.add_result(json!({
            "properties": "released",
            "purchase_id": purchase.id.to_string(),
        }));
    } else {
        ctx.add_result(json!({
            "properties": "skipped",
            "purchase_id": purchase.id.to_string(),
            "reason": format!("block in status {} cannot be released", block.status.as_str()),
        }));
    }
}

// ---------------------------------------------------------------------------
// Property handlers
// ---------------------------------------------------------------------------

async fn list_properties(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<PropertiesState>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PROPERTIES_VIEW)?;
        let params = query.into_inner();
        let status_filter = params.filters.get("status").cloned();
        let company_filter = params.filters.get("company_id").cloned();
        let search = params.search.clone().unwrap_or_default().to_lowercase();
        let mut items: Vec<Property> = state
            .properties
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|property| {
                if let Some(status) = &status_filter {
                    if property.status.as_str() != status.as_str() {
                        return false;
                    }
                }
                if let Some(company_id) = &company_filter {
                    if property.company_id.map(|id| id.to_string()) != Some(company_id.clone()) {
                        return false;
                    }
                }
                if !search.is_empty() {
                    let haystack = format!(
                        "{} {} {} {}",
                        property.title,
                        property.description.clone().unwrap_or_default(),
                        property.slug,
                        property.address.clone().unwrap_or_default()
                    )
                    .to_lowercase();
                    if !haystack.contains(&search) {
                        return false;
                    }
                }
                true
            })
            .collect();
        let sort = params.allowed_sort(&["title", "created_at"], "created_at");
        let descending = params.normalized().order == "desc";
        items.sort_by(|a, b| {
            let ordering = match sort.as_str() {
                "title" => a.title.cmp(&b.title),
                _ => a.created_at.cmp(&b.created_at),
            };
            if descending {
                ordering.reverse()
            } else {
                ordering
            }
        });
        Ok(paginate(items, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn create_property(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<PropertiesState>,
    req: HttpRequest,
    body: web::Json<CreatePropertyRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Property> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PROPERTIES_CREATE)?;
        let input = body.into_inner();
        let title = require_text(&input.title, "title", MAX_TITLE_LEN)?;
        let slug = match input.slug {
            Some(raw) => {
                let slug = raw.trim().to_lowercase();
                if !valid_slug(&slug) {
                    return Err(UnifiedError::bad_request(
                        "slug must be lowercase letters, digits, and single hyphens only",
                    ));
                }
                slug
            }
            None => slugify(&title),
        };
        if slug_in_use(&state, &slug, None) {
            return Err(UnifiedError::conflict(format!(
                "A property with slug \"{slug}\" already exists"
            )));
        }
        let latitude = input
            .latitude
            .map(valid_latitude)
            .transpose()?;
        let longitude = input
            .longitude
            .map(valid_longitude)
            .transpose()?;
        if latitude.is_some() != longitude.is_some() {
            return Err(UnifiedError::bad_request(
                "latitude and longitude must be given together",
            ));
        }
        let address = input
            .address
            .map(|value| require_text(&value, "address", MAX_ADDRESS_LEN))
            .transpose()?;
        let total_area_sqm = match input.total_area_sqm {
            Some(area) => {
                if !area.is_finite() || area < 0.0 {
                    return Err(UnifiedError::bad_request(
                        "total_area_sqm must be a finite number >= 0",
                    ));
                }
                Some((area * 10_000.0).round() / 10_000.0)
            }
            None => None,
        };
        if input.cover_storage_target.is_some() != input.cover_storage_key.is_some() {
            return Err(UnifiedError::bad_request(
                "cover_storage_target and cover_storage_key must be given together",
            ));
        }
        if let (Some(target), Some(key)) = (&input.cover_storage_target, &input.cover_storage_key) {
            validate_storage_reference(target, key)?;
        }
        let description = input
            .description
            .map(|value| require_text(&value, "description", MAX_DESCRIPTION_LEN))
            .transpose()?;
        let now = Utc::now();
        let property = Property {
            id: Uuid::now_v7(),
            title,
            description,
            slug,
            latitude,
            longitude,
            address,
            total_area_sqm,
            company_id: input.company_id,
            status: PropertyStatus::Draft,
            cover_storage_target: input.cover_storage_target,
            cover_storage_key: input.cover_storage_key,
            created_by: caller.actor.clone(),
            created_at: now,
            updated_at: now,
        };
        state.properties.insert(property.id, property.clone());
        state.emit(
            "properties.created",
            json!({
                "property_id": property.id.to_string(),
                "slug": property.slug,
                "title": property.title,
                "actor_id": caller.actor,
            }),
        );
        Ok(property)
    })();
    match result {
        Ok(property) => created(property),
        Err(error) => err(error, debug),
    }
}

async fn get_property(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<PropertiesState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Property> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PROPERTIES_VIEW)?;
        load_property(&state, &path.into_inner())
    })();
    match result {
        Ok(property) => ok(property),
        Err(error) => err(error, debug),
    }
}

/// The caller may edit a property they created; anyone else needs
/// the manage capability.
fn can_edit_property(ctx: &AppContext, role: &str, property: &Property, actor: &str) -> UnifiedResult<()> {
    if property.created_by == actor {
        return Ok(());
    }
    require_permission(ctx, role, PERM_PROPERTIES_MANAGE)
}

async fn update_property(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<PropertiesState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdatePropertyRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Property> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PROPERTIES_VIEW)?;
        let mut property = load_property(&state, &path.into_inner())?;
        can_edit_property(&ctx, &caller.role, &property, &caller.actor)?;
        if matches!(property.status, PropertyStatus::Archived | PropertyStatus::SoldOut) {
            return Err(UnifiedError::conflict(format!(
                "A {} property cannot be edited",
                property.status.as_str()
            )));
        }
        let input = body.into_inner();
        if let Some(title) = input.title {
            property.title = require_text(&title, "title", MAX_TITLE_LEN)?;
        }
        if let Some(description) = input.description {
            property.description = description
                .map(|value| require_text(&value, "description", MAX_DESCRIPTION_LEN))
                .transpose()?;
        }
        if let Some(slug) = input.slug {
            let slug = slug.trim().to_lowercase();
            if !valid_slug(&slug) {
                return Err(UnifiedError::bad_request(
                    "slug must be lowercase letters, digits, and single hyphens only",
                ));
            }
            if slug_in_use(&state, &slug, Some(property.id)) {
                return Err(UnifiedError::conflict(format!(
                    "A property with slug \"{slug}\" already exists"
                )));
            }
            property.slug = slug;
        }
        if let Some(latitude) = input.latitude {
            property.latitude = latitude.map(valid_latitude).transpose()?;
        }
        if let Some(longitude) = input.longitude {
            property.longitude = longitude.map(valid_longitude).transpose()?;
        }
        if property.latitude.is_some() != property.longitude.is_some() {
            return Err(UnifiedError::bad_request(
                "latitude and longitude must be given together",
            ));
        }
        if let Some(address) = input.address {
            property.address = address
                .map(|value| require_text(&value, "address", MAX_ADDRESS_LEN))
                .transpose()?;
        }
        if let Some(total_area_sqm) = input.total_area_sqm {
            property.total_area_sqm = match total_area_sqm {
                Some(area) => {
                    if !area.is_finite() || area < 0.0 {
                        return Err(UnifiedError::bad_request(
                            "total_area_sqm must be a finite number >= 0",
                        ));
                    }
                    Some((area * 10_000.0).round() / 10_000.0)
                }
                None => None,
            };
        }
        if let Some(company_id) = input.company_id {
            property.company_id = company_id;
        }
        if let Some(target) = input.cover_storage_target {
            property.cover_storage_target = target;
        }
        if let Some(key) = input.cover_storage_key {
            property.cover_storage_key = key;
        }
        if property.cover_storage_target.is_some() != property.cover_storage_key.is_some() {
            return Err(UnifiedError::bad_request(
                "cover_storage_target and cover_storage_key must be given together",
            ));
        }
        if let (Some(target), Some(key)) =
            (&property.cover_storage_target, &property.cover_storage_key)
        {
            validate_storage_reference(target, key)?;
        }
        property.updated_at = Utc::now();
        state.properties.insert(property.id, property.clone());
        state.emit(
            "properties.updated",
            json!({
                "property_id": property.id.to_string(),
                "actor_id": caller.actor,
            }),
        );
        Ok(property)
    })();
    match result {
        Ok(property) => ok(property),
        Err(error) => err(error, debug),
    }
}

async fn delete_property(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<PropertiesState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PROPERTIES_MANAGE)?;
        let property = load_property(&state, &path.into_inner())?;
        if !matches!(property.status, PropertyStatus::Draft | PropertyStatus::Archived) {
            return Err(UnifiedError::conflict(format!(
                "Only a draft or archived property can be deleted (status is {})",
                property.status.as_str()
            )));
        }
        // Cascade in memory: plans, blocks, history, purchases.
        let plan_ids: Vec<Uuid> = state
            .plans_of_property(property.id)
            .into_iter()
            .map(|plan| plan.id)
            .collect();
        for plan_id in &plan_ids {
            let block_ids: Vec<Uuid> = state
                .blocks_of_plan(*plan_id)
                .into_iter()
                .map(|block| block.id)
                .collect();
            for block_id in block_ids {
                state.blocks.remove(&block_id);
                let history_ids: Vec<Uuid> = state
                    .history
                    .iter()
                    .filter(|entry| entry.value().block_id == block_id)
                    .map(|entry| *entry.key())
                    .collect();
                for history_id in history_ids {
                    state.history.remove(&history_id);
                }
            }
            state.plans.remove(plan_id);
        }
        let purchase_ids: Vec<Uuid> = state
            .purchases
            .iter()
            .filter(|entry| entry.value().property_id == property.id)
            .map(|entry| *entry.key())
            .collect();
        for purchase_id in purchase_ids {
            state.purchases.remove(&purchase_id);
        }
        state.properties.remove(&property.id);
        state.emit(
            "properties.deleted",
            json!({
                "property_id": property.id.to_string(),
                "actor_id": caller.actor,
            }),
        );
        Ok(json!({ "deleted": property.id.to_string() }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

fn change_property_status(
    state: &PropertiesState,
    property: &mut Property,
    to: PropertyStatus,
    actor: &str,
    event: &str,
) {
    property.status = to;
    property.updated_at = Utc::now();
    state.emit(
        event,
        json!({
            "property_id": property.id.to_string(),
            "status": to.as_str(),
            "actor_id": actor,
        }),
    );
}

async fn publish_property(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<PropertiesState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Property> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PROPERTIES_VIEW)?;
        let mut property = load_property(&state, &path.into_inner())?;
        can_edit_property(&ctx, &caller.role, &property, &caller.actor)?;
        if property.status != PropertyStatus::Draft {
            return Err(UnifiedError::conflict(format!(
                "Only a draft property can be published (status is {})",
                property.status.as_str()
            )));
        }
        change_property_status(
            &state,
            &mut property,
            PropertyStatus::Published,
            &caller.actor,
            "properties.published",
        );
        state.properties.insert(property.id, property.clone());
        Ok(property)
    })();
    match result {
        Ok(property) => ok(property),
        Err(error) => err(error, debug),
    }
}

async fn archive_property(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<PropertiesState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Property> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PROPERTIES_VIEW)?;
        let mut property = load_property(&state, &path.into_inner())?;
        can_edit_property(&ctx, &caller.role, &property, &caller.actor)?;
        if property.status == PropertyStatus::Archived {
            return Err(UnifiedError::conflict("Property is already archived"));
        }
        change_property_status(
            &state,
            &mut property,
            PropertyStatus::Archived,
            &caller.actor,
            "properties.archived",
        );
        state.properties.insert(property.id, property.clone());
        Ok(property)
    })();
    match result {
        Ok(property) => ok(property),
        Err(error) => err(error, debug),
    }
}

async fn mark_property_sold_out(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<PropertiesState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Property> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PROPERTIES_VIEW)?;
        let mut property = load_property(&state, &path.into_inner())?;
        can_edit_property(&ctx, &caller.role, &property, &caller.actor)?;
        if property.status != PropertyStatus::Published {
            return Err(UnifiedError::conflict(format!(
                "Only a published property can be marked sold out (status is {})",
                property.status.as_str()
            )));
        }
        change_property_status(
            &state,
            &mut property,
            PropertyStatus::SoldOut,
            &caller.actor,
            "properties.sold_out",
        );
        state.properties.insert(property.id, property.clone());
        Ok(property)
    })();
    match result {
        Ok(property) => ok(property),
        Err(error) => err(error, debug),
    }
}

// ---------------------------------------------------------------------------
// Plan handlers
// ---------------------------------------------------------------------------

async fn list_plans(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<PropertiesState>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PROPERTIES_VIEW)?;
        let property = load_property(&state, &path.into_inner())?;
        let params = query.into_inner();
        let mut items = state.plans_of_property(property.id);
        let sort = params.allowed_sort(&["name", "created_at"], "created_at");
        let descending = params.normalized().order == "desc";
        items.sort_by(|a, b| {
            let ordering = match sort.as_str() {
                "name" => a.name.cmp(&b.name),
                _ => a.created_at.cmp(&b.created_at),
            };
            if descending {
                ordering.reverse()
            } else {
                ordering
            }
        });
        Ok(paginate(items, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn create_plan(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<PropertiesState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<CreatePlanRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PropertyPlan> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PROPERTIES_CREATE)?;
        let property = load_property(&state, &path.into_inner())?;
        if matches!(property.status, PropertyStatus::Archived | PropertyStatus::SoldOut) {
            return Err(UnifiedError::conflict(format!(
                "Plans cannot be added to a {} property",
                property.status.as_str()
            )));
        }
        let input = body.into_inner();
        let name = require_text(&input.name, "name", MAX_NAME_LEN)?;
        let description = input
            .description
            .map(|value| require_text(&value, "description", MAX_DESCRIPTION_LEN))
            .transpose()?;
        let (geometry, geometry_hash) = match input.geometry {
            Some(value) => {
                let validated = validate_geometry(&value)?;
                if validated.kind == "LineString" {
                    return Err(UnifiedError::bad_request(
                        "plan boundary must be a Polygon or MultiPolygon, not a LineString",
                    ));
                }
                (Some(value.clone()), Some(geometry_hash(&value)))
            }
            None => (None, None),
        };
        let now = Utc::now();
        let plan = PropertyPlan {
            id: Uuid::now_v7(),
            property_id: property.id,
            name,
            description,
            geometry,
            geometry_hash,
            created_at: now,
            updated_at: now,
        };
        state.plans.insert(plan.id, plan.clone());
        state.emit(
            "properties.plan.created",
            json!({
                "property_id": property.id.to_string(),
                "plan_id": plan.id.to_string(),
                "actor_id": caller.actor,
            }),
        );
        Ok(plan)
    })();
    match result {
        Ok(plan) => created(plan),
        Err(error) => err(error, debug),
    }
}

async fn get_plan(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<PropertiesState>,
    req: HttpRequest,
    path: web::Path<(String, String)>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PROPERTIES_VIEW)?;
        let (property_id, plan_id) = path.into_inner();
        let property = load_property(&state, &property_id)?;
        let plan = load_plan(&state, property.id, &plan_id)?;
        Ok(plan_view(&state, &plan))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn update_plan(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<PropertiesState>,
    req: HttpRequest,
    path: web::Path<(String, String)>,
    body: web::Json<UpdatePlanRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PropertyPlan> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PROPERTIES_VIEW)?;
        let (property_id, plan_id) = path.into_inner();
        let property = load_property(&state, &property_id)?;
        can_edit_property(&ctx, &caller.role, &property, &caller.actor)?;
        let mut plan = load_plan(&state, property.id, &plan_id)?;
        let input = body.into_inner();
        if let Some(name) = input.name {
            plan.name = require_text(&name, "name", MAX_NAME_LEN)?;
        }
        if let Some(description) = input.description {
            plan.description = description
                .map(|value| require_text(&value, "description", MAX_DESCRIPTION_LEN))
                .transpose()?;
        }
        if let Some(geometry) = input.geometry {
            match geometry {
                Some(value) => {
                    let validated = validate_geometry(&value)?;
                    if validated.kind == "LineString" {
                        return Err(UnifiedError::bad_request(
                            "plan boundary must be a Polygon or MultiPolygon, not a LineString",
                        ));
                    }
                    plan.geometry = Some(value.clone());
                    plan.geometry_hash = Some(geometry_hash(&value));
                }
                None => {
                    plan.geometry = None;
                    plan.geometry_hash = None;
                }
            }
        }
        plan.updated_at = Utc::now();
        state.plans.insert(plan.id, plan.clone());
        state.emit(
            "properties.plan.updated",
            json!({
                "property_id": property.id.to_string(),
                "plan_id": plan.id.to_string(),
                "actor_id": caller.actor,
            }),
        );
        Ok(plan)
    })();
    match result {
        Ok(plan) => ok(plan),
        Err(error) => err(error, debug),
    }
}

async fn delete_plan(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<PropertiesState>,
    req: HttpRequest,
    path: web::Path<(String, String)>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PROPERTIES_MANAGE)?;
        let (property_id, plan_id) = path.into_inner();
        let property = load_property(&state, &property_id)?;
        let plan = load_plan(&state, property.id, &plan_id)?;
        let blocks = state.blocks_of_plan(plan.id);
        if blocks.iter().any(|block| {
            matches!(
                block.status,
                BlockStatus::Reserved | BlockStatus::Sold | BlockStatus::Assigned
            )
        }) {
            return Err(UnifiedError::conflict(
                "Plan cannot be deleted while it has reserved, sold, or assigned blocks",
            ));
        }
        for block in blocks {
            state.blocks.remove(&block.id);
            let history_ids: Vec<Uuid> = state
                .history
                .iter()
                .filter(|entry| entry.value().block_id == block.id)
                .map(|entry| *entry.key())
                .collect();
            for history_id in history_ids {
                state.history.remove(&history_id);
            }
        }
        state.plans.remove(&plan.id);
        state.emit(
            "properties.plan.deleted",
            json!({
                "property_id": property.id.to_string(),
                "plan_id": plan.id.to_string(),
                "actor_id": caller.actor,
            }),
        );
        Ok(json!({ "deleted": plan.id.to_string() }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

// ---------------------------------------------------------------------------
// Block handlers
// ---------------------------------------------------------------------------

async fn list_blocks(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<PropertiesState>,
    req: HttpRequest,
    path: web::Path<(String, String)>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PROPERTIES_VIEW)?;
        let (property_id, plan_id) = path.into_inner();
        let property = load_property(&state, &property_id)?;
        let plan = load_plan(&state, property.id, &plan_id)?;
        let params = query.into_inner();
        let status_filter = params.filters.get("status").cloned();
        let mut items: Vec<PropertyBlock> = state
            .blocks_of_plan(plan.id)
            .into_iter()
            .filter(|block| {
                if let Some(status) = &status_filter {
                    return block.status.as_str() == status.as_str();
                }
                true
            })
            .collect();
        let sort = params.allowed_sort(&["title", "price", "created_at"], "created_at");
        let descending = params.normalized().order == "desc";
        items.sort_by(|a, b| {
            let ordering = match sort.as_str() {
                "title" => a.title.cmp(&b.title),
                "price" => a.price.partial_cmp(&b.price).unwrap_or(std::cmp::Ordering::Equal),
                _ => a.created_at.cmp(&b.created_at),
            };
            if descending {
                ordering.reverse()
            } else {
                ordering
            }
        });
        Ok(paginate(items, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn create_block(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<PropertiesState>,
    req: HttpRequest,
    path: web::Path<(String, String)>,
    body: web::Json<CreateBlockRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PropertyBlock> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PROPERTIES_CREATE)?;
        let (property_id, plan_id) = path.into_inner();
        let property = load_property(&state, &property_id)?;
        let plan = load_plan(&state, property.id, &plan_id)?;
        if matches!(property.status, PropertyStatus::Archived | PropertyStatus::SoldOut) {
            return Err(UnifiedError::conflict(format!(
                "Blocks cannot be added to a {} property",
                property.status.as_str()
            )));
        }
        let input = body.into_inner();
        let title = require_text(&input.title, "title", MAX_TITLE_LEN)?;
        let notes = input
            .notes
            .map(|value| require_text(&value, "notes", MAX_NOTES_LEN))
            .transpose()?;
        let validated = validate_geometry(&input.geometry)?;
        let price = valid_price(input.price.unwrap_or(0.0))?;
        let currency = valid_currency(input.currency.as_deref().unwrap_or("USD"))?;
        let link_allowlist = {
            let settings = state
                .location_settings
                .read()
                .expect("settings lock poisoned");
            settings.link_allowlist_hosts.clone()
        };
        let behavior = input.click_behavior.unwrap_or_default();
        let (click_link_kind, click_link_target) = validate_click_target(
            behavior,
            input.click_link_kind,
            input.click_link_target.as_deref(),
            &link_allowlist,
        )?;
        let now = Utc::now();
        let geometry_hash_value = geometry_hash(&input.geometry);
        let block = PropertyBlock {
            id: Uuid::now_v7(),
            plan_id: plan.id,
            property_id: property.id,
            title,
            notes,
            geometry: input.geometry,
            geometry_hash: geometry_hash_value,
            area_sqm: validated.area_sqm.map(round_money),
            length_m: validated.length_m.map(round_money),
            price,
            currency,
            status: BlockStatus::Available,
            assigned_customer_id: None,
            click_behavior: behavior,
            click_link_kind,
            click_link_target,
            order_id: None,
            created_at: now,
            updated_at: now,
        };
        state.blocks.insert(block.id, block.clone());
        state.emit(
            "properties.block.created",
            json!({
                "property_id": property.id.to_string(),
                "plan_id": plan.id.to_string(),
                "block_id": block.id.to_string(),
                "actor_id": caller.actor,
            }),
        );
        Ok(block)
    })();
    match result {
        Ok(block) => created(block),
        Err(error) => err(error, debug),
    }
}

async fn get_block(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<PropertiesState>,
    req: HttpRequest,
    path: web::Path<(String, String, String)>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PropertyBlock> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PROPERTIES_VIEW)?;
        let (property_id, plan_id, block_id) = path.into_inner();
        let property = load_property(&state, &property_id)?;
        let plan = load_plan(&state, property.id, &plan_id)?;
        load_block(&state, property.id, plan.id, &block_id)
    })();
    match result {
        Ok(block) => ok(block),
        Err(error) => err(error, debug),
    }
}

async fn update_block(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<PropertiesState>,
    req: HttpRequest,
    path: web::Path<(String, String, String)>,
    body: web::Json<UpdateBlockRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PropertyBlock> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PROPERTIES_VIEW)?;
        let (property_id, plan_id, block_id) = path.into_inner();
        let property = load_property(&state, &property_id)?;
        can_edit_property(&ctx, &caller.role, &property, &caller.actor)?;
        let plan = load_plan(&state, property.id, &plan_id)?;
        let mut block = load_block(&state, property.id, plan.id, &block_id)?;
        let input = body.into_inner();
        // Edit rules by status: a sold / assigned block is a sale
        // record — only its notes may change. A reserved block may
        // change title, notes, and click behaviour, but not its
        // geometry or price. An available / blocked block may
        // change everything.
        match block.status {
            BlockStatus::Sold | BlockStatus::Assigned => {
                if input.geometry.is_some() || input.price.is_some() || input.title.is_some()
                    || input.click_behavior.is_some() || input.click_link_kind.is_some()
                    || input.click_link_target.is_some() || input.currency.is_some()
                {
                    return Err(UnifiedError::conflict(format!(
                        "Only the notes of a {} block can be edited",
                        block.status.as_str()
                    )));
                }
            }
            BlockStatus::Reserved => {
                if input.geometry.is_some() || input.price.is_some() || input.currency.is_some() {
                    return Err(UnifiedError::conflict(
                        "Geometry and price of a reserved block cannot be edited",
                    ));
                }
            }
            BlockStatus::Available | BlockStatus::Blocked => {}
        }
        if let Some(title) = input.title {
            block.title = require_text(&title, "title", MAX_TITLE_LEN)?;
        }
        if let Some(notes) = input.notes {
            block.notes = notes
                .map(|value| require_text(&value, "notes", MAX_NOTES_LEN))
                .transpose()?;
        }
        if let Some(geometry) = input.geometry {
            let validated = validate_geometry(&geometry)?;
            block.geometry = geometry;
            block.geometry_hash = geometry_hash(&block.geometry);
            block.area_sqm = validated.area_sqm.map(round_money);
            block.length_m = validated.length_m.map(round_money);
        }
        if let Some(price) = input.price {
            block.price = valid_price(price)?;
        }
        if let Some(currency) = input.currency {
            block.currency = valid_currency(&currency)?;
        }
        if input.click_behavior.is_some() || input.click_link_kind.is_some() || input.click_link_target.is_some() {
            let behavior = input.click_behavior.unwrap_or(block.click_behavior);
            let link_allowlist = {
                let settings = state
                    .location_settings
                    .read()
                    .expect("settings lock poisoned");
                settings.link_allowlist_hosts.clone()
            };
            let (kind, target) = validate_click_target(
                behavior,
                input.click_link_kind.or(block.click_link_kind),
                input
                    .click_link_target
                    .as_deref()
                    .or(block.click_link_target.as_deref()),
                &link_allowlist,
            )?;
            block.click_behavior = behavior;
            block.click_link_kind = kind;
            block.click_link_target = target;
        }
        block.updated_at = Utc::now();
        state.blocks.insert(block.id, block.clone());
        state.emit(
            "properties.block.updated",
            json!({
                "property_id": block.property_id.to_string(),
                "plan_id": block.plan_id.to_string(),
                "block_id": block.id.to_string(),
                "actor_id": caller.actor,
            }),
        );
        Ok(block)
    })();
    match result {
        Ok(block) => ok(block),
        Err(error) => err(error, debug),
    }
}

async fn delete_block(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<PropertiesState>,
    req: HttpRequest,
    path: web::Path<(String, String, String)>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PROPERTIES_MANAGE)?;
        let (property_id, plan_id, block_id) = path.into_inner();
        let property = load_property(&state, &property_id)?;
        let plan = load_plan(&state, property.id, &plan_id)?;
        let block = load_block(&state, property.id, plan.id, &block_id)?;
        if !matches!(block.status, BlockStatus::Available | BlockStatus::Blocked) {
            return Err(UnifiedError::conflict(format!(
                "Only an available or blocked block can be deleted (status is {})",
                block.status.as_str()
            )));
        }
        state.blocks.remove(&block.id);
        let history_ids: Vec<Uuid> = state
            .history
            .iter()
            .filter(|entry| entry.value().block_id == block.id)
            .map(|entry| *entry.key())
            .collect();
        for history_id in history_ids {
            state.history.remove(&history_id);
        }
        state.emit(
            "properties.block.deleted",
            json!({
                "property_id": block.property_id.to_string(),
                "plan_id": block.plan_id.to_string(),
                "block_id": block.id.to_string(),
                "actor_id": caller.actor,
            }),
        );
        Ok(json!({ "deleted": block.id.to_string() }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

/// Resolve the buyer for a block action: the caller by default;
/// acting for another user needs the manage capability.
fn resolve_buyer(
    ctx: &AppContext,
    caller: &Caller,
    requested: Option<String>,
) -> UnifiedResult<String> {
    match requested {
        Some(buyer_id) => {
            let buyer_id = buyer_id.trim().to_string();
            if buyer_id.is_empty() {
                return Err(UnifiedError::bad_request("buyer_id must not be empty"));
            }
            if buyer_id != caller.actor {
                require_permission(ctx, &caller.role, PERM_PROPERTIES_MANAGE)?;
            }
            Ok(buyer_id)
        }
        None => Ok(caller.actor.clone()),
    }
}

async fn reserve_block(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<PropertiesState>,
    req: HttpRequest,
    path: web::Path<(String, String, String)>,
    body: web::Json<BlockActionRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PropertyBlock> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PROPERTIES_VIEW)?;
        let (property_id, plan_id, block_id) = path.into_inner();
        let property = load_property(&state, &property_id)?;
        let plan = load_plan(&state, property.id, &plan_id)?;
        let mut block = load_block(&state, property.id, plan.id, &block_id)?;
        if block.status != BlockStatus::Available {
            return Err(UnifiedError::conflict(format!(
                "Only an available block can be reserved (status is {})",
                block.status.as_str()
            )));
        }
        let input = body.into_inner();
        let buyer = resolve_buyer(&ctx, &caller, input.buyer_id)?;
        let reason = input
            .reason
            .map(|value| require_text(&value, "reason", MAX_REASON_LEN))
            .transpose()?;
        block.assigned_customer_id = Some(buyer.clone());
        move_block_status(
            &state,
            &mut block,
            BlockStatus::Reserved,
            &caller.actor,
            reason,
            None,
            "properties.block.reserved",
        );
        state.blocks.insert(block.id, block.clone());
        state.notify(
            "properties.block.reserved",
            &buyer,
            &format!("Block \"{}\" reserved", block.title),
            &format!(
                "Block \"{}\" is reserved for you. Price: {} {}.",
                block.title,
                format_money(block.price),
                block.currency
            ),
            Some(format!("/api/v1/properties/{}", block.property_id)),
        );
        Ok(block)
    })();
    match result {
        Ok(block) => ok(block),
        Err(error) => err(error, debug),
    }
}

async fn release_block(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<PropertiesState>,
    req: HttpRequest,
    path: web::Path<(String, String, String)>,
    body: web::Json<BlockActionRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PropertyBlock> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PROPERTIES_VIEW)?;
        let (property_id, plan_id, block_id) = path.into_inner();
        let property = load_property(&state, &property_id)?;
        let plan = load_plan(&state, property.id, &plan_id)?;
        let mut block = load_block(&state, property.id, plan.id, &block_id)?;
        if block.status != BlockStatus::Reserved {
            return Err(UnifiedError::conflict(format!(
                "Only a reserved block can be released (status is {})",
                block.status.as_str()
            )));
        }
        // Only the buyer who holds the reservation, or a manager,
        // may release it.
        let holder = block.assigned_customer_id.clone().unwrap_or_default();
        if holder != caller.actor {
            require_permission(&ctx, &caller.role, PERM_PROPERTIES_MANAGE)?;
        }
        let input = body.into_inner();
        let reason = input
            .reason
            .map(|value| require_text(&value, "reason", MAX_REASON_LEN))
            .transpose()?;
        // Cancel any purchase still waiting on this block.
        let pending: Vec<Uuid> = state
            .purchases
            .iter()
            .filter(|entry| {
                entry.value().block_id == block.id
                    && matches!(
                        entry.value().status.as_str(),
                        "requested" | "order_created" | "pending_verification"
                    )
            })
            .map(|entry| *entry.key())
            .collect();
        for purchase_id in pending {
            if let Some(mut purchase) = state.purchases.get_mut(&purchase_id) {
                purchase.status = "cancelled".to_string();
                purchase.updated_at = Utc::now();
            }
        }
        block.assigned_customer_id = None;
        block.order_id = None;
        move_block_status(
            &state,
            &mut block,
            BlockStatus::Available,
            &caller.actor,
            reason,
            None,
            "properties.block.released",
        );
        state.blocks.insert(block.id, block.clone());
        Ok(block)
    })();
    match result {
        Ok(block) => ok(block),
        Err(error) => err(error, debug),
    }
}

async fn purchase_block(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<PropertiesState>,
    req: HttpRequest,
    path: web::Path<(String, String, String)>,
    body: web::Json<BlockActionRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<BlockPurchase> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PROPERTIES_VIEW)?;
        let (property_id, plan_id, block_id) = path.into_inner();
        let property = load_property(&state, &property_id)?;
        if property.status != PropertyStatus::Published {
            return Err(UnifiedError::conflict(format!(
                "Blocks of a {} property cannot be purchased",
                property.status.as_str()
            )));
        }
        let plan = load_plan(&state, property.id, &plan_id)?;
        let mut block = load_block(&state, property.id, plan.id, &block_id)?;
        if block.status != BlockStatus::Available {
            return Err(UnifiedError::conflict(format!(
                "Only an available block can be purchased (status is {})",
                block.status.as_str()
            )));
        }
        let input = body.into_inner();
        let buyer = resolve_buyer(&ctx, &caller, input.buyer_id)?;
        let reason = input
            .reason
            .map(|value| require_text(&value, "reason", MAX_REASON_LEN))
            .transpose()?;
        let now = Utc::now();
        let purchase = BlockPurchase {
            id: Uuid::now_v7(),
            property_id: property.id,
            plan_id: plan.id,
            block_id: block.id,
            buyer_id: buyer.clone(),
            price: block.price,
            currency: block.currency.clone(),
            order_id: None,
            status: "requested".to_string(),
            created_at: now,
            updated_at: now,
        };
        state.purchases.insert(purchase.id, purchase.clone());
        // Hold the block while the store creates the order and the
        // buyer pays: available -> reserved.
        block.assigned_customer_id = Some(buyer.clone());
        move_block_status(
            &state,
            &mut block,
            BlockStatus::Reserved,
            &caller.actor,
            reason,
            None,
            "properties.block.reserved",
        );
        state.blocks.insert(block.id, block.clone());
        state.emit(
            "properties.block.purchase_requested",
            json!({
                "purchase_id": purchase.id.to_string(),
                "property_id": property.id.to_string(),
                "plan_id": plan.id.to_string(),
                "block_id": block.id.to_string(),
                "buyer_id": buyer,
                "price": purchase.price,
                "currency": purchase.currency,
            }),
        );
        state.notify(
            "properties.block.purchase_requested",
            &purchase.buyer_id,
            &format!("Purchase started for block \"{}\"", block.title),
            &format!(
                "Your purchase of block \"{}\" has started. Price: {} {}. Complete the payment to confirm the sale.",
                block.title,
                format_money(block.price),
                block.currency
            ),
            Some(format!("/api/v1/properties/{}", block.property_id)),
        );
        Ok(purchase)
    })();
    match result {
        Ok(purchase) => created(purchase),
        Err(error) => err(error, debug),
    }
}

async fn assign_block(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<PropertiesState>,
    req: HttpRequest,
    path: web::Path<(String, String, String)>,
    body: web::Json<BlockActionRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PropertyBlock> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PROPERTIES_MANAGE)?;
        let (property_id, plan_id, block_id) = path.into_inner();
        let property = load_property(&state, &property_id)?;
        let plan = load_plan(&state, property.id, &plan_id)?;
        let mut block = load_block(&state, property.id, plan.id, &block_id)?;
        if !matches!(block.status, BlockStatus::Available | BlockStatus::Reserved) {
            return Err(UnifiedError::conflict(format!(
                "Only an available or reserved block can be assigned (status is {})",
                block.status.as_str()
            )));
        }
        let input = body.into_inner();
        let buyer = match input.buyer_id {
            Some(buyer_id) => {
                let buyer_id = buyer_id.trim().to_string();
                if buyer_id.is_empty() {
                    return Err(UnifiedError::bad_request("buyer_id must not be empty"));
                }
                buyer_id
            }
            None => {
                return Err(UnifiedError::bad_request(
                    "buyer_id is required to assign a block",
                ));
            }
        };
        let reason = input
            .reason
            .map(|value| require_text(&value, "reason", MAX_REASON_LEN))
            .transpose()?;
        block.assigned_customer_id = Some(buyer.clone());
        move_block_status(
            &state,
            &mut block,
            BlockStatus::Assigned,
            &caller.actor,
            reason,
            None,
            "properties.block.assigned",
        );
        state.blocks.insert(block.id, block.clone());
        state.notify(
            "properties.block.assigned",
            &buyer,
            &format!("Block \"{}\" assigned to you", block.title),
            &format!(
                "Block \"{}\" has been assigned to you. Price: {} {}.",
                block.title,
                format_money(block.price),
                block.currency
            ),
            Some(format!("/api/v1/properties/{}", block.property_id)),
        );
        Ok(block)
    })();
    match result {
        Ok(block) => ok(block),
        Err(error) => err(error, debug),
    }
}

async fn block_block(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<PropertiesState>,
    req: HttpRequest,
    path: web::Path<(String, String, String)>,
    body: web::Json<BlockActionRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PropertyBlock> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PROPERTIES_MANAGE)?;
        let (property_id, plan_id, block_id) = path.into_inner();
        let property = load_property(&state, &property_id)?;
        let plan = load_plan(&state, property.id, &plan_id)?;
        let mut block = load_block(&state, property.id, plan.id, &block_id)?;
        if block.status == BlockStatus::Sold {
            return Err(UnifiedError::conflict("A sold block cannot be blocked"));
        }
        if block.status == BlockStatus::Blocked {
            return Err(UnifiedError::conflict("Block is already blocked"));
        }
        let input = body.into_inner();
        let reason = input
            .reason
            .map(|value| require_text(&value, "reason", MAX_REASON_LEN))
            .transpose()?;
        move_block_status(
            &state,
            &mut block,
            BlockStatus::Blocked,
            &caller.actor,
            reason,
            None,
            "properties.block.blocked",
        );
        state.blocks.insert(block.id, block.clone());
        Ok(block)
    })();
    match result {
        Ok(block) => ok(block),
        Err(error) => err(error, debug),
    }
}

async fn unblock_block(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<PropertiesState>,
    req: HttpRequest,
    path: web::Path<(String, String, String)>,
    body: web::Json<BlockActionRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PropertyBlock> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PROPERTIES_MANAGE)?;
        let (property_id, plan_id, block_id) = path.into_inner();
        let property = load_property(&state, &property_id)?;
        let plan = load_plan(&state, property.id, &plan_id)?;
        let mut block = load_block(&state, property.id, plan.id, &block_id)?;
        if block.status != BlockStatus::Blocked {
            return Err(UnifiedError::conflict(format!(
                "Only a blocked block can be unblocked (status is {})",
                block.status.as_str()
            )));
        }
        let input = body.into_inner();
        let reason = input
            .reason
            .map(|value| require_text(&value, "reason", MAX_REASON_LEN))
            .transpose()?;
        block.assigned_customer_id = None;
        move_block_status(
            &state,
            &mut block,
            BlockStatus::Available,
            &caller.actor,
            reason,
            None,
            "properties.block.unblocked",
        );
        state.blocks.insert(block.id, block.clone());
        Ok(block)
    })();
    match result {
        Ok(block) => ok(block),
        Err(error) => err(error, debug),
    }
}

async fn correct_block_status(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<PropertiesState>,
    req: HttpRequest,
    path: web::Path<(String, String, String)>,
    body: web::Json<CorrectStatusRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PropertyBlock> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PROPERTIES_MANAGE)?;
        let (property_id, plan_id, block_id) = path.into_inner();
        let property = load_property(&state, &property_id)?;
        let plan = load_plan(&state, property.id, &plan_id)?;
        let mut block = load_block(&state, property.id, plan.id, &block_id)?;
        if block.status != BlockStatus::Sold {
            return Err(UnifiedError::conflict(format!(
                "Status correction applies to sold blocks only (status is {})",
                block.status.as_str()
            )));
        }
        let input = body.into_inner();
        let reason = require_text(&input.reason, "reason", MAX_REASON_LEN)?;
        let target = match input.target_status.as_str() {
            "available" => BlockStatus::Available,
            "reserved" => BlockStatus::Reserved,
            "assigned" => BlockStatus::Assigned,
            other => {
                return Err(UnifiedError::bad_request(format!(
                    "target_status \"{other}\" is not a valid correction target (available, reserved, assigned)"
                )));
            }
        };
        match target {
            BlockStatus::Reserved | BlockStatus::Assigned => {
                let buyer = match input.buyer_id {
                    Some(buyer_id) => {
                        let buyer_id = buyer_id.trim().to_string();
                        if buyer_id.is_empty() {
                            return Err(UnifiedError::bad_request(
                                "buyer_id must not be empty",
                            ));
                        }
                        buyer_id
                    }
                    None => {
                        return Err(UnifiedError::bad_request(
                            "buyer_id is required when correcting to reserved or assigned",
                        ));
                    }
                };
                block.assigned_customer_id = Some(buyer);
            }
            BlockStatus::Available => {
                block.assigned_customer_id = None;
                block.order_id = None;
            }
            _ => {}
        }
        move_block_status(
            &state,
            &mut block,
            target,
            &caller.actor,
            Some(format!("admin correction: {reason}")),
            None,
            "properties.block.status_corrected",
        );
        state.blocks.insert(block.id, block.clone());
        Ok(block)
    })();
    match result {
        Ok(block) => ok(block),
        Err(error) => err(error, debug),
    }
}

async fn list_block_history(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<PropertiesState>,
    req: HttpRequest,
    path: web::Path<(String, String, String)>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PROPERTIES_VIEW)?;
        let (property_id, plan_id, block_id) = path.into_inner();
        let property = load_property(&state, &property_id)?;
        let plan = load_plan(&state, property.id, &plan_id)?;
        let block = load_block(&state, property.id, plan.id, &block_id)?;
        let params = query.into_inner();
        let mut items: Vec<PropertyBlockHistory> = state
            .history
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|entry| entry.block_id == block.id)
            .collect();
        items.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        Ok(paginate(items, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn export_blocks(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<PropertiesState>,
    req: HttpRequest,
    path: web::Path<(String, String)>,
    query: web::Query<ExportQuery>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let caller_result = caller(&req);
    let caller = match caller_result {
        Ok(caller) => caller,
        Err(error) => return err(error, debug),
    };
    if let Err(error) = require_permission(&ctx, &caller.role, PERM_PROPERTIES_VIEW) {
        return err(error, debug);
    }
    let (property_id, plan_id) = path.into_inner();
    let load = (|| {
        let property = load_property(&state, &property_id)?;
        let plan = load_plan(&state, property.id, &plan_id)?;
        Ok::<(Property, PropertyPlan), UnifiedError>((property, plan))
    })();
    let (property, plan) = match load {
        Ok(pair) => pair,
        Err(error) => return err(error, debug),
    };
    let blocks = state.blocks_of_plan(plan.id);
    match query.format.as_str() {
        "csv" => HttpResponse::Ok()
            .content_type("text/csv; charset=utf-8")
            .insert_header((
                "Content-Disposition",
                // The filename uses the plan id, never the plan
                // name, so user-supplied text can never reach a
                // header value.
                format!("attachment; filename=\"plan-{}-blocks.csv\"", plan.id),
            ))
            .body(blocks_to_csv(&blocks)),
        "json" => ok(json!({
            "property_id": property.id.to_string(),
            "plan_id": plan.id.to_string(),
            "blocks": blocks,
        })),
        other => err(
            UnifiedError::bad_request(format!(
                "Unknown export format \"{other}\": expected csv or json"
            )),
            debug,
        ),
    }
}

// ---------------------------------------------------------------------------
// SVG / print handlers (authenticated)
// ---------------------------------------------------------------------------

async fn get_plan_svg(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<PropertiesState>,
    req: HttpRequest,
    path: web::Path<(String, String)>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let caller_result = caller(&req);
    let caller = match caller_result {
        Ok(caller) => caller,
        Err(error) => return err(error, debug),
    };
    if let Err(error) = require_permission(&ctx, &caller.role, PERM_PROPERTIES_VIEW) {
        return err(error, debug);
    }
    let (property_id, plan_id) = path.into_inner();
    let loaded = (|| {
        let property = load_property(&state, &property_id)?;
        let plan = load_plan(&state, property.id, &plan_id)?;
        Ok::<(Property, PropertyPlan), UnifiedError>((property, plan))
    })();
    let (property, plan) = match loaded {
        Ok(pair) => pair,
        Err(error) => return err(error, debug),
    };
    let blocks = state.blocks_of_plan(plan.id);
    match render_plan_svg(&plan, &blocks, &format!("{} — {}", property.title, plan.name)) {
        Ok(svg) => HttpResponse::Ok()
            .content_type("image/svg+xml; charset=utf-8")
            .body(svg),
        Err(error) => err(error, debug),
    }
}

async fn get_plan_print(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<PropertiesState>,
    req: HttpRequest,
    path: web::Path<(String, String)>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let caller_result = caller(&req);
    let caller = match caller_result {
        Ok(caller) => caller,
        Err(error) => return err(error, debug),
    };
    if let Err(error) = require_permission(&ctx, &caller.role, PERM_PROPERTIES_VIEW) {
        return err(error, debug);
    }
    let (property_id, plan_id) = path.into_inner();
    let loaded = (|| {
        let property = load_property(&state, &property_id)?;
        let plan = load_plan(&state, property.id, &plan_id)?;
        Ok::<(Property, PropertyPlan), UnifiedError>((property, plan))
    })();
    let (property, plan) = match loaded {
        Ok(pair) => pair,
        Err(error) => return err(error, debug),
    };
    let blocks = state.blocks_of_plan(plan.id);
    match render_plan_svg(&plan, &blocks, &format!("{} — {}", property.title, plan.name)) {
        Ok(svg) => {
            let html = render_print_html(&property, &plan, &blocks, &svg, true);
            HttpResponse::Ok()
                .content_type("text/html; charset=utf-8")
                .body(html)
        }
        Err(error) => err(error, debug),
    }
}

// ---------------------------------------------------------------------------
// Location handlers
// ---------------------------------------------------------------------------

async fn search_location(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<PropertiesState>,
    req: HttpRequest,
    query: web::Query<LocationSearchQuery>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let caller_result = caller(&req);
    let caller = match caller_result {
        Ok(caller) => caller,
        Err(error) => return err(error, debug),
    };
    if let Err(error) = require_permission(&ctx, &caller.role, PERM_PROPERTIES_VIEW) {
        return err(error, debug);
    }
    let query_text = query.q.trim().to_string();
    if query_text.is_empty() {
        return err(
            UnifiedError::bad_request("q must not be empty"),
            debug,
        );
    }
    if query_text.len() > MAX_SEARCH_QUERY_LEN {
        return err(
            UnifiedError::bad_request(format!(
                "q must be at most {MAX_SEARCH_QUERY_LEN} characters"
            )),
            debug,
        );
    }
    let provider = state.provider();
    let provider_name = provider.name().to_string();
    match provider.search(&query_text).await {
        Ok(results) => ok(json!({
            "provider": provider_name,
            "query": query_text,
            "results": results,
        })),
        Err(error) => err(error, debug),
    }
}

async fn reverse_location(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<PropertiesState>,
    req: HttpRequest,
    query: web::Query<LocationReverseQuery>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let caller_result = caller(&req);
    let caller = match caller_result {
        Ok(caller) => caller,
        Err(error) => return err(error, debug),
    };
    if let Err(error) = require_permission(&ctx, &caller.role, PERM_PROPERTIES_VIEW) {
        return err(error, debug);
    }
    let latitude = match valid_latitude(query.lat) {
        Ok(latitude) => latitude,
        Err(error) => return err(error, debug),
    };
    let longitude = match valid_longitude(query.lng) {
        Ok(longitude) => longitude,
        Err(error) => return err(error, debug),
    };
    let provider = state.provider();
    let provider_name = provider.name().to_string();
    match provider.reverse_geocode(latitude, longitude).await {
        Ok(result) => ok(json!({
            "provider": provider_name,
            "latitude": latitude,
            "longitude": longitude,
            "result": result,
        })),
        Err(error) => err(error, debug),
    }
}

async fn get_location_settings(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<PropertiesState>,
    req: HttpRequest,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<LocationSettings> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PROPERTIES_MANAGE)?;
        let settings = state
            .location_settings
            .read()
            .expect("settings lock poisoned")
            .clone();
        Ok(settings)
    })();
    match result {
        Ok(settings) => ok(settings),
        Err(error) => err(error, debug),
    }
}

async fn update_location_settings(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<PropertiesState>,
    req: HttpRequest,
    body: web::Json<UpdateLocationSettingsRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<LocationSettings> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PROPERTIES_MANAGE)?;
        let input = body.into_inner();
        let mut settings = state
            .location_settings
            .read()
            .expect("settings lock poisoned")
            .clone();
        if let Some(provider) = input.provider {
            let provider = provider.trim().to_lowercase();
            if provider != "none" && provider != "nominatim" {
                return Err(UnifiedError::bad_request(
                    "provider must be \"none\" or \"nominatim\"",
                ));
            }
            settings.provider = provider;
        }
        if let Some(base_url) = input.base_url {
            settings.base_url = base_url
                .map(|value| value.trim().to_string())
                .filter(|value| !value.is_empty());
        }
        if let Some(allowed_host) = input.allowed_host {
            let allowed_host = allowed_host.trim().to_lowercase();
            if !valid_hostname(&allowed_host) {
                return Err(UnifiedError::bad_request(format!(
                    "allowed_host \"{allowed_host}\" is not a valid hostname"
                )));
            }
            settings.allowed_host = allowed_host;
        }
        if let Some(timeout_secs) = input.timeout_secs {
            if timeout_secs == 0 || timeout_secs > MAX_GEOCODER_TIMEOUT_SECS {
                return Err(UnifiedError::bad_request(format!(
                    "timeout_secs must be between 1 and {MAX_GEOCODER_TIMEOUT_SECS}"
                )));
            }
            settings.timeout_secs = timeout_secs;
        }
        if let Some(hosts) = input.link_allowlist_hosts {
            for host in &hosts {
                if !valid_hostname(host.trim()) {
                    return Err(UnifiedError::bad_request(format!(
                        "link_allowlist_hosts entry \"{host}\" is not a valid hostname"
                    )));
                }
            }
            settings.link_allowlist_hosts = hosts
                .into_iter()
                .map(|host| host.trim().to_lowercase())
                .collect();
        }
        // Build the provider BEFORE storing the settings, so an
        // invalid configuration is rejected instead of saved.
        let provider = build_provider(&settings)?;
        *state
            .location_settings
            .write()
            .expect("settings lock poisoned") = settings.clone();
        *state
            .location_provider
            .write()
            .expect("provider lock poisoned") = provider;
        state.emit(
            "properties.location.settings_updated",
            json!({
                "provider": settings.provider,
                "actor_id": caller.actor,
            }),
        );
        Ok(settings)
    })();
    match result {
        Ok(settings) => ok(settings),
        Err(error) => err(error, debug),
    }
}

// ---------------------------------------------------------------------------
// Public handlers (no auth; gated by the API Manager ApiAccessRule)
// ---------------------------------------------------------------------------

/// Public list view: the property plus a plan count. Never
/// exposes buyer or order data (blocks are not listed here).
fn public_list_view(state: &PropertiesState, property: &Property) -> Value {
    let mut value = serde_json::to_value(property).unwrap_or(Value::Null);
    if let Value::Object(map) = &mut value {
        map.insert(
            "plan_count".to_string(),
            Value::Number(state.plans_of_property(property.id).len().into()),
        );
    }
    value
}

async fn list_public_properties(
    state: web::Data<PropertiesState>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let params = query.into_inner();
    let search = params.search.clone().unwrap_or_default().to_lowercase();
    let mut items: Vec<Value> = state
        .properties
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|property| property.status == PropertyStatus::Published)
        .filter(|property| {
            if search.is_empty() {
                return true;
            }
            let haystack = format!(
                "{} {} {} {}",
                property.title,
                property.description.clone().unwrap_or_default(),
                property.slug,
                property.address.clone().unwrap_or_default()
            )
            .to_lowercase();
            haystack.contains(&search)
        })
        .map(|property| public_list_view(&state, &property))
        .collect();
    let sort = params.allowed_sort(&["title", "created_at"], "created_at");
    let descending = params.normalized().order == "desc";
    items.sort_by(|a, b| {
        let ordering = match sort.as_str() {
            "title" => a
                .get("title")
                .and_then(Value::as_str)
                .cmp(&b.get("title").and_then(Value::as_str)),
            _ => a
                .get("created_at")
                .and_then(Value::as_str)
                .cmp(&b.get("created_at").and_then(Value::as_str)),
        };
        if descending {
            ordering.reverse()
        } else {
            ordering
        }
    });
    ok(paginate(items, &params).to_json())
}

async fn get_public_property(
    state: web::Data<PropertiesState>,
    path: web::Path<String>,
) -> HttpResponse {
    let slug = path.into_inner();
    match load_property_by_slug(&state, &slug) {
        Ok(property) => {
            if property.status != PropertyStatus::Published {
                return err(
                    UnifiedError::not_found(format!("Property not found: {slug}")),
                    false,
                );
            }
            ok(public_property_view(&state, &property))
        }
        Err(error) => err(error, false),
    }
}

async fn get_public_plan_svg(
    state: web::Data<PropertiesState>,
    path: web::Path<(String, String)>,
) -> HttpResponse {
    let (slug, plan_id) = path.into_inner();
    let loaded = (|| {
        let property = load_property_by_slug(&state, &slug)?;
        if property.status != PropertyStatus::Published {
            return Err(UnifiedError::not_found(format!(
                "Property not found: {slug}"
            )));
        }
        let plan = load_plan(&state, property.id, &plan_id)?;
        Ok::<(Property, PropertyPlan), UnifiedError>((property, plan))
    })();
    let (property, plan) = match loaded {
        Ok(pair) => pair,
        Err(error) => return err(error, false),
    };
    let blocks = state.blocks_of_plan(plan.id);
    match render_plan_svg(&plan, &blocks, &format!("{} — {}", property.title, plan.name)) {
        Ok(svg) => HttpResponse::Ok()
            .content_type("image/svg+xml; charset=utf-8")
            .body(svg),
        Err(error) => err(error, false),
    }
}

async fn get_public_plan_print(
    state: web::Data<PropertiesState>,
    path: web::Path<(String, String)>,
) -> HttpResponse {
    let (slug, plan_id) = path.into_inner();
    let loaded = (|| {
        let property = load_property_by_slug(&state, &slug)?;
        if property.status != PropertyStatus::Published {
            return Err(UnifiedError::not_found(format!(
                "Property not found: {slug}"
            )));
        }
        let plan = load_plan(&state, property.id, &plan_id)?;
        Ok::<(Property, PropertyPlan), UnifiedError>((property, plan))
    })();
    let (property, plan) = match loaded {
        Ok(pair) => pair,
        Err(error) => return err(error, false),
    };
    let blocks = state.blocks_of_plan(plan.id);
    match render_plan_svg(&plan, &blocks, &format!("{} — {}", property.title, plan.name)) {
        Ok(svg) => {
            let html = render_print_html(&property, &plan, &blocks, &svg, false);
            HttpResponse::Ok()
                .content_type("text/html; charset=utf-8")
                .body(html)
        }
        Err(error) => err(error, false),
    }
}

// ---------------------------------------------------------------------------
// Module
// ---------------------------------------------------------------------------

pub struct PropertiesModule {
    state: PropertiesState,
}

impl PropertiesModule {
    pub fn new() -> Self {
        Self {
            state: PropertiesState::default(),
        }
    }
}

impl Default for PropertiesModule {
    fn default() -> Self {
        Self::new()
    }
}

impl Module for PropertiesModule {
    fn id(&self) -> &'static str {
        "properties"
    }

    fn name(&self) -> &'static str {
        "Property Manager"
    }

    fn description(&self) -> &'static str {
        "Land parcels and housing developments with 2D site plans (master plans), blocks / plots for sale with a status machine and audit history, location search through a pluggable geocoder provider, server-generated SVG plans and print pages, and a buy flow that hands checkout to the store module and marks blocks sold on payment.captured."
    }

    fn requires(&self) -> &[&'static str] {
        &["users", "roles", "store", "billing", "notifications", "company", "crm", "storage"]
    }

    fn dependencies(&self) -> Vec<&'static str> {
        vec!["users", "roles", "store", "billing", "notifications", "company", "crm", "storage"]
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
            PROPERTIES_ORDER_CREATED,
            "properties",
            Arc::new(move |ctx: &mut HookContext| {
                on_properties_order_created(Arc::clone(&state), ctx)
            }),
        );
        let state = Arc::new(self.state.clone());
        bus.register(
            PAYMENT_CAPTURED,
            "properties",
            Arc::new(move |ctx: &mut HookContext| {
                on_payment_captured(Arc::clone(&state), ctx)
            }),
        );
        let state = Arc::new(self.state.clone());
        bus.register(
            BANK_TRANSFER_VERIFICATION_CREATED,
            "properties",
            Arc::new(move |ctx: &mut HookContext| {
                on_bank_transfer_verification_created(Arc::clone(&state), ctx)
            }),
        );
        let state = Arc::new(self.state.clone());
        bus.register(
            BANK_TRANSFER_REJECTED,
            "properties",
            Arc::new(move |ctx: &mut HookContext| {
                on_bank_transfer_rejected(Arc::clone(&state), ctx)
            }),
        );
    }

    fn configure_routes(&self, cfg: &mut web::ServiceConfig) {
        let state = web::Data::new(self.state.clone());
        cfg.service(
            web::scope("/api/v1/properties")
                .app_data(state)
                .route("", web::get().to(list_properties))
                .route("", web::post().to(create_property))
                .route("/public", web::get().to(list_public_properties))
                .route("/public/{slug}", web::get().to(get_public_property))
                .route("/public/{slug}/plans/{plan_id}/svg", web::get().to(get_public_plan_svg))
                .route("/public/{slug}/plans/{plan_id}/print", web::get().to(get_public_plan_print))
                .route("/location/search", web::get().to(search_location))
                .route("/location/reverse", web::get().to(reverse_location))
                .route("/location/settings", web::get().to(get_location_settings))
                .route("/location/settings", web::patch().to(update_location_settings))
                .route("/{id}", web::get().to(get_property))
                .route("/{id}", web::patch().to(update_property))
                .route("/{id}", web::delete().to(delete_property))
                .route("/{id}/publish", web::post().to(publish_property))
                .route("/{id}/archive", web::post().to(archive_property))
                .route("/{id}/mark-sold-out", web::post().to(mark_property_sold_out))
                .route("/{id}/plans", web::get().to(list_plans))
                .route("/{id}/plans", web::post().to(create_plan))
                .route("/{id}/plans/{plan_id}", web::get().to(get_plan))
                .route("/{id}/plans/{plan_id}", web::patch().to(update_plan))
                .route("/{id}/plans/{plan_id}", web::delete().to(delete_plan))
                .route("/{id}/plans/{plan_id}/svg", web::get().to(get_plan_svg))
                .route("/{id}/plans/{plan_id}/print", web::get().to(get_plan_print))
                .route("/{id}/plans/{plan_id}/blocks", web::get().to(list_blocks))
                .route("/{id}/plans/{plan_id}/blocks", web::post().to(create_block))
                .route("/{id}/plans/{plan_id}/blocks/export", web::get().to(export_blocks))
                .route(
                    "/{id}/plans/{plan_id}/blocks/{block_id}",
                    web::get().to(get_block),
                )
                .route(
                    "/{id}/plans/{plan_id}/blocks/{block_id}",
                    web::patch().to(update_block),
                )
                .route(
                    "/{id}/plans/{plan_id}/blocks/{block_id}",
                    web::delete().to(delete_block),
                )
                .route(
                    "/{id}/plans/{plan_id}/blocks/{block_id}/reserve",
                    web::post().to(reserve_block),
                )
                .route(
                    "/{id}/plans/{plan_id}/blocks/{block_id}/release",
                    web::post().to(release_block),
                )
                .route(
                    "/{id}/plans/{plan_id}/blocks/{block_id}/purchase",
                    web::post().to(purchase_block),
                )
                .route(
                    "/{id}/plans/{plan_id}/blocks/{block_id}/assign",
                    web::post().to(assign_block),
                )
                .route(
                    "/{id}/plans/{plan_id}/blocks/{block_id}/block",
                    web::post().to(block_block),
                )
                .route(
                    "/{id}/plans/{plan_id}/blocks/{block_id}/unblock",
                    web::post().to(unblock_block),
                )
                .route(
                    "/{id}/plans/{plan_id}/blocks/{block_id}/correct-status",
                    web::post().to(correct_block_status),
                )
                .route(
                    "/{id}/plans/{plan_id}/blocks/{block_id}/history",
                    web::get().to(list_block_history),
                ),
        );
    }

    fn route_info(&self) -> Vec<RouteInfo> {
        vec![
            RouteInfo { method: "GET", path: "/api/v1/properties", owner: "properties", description: "List properties (paginated, filter by status, company_id, search)." },
            RouteInfo { method: "POST", path: "/api/v1/properties", owner: "properties", description: "Create a property (draft)." },
            RouteInfo { method: "GET", path: "/api/v1/properties/{id}", owner: "properties", description: "Get one property." },
            RouteInfo { method: "PATCH", path: "/api/v1/properties/{id}", owner: "properties", description: "Update a draft or published property (creator or manage capability)." },
            RouteInfo { method: "DELETE", path: "/api/v1/properties/{id}", owner: "properties", description: "Delete a draft or archived property (cascades to plans, blocks, history, purchases)." },
            RouteInfo { method: "POST", path: "/api/v1/properties/{id}/publish", owner: "properties", description: "Publish a draft property." },
            RouteInfo { method: "POST", path: "/api/v1/properties/{id}/archive", owner: "properties", description: "Archive a property (terminal)." },
            RouteInfo { method: "POST", path: "/api/v1/properties/{id}/mark-sold-out", owner: "properties", description: "Mark a published property sold out." },
            RouteInfo { method: "GET", path: "/api/v1/properties/{id}/plans", owner: "properties", description: "List plans of a property (paginated)." },
            RouteInfo { method: "POST", path: "/api/v1/properties/{id}/plans", owner: "properties", description: "Create a site plan (boundary must be Polygon / MultiPolygon)." },
            RouteInfo { method: "GET", path: "/api/v1/properties/{id}/plans/{plan_id}", owner: "properties", description: "Get one plan with its blocks." },
            RouteInfo { method: "PATCH", path: "/api/v1/properties/{id}/plans/{plan_id}", owner: "properties", description: "Update a plan (name, description, boundary geometry)." },
            RouteInfo { method: "DELETE", path: "/api/v1/properties/{id}/plans/{plan_id}", owner: "properties", description: "Delete a plan with no reserved, sold, or assigned blocks." },
            RouteInfo { method: "GET", path: "/api/v1/properties/{id}/plans/{plan_id}/svg", owner: "properties", description: "Render a plan as SVG (status colours, labels, fitted viewBox)." },
            RouteInfo { method: "GET", path: "/api/v1/properties/{id}/plans/{plan_id}/print", owner: "properties", description: "Print-friendly HTML page for a plan (inline SVG + block table)." },
            RouteInfo { method: "GET", path: "/api/v1/properties/{id}/plans/{plan_id}/blocks", owner: "properties", description: "List blocks of a plan (paginated, filter by status)." },
            RouteInfo { method: "POST", path: "/api/v1/properties/{id}/plans/{plan_id}/blocks", owner: "properties", description: "Create a block (GeoJSON validated; area / length computed server-side)." },
            RouteInfo { method: "GET", path: "/api/v1/properties/{id}/plans/{plan_id}/blocks/{block_id}", owner: "properties", description: "Get one block." },
            RouteInfo { method: "PATCH", path: "/api/v1/properties/{id}/plans/{plan_id}/blocks/{block_id}", owner: "properties", description: "Update a block (edit rules depend on status; sold / assigned blocks only allow note edits)." },
            RouteInfo { method: "DELETE", path: "/api/v1/properties/{id}/plans/{plan_id}/blocks/{block_id}", owner: "properties", description: "Delete an available or blocked block." },
            RouteInfo { method: "POST", path: "/api/v1/properties/{id}/plans/{plan_id}/blocks/{block_id}/reserve", owner: "properties", description: "Reserve an available block for a buyer (self or, with manage, another user)." },
            RouteInfo { method: "POST", path: "/api/v1/properties/{id}/plans/{plan_id}/blocks/{block_id}/release", owner: "properties", description: "Release a reserved block back to available (holder or manage; cancels pending purchases)." },
            RouteInfo { method: "POST", path: "/api/v1/properties/{id}/plans/{plan_id}/blocks/{block_id}/purchase", owner: "properties", description: "Start a block purchase: reserves the block and emits properties.block.purchase_requested for the store module." },
            RouteInfo { method: "POST", path: "/api/v1/properties/{id}/plans/{plan_id}/blocks/{block_id}/assign", owner: "properties", description: "Assign an available or reserved block to a buyer (manage only)." },
            RouteInfo { method: "POST", path: "/api/v1/properties/{id}/plans/{plan_id}/blocks/{block_id}/block", owner: "properties", description: "Block a block (admin-only; not for sold blocks)." },
            RouteInfo { method: "POST", path: "/api/v1/properties/{id}/plans/{plan_id}/blocks/{block_id}/unblock", owner: "properties", description: "Unblock a blocked block back to available (manage only)." },
            RouteInfo { method: "POST", path: "/api/v1/properties/{id}/plans/{plan_id}/blocks/{block_id}/correct-status", owner: "properties", description: "Audited admin correction of a sold block (target available / reserved / assigned, reason required)." },
            RouteInfo { method: "GET", path: "/api/v1/properties/{id}/plans/{plan_id}/blocks/{block_id}/history", owner: "properties", description: "List the audit history of a block (paginated, newest first)." },
            RouteInfo { method: "GET", path: "/api/v1/properties/{id}/plans/{plan_id}/blocks/export", owner: "properties", description: "Export a plan's blocks as CSV or JSON (?format=csv|json)." },
            RouteInfo { method: "GET", path: "/api/v1/properties/location/search", owner: "properties", description: "Search locations through the configured geocoder provider (none provider answers with a not-configured error)." },
            RouteInfo { method: "GET", path: "/api/v1/properties/location/reverse", owner: "properties", description: "Reverse-geocode coordinates through the configured geocoder provider." },
            RouteInfo { method: "GET", path: "/api/v1/properties/location/settings", owner: "properties", description: "Get the geocoder provider settings (manage only)." },
            RouteInfo { method: "PATCH", path: "/api/v1/properties/location/settings", owner: "properties", description: "Update the geocoder provider settings (manage only; provider is rebuilt and validated before saving)." },
            RouteInfo { method: "GET", path: "/api/v1/properties/public", owner: "properties", description: "PUBLIC: list published properties (no auth; gated by the API Manager ApiAccessRule for this path)." },
            RouteInfo { method: "GET", path: "/api/v1/properties/public/{slug}", owner: "properties", description: "PUBLIC: one published property with plans and blocks, buyer and order ids stripped (no auth; gated by the API Manager ApiAccessRule)." },
            RouteInfo { method: "GET", path: "/api/v1/properties/public/{slug}/plans/{plan_id}/svg", owner: "properties", description: "PUBLIC: one published property's plan as SVG (no auth; gated by the API Manager ApiAccessRule)." },
            RouteInfo { method: "GET", path: "/api/v1/properties/public/{slug}/plans/{plan_id}/print", owner: "properties", description: "PUBLIC: print page for one published property's plan, without the buyer column (no auth; gated by the API Manager ApiAccessRule)." },
        ]
    }

    fn route_version_support(&self) -> RouteVersionSupport {
        RouteVersionSupport::All
    }
}

pub fn register(registry: &ModuleRegistry) {
    registry.register(Arc::new(PropertiesModule::new()));
}
