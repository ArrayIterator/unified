// Asset & Inventory module — hardware / software inventory,
// assignment to users, warranty and licence expiry alerts,
// depreciation through the Calculator module, consumable stock
// levels, audited stock adjustments, and stock-take audits.
//
// Routes live under `/api/v1/assets`:
//
//   Assets:
//     GET/POST / — list assets / create an asset.
//     GET/PATCH/DELETE /{id}.
//     POST /{id}/assign — assign to a user (append-only history).
//     POST /{id}/return — return an assigned asset.
//     POST /{id}/retire — retire an asset.
//     POST /{id}/dispose — dispose of an asset.
//     POST /{id}/send-for-repair, /{id}/return-from-repair.
//     GET  /{id}/assignments — assignment history (never overwritten).
//     GET  /{id}/depreciation — depreciation schedule / status.
//     POST /{id}/depreciation/compute — ask the Calculator module
//          for a depreciation schedule (see the honesty note).
//   Warranty / licence expiry:
//     GET  /warranty/expiring?days=30 — assets whose warranty or
//          software licence ends within the window.
//     POST /warranty/check — run the expiry check now (alerts via
//          `notification.requested`, job via `scheduler.enqueue`).
//   Consumable stock:
//     GET/POST /stock — list stock items / create a stock item.
//     GET/PATCH /stock/{stock_id}.
//     POST /stock/{stock_id}/adjust — audited quantity change
//          (reason required; low-stock alert on crossing).
//     GET  /stock/{stock_id}/adjustments — adjustment history.
//   Stock takes:
//     GET/POST /stock-takes — list / start a stock-take.
//     GET  /stock-takes/{id}.
//     POST /stock-takes/{id}/lines — add a count line.
//     PATCH /stock-takes/{id}/lines/{line_id} — record the count.
//     POST /stock-takes/{id}/close — close with a discrepancy report.
//
// HONESTY — READ THIS FIRST. What is real in this module:
// in-memory asset, assignment, stock, adjustment, stock-take, and
// stock-take-line management; append-only assignment history (a
// return closes the active assignment row, it never deletes or
// overwrites it); audited stock adjustments (every change keeps its
// reason, the actor, and the quantity after the change); stock-take
// counting with a discrepancy report on close; warranty and licence
// expiry windows computed from stored dates with alerts emitted as
// `notification.requested` to the asset owner and, when assigned,
// the current assignee. What is NOT built here: the depreciation
// math itself — this module never computes a depreciation figure.
// It emits `calculator.depreciation_requested` and stores the
// schedule the Calculator module returns; when the Calculator
// module is not enabled, or no handler answers, the asset records
// `depreciation_status: "pending"` with the reason instead of a
// guessed schedule (see `request_depreciation`). Actual email /
// push delivery belongs to the notifications module (this module
// only emits `notification.requested`); no payment, procurement,
// or vendor order is created here — `vendor` is a plain text
// reference to the vendor record kept by the procurement side,
// and no procurement module is consulted. Warranty alerts are
// checked on demand (`POST /warranty/check`) and through the
// `assets.warranty.check_requested` hook the scheduler fires;
// `assets.warranty.check` is NOT in the scheduler's `KNOWN_KINDS`
// list yet, so today the scheduler answers
// `{"queued": false, "error": "Unknown job kind: ..."}` and the
// check records `scheduler_status: "declined"` instead of
// pretending the job was queued. The follow-up is to add
// `"assets.warranty.check"` to `KNOWN_KINDS` in `scheduler.rs`
// and arm the dispatch so the job emits
// `assets.warranty.check_requested`.
//
// There are no public (unauthenticated) routes in this module:
// every route requires the `X-User-Id` / `X-Role` headers and a
// capability check. Stock data is internal and is never exposed
// through the API Manager public-access rules.
//
// Actor identity: until the auth module provides an extractor, the
// caller is identified by the `X-User-Id` header (required) and
// `X-Role` carries the role slug the capability check runs
// against, exactly like the ai module. Capability checks use the
// `role.check` hook and fail closed (no answer means not allowed).
//
// Events emitted on the hook bus:
//   - `assets.created`, `assets.updated`, `assets.deleted`,
//     `assets.assigned`, `assets.returned`, `assets.retired`,
//     `assets.disposed`, `assets.repair.started`,
//     `assets.repair.completed`
//   - `assets.depreciation.computed`,
//     `assets.depreciation.pending`
//   - `assets.warranty.expiring`, `assets.license.expiring`
//   - `assets.stock.created`, `assets.stock.updated`,
//     `assets.stock.adjusted`, `assets.stock.low`
//   - `assets.stock_take.started`, `assets.stock_take.closed`
//   - `notification.requested` (warranty / licence expiry alerts
//     and low-stock alerts) with the notifications module's
//     payload shape: `{"kind", "user_id", "title", "body",
//     "module": "assets", "link", "priority", "channels"}`.
//   - `scheduler.enqueue` with `{"kind": "assets.warranty.check",
//     "payload": {"checked_at", ...}}` when a warranty check runs.
//   - `calculator.depreciation_requested` with `{"asset_id",
//     "method", "purchase_price", "salvage_value",
//     "useful_life_years", "purchase_date", "currency"}`; the
//     Calculator module answers with a result carrying a
//     `"schedule"` array (one entry per period with at least
//     `period`, `depreciation`, and `book_value`) or an `"error"`.
// Events this module listens to:
//   - `assets.warranty.check_requested` — the event the scheduler
//     emits when an `assets.warranty.check` job fires, following
//     its naming rule (`backup.run` -> `backup.run_requested`);
//     runs the expiry check and emits `notification.requested`
//     for every asset inside the alert windows.
//   - `manufacturing.stock.requested` — answered with per-product
//     levels (`{"stock": {product_id: {available, reserved,
//     unit}}}`) for the products this module holds a stock row
//     for. Products with no stock row are omitted, never
//     invented: the requester reports them as `unknown`. When
//     no requested product has a row, no answer is given at all,
//     so the requester keeps its `unknown` / `pending_confirmation`
//     behaviour instead of treating an empty answer as a level.
//     `reserved` is always 0 (this module holds no reservations)
//     and `unit` is null (stock rows carry no unit).
//   - `assets.stock.decrement_requested` /
//     `assets.stock.increment_requested` — POS sales and refunds
//     and manufacturing consumption move stock by a positive
//     `quantity`; the change is recorded as an audited adjustment
//     (same shape as the HTTP adjust) and answered with
//     `{"applied", "stock_id", "quantity_after"}`. A decrement
//     that would take stock below zero is refused (`applied:
//     false`, reason `insufficient_stock`) — stock is never
//     driven negative by a hook.
//   - `assets.stock.adjust_requested` — field-service part
//     consumption moves stock by a signed `delta`; same audited
//     recording and answer shape as above. A request naming a
//     stock row that does not exist is answered `applied: false`
//     with reason `stock_not_found`; no stock row is ever created
//     from a hook — creation stays an admin HTTP action.

use crate::core::context::AppContext;
use crate::core::error::{UnifiedError, UnifiedResult};
use crate::core::hooks::{HookBus, HookContext};
use crate::core::json;
use crate::core::modules::{Module, ModuleRegistry, RouteInfo, RouteVersionSupport};
use crate::core::pagination::{PageResult, PaginationParams};
use actix_web::{web, HttpRequest, HttpResponse};
use chrono::{DateTime, NaiveDate, Utc};
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::sync::{Arc, RwLock};
use uuid::Uuid;

/// Maximum lengths enforced on user supplied text.
const MAX_NAME_LEN: usize = 200;
const MAX_DESCRIPTION_LEN: usize = 5000;
const MAX_SERIAL_LEN: usize = 128;
const MAX_LOCATION_LEN: usize = 200;
const MAX_VENDOR_LEN: usize = 200;
const MAX_REASON_LEN: usize = 1000;
const MAX_NOTES_LEN: usize = 5000;
/// Warranty / licence alert windows, in days before expiry.
const ALERT_WINDOWS_DAYS: [i64; 3] = [30, 14, 7];
/// Default window for `GET /warranty/expiring`.
const DEFAULT_EXPIRING_DAYS: i64 = 30;
/// Maximum window a caller may ask for on the expiring list.
const MAX_EXPIRING_DAYS: i64 = 365;

/// Capability names checked through the `role.check` hook.
const PERM_ASSETS_VIEW: &str = "assets.view";
const PERM_ASSETS_MANAGE: &str = "assets.manage";

/// Hook event names this module listens to.
const WARRANTY_CHECK_REQUESTED: &str = "assets.warranty.check_requested";
/// Stock query from manufacturing: per-product levels for
/// `manufacturing.stock.requested` (`{product_ids}`).
const MANUFACTURING_STOCK_REQUESTED: &str = "manufacturing.stock.requested";
/// Stock movement requests from POS, manufacturing, and field
/// service. Each carries a positive `quantity` (decrement /
/// increment) or a signed `delta` (adjust), plus `asset_id` /
/// `product_id` and/or `stock_id` identifying the stock row.
const STOCK_DECREMENT_REQUESTED: &str = "assets.stock.decrement_requested";
const STOCK_INCREMENT_REQUESTED: &str = "assets.stock.increment_requested";
const STOCK_ADJUST_REQUESTED: &str = "assets.stock.adjust_requested";

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

/// What kind of asset this is. `consumable` assets are tracked
/// through the stock tables (quantity on hand); every other kind is
/// tracked as a single item with a lifecycle status.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AssetType {
    Hardware,
    Software,
    Consumable,
    Furniture,
    Vehicle,
    Other,
}

impl Default for AssetType {
    fn default() -> Self {
        Self::Other
    }
}

impl AssetType {
    fn as_str(&self) -> &'static str {
        match self {
            Self::Hardware => "hardware",
            Self::Software => "software",
            Self::Consumable => "consumable",
            Self::Furniture => "furniture",
            Self::Vehicle => "vehicle",
            Self::Other => "other",
        }
    }
}

/// Lifecycle of a single (non-consumable) asset: `in_stock` ->
/// `assigned` -> `in_stock` (return) or `in_repair`; `retired` and
/// `disposed` are terminal. A consumable asset normally stays
/// `in_stock` and its quantity lives in the stock tables.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AssetStatus {
    InStock,
    Assigned,
    InRepair,
    Retired,
    Disposed,
}

impl Default for AssetStatus {
    fn default() -> Self {
        Self::InStock
    }
}

impl AssetStatus {
    fn as_str(&self) -> &'static str {
        match self {
            Self::InStock => "in_stock",
            Self::Assigned => "assigned",
            Self::InRepair => "in_repair",
            Self::Retired => "retired",
            Self::Disposed => "disposed",
        }
    }
}

/// Depreciation method requested from the Calculator module.
/// The math lives there; this module only stores the method name
/// and the schedule it gets back.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DepreciationMethod {
    StraightLine,
    DecliningBalance,
}

impl DepreciationMethod {
    fn as_str(&self) -> &'static str {
        match self {
            Self::StraightLine => "straight_line",
            Self::DecliningBalance => "declining_balance",
        }
    }
}

/// One asset: a single tracked item. Money is NUMERIC(19,4) in the
/// migration, validated non-negative here and rounded to 4 decimal
/// places on write. `serial_number` is unique across assets when
/// present. `vendor` is a plain text reference to the vendor
/// record kept on the procurement side; no procurement record is
/// read or created here. `license_expires_at` applies to software
/// assets (licence expiry alerting); `warranty_end` applies to
/// hardware (and any other asset with a warranty).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Asset {
    pub id: Uuid,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub asset_type: AssetType,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub serial_number: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub purchase_date: Option<NaiveDate>,
    pub purchase_price: f64,
    pub currency: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vendor: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub location: Option<String>,
    pub status: AssetStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub warranty_end: Option<NaiveDate>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub license_expires_at: Option<NaiveDate>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub license_seats: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub depreciation_method: Option<DepreciationMethod>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub useful_life_years: Option<i64>,
    pub salvage_value: f64,
    /// `none` (no method configured), `pending` (requested but no
    /// schedule yet — see the honesty note), `computed`.
    pub depreciation_status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub depreciation_schedule: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub depreciation_error: Option<String>,
    pub created_by: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// One assignment of an asset to a user. History is append-only:
/// a return sets `returned_at` (and `return_reason`) on the active
/// row; rows are never deleted or overwritten, so the full chain
/// of who held an asset stays readable through
/// `GET /{id}/assignments`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AssetAssignment {
    pub id: Uuid,
    pub asset_id: Uuid,
    pub user_id: String,
    pub assigned_by: String,
    pub assigned_at: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub returned_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub return_reason: Option<String>,
    /// `active` or `returned`.
    pub status: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Stock level of one consumable asset. `quantity_on_hand` is the
/// live count; `reorder_threshold` is the level at or below which
/// the item counts as low stock and raises `assets.stock.low`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AssetStock {
    pub id: Uuid,
    pub asset_id: Uuid,
    pub quantity_on_hand: i64,
    pub reorder_threshold: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub location: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// One audited stock change. `delta` is signed (negative for
/// consumption / loss, positive for restock); `quantity_after` is
/// the on-hand count right after this change; `reason` is always
/// required, so a change without an explanation cannot be stored.
#[derive(Debug, Clone, Serialize, Deserialize)]
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

/// Status of a stock take: `open` (counting in progress) ->
/// `closed` (report final). A closed stock take never reopens.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StockTakeStatus {
    Open,
    Closed,
}

impl StockTakeStatus {
    fn as_str(&self) -> &'static str {
        match self {
            Self::Open => "open",
            Self::Closed => "closed",
        }
    }
}

/// One stock-take audit run.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AssetStockTake {
    pub id: Uuid,
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,
    pub status: StockTakeStatus,
    pub started_by: String,
    pub started_at: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub closed_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub closed_by: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// One counted line of a stock take. `expected_quantity` is the
/// on-hand count when the line was added; `counted_quantity` is
/// what the counter found (`None` until counted); `discrepancy`
/// is `counted - expected` once counted, and is computed on read
/// and on close — it is never typed in by hand.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AssetStockTakeLine {
    pub id: Uuid,
    pub stock_take_id: Uuid,
    pub asset_id: Uuid,
    pub stock_id: Uuid,
    pub expected_quantity: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub counted_quantity: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub counted_by: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl AssetStockTakeLine {
    fn discrepancy(&self) -> Option<i64> {
        self.counted_quantity
            .map(|counted| counted - self.expected_quantity)
    }
}

// ---------------------------------------------------------------------------
// Request bodies
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct CreateAssetRequest {
    pub name: String,
    pub description: Option<String>,
    pub asset_type: Option<AssetType>,
    pub serial_number: Option<String>,
    pub purchase_date: Option<NaiveDate>,
    pub purchase_price: Option<f64>,
    pub currency: Option<String>,
    pub vendor: Option<String>,
    pub location: Option<String>,
    pub warranty_end: Option<NaiveDate>,
    pub license_expires_at: Option<NaiveDate>,
    pub license_seats: Option<i64>,
    pub depreciation_method: Option<DepreciationMethod>,
    pub useful_life_years: Option<i64>,
    pub salvage_value: Option<f64>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateAssetRequest {
    pub name: Option<String>,
    pub description: Option<Option<String>>,
    pub asset_type: Option<AssetType>,
    pub serial_number: Option<Option<String>>,
    pub purchase_date: Option<Option<NaiveDate>>,
    pub purchase_price: Option<f64>,
    pub currency: Option<String>,
    pub vendor: Option<Option<String>>,
    pub location: Option<Option<String>>,
    pub warranty_end: Option<Option<NaiveDate>>,
    pub license_expires_at: Option<Option<NaiveDate>>,
    pub license_seats: Option<Option<i64>>,
    pub depreciation_method: Option<Option<DepreciationMethod>>,
    pub useful_life_years: Option<Option<i64>>,
    pub salvage_value: Option<f64>,
}

#[derive(Debug, Deserialize)]
pub struct AssignAssetRequest {
    pub user_id: String,
}

#[derive(Debug, Deserialize)]
pub struct ReturnAssetRequest {
    pub reason: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct CreateStockRequest {
    pub asset_id: Uuid,
    pub quantity_on_hand: i64,
    pub reorder_threshold: Option<i64>,
    pub location: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateStockRequest {
    pub reorder_threshold: Option<i64>,
    pub location: Option<Option<String>>,
}

#[derive(Debug, Deserialize)]
pub struct AdjustStockRequest {
    pub delta: i64,
    pub reason: String,
}

#[derive(Debug, Deserialize)]
pub struct CreateStockTakeRequest {
    pub title: String,
    pub notes: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct AddStockTakeLineRequest {
    pub asset_id: Uuid,
    pub stock_id: Option<Uuid>,
}

#[derive(Debug, Deserialize)]
pub struct CountStockTakeLineRequest {
    pub counted_quantity: i64,
    pub note: Option<String>,
}

// ---------------------------------------------------------------------------
// Shared state
// ---------------------------------------------------------------------------

/// In-memory stores for assets. The hook bus is attached in `init`
/// (the bus lives in `AppContext`); handlers tolerate a missing bus
/// by skipping event emission. Durable rows for these types are
/// defined by migration `2026-11-22-000050_assets`; until that
/// migration is executed, assets do not survive a restart.
#[derive(Clone)]
pub struct AssetsState {
    pub assets: Arc<DashMap<Uuid, Asset>>,
    pub assignments: Arc<DashMap<Uuid, AssetAssignment>>,
    pub stock: Arc<DashMap<Uuid, AssetStock>>,
    pub adjustments: Arc<DashMap<Uuid, AssetStockAdjustment>>,
    pub stock_takes: Arc<DashMap<Uuid, AssetStockTake>>,
    pub stock_take_lines: Arc<DashMap<Uuid, AssetStockTakeLine>>,
    pub hooks: Arc<RwLock<Option<Arc<HookBus>>>>,
}

impl Default for AssetsState {
    fn default() -> Self {
        Self {
            assets: Arc::new(DashMap::new()),
            assignments: Arc::new(DashMap::new()),
            stock: Arc::new(DashMap::new()),
            adjustments: Arc::new(DashMap::new()),
            stock_takes: Arc::new(DashMap::new()),
            stock_take_lines: Arc::new(DashMap::new()),
            hooks: Arc::new(RwLock::new(None)),
        }
    }
}

impl AssetsState {
    fn bus(&self) -> Option<Arc<HookBus>> {
        self.hooks.read().expect("hooks lock poisoned").clone()
    }

    fn emit(&self, event: &str, data: Value) {
        if let Some(bus) = self.bus() {
            bus.emit(event, data);
        }
    }

    /// Emit a `notification.requested` event in the notifications
    /// module's payload shape: `kind`, `user_id`, `title`, `body`,
    /// `module: "assets"`, `link`, `priority`, `channels`. See
    /// `notifications.rs` `on_notification_requested` and
    /// `resolve_recipients`: a payload needs `user_id`, `user_ids`,
    /// or `audience`.
    fn notify(&self, kind: &str, user_id: &str, title: &str, body: &str, link: Option<String>) {
        self.emit(
            "notification.requested",
            json!({
                "kind": kind,
                "user_id": user_id,
                "title": title,
                "body": body,
                "module": "assets",
                "category": "assets",
                "priority": "normal",
                "link": link,
            }),
        );
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
/// the `role.check` hook. Fails closed: no answer means not allowed
/// (same fail-closed shape as the ai module's capability check).
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

/// Money: finite, non-negative, rounded to 4 decimal places.
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

fn load_asset(state: &AssetsState, raw_id: &str) -> UnifiedResult<Asset> {
    let id = parse_uuid(raw_id, "asset")?;
    state
        .assets
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Asset not found: {id}")))
}

fn load_stock(state: &AssetsState, raw_id: &str) -> UnifiedResult<AssetStock> {
    let id = parse_uuid(raw_id, "stock item")?;
    state
        .stock
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Stock item not found: {id}")))
}

fn load_stock_take(state: &AssetsState, raw_id: &str) -> UnifiedResult<AssetStockTake> {
    let id = parse_uuid(raw_id, "stock take")?;
    state
        .stock_takes
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Stock take not found: {id}")))
}

fn load_stock_take_line(
    state: &AssetsState,
    stock_take_id: Uuid,
    raw_id: &str,
) -> UnifiedResult<AssetStockTakeLine> {
    let id = parse_uuid(raw_id, "stock take line")?;
    state
        .stock_take_lines
        .get(&id)
        .map(|entry| entry.value().clone())
        .filter(|line| line.stock_take_id == stock_take_id)
        .ok_or_else(|| UnifiedError::not_found(format!("Stock take line not found: {id}")))
}

/// The active (unreturned) assignment of an asset, if any.
fn active_assignment(state: &AssetsState, asset_id: Uuid) -> Option<AssetAssignment> {
    state
        .assignments
        .iter()
        .map(|entry| entry.value().clone())
        .find(|assignment| assignment.asset_id == asset_id && assignment.status == "active")
}

/// True when no other asset already uses this serial number.
fn serial_is_free(state: &AssetsState, serial: &str, except: Option<Uuid>) -> bool {
    !state.assets.iter().any(|entry| {
        let asset = entry.value();
        asset.serial_number.as_deref() == Some(serial) && Some(asset.id) != except
    })
}

/// Depreciation: ask the Calculator module for a schedule and store
/// what comes back. The math is never done here. When the
/// Calculator module is not registered, or no handler answers, or
/// the answer carries an error, the asset is left with
/// `depreciation_status: "pending"` and the reason in
/// `depreciation_error` — an honest record, never a guessed
/// schedule. Returns true when a schedule was stored.
fn request_depreciation(ctx: &AppContext, state: &AssetsState, asset: &mut Asset) -> bool {
    let Some(method) = asset.depreciation_method else {
        asset.depreciation_status = "none".to_string();
        asset.depreciation_schedule = None;
        asset.depreciation_error = None;
        return false;
    };
    let Some(useful_life) = asset.useful_life_years else {
        asset.depreciation_status = "pending".to_string();
        asset.depreciation_schedule = None;
        asset.depreciation_error =
            Some("Depreciation needs useful_life_years before a schedule can be requested".to_string());
        state.emit(
            "assets.depreciation.pending",
            json!({
                "asset_id": asset.id.to_string(),
                "method": method.as_str(),
                "reason": "missing useful_life_years",
            }),
        );
        return false;
    };
    if ctx.modules.get("calculator").is_none() {
        asset.depreciation_status = "pending".to_string();
        asset.depreciation_schedule = None;
        asset.depreciation_error =
            Some("Calculator module is not enabled, so no depreciation schedule is available".to_string());
        state.emit(
            "assets.depreciation.pending",
            json!({
                "asset_id": asset.id.to_string(),
                "method": method.as_str(),
                "reason": "calculator module is not enabled",
            }),
        );
        return false;
    }
    let answer = ctx.hooks.emit(
        "calculator.depreciation_requested",
        json!({
            "asset_id": asset.id.to_string(),
            "method": method.as_str(),
            "purchase_price": round_money(asset.purchase_price),
            "salvage_value": round_money(asset.salvage_value),
            "useful_life_years": useful_life,
            "purchase_date": asset.purchase_date.map(|date| date.to_string()),
            "currency": asset.currency,
        }),
    );
    match answer.results.first() {
        Some(result) if result.get("schedule").and_then(Value::as_array).is_some() => {
            asset.depreciation_status = "computed".to_string();
            asset.depreciation_schedule = result.get("schedule").cloned();
            asset.depreciation_error = None;
            state.emit(
                "assets.depreciation.computed",
                json!({
                    "asset_id": asset.id.to_string(),
                    "method": method.as_str(),
                }),
            );
            true
        }
        Some(result) => {
            let reason = result
                .get("error")
                .and_then(Value::as_str)
                .unwrap_or("Calculator module did not return a depreciation schedule")
                .to_string();
            asset.depreciation_status = "pending".to_string();
            asset.depreciation_schedule = None;
            asset.depreciation_error = Some(reason.clone());
            state.emit(
                "assets.depreciation.pending",
                json!({
                    "asset_id": asset.id.to_string(),
                    "method": method.as_str(),
                    "reason": reason,
                }),
            );
            false
        }
        None => {
            asset.depreciation_status = "pending".to_string();
            asset.depreciation_schedule = None;
            asset.depreciation_error = Some(
                "Calculator module did not answer calculator.depreciation_requested".to_string(),
            );
            state.emit(
                "assets.depreciation.pending",
                json!({
                    "asset_id": asset.id.to_string(),
                    "method": method.as_str(),
                    "reason": "no handler answered calculator.depreciation_requested",
                }),
            );
            false
        }
    }
}

/// One expiry alert window hit, for the warranty check report.
#[derive(Debug, Clone, Serialize)]
struct ExpiryAlert {
    asset_id: Uuid,
    asset_name: String,
    kind: String,
    expires_at: String,
    days_until: i64,
    window: String,
}

/// Run the warranty / licence expiry check: every asset whose
/// warranty (or, for software, licence) ends within the 30/14/7
/// day windows — or has already expired — gets a
/// `notification.requested` to its owner (and its current assignee
/// when assigned) plus an `assets.warranty.expiring` /
/// `assets.license.expiring` event. Returns the alerts raised.
/// The scheduler job is queued separately by the caller, because
/// the check also runs from the `assets.warranty.check_requested`
/// hook where no scheduler round-trip is needed.
fn run_expiry_check(state: &AssetsState) -> Vec<ExpiryAlert> {
    let today = Utc::now().date_naive();
    let mut alerts: Vec<ExpiryAlert> = Vec::new();
    for entry in state.assets.iter() {
        let asset = entry.value().clone();
        let mut targets: Vec<(String, NaiveDate, &str)> = Vec::new();
        if let Some(warranty_end) = asset.warranty_end {
            targets.push(("warranty".to_string(), warranty_end, "assets.warranty.expiring"));
        }
        if asset.asset_type == AssetType::Software {
            if let Some(license_end) = asset.license_expires_at {
                targets.push(("license".to_string(), license_end, "assets.license.expiring"));
            }
        }
        for (kind, expires_at, event_name) in targets {
            let days_until = (expires_at - today).num_days();
            if days_until > ALERT_WINDOWS_DAYS[0] {
                continue;
            }
            let window = if days_until < 0 {
                "expired".to_string()
            } else if days_until <= ALERT_WINDOWS_DAYS[2] {
                "7-day".to_string()
            } else if days_until <= ALERT_WINDOWS_DAYS[1] {
                "14-day".to_string()
            } else {
                "30-day".to_string()
            };
            let title = if days_until < 0 {
                format!("{} {} has expired", asset.name, kind)
            } else if days_until == 0 {
                format!("{} {} expires today", asset.name, kind)
            } else {
                format!(
                    "{} {} expires in {} days",
                    asset.name, kind, days_until
                )
            };
            let body = format!(
                "Asset \"{}\" ({}): {} ends on {}. Window: {}.",
                asset.name,
                asset.id,
                kind,
                expires_at,
                window
            );
            let link = format!("/api/v1/assets/{}", asset.id);
            state.notify(&format!("assets.{kind}.expiring"), &asset.created_by, &title, &body, Some(link.clone()));
            if let Some(assignment) = active_assignment(state, asset.id) {
                if assignment.user_id != asset.created_by {
                    state.notify(
                        &format!("assets.{kind}.expiring"),
                        &assignment.user_id,
                        &title,
                        &body,
                        Some(link),
                    );
                }
            }
            state.emit(
                event_name,
                json!({
                    "asset_id": asset.id.to_string(),
                    "kind": kind,
                    "expires_at": expires_at.to_string(),
                    "days_until": days_until,
                    "window": window,
                }),
            );
            alerts.push(ExpiryAlert {
                asset_id: asset.id,
                asset_name: asset.name.clone(),
                kind,
                expires_at: expires_at.to_string(),
                days_until,
                window,
            });
        }
    }
    alerts.sort_by(|a, b| a.days_until.cmp(&b.days_until));
    alerts
}

/// Queue the recurring warranty check through `scheduler.enqueue`.
/// Mirrors the events module's `schedule_reminder`: the scheduler
/// answers `{"queued": true, "job_id": ...}` when it knows the
/// kind, and `{"queued": false, "error": "Unknown job kind: ..."}`
/// today, because `assets.warranty.check` is not in `KNOWN_KINDS`
/// yet. The check itself still runs; only the recurring queueing
/// is recorded honestly as `queued` / `declined`.
fn queue_warranty_check(state: &AssetsState) -> (bool, Option<String>) {
    let Some(bus) = state.bus() else {
        return (false, None);
    };
    let answer = bus.emit(
        "scheduler.enqueue",
        json!({
            "kind": "assets.warranty.check",
            "payload": { "checked_at": Utc::now().to_rfc3339() },
        }),
    );
    match answer.results.first() {
        Some(result) if result.get("queued").and_then(Value::as_bool) == Some(true) => (
            true,
            result
                .get("job_id")
                .and_then(|value| value.as_str())
                .map(str::to_string),
        ),
        _ => (false, None),
    }
}

/// Emit a low-stock alert when a stock item sits at or below its
/// reorder threshold. Called after every adjustment and stock
/// creation; the caller decides whether the crossing is new.
fn check_low_stock(state: &AssetsState, stock: &AssetStock) {
    if stock.quantity_on_hand > stock.reorder_threshold {
        return;
    }
    let Some(asset) = state.assets.get(&stock.asset_id).map(|entry| entry.value().clone()) else {
        return;
    };
    let title = format!("Low stock: {}", asset.name);
    let body = format!(
        "Asset \"{}\" ({}): {} on hand, reorder threshold is {}.",
        asset.name, asset.id, stock.quantity_on_hand, stock.reorder_threshold
    );
    let link = format!("/api/v1/assets/stock/{}", stock.id);
    state.notify("assets.stock.low", &asset.created_by, &title, &body, Some(link));
    state.emit(
        "assets.stock.low",
        json!({
            "asset_id": asset.id.to_string(),
            "stock_id": stock.id.to_string(),
            "quantity_on_hand": stock.quantity_on_hand,
            "reorder_threshold": stock.reorder_threshold,
        }),
    );
}

fn on_warranty_check_requested(state: AssetsState, ctx: &mut HookContext) {
    let alerts = run_expiry_check(&state);
    ctx.add_result(json!({
        "checked": true,
        "alerts": alerts.len(),
        "alerts_detail": alerts,
    }));
}

// --- Stock hook answerers -------------------------------------------------
//
// Manufacturing asks for levels (`manufacturing.stock.requested`)
// and POS / manufacturing / field service ask for movements
// (`assets.stock.decrement/increment/adjust_requested`). All four
// are answered here, against the same stock rows the HTTP stock
// endpoints manage, with the same audited-adjustment recording.

/// Read a trimmed, non-empty string field from a hook payload.
fn hook_string(ctx: &HookContext, key: &str) -> Option<String> {
    ctx.get(key)
        .and_then(|value| value.as_str())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

/// Read a quantity from a hook payload: an integer, or a whole
/// float. A fractional quantity is not a stock count and is
/// refused by the caller.
fn hook_quantity(ctx: &HookContext, key: &str) -> Option<i64> {
    let value = ctx.get(key)?;
    if let Some(number) = value.as_i64() {
        return Some(number);
    }
    let number = value.as_f64()?;
    if number.is_finite()
        && number.fract() == 0.0
        && number >= i64::MIN as f64
        && number <= i64::MAX as f64
    {
        Some(number as i64)
    } else {
        None
    }
}

/// Resolve the stock row a stock event names: an explicit
/// `stock_id` wins; otherwise the single stock row of the named
/// asset (`asset_id`, falling back to `product_id` — POS and
/// manufacturing send the product id as the asset id). Stock is
/// one row per asset (creation refuses a second row), so the asset
/// lookup is unambiguous. Returns `None` when the payload names
/// no row this module holds.
fn stock_row_for_event(state: &AssetsState, ctx: &HookContext) -> Option<AssetStock> {
    if let Some(raw) = hook_string(ctx, "stock_id") {
        if let Ok(id) = Uuid::parse_str(&raw) {
            if let Some(stock) = state.stock.get(&id).map(|entry| entry.value().clone()) {
                return Some(stock);
            }
        }
    }
    let asset_id = hook_string(ctx, "asset_id").or_else(|| hook_string(ctx, "product_id"))?;
    let asset_id = Uuid::parse_str(&asset_id).ok()?;
    state
        .stock
        .iter()
        .map(|entry| entry.value().clone())
        .find(|stock| stock.asset_id == asset_id)
}

/// Answer a stock movement event as refused, without moving stock.
fn refuse_stock_change(ctx: &mut HookContext, reason: &str) {
    ctx.set("applied", json!(false));
    ctx.add_result(json!({
        "applied": false,
        "stock_id": Value::Null,
        "quantity_after": Value::Null,
        "reason": reason,
    }));
}

/// Apply a signed stock change for one event and answer it. The
/// change is recorded as an audited `AssetStockAdjustment` (the
/// same shape as the HTTP adjust), `assets.stock.adjusted` is
/// emitted, and the low-stock alert runs when the row crosses down
/// into low stock. The answer is set on the context (`applied`,
/// `stock_id`, `quantity_after`) and added as a result row, so
/// both synchronous readers and `emit_request`-style callers see
/// it. Refusals never move stock: unknown row, zero delta, a
/// quantity that would overflow, or a change that would take the
/// row below zero.
fn apply_stock_change(state: &AssetsState, ctx: &mut HookContext, delta: i64, source: &str) {
    let Some(mut stock) = stock_row_for_event(state, ctx) else {
        refuse_stock_change(ctx, "stock_not_found");
        return;
    };
    if delta == 0 {
        refuse_stock_change(ctx, "zero_delta");
        return;
    }
    let Some(quantity_after) = stock.quantity_on_hand.checked_add(delta) else {
        refuse_stock_change(ctx, "quantity_out_of_range");
        return;
    };
    if quantity_after < 0 {
        refuse_stock_change(ctx, "insufficient_stock");
        return;
    }
    let reason = hook_string(ctx, "reason")
        .unwrap_or_else(|| format!("Stock change via {source}"))
        .chars()
        .take(MAX_REASON_LEN)
        .collect::<String>();
    let actor = hook_string(ctx, "requested_by")
        .or_else(|| hook_string(ctx, "actor"))
        .unwrap_or_else(|| source.to_string());
    let was_low = stock.quantity_on_hand <= stock.reorder_threshold;
    stock.quantity_on_hand = quantity_after;
    stock.updated_at = Utc::now();
    state.stock.insert(stock.id, stock.clone());
    let adjustment = AssetStockAdjustment {
        id: Uuid::now_v7(),
        stock_id: stock.id,
        asset_id: stock.asset_id,
        delta,
        quantity_after,
        reason,
        adjusted_by: actor.clone(),
        created_at: Utc::now(),
    };
    state.adjustments.insert(adjustment.id, adjustment.clone());
    if !was_low {
        check_low_stock(state, &stock);
    }
    state.emit(
        "assets.stock.adjusted",
        json!({
            "asset_id": stock.asset_id.to_string(),
            "stock_id": stock.id.to_string(),
            "adjustment_id": adjustment.id.to_string(),
            "delta": delta,
            "quantity_after": quantity_after,
            "actor": actor,
        }),
    );
    ctx.set("applied", json!(true));
    ctx.set("stock_id", json!(stock.id.to_string()));
    ctx.set("quantity_after", json!(quantity_after));
    ctx.add_result(json!({
        "applied": true,
        "stock_id": stock.id.to_string(),
        "quantity_after": quantity_after,
        "adjustment_id": adjustment.id.to_string(),
    }));
}

/// Answer `assets.stock.decrement_requested` (POS sale,
/// manufacturing consumption): subtract a positive `quantity`
/// from the named stock row.
fn on_stock_decrement_requested(state: AssetsState, ctx: &mut HookContext) {
    match hook_quantity(ctx, "quantity") {
        Some(quantity) if quantity > 0 => {
            apply_stock_change(&state, ctx, -quantity, "stock_decrement")
        }
        _ => refuse_stock_change(ctx, "invalid_quantity"),
    }
}

/// Answer `assets.stock.increment_requested` (POS refund): add a
/// positive `quantity` to the named stock row.
fn on_stock_increment_requested(state: AssetsState, ctx: &mut HookContext) {
    match hook_quantity(ctx, "quantity") {
        Some(quantity) if quantity > 0 => {
            apply_stock_change(&state, ctx, quantity, "stock_increment")
        }
        _ => refuse_stock_change(ctx, "invalid_quantity"),
    }
}

/// Answer `assets.stock.adjust_requested` (field-service part
/// consumption): move the named stock row by a signed `delta`.
fn on_stock_adjust_requested(state: AssetsState, ctx: &mut HookContext) {
    match hook_quantity(ctx, "delta") {
        Some(delta) => apply_stock_change(&state, ctx, delta, "stock_adjust"),
        None => refuse_stock_change(ctx, "invalid_delta"),
    }
}

/// Answer `manufacturing.stock.requested` with per-product levels
/// for the requested products this module holds a stock row for.
/// Products with no stock row are omitted, never invented, and
/// when no requested product has a row no answer is given at all
/// — the requester then keeps its `unknown` /
/// `pending_confirmation` behaviour instead of treating an empty
/// answer as a level. `reserved` is always 0 (this module holds
/// no reservations) and `unit` is null (stock rows carry no unit).
fn on_manufacturing_stock_requested(state: AssetsState, ctx: &mut HookContext) {
    let Some(product_ids) = ctx.get("product_ids").and_then(|value| value.as_array()) else {
        return;
    };
    let mut stock = serde_json::Map::new();
    for raw in product_ids {
        let Some(product_id) = raw.as_str().map(str::trim).filter(|value| !value.is_empty())
        else {
            continue;
        };
        let Ok(asset_id) = Uuid::parse_str(product_id) else {
            continue;
        };
        let Some(row) = state
            .stock
            .iter()
            .map(|entry| entry.value().clone())
            .find(|row| row.asset_id == asset_id)
        else {
            continue;
        };
        stock.insert(
            product_id.to_string(),
            json!({
                "available": row.quantity_on_hand,
                "reserved": 0,
                "unit": Value::Null,
            }),
        );
    }
    if stock.is_empty() {
        return;
    }
    let answer = json!({ "stock": stock });
    ctx.set("stock", answer["stock"].clone());
    ctx.add_result(answer);
}

// ---------------------------------------------------------------------------
// Module
// ---------------------------------------------------------------------------

/// The Asset & Inventory module.
pub struct AssetsModule {
    state: AssetsState,
}

impl AssetsModule {
    pub fn new() -> Self {
        Self {
            state: AssetsState::default(),
        }
    }
}

impl Default for AssetsModule {
    fn default() -> Self {
        Self::new()
    }
}

impl Module for AssetsModule {
    fn id(&self) -> &'static str {
        "assets"
    }

    fn name(&self) -> &'static str {
        "Asset & Inventory"
    }

    fn description(&self) -> &'static str {
        "Hardware and software inventory with assignment history, warranty and licence expiry alerts, depreciation through the Calculator module, consumable stock levels with audited adjustments, and stock-take audits."
    }

    fn is_core(&self) -> bool {
        false
    }

    fn requires(&self) -> &[&'static str] {
        &["users", "roles", "notifications", "scheduler", "calculator"]
    }

    fn dependencies(&self) -> Vec<&'static str> {
        vec!["users", "roles", "notifications", "scheduler", "calculator"]
    }

    fn requires_libraries(&self) -> &[&'static str] {
        &["actix-web", "dashmap", "serde", "chrono", "uuid"]
    }

    fn available_as_product(&self) -> bool {
        true
    }

    fn product_category(&self) -> Option<&'static str> {
        Some("productivity")
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
        let state = self.state.clone();
        bus.register(
            WARRANTY_CHECK_REQUESTED,
            "assets",
            Arc::new(move |ctx: &mut HookContext| {
                on_warranty_check_requested(state.clone(), ctx)
            }),
        );
        let state = self.state.clone();
        bus.register(
            MANUFACTURING_STOCK_REQUESTED,
            "assets",
            Arc::new(move |ctx: &mut HookContext| {
                on_manufacturing_stock_requested(state.clone(), ctx)
            }),
        );
        let state = self.state.clone();
        bus.register(
            STOCK_DECREMENT_REQUESTED,
            "assets",
            Arc::new(move |ctx: &mut HookContext| {
                on_stock_decrement_requested(state.clone(), ctx)
            }),
        );
        let state = self.state.clone();
        bus.register(
            STOCK_INCREMENT_REQUESTED,
            "assets",
            Arc::new(move |ctx: &mut HookContext| {
                on_stock_increment_requested(state.clone(), ctx)
            }),
        );
        let state = self.state.clone();
        bus.register(
            STOCK_ADJUST_REQUESTED,
            "assets",
            Arc::new(move |ctx: &mut HookContext| {
                on_stock_adjust_requested(state.clone(), ctx)
            }),
        );
    }

    fn configure_routes(&self, cfg: &mut web::ServiceConfig) {
        let state = web::Data::new(self.state.clone());
        cfg.service(
            web::scope("/api/v1/assets")
                .app_data(state)
                .route("", web::get().to(list_assets))
                .route("", web::post().to(create_asset))
                .route("/warranty/expiring", web::get().to(list_expiring))
                .route("/warranty/check", web::post().to(run_warranty_check_handler))
                .route("/stock", web::get().to(list_stock))
                .route("/stock", web::post().to(create_stock))
                .route("/stock/{stock_id}", web::get().to(get_stock))
                .route("/stock/{stock_id}", web::patch().to(update_stock))
                .route("/stock/{stock_id}/adjust", web::post().to(adjust_stock))
                .route(
                    "/stock/{stock_id}/adjustments",
                    web::get().to(list_stock_adjustments),
                )
                .route("/stock-takes", web::get().to(list_stock_takes))
                .route("/stock-takes", web::post().to(create_stock_take))
                .route("/stock-takes/{id}", web::get().to(get_stock_take))
                .route(
                    "/stock-takes/{id}/lines",
                    web::post().to(add_stock_take_line),
                )
                .route(
                    "/stock-takes/{id}/lines/{line_id}",
                    web::patch().to(count_stock_take_line),
                )
                .route("/stock-takes/{id}/close", web::post().to(close_stock_take))
                .route("/{id}", web::get().to(get_asset))
                .route("/{id}", web::patch().to(update_asset))
                .route("/{id}", web::delete().to(delete_asset))
                .route("/{id}/assign", web::post().to(assign_asset))
                .route("/{id}/return", web::post().to(return_asset))
                .route("/{id}/retire", web::post().to(retire_asset))
                .route("/{id}/dispose", web::post().to(dispose_asset))
                .route("/{id}/send-for-repair", web::post().to(send_for_repair))
                .route("/{id}/return-from-repair", web::post().to(return_from_repair))
                .route("/{id}/assignments", web::get().to(list_assignments))
                .route("/{id}/depreciation", web::get().to(get_depreciation))
                .route(
                    "/{id}/depreciation/compute",
                    web::post().to(compute_depreciation),
                ),
        );
    }

    fn route_info(&self) -> Vec<RouteInfo> {
        vec![
            RouteInfo { method: "GET", path: "/api/v1/assets", owner: "assets", description: "List assets (paginated, filter by status, type, location, search)." },
            RouteInfo { method: "POST", path: "/api/v1/assets", owner: "assets", description: "Create an asset (serial number must be unique when given)." },
            RouteInfo { method: "GET", path: "/api/v1/assets/{id}", owner: "assets", description: "Get one asset." },
            RouteInfo { method: "PATCH", path: "/api/v1/assets/{id}", owner: "assets", description: "Update an asset (retired/disposed assets cannot be edited)." },
            RouteInfo { method: "DELETE", path: "/api/v1/assets/{id}", owner: "assets", description: "Delete an asset with no active assignment." },
            RouteInfo { method: "POST", path: "/api/v1/assets/{id}/assign", owner: "assets", description: "Assign an in-stock asset to a user (append-only history)." },
            RouteInfo { method: "POST", path: "/api/v1/assets/{id}/return", owner: "assets", description: "Return an assigned asset (closes the active assignment row)." },
            RouteInfo { method: "POST", path: "/api/v1/assets/{id}/retire", owner: "assets", description: "Retire an asset (terminal for assignment; emits assets.retired)." },
            RouteInfo { method: "POST", path: "/api/v1/assets/{id}/dispose", owner: "assets", description: "Dispose of an asset (terminal; emits assets.disposed)." },
            RouteInfo { method: "POST", path: "/api/v1/assets/{id}/send-for-repair", owner: "assets", description: "Send an asset for repair (in_stock/assigned -> in_repair)." },
            RouteInfo { method: "POST", path: "/api/v1/assets/{id}/return-from-repair", owner: "assets", description: "Return an asset from repair to in-stock." },
            RouteInfo { method: "GET", path: "/api/v1/assets/{id}/assignments", owner: "assets", description: "Assignment history of an asset (append-only, paginated)." },
            RouteInfo { method: "GET", path: "/api/v1/assets/{id}/depreciation", owner: "assets", description: "Depreciation status and schedule of an asset." },
            RouteInfo { method: "POST", path: "/api/v1/assets/{id}/depreciation/compute", owner: "assets", description: "Ask the Calculator module for a depreciation schedule; records pending when it is not enabled." },
            RouteInfo { method: "GET", path: "/api/v1/assets/warranty/expiring", owner: "assets", description: "Assets whose warranty or licence ends within ?days (default 30)." },
            RouteInfo { method: "POST", path: "/api/v1/assets/warranty/check", owner: "assets", description: "Run the warranty/licence expiry check now and queue the recurring job." },
            RouteInfo { method: "GET", path: "/api/v1/assets/stock", owner: "assets", description: "List consumable stock items (paginated, filter by asset_id, low_stock=true)." },
            RouteInfo { method: "POST", path: "/api/v1/assets/stock", owner: "assets", description: "Create a stock item for a consumable asset." },
            RouteInfo { method: "GET", path: "/api/v1/assets/stock/{stock_id}", owner: "assets", description: "Get one stock item." },
            RouteInfo { method: "PATCH", path: "/api/v1/assets/stock/{stock_id}", owner: "assets", description: "Update a stock item's reorder threshold / location." },
            RouteInfo { method: "POST", path: "/api/v1/assets/stock/{stock_id}/adjust", owner: "assets", description: "Audited stock adjustment (delta + required reason; low-stock alert on crossing)." },
            RouteInfo { method: "GET", path: "/api/v1/assets/stock/{stock_id}/adjustments", owner: "assets", description: "Adjustment history of a stock item (paginated)." },
            RouteInfo { method: "GET", path: "/api/v1/assets/stock-takes", owner: "assets", description: "List stock takes (paginated, filter by status)." },
            RouteInfo { method: "POST", path: "/api/v1/assets/stock-takes", owner: "assets", description: "Start a stock take." },
            RouteInfo { method: "GET", path: "/api/v1/assets/stock-takes/{id}", owner: "assets", description: "Get one stock take with its count lines and discrepancies." },
            RouteInfo { method: "POST", path: "/api/v1/assets/stock-takes/{id}/lines", owner: "assets", description: "Add a count line (expected quantity is the live on-hand count)." },
            RouteInfo { method: "PATCH", path: "/api/v1/assets/stock-takes/{id}/lines/{line_id}", owner: "assets", description: "Record the counted quantity of a line (open stock takes only)." },
            RouteInfo { method: "POST", path: "/api/v1/assets/stock-takes/{id}/close", owner: "assets", description: "Close a stock take with a discrepancy report." },
        ]
    }

    fn route_version_support(&self) -> RouteVersionSupport {
        RouteVersionSupport::All
    }
}

pub fn register(registry: &ModuleRegistry) {
    registry.register(Arc::new(AssetsModule::new()));
}

// ---------------------------------------------------------------------------
// Asset handlers
// ---------------------------------------------------------------------------

async fn list_assets(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<AssetsState>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ASSETS_VIEW)?;
        let params = query.into_inner();
        let status_filter = params.filters.get("status").cloned();
        let type_filter = params.filters.get("type").cloned();
        let location_filter = params.filters.get("location").cloned();
        let search = params.search.clone().unwrap_or_default().to_lowercase();
        let mut items: Vec<Asset> = state
            .assets
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|asset| {
                if let Some(status) = &status_filter {
                    if asset.status.as_str() != status.as_str() {
                        return false;
                    }
                }
                if let Some(asset_type) = &type_filter {
                    if asset.asset_type.as_str() != asset_type.as_str() {
                        return false;
                    }
                }
                if let Some(location) = &location_filter {
                    if asset.location.as_deref() != Some(location.as_str()) {
                        return false;
                    }
                }
                if !search.is_empty() {
                    let haystack = format!(
                        "{} {} {} {}",
                        asset.name,
                        asset.description.clone().unwrap_or_default(),
                        asset.serial_number.clone().unwrap_or_default(),
                        asset.vendor.clone().unwrap_or_default()
                    )
                    .to_lowercase();
                    if !haystack.contains(&search) {
                        return false;
                    }
                }
                true
            })
            .collect();
        let sort = params.allowed_sort(&["name", "purchase_date", "created_at"], "created_at");
        let descending = params.normalized().order == "desc";
        items.sort_by(|a, b| {
            let ordering = match sort.as_str() {
                "name" => a.name.to_lowercase().cmp(&b.name.to_lowercase()),
                "purchase_date" => a.purchase_date.cmp(&b.purchase_date),
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

async fn create_asset(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<AssetsState>,
    req: HttpRequest,
    body: web::Json<CreateAssetRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Asset> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ASSETS_MANAGE)?;
        let input = body.into_inner();
        let name = require_text(&input.name, "name", MAX_NAME_LEN)?;
        let description = match &input.description {
            Some(raw) => Some(require_text(raw, "description", MAX_DESCRIPTION_LEN)?),
            None => None,
        };
        let asset_type = input.asset_type.unwrap_or_default();
        let serial_number = match &input.serial_number {
            Some(raw) => {
                let serial = require_text(raw, "serial_number", MAX_SERIAL_LEN)?;
                if !serial_is_free(&state, &serial, None) {
                    return Err(UnifiedError::conflict(format!(
                        "Serial number \"{serial}\" is already used by another asset"
                    )));
                }
                Some(serial)
            }
            None => None,
        };
        let purchase_price = match input.purchase_price {
            Some(price) => valid_price(price)?,
            None => 0.0,
        };
        let currency = match &input.currency {
            Some(raw) => valid_currency(raw)?,
            None => "USD".to_string(),
        };
        let vendor = match &input.vendor {
            Some(raw) => Some(require_text(raw, "vendor", MAX_VENDOR_LEN)?),
            None => None,
        };
        let location = match &input.location {
            Some(raw) => Some(require_text(raw, "location", MAX_LOCATION_LEN)?),
            None => None,
        };
        if let Some(license_seats) = input.license_seats {
            if license_seats < 0 {
                return Err(UnifiedError::bad_request("license_seats must be >= 0"));
            }
        }
        if let Some(useful_life) = input.useful_life_years {
            if useful_life <= 0 || useful_life > 100 {
                return Err(UnifiedError::bad_request(
                    "useful_life_years must be between 1 and 100",
                ));
            }
        }
        let salvage_value = match input.salvage_value {
            Some(value) => valid_price(value)?,
            None => 0.0,
        };
        if salvage_value > purchase_price {
            return Err(UnifiedError::bad_request(
                "salvage_value must not exceed purchase_price",
            ));
        }
        if let Some(warranty_end) = input.warranty_end {
            if let Some(purchase_date) = input.purchase_date {
                if warranty_end < purchase_date {
                    return Err(UnifiedError::bad_request(
                        "warranty_end must not be before purchase_date",
                    ));
                }
            }
        }
        let now = Utc::now();
        let mut asset = Asset {
            id: Uuid::now_v7(),
            name,
            description,
            asset_type,
            serial_number,
            purchase_date: input.purchase_date,
            purchase_price,
            currency,
            vendor,
            location,
            status: AssetStatus::InStock,
            warranty_end: input.warranty_end,
            license_expires_at: input.license_expires_at,
            license_seats: input.license_seats,
            depreciation_method: input.depreciation_method,
            useful_life_years: input.useful_life_years,
            salvage_value,
            depreciation_status: if input.depreciation_method.is_some() {
                "pending".to_string()
            } else {
                "none".to_string()
            },
            depreciation_schedule: None,
            depreciation_error: None,
            created_by: caller.actor.clone(),
            created_at: now,
            updated_at: now,
        };
        if asset.depreciation_method.is_some() {
            request_depreciation(&ctx, &state, &mut asset);
        }
        state.assets.insert(asset.id, asset.clone());
        state.emit(
            "assets.created",
            json!({
                "asset_id": asset.id.to_string(),
                "name": asset.name,
                "type": asset.asset_type.as_str(),
                "actor": caller.actor,
            }),
        );
        Ok(asset)
    })();
    match result {
        Ok(data) => created(data),
        Err(error) => err(error, debug),
    }
}

async fn get_asset(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<AssetsState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Asset> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ASSETS_VIEW)?;
        load_asset(&state, &path.into_inner())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn update_asset(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<AssetsState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdateAssetRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Asset> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ASSETS_MANAGE)?;
        let mut asset = load_asset(&state, &path.into_inner())?;
        if matches!(asset.status, AssetStatus::Retired | AssetStatus::Disposed) {
            return Err(UnifiedError::conflict(format!(
                "Asset is {} and cannot be edited",
                asset.status.as_str()
            )));
        }
        let input = body.into_inner();
        if let Some(raw) = &input.name {
            asset.name = require_text(raw, "name", MAX_NAME_LEN)?;
        }
        if let Some(raw) = &input.description {
            asset.description = match raw {
                Some(value) => Some(require_text(value, "description", MAX_DESCRIPTION_LEN)?),
                None => None,
            };
        }
        if let Some(asset_type) = input.asset_type {
            asset.asset_type = asset_type;
        }
        if let Some(raw) = &input.serial_number {
            asset.serial_number = match raw {
                Some(value) => {
                    let serial = require_text(value, "serial_number", MAX_SERIAL_LEN)?;
                    if !serial_is_free(&state, &serial, Some(asset.id)) {
                        return Err(UnifiedError::conflict(format!(
                            "Serial number \"{serial}\" is already used by another asset"
                        )));
                    }
                    Some(serial)
                }
                None => None,
            };
        }
        if let Some(raw) = &input.purchase_date {
            asset.purchase_date = *raw;
        }
        if let Some(price) = input.purchase_price {
            asset.purchase_price = valid_price(price)?;
        }
        if let Some(raw) = &input.currency {
            asset.currency = valid_currency(raw)?;
        }
        if let Some(raw) = &input.vendor {
            asset.vendor = match raw {
                Some(value) => Some(require_text(value, "vendor", MAX_VENDOR_LEN)?),
                None => None,
            };
        }
        if let Some(raw) = &input.location {
            asset.location = match raw {
                Some(value) => Some(require_text(value, "location", MAX_LOCATION_LEN)?),
                None => None,
            };
        }
        if let Some(raw) = &input.warranty_end {
            asset.warranty_end = *raw;
        }
        if let Some(raw) = &input.license_expires_at {
            asset.license_expires_at = *raw;
        }
        if let Some(raw) = &input.license_seats {
            match raw {
                Some(seats) => {
                    if *seats < 0 {
                        return Err(UnifiedError::bad_request("license_seats must be >= 0"));
                    }
                    asset.license_seats = Some(*seats);
                }
                None => asset.license_seats = None,
            }
        }
        if let Some(raw) = &input.depreciation_method {
            asset.depreciation_method = *raw;
        }
        if let Some(raw) = &input.useful_life_years {
            match raw {
                Some(years) => {
                    if *years <= 0 || *years > 100 {
                        return Err(UnifiedError::bad_request(
                            "useful_life_years must be between 1 and 100",
                        ));
                    }
                    asset.useful_life_years = Some(*years);
                }
                None => asset.useful_life_years = None,
            }
        }
        if let Some(value) = input.salvage_value {
            asset.salvage_value = valid_price(value)?;
        }
        if asset.salvage_value > asset.purchase_price {
            return Err(UnifiedError::bad_request(
                "salvage_value must not exceed purchase_price",
            ));
        }
        if let (Some(warranty_end), Some(purchase_date)) = (asset.warranty_end, asset.purchase_date)
        {
            if warranty_end < purchase_date {
                return Err(UnifiedError::bad_request(
                    "warranty_end must not be before purchase_date",
                ));
            }
        }
        if asset.depreciation_method.is_some() {
            request_depreciation(&ctx, &state, &mut asset);
        } else {
            asset.depreciation_status = "none".to_string();
            asset.depreciation_schedule = None;
            asset.depreciation_error = None;
        }
        asset.updated_at = Utc::now();
        state.assets.insert(asset.id, asset.clone());
        state.emit(
            "assets.updated",
            json!({
                "asset_id": asset.id.to_string(),
                "actor": caller.actor,
            }),
        );
        Ok(asset)
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn delete_asset(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<AssetsState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Asset> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ASSETS_MANAGE)?;
        let asset = load_asset(&state, &path.into_inner())?;
        if active_assignment(&state, asset.id).is_some() {
            return Err(UnifiedError::conflict(
                "Asset has an active assignment and cannot be deleted; return it first",
            ));
        }
        if state
            .assignments
            .iter()
            .any(|entry| entry.value().asset_id == asset.id)
        {
            return Err(UnifiedError::conflict(
                "Asset has assignment history and cannot be deleted; retire it instead",
            ));
        }
        if state
            .stock
            .iter()
            .any(|entry| entry.value().asset_id == asset.id)
        {
            return Err(UnifiedError::conflict(
                "Asset has stock records and cannot be deleted; retire it instead",
            ));
        }
        state.assets.remove(&asset.id);
        state.emit(
            "assets.deleted",
            json!({
                "asset_id": asset.id.to_string(),
                "actor": caller.actor,
            }),
        );
        Ok(asset)
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn assign_asset(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<AssetsState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<AssignAssetRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<AssetAssignment> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ASSETS_MANAGE)?;
        let mut asset = load_asset(&state, &path.into_inner())?;
        if asset.status != AssetStatus::InStock {
            return Err(UnifiedError::conflict(format!(
                "Only an in-stock asset can be assigned (asset is {})",
                asset.status.as_str()
            )));
        }
        if active_assignment(&state, asset.id).is_some() {
            return Err(UnifiedError::conflict(
                "Asset already has an active assignment",
            ));
        }
        let user_id = require_text(&body.into_inner().user_id, "user_id", MAX_NAME_LEN)?;
        let now = Utc::now();
        let assignment = AssetAssignment {
            id: Uuid::now_v7(),
            asset_id: asset.id,
            user_id: user_id.clone(),
            assigned_by: caller.actor.clone(),
            assigned_at: now,
            returned_at: None,
            return_reason: None,
            status: "active".to_string(),
            created_at: now,
            updated_at: now,
        };
        state.assignments.insert(assignment.id, assignment.clone());
        asset.status = AssetStatus::Assigned;
        asset.updated_at = now;
        state.assets.insert(asset.id, asset);
        state.emit(
            "assets.assigned",
            json!({
                "asset_id": assignment.asset_id.to_string(),
                "assignment_id": assignment.id.to_string(),
                "user_id": user_id,
                "actor": caller.actor,
            }),
        );
        Ok(assignment)
    })();
    match result {
        Ok(data) => created(data),
        Err(error) => err(error, debug),
    }
}

async fn return_asset(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<AssetsState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<ReturnAssetRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<AssetAssignment> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ASSETS_MANAGE)?;
        let mut asset = load_asset(&state, &path.into_inner())?;
        let Some(mut assignment) = active_assignment(&state, asset.id) else {
            return Err(UnifiedError::conflict(
                "Asset has no active assignment to return",
            ));
        };
        let reason = match &body.into_inner().reason {
            Some(raw) => Some(require_text(raw, "reason", MAX_REASON_LEN)?),
            None => None,
        };
        let now = Utc::now();
        assignment.returned_at = Some(now);
        assignment.return_reason = reason;
        assignment.status = "returned".to_string();
        assignment.updated_at = now;
        state.assignments.insert(assignment.id, assignment.clone());
        asset.status = AssetStatus::InStock;
        asset.updated_at = now;
        state.assets.insert(asset.id, asset);
        state.emit(
            "assets.returned",
            json!({
                "asset_id": assignment.asset_id.to_string(),
                "assignment_id": assignment.id.to_string(),
                "user_id": assignment.user_id,
                "actor": caller.actor,
            }),
        );
        Ok(assignment)
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

/// Shared lifecycle transition: retire / dispose / repair moves.
/// Only the transitions listed in the error message are allowed.
fn transition_asset(
    state: &AssetsState,
    asset: &mut Asset,
    from: &[AssetStatus],
    to: AssetStatus,
    event: &str,
    actor: &str,
) -> UnifiedResult<()> {
    if !from.contains(&asset.status) {
        return Err(UnifiedError::conflict(format!(
            "Asset is {} and cannot move to {}",
            asset.status.as_str(),
            to.as_str()
        )));
    }
    asset.status = to;
    asset.updated_at = Utc::now();
    state.assets.insert(asset.id, asset.clone());
    state.emit(
        event,
        json!({
            "asset_id": asset.id.to_string(),
            "status": to.as_str(),
            "actor": actor,
        }),
    );
    Ok(())
}

async fn retire_asset(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<AssetsState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Asset> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ASSETS_MANAGE)?;
        let mut asset = load_asset(&state, &path.into_inner())?;
        if active_assignment(&state, asset.id).is_some() {
            return Err(UnifiedError::conflict(
                "Asset has an active assignment and cannot be retired; return it first",
            ));
        }
        transition_asset(
            &state,
            &mut asset,
            &[AssetStatus::InStock, AssetStatus::InRepair],
            AssetStatus::Retired,
            "assets.retired",
            &caller.actor,
        )?;
        Ok(asset)
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn dispose_asset(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<AssetsState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Asset> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ASSETS_MANAGE)?;
        let mut asset = load_asset(&state, &path.into_inner())?;
        if active_assignment(&state, asset.id).is_some() {
            return Err(UnifiedError::conflict(
                "Asset has an active assignment and cannot be disposed of; return it first",
            ));
        }
        transition_asset(
            &state,
            &mut asset,
            &[AssetStatus::InStock, AssetStatus::InRepair, AssetStatus::Retired],
            AssetStatus::Disposed,
            "assets.disposed",
            &caller.actor,
        )?;
        Ok(asset)
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn send_for_repair(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<AssetsState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Asset> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ASSETS_MANAGE)?;
        let mut asset = load_asset(&state, &path.into_inner())?;
        if asset.status == AssetStatus::Assigned {
            return Err(UnifiedError::conflict(
                "An assigned asset cannot be sent for repair; return it first",
            ));
        }
        transition_asset(
            &state,
            &mut asset,
            &[AssetStatus::InStock],
            AssetStatus::InRepair,
            "assets.repair.started",
            &caller.actor,
        )?;
        Ok(asset)
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn return_from_repair(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<AssetsState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Asset> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ASSETS_MANAGE)?;
        let mut asset = load_asset(&state, &path.into_inner())?;
        transition_asset(
            &state,
            &mut asset,
            &[AssetStatus::InRepair],
            AssetStatus::InStock,
            "assets.repair.completed",
            &caller.actor,
        )?;
        Ok(asset)
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn list_assignments(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<AssetsState>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ASSETS_VIEW)?;
        let asset = load_asset(&state, &path.into_inner())?;
        let params = query.into_inner();
        let status_filter = params.filters.get("status").cloned();
        let mut items: Vec<AssetAssignment> = state
            .assignments
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|assignment| {
                if assignment.asset_id != asset.id {
                    return false;
                }
                if let Some(status) = &status_filter {
                    if &assignment.status != status {
                        return false;
                    }
                }
                true
            })
            .collect();
        items.sort_by(|a, b| b.assigned_at.cmp(&a.assigned_at));
        Ok(paginate(items, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn get_depreciation(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<AssetsState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ASSETS_VIEW)?;
        let asset = load_asset(&state, &path.into_inner())?;
        Ok(json!({
            "asset_id": asset.id,
            "method": asset.depreciation_method.map(|method| method.as_str()),
            "useful_life_years": asset.useful_life_years,
            "purchase_price": asset.purchase_price,
            "salvage_value": asset.salvage_value,
            "currency": asset.currency,
            "status": asset.depreciation_status,
            "schedule": asset.depreciation_schedule,
            "error": asset.depreciation_error,
        }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn compute_depreciation(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<AssetsState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ASSETS_MANAGE)?;
        let mut asset = load_asset(&state, &path.into_inner())?;
        if asset.depreciation_method.is_none() {
            return Err(UnifiedError::bad_request(
                "Asset has no depreciation_method configured; set one first",
            ));
        }
        let computed = request_depreciation(&ctx, &state, &mut asset);
        asset.updated_at = Utc::now();
        state.assets.insert(asset.id, asset.clone());
        Ok(json!({
            "asset_id": asset.id,
            "method": asset.depreciation_method.map(|method| method.as_str()),
            "status": asset.depreciation_status,
            "schedule": asset.depreciation_schedule,
            "error": asset.depreciation_error,
            "computed": computed,
        }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn list_expiring(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<AssetsState>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ASSETS_VIEW)?;
        let params = query.into_inner();
        let days: i64 = params
            .filters
            .get("days")
            .and_then(|raw| raw.parse::<i64>().ok())
            .unwrap_or(DEFAULT_EXPIRING_DAYS)
            .clamp(0, MAX_EXPIRING_DAYS);
        let kind_filter = params.filters.get("kind").cloned();
        let today = Utc::now().date_naive();
        let mut items: Vec<Value> = Vec::new();
        for entry in state.assets.iter() {
            let asset = entry.value().clone();
            let mut targets: Vec<(&str, NaiveDate)> = Vec::new();
            if let Some(warranty_end) = asset.warranty_end {
                targets.push(("warranty", warranty_end));
            }
            if asset.asset_type == AssetType::Software {
                if let Some(license_end) = asset.license_expires_at {
                    targets.push(("license", license_end));
                }
            }
            for (kind, expires_at) in targets {
                if let Some(filter) = &kind_filter {
                    if kind != filter.as_str() {
                        continue;
                    }
                }
                let days_until = (expires_at - today).num_days();
                if days_until > days {
                    continue;
                }
                items.push(json!({
                    "asset_id": asset.id,
                    "asset_name": asset.name,
                    "asset_type": asset.asset_type.as_str(),
                    "status": asset.status.as_str(),
                    "kind": kind,
                    "expires_at": expires_at.to_string(),
                    "days_until": days_until,
                }));
            }
        }
        items.sort_by(|a, b| {
            a.get("days_until")
                .and_then(Value::as_i64)
                .cmp(&b.get("days_until").and_then(Value::as_i64))
        });
        Ok(paginate(items, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn run_warranty_check_handler(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<AssetsState>,
    req: HttpRequest,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ASSETS_MANAGE)?;
        let alerts = run_expiry_check(&state);
        let (queued, job_id) = queue_warranty_check(&state);
        Ok(json!({
            "checked_at": Utc::now().to_rfc3339(),
            "alerts": alerts.len(),
            "alerts_detail": alerts,
            "scheduler_queued": queued,
            "scheduler_job_id": job_id,
            "scheduler_status": if queued { "queued" } else { "declined" },
            "actor": caller.actor,
        }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

// ---------------------------------------------------------------------------
// Stock handlers
// ---------------------------------------------------------------------------

async fn list_stock(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<AssetsState>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ASSETS_VIEW)?;
        let params = query.into_inner();
        let asset_filter = params.filters.get("asset_id").cloned();
        let low_stock_only = params
            .filters
            .get("low_stock")
            .map(|raw| raw == "true")
            .unwrap_or(false);
        let search = params.search.clone().unwrap_or_default().to_lowercase();
        let mut items: Vec<AssetStock> = state
            .stock
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|stock| {
                if let Some(asset_id) = &asset_filter {
                    if stock.asset_id.to_string() != *asset_id {
                        return false;
                    }
                }
                if low_stock_only && stock.quantity_on_hand > stock.reorder_threshold {
                    return false;
                }
                if !search.is_empty() {
                    let asset_name = state
                        .assets
                        .get(&stock.asset_id)
                        .map(|entry| entry.value().name.clone())
                        .unwrap_or_default()
                        .to_lowercase();
                    if !asset_name.contains(&search) {
                        return false;
                    }
                }
                true
            })
            .collect();
        items.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
        Ok(paginate(items, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn create_stock(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<AssetsState>,
    req: HttpRequest,
    body: web::Json<CreateStockRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<AssetStock> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ASSETS_MANAGE)?;
        let input = body.into_inner();
        let asset = state
            .assets
            .get(&input.asset_id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found(format!("Asset not found: {}", input.asset_id)))?;
        if asset.asset_type != AssetType::Consumable {
            return Err(UnifiedError::bad_request(
                "Stock items can only be created for consumable assets",
            ));
        }
        if state
            .stock
            .iter()
            .any(|entry| entry.value().asset_id == input.asset_id)
        {
            return Err(UnifiedError::conflict(
                "A stock item already exists for this asset",
            ));
        }
        if input.quantity_on_hand < 0 {
            return Err(UnifiedError::bad_request("quantity_on_hand must be >= 0"));
        }
        let reorder_threshold = match input.reorder_threshold {
            Some(threshold) => {
                if threshold < 0 {
                    return Err(UnifiedError::bad_request("reorder_threshold must be >= 0"));
                }
                threshold
            }
            None => 0,
        };
        let location = match &input.location {
            Some(raw) => Some(require_text(raw, "location", MAX_LOCATION_LEN)?),
            None => None,
        };
        let now = Utc::now();
        let stock = AssetStock {
            id: Uuid::now_v7(),
            asset_id: input.asset_id,
            quantity_on_hand: input.quantity_on_hand,
            reorder_threshold,
            location,
            created_at: now,
            updated_at: now,
        };
        state.stock.insert(stock.id, stock.clone());
        check_low_stock(&state, &stock);
        state.emit(
            "assets.stock.created",
            json!({
                "asset_id": stock.asset_id.to_string(),
                "stock_id": stock.id.to_string(),
                "quantity_on_hand": stock.quantity_on_hand,
                "actor": caller.actor,
            }),
        );
        Ok(stock)
    })();
    match result {
        Ok(data) => created(data),
        Err(error) => err(error, debug),
    }
}

async fn get_stock(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<AssetsState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<AssetStock> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ASSETS_VIEW)?;
        load_stock(&state, &path.into_inner())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn update_stock(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<AssetsState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdateStockRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<AssetStock> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ASSETS_MANAGE)?;
        let mut stock = load_stock(&state, &path.into_inner())?;
        let input = body.into_inner();
        if let Some(threshold) = input.reorder_threshold {
            if threshold < 0 {
                return Err(UnifiedError::bad_request("reorder_threshold must be >= 0"));
            }
            stock.reorder_threshold = threshold;
        }
        if let Some(raw) = &input.location {
            stock.location = match raw {
                Some(value) => Some(require_text(value, "location", MAX_LOCATION_LEN)?),
                None => None,
            };
        }
        stock.updated_at = Utc::now();
        state.stock.insert(stock.id, stock.clone());
        check_low_stock(&state, &stock);
        state.emit(
            "assets.stock.updated",
            json!({
                "asset_id": stock.asset_id.to_string(),
                "stock_id": stock.id.to_string(),
                "actor": caller.actor,
            }),
        );
        Ok(stock)
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn adjust_stock(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<AssetsState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<AdjustStockRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<AssetStockAdjustment> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ASSETS_MANAGE)?;
        let mut stock = load_stock(&state, &path.into_inner())?;
        let input = body.into_inner();
        if input.delta == 0 {
            return Err(UnifiedError::bad_request("delta must not be 0"));
        }
        let reason = require_text(&input.reason, "reason", MAX_REASON_LEN)?;
        let quantity_after = stock.quantity_on_hand + input.delta;
        if quantity_after < 0 {
            return Err(UnifiedError::conflict(format!(
                "Adjustment of {} would take stock below 0 (on hand: {})",
                input.delta, stock.quantity_on_hand
            )));
        }
        let was_low = stock.quantity_on_hand <= stock.reorder_threshold;
        stock.quantity_on_hand = quantity_after;
        stock.updated_at = Utc::now();
        state.stock.insert(stock.id, stock.clone());
        let adjustment = AssetStockAdjustment {
            id: Uuid::now_v7(),
            stock_id: stock.id,
            asset_id: stock.asset_id,
            delta: input.delta,
            quantity_after,
            reason,
            adjusted_by: caller.actor.clone(),
            created_at: Utc::now(),
        };
        state.adjustments.insert(adjustment.id, adjustment.clone());
        if !was_low {
            check_low_stock(&state, &stock);
        }
        state.emit(
            "assets.stock.adjusted",
            json!({
                "asset_id": stock.asset_id.to_string(),
                "stock_id": stock.id.to_string(),
                "adjustment_id": adjustment.id.to_string(),
                "delta": input.delta,
                "quantity_after": quantity_after,
                "actor": caller.actor,
            }),
        );
        Ok(adjustment)
    })();
    match result {
        Ok(data) => created(data),
        Err(error) => err(error, debug),
    }
}

async fn list_stock_adjustments(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<AssetsState>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ASSETS_VIEW)?;
        let stock = load_stock(&state, &path.into_inner())?;
        let params = query.into_inner();
        let mut items: Vec<AssetStockAdjustment> = state
            .adjustments
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|adjustment| adjustment.stock_id == stock.id)
            .collect();
        items.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        Ok(paginate(items, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

// ---------------------------------------------------------------------------
// Stock-take handlers
// ---------------------------------------------------------------------------

async fn list_stock_takes(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<AssetsState>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ASSETS_VIEW)?;
        let params = query.into_inner();
        let status_filter = params.filters.get("status").cloned();
        let search = params.search.clone().unwrap_or_default().to_lowercase();
        let mut items: Vec<AssetStockTake> = state
            .stock_takes
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|take| {
                if let Some(status) = &status_filter {
                    if take.status.as_str() != status.as_str() {
                        return false;
                    }
                }
                if !search.is_empty() && !take.title.to_lowercase().contains(&search) {
                    return false;
                }
                true
            })
            .collect();
        items.sort_by(|a, b| b.started_at.cmp(&a.started_at));
        Ok(paginate(items, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn create_stock_take(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<AssetsState>,
    req: HttpRequest,
    body: web::Json<CreateStockTakeRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<AssetStockTake> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ASSETS_MANAGE)?;
        let input = body.into_inner();
        let title = require_text(&input.title, "title", MAX_NAME_LEN)?;
        let notes = match &input.notes {
            Some(raw) => Some(require_text(raw, "notes", MAX_NOTES_LEN)?),
            None => None,
        };
        let now = Utc::now();
        let take = AssetStockTake {
            id: Uuid::now_v7(),
            title,
            notes,
            status: StockTakeStatus::Open,
            started_by: caller.actor.clone(),
            started_at: now,
            closed_at: None,
            closed_by: None,
            created_at: now,
            updated_at: now,
        };
        state.stock_takes.insert(take.id, take.clone());
        state.emit(
            "assets.stock_take.started",
            json!({
                "stock_take_id": take.id.to_string(),
                "actor": caller.actor,
            }),
        );
        Ok(take)
    })();
    match result {
        Ok(data) => created(data),
        Err(error) => err(error, debug),
    }
}

fn stock_take_view(state: &AssetsState, take: &AssetStockTake) -> Value {
    let mut lines: Vec<Value> = state
        .stock_take_lines
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|line| line.stock_take_id == take.id)
        .map(|line| {
            json!({
                "id": line.id,
                "stock_take_id": line.stock_take_id,
                "asset_id": line.asset_id,
                "stock_id": line.stock_id,
                "expected_quantity": line.expected_quantity,
                "counted_quantity": line.counted_quantity,
                "discrepancy": line.discrepancy(),
                "counted_by": line.counted_by,
                "note": line.note,
                "created_at": line.created_at,
                "updated_at": line.updated_at,
            })
        })
        .collect();
    lines.sort_by(|a, b| {
        a.get("created_at")
            .and_then(Value::as_str)
            .cmp(&b.get("created_at").and_then(Value::as_str))
    });
    let mut value = serde_json::to_value(take).unwrap_or(Value::Null);
    if let Value::Object(map) = &mut value {
        map.insert("lines".to_string(), Value::Array(lines));
    }
    value
}

async fn get_stock_take(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<AssetsState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ASSETS_VIEW)?;
        let take = load_stock_take(&state, &path.into_inner())?;
        Ok(stock_take_view(&state, &take))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn add_stock_take_line(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<AssetsState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<AddStockTakeLineRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<AssetStockTakeLine> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ASSETS_MANAGE)?;
        let take = load_stock_take(&state, &path.into_inner())?;
        if take.status != StockTakeStatus::Open {
            return Err(UnifiedError::conflict(
                "Lines can only be added to an open stock take",
            ));
        }
        let input = body.into_inner();
        let asset = state
            .assets
            .get(&input.asset_id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found(format!("Asset not found: {}", input.asset_id)))?;
        if asset.asset_type != AssetType::Consumable {
            return Err(UnifiedError::bad_request(
                "Stock takes count consumable assets only",
            ));
        }
        let stock = match input.stock_id {
            Some(stock_id) => state
                .stock
                .get(&stock_id)
                .map(|entry| entry.value().clone())
                .filter(|item| item.asset_id == input.asset_id)
                .ok_or_else(|| {
                    UnifiedError::not_found(format!("Stock item not found: {stock_id}"))
                })?,
            None => state
                .stock
                .iter()
                .map(|entry| entry.value().clone())
                .find(|item| item.asset_id == input.asset_id)
                .ok_or_else(|| {
                    UnifiedError::not_found(format!(
                        "No stock item exists for asset {}",
                        input.asset_id
                    ))
                })?,
        };
        if state.stock_take_lines.iter().any(|entry| {
            entry.value().stock_take_id == take.id && entry.value().asset_id == input.asset_id
        }) {
            return Err(UnifiedError::conflict(
                "This asset already has a count line in this stock take",
            ));
        }
        let now = Utc::now();
        let line = AssetStockTakeLine {
            id: Uuid::now_v7(),
            stock_take_id: take.id,
            asset_id: input.asset_id,
            stock_id: stock.id,
            expected_quantity: stock.quantity_on_hand,
            counted_quantity: None,
            counted_by: None,
            note: None,
            created_at: now,
            updated_at: now,
        };
        state.stock_take_lines.insert(line.id, line.clone());
        Ok(line)
    })();
    match result {
        Ok(data) => created(data),
        Err(error) => err(error, debug),
    }
}

async fn count_stock_take_line(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<AssetsState>,
    req: HttpRequest,
    path: web::Path<(String, String)>,
    body: web::Json<CountStockTakeLineRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<AssetStockTakeLine> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ASSETS_MANAGE)?;
        let (take_id, line_id) = path.into_inner();
        let take = load_stock_take(&state, &take_id)?;
        if take.status != StockTakeStatus::Open {
            return Err(UnifiedError::conflict(
                "Lines can only be counted on an open stock take",
            ));
        }
        let mut line = load_stock_take_line(&state, take.id, &line_id)?;
        let input = body.into_inner();
        if input.counted_quantity < 0 {
            return Err(UnifiedError::bad_request("counted_quantity must be >= 0"));
        }
        let note = match &input.note {
            Some(raw) => Some(require_text(raw, "note", MAX_REASON_LEN)?),
            None => None,
        };
        line.counted_quantity = Some(input.counted_quantity);
        line.counted_by = Some(caller.actor.clone());
        line.note = note;
        line.updated_at = Utc::now();
        state.stock_take_lines.insert(line.id, line.clone());
        Ok(line)
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn close_stock_take(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<AssetsState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ASSETS_MANAGE)?;
        let mut take = load_stock_take(&state, &path.into_inner())?;
        if take.status != StockTakeStatus::Open {
            return Err(UnifiedError::conflict("Stock take is already closed"));
        }
        let lines: Vec<AssetStockTakeLine> = state
            .stock_take_lines
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|line| line.stock_take_id == take.id)
            .collect();
        if lines.is_empty() {
            return Err(UnifiedError::conflict(
                "A stock take with no count lines cannot be closed",
            ));
        }
        let uncounted = lines
            .iter()
            .filter(|line| line.counted_quantity.is_none())
            .count();
        if uncounted > 0 {
            return Err(UnifiedError::conflict(format!(
                "{uncounted} count line(s) still have no counted quantity"
            )));
        }
        let now = Utc::now();
        take.status = StockTakeStatus::Closed;
        take.closed_at = Some(now);
        take.closed_by = Some(caller.actor.clone());
        take.updated_at = now;
        state.stock_takes.insert(take.id, take.clone());
        let mut discrepancies: Vec<Value> = Vec::new();
        let mut total_expected: i64 = 0;
        let mut total_counted: i64 = 0;
        for line in &lines {
            let counted = line.counted_quantity.unwrap_or(0);
            let discrepancy = line.discrepancy().unwrap_or(0);
            total_expected += line.expected_quantity;
            total_counted += counted;
            if discrepancy != 0 {
                discrepancies.push(json!({
                    "line_id": line.id,
                    "asset_id": line.asset_id,
                    "stock_id": line.stock_id,
                    "expected_quantity": line.expected_quantity,
                    "counted_quantity": counted,
                    "discrepancy": discrepancy,
                }));
            }
        }
        let report = json!({
            "stock_take_id": take.id,
            "title": take.title,
            "status": take.status.as_str(),
            "closed_at": take.closed_at,
            "closed_by": take.closed_by,
            "lines_total": lines.len(),
            "lines_with_discrepancy": discrepancies.len(),
            "total_expected": total_expected,
            "total_counted": total_counted,
            "total_discrepancy": total_counted - total_expected,
            "discrepancies": discrepancies,
        });
        state.emit(
            "assets.stock_take.closed",
            json!({
                "stock_take_id": take.id.to_string(),
                "actor": caller.actor,
                "lines_total": lines.len(),
                "lines_with_discrepancy": discrepancies.len(),
                "total_discrepancy": total_counted - total_expected,
            }),
        );
        Ok(report)
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}
