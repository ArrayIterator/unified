//! Public data types, request/response shapes and their impls.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::{HookBus, HookContext};
use unified_core::json;
use unified_core::modules::{Module, ModuleRegistry, RouteInfo, RouteVersionSupport};
use unified_core::pagination::{PageResult, PaginationParams};
use actix_web::{web, HttpRequest, HttpResponse};
use chrono::{DateTime, NaiveDate, Utc};
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::sync::{Arc, RwLock};
use uuid::Uuid;

use super::*;

/// Maximum lengths enforced on user supplied text.
pub(crate) const MAX_NAME_LEN: usize = 200;
pub(crate) const MAX_DESCRIPTION_LEN: usize = 5000;
pub(crate) const MAX_SERIAL_LEN: usize = 128;
pub(crate) const MAX_LOCATION_LEN: usize = 200;
pub(crate) const MAX_VENDOR_LEN: usize = 200;
pub(crate) const MAX_REASON_LEN: usize = 1000;
pub(crate) const MAX_NOTES_LEN: usize = 5000;
/// Warranty / licence alert windows, in days before expiry.
pub(crate) const ALERT_WINDOWS_DAYS: [i64; 3] = [30, 14, 7];
/// Default window for `GET /warranty/expiring`.
pub(crate) const DEFAULT_EXPIRING_DAYS: i64 = 30;
/// Maximum window a caller may ask for on the expiring list.
pub(crate) const MAX_EXPIRING_DAYS: i64 = 365;

/// Capability names checked through the `role.check` hook.
pub(crate) const PERM_ASSETS_VIEW: &str = "assets.view";
pub(crate) const PERM_ASSETS_MANAGE: &str = "assets.manage";

/// Hook event names this module listens to.
pub(crate) const WARRANTY_CHECK_REQUESTED: &str = "assets.warranty.check_requested";
/// Stock query from manufacturing: per-product levels for
/// `manufacturing.stock.requested` (`{product_ids}`).
pub(crate) const MANUFACTURING_STOCK_REQUESTED: &str = "manufacturing.stock.requested";
/// Stock movement requests from POS, manufacturing, and field
/// service. Each carries a positive `quantity` (decrement /
/// increment) or a signed `delta` (adjust), plus `asset_id` /
/// `product_id` and/or `stock_id` identifying the stock row.
pub(crate) const STOCK_DECREMENT_REQUESTED: &str = "assets.stock.decrement_requested";
pub(crate) const STOCK_INCREMENT_REQUESTED: &str = "assets.stock.increment_requested";
pub(crate) const STOCK_ADJUST_REQUESTED: &str = "assets.stock.adjust_requested";

// Types

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
    pub(crate) fn as_str(&self) -> &'static str {
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
    pub(crate) fn as_str(&self) -> &'static str {
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
    pub(crate) fn as_str(&self) -> &'static str {
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
    /// Purchase price in minor units of `currency`. JSON carries major units.
    #[serde(with = "unified_core::money::serde_major")]
    pub purchase_price: i64,
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
    /// Residual value at end of useful life, in minor units. JSON carries major units.
    #[serde(with = "unified_core::money::serde_major")]
    pub salvage_value: i64,
    /// `none` (no method configured), `pending` (requested but no
    /// schedule yet - see the honesty note), `computed`.
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
    pub(crate) fn as_str(&self) -> &'static str {
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
/// and on close - it is never typed in by hand.
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
    /// `counted - expected` once counted (`None` until counted,
    /// and `None` when the difference does not fit in an `i64`).
    /// Uses `checked_sub` so extreme quantities can never panic
    /// (debug) or wrap (release); both operands are non-negative
    /// through the API, so overflow is unreachable in practice,
    /// but the subtraction is still checked rather than trusted.
    pub(crate) fn discrepancy(&self) -> Option<i64> {
        self.counted_quantity
            .and_then(|counted| counted.checked_sub(self.expected_quantity))
    }
}

// Request bodies

#[derive(Debug, Deserialize)]
pub struct CreateAssetRequest {
    pub name: String,
    pub description: Option<String>,
    pub asset_type: Option<AssetType>,
    pub serial_number: Option<String>,
    pub purchase_date: Option<NaiveDate>,
    #[serde(default, with = "unified_core::money::serde_major::option")]
    pub purchase_price: Option<i64>,
    pub currency: Option<String>,
    pub vendor: Option<String>,
    pub location: Option<String>,
    pub warranty_end: Option<NaiveDate>,
    pub license_expires_at: Option<NaiveDate>,
    pub license_seats: Option<i64>,
    pub depreciation_method: Option<DepreciationMethod>,
    pub useful_life_years: Option<i64>,
    #[serde(default, with = "unified_core::money::serde_major::option")]
    pub salvage_value: Option<i64>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateAssetRequest {
    pub name: Option<String>,
    pub description: Option<Option<String>>,
    pub asset_type: Option<AssetType>,
    pub serial_number: Option<Option<String>>,
    pub purchase_date: Option<Option<NaiveDate>>,
    #[serde(default, with = "unified_core::money::serde_major::option")]
    pub purchase_price: Option<i64>,
    pub currency: Option<String>,
    pub vendor: Option<Option<String>>,
    pub location: Option<Option<String>>,
    pub warranty_end: Option<Option<NaiveDate>>,
    pub license_expires_at: Option<Option<NaiveDate>>,
    pub license_seats: Option<Option<i64>>,
    pub depreciation_method: Option<Option<DepreciationMethod>>,
    pub useful_life_years: Option<Option<i64>>,
    #[serde(default, with = "unified_core::money::serde_major::option")]
    pub salvage_value: Option<i64>,
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

// Shared state

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
    pub(crate) fn bus(&self) -> Option<Arc<HookBus>> {
        self.hooks.read().expect("hooks lock poisoned").clone()
    }

    pub(crate) fn emit(&self, event: &str, data: Value) {
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
    pub(crate) fn notify(
        &self,
        kind: &str,
        user_id: &str,
        title: &str,
        body: &str,
        link: Option<String>,
    ) {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn asset_type_spellings() {
        assert_eq!(AssetType::Hardware.as_str(), "hardware");
        assert_eq!(AssetType::Software.as_str(), "software");
        assert_eq!(AssetType::Consumable.as_str(), "consumable");
        assert_eq!(AssetType::Furniture.as_str(), "furniture");
        assert_eq!(AssetType::Vehicle.as_str(), "vehicle");
        assert_eq!(AssetType::Other.as_str(), "other");
    }

    #[test]
    fn asset_status_spellings() {
        assert_eq!(AssetStatus::InStock.as_str(), "in_stock");
        assert_eq!(AssetStatus::Assigned.as_str(), "assigned");
        assert_eq!(AssetStatus::InRepair.as_str(), "in_repair");
        assert_eq!(AssetStatus::Retired.as_str(), "retired");
        assert_eq!(AssetStatus::Disposed.as_str(), "disposed");
    }

    #[test]
    fn depreciation_method_spellings() {
        assert_eq!(DepreciationMethod::StraightLine.as_str(), "straight_line");
        assert_eq!(DepreciationMethod::DecliningBalance.as_str(), "declining_balance");
    }

    #[test]
    fn stock_take_status_spellings() {
        assert_eq!(StockTakeStatus::Open.as_str(), "open");
        assert_eq!(StockTakeStatus::Closed.as_str(), "closed");
    }

    fn test_line(counted: Option<i64>) -> AssetStockTakeLine {
        AssetStockTakeLine {
            id: Uuid::new_v4(),
            stock_take_id: Uuid::new_v4(),
            asset_id: Uuid::new_v4(),
            stock_id: Uuid::new_v4(),
            expected_quantity: 10,
            counted_quantity: counted,
            counted_by: None,
            note: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }

    #[test]
    fn stock_take_line_discrepancy() {
        assert_eq!(test_line(Some(8)).discrepancy(), Some(-2));
        assert_eq!(test_line(None).discrepancy(), None);
        assert_eq!(test_line(Some(12)).discrepancy(), Some(2));
        assert_eq!(test_line(Some(10)).discrepancy(), Some(0));
    }

    #[test]
    fn stock_take_line_discrepancy_refuses_overflow() {
        // counted = i64::MAX against expected = i64::MIN cannot be
        // represented; checked_sub reports it as None instead of
        // panicking (debug) or wrapping (release).
        let mut line = test_line(Some(i64::MAX));
        line.expected_quantity = i64::MIN;
        assert_eq!(line.discrepancy(), None);
    }
}
