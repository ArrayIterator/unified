//! Logistics module - shipments, fleet, warehouses, delivery - Module constants.
//!
//! Split from `src/modules/logistics.rs`; behaviour unchanged.

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
use super::types::*;
use super::vehicles::*;
use super::warehouses::*;

/// Maximum lengths enforced on user supplied text.
pub(super) const MAX_ADDRESS_LEN: usize = 500;
pub(super) const MAX_NAME_LEN: usize = 200;
pub(super) const MAX_NOTE_LEN: usize = 2000;
pub(super) const MAX_REASON_LEN: usize = 500;
pub(super) const MAX_PLATE_LEN: usize = 20;
pub(super) const MAX_LICENSE_LEN: usize = 64;
pub(super) const MAX_PHONE_LEN: usize = 32;
pub(super) const MAX_ZONE_LEN: usize = 64;
pub(super) const MAX_SERVICE_LEVEL_LEN: usize = 64;
pub(super) const MAX_CURRENCY_LEN: usize = 3;
/// Public tracking rate limit: lookups per tracking number per
/// client address per window.
pub(super) const TRACK_RATE_LIMIT: u64 = 30;
pub(super) const TRACK_RATE_WINDOW_SECS: i64 = 600;
/// Volumetric divisor (cubic centimetres per kilogram) used for
/// dimensional weight, matching common carrier practice.
pub(super) const DIM_DIVISOR: f64 = 5000.0;
/// Maximum weight / dimension accepted on a shipment.
pub(super) const MAX_WEIGHT_KG: f64 = 100_000.0;
pub(super) const MAX_DIMENSION_CM: f64 = 10_000.0;

/// Capability names checked through the `role.check` hook.
pub(super) const PERM_LOGISTICS_VIEW: &str = "logistics.view";
pub(super) const PERM_LOGISTICS_CREATE: &str = "logistics.create";
pub(super) const PERM_LOGISTICS_MANAGE: &str = "logistics.manage";
pub(super) const PERM_LOGISTICS_DRIVER: &str = "logistics.driver";
pub(super) const PERM_LOGISTICS_QUOTE: &str = "logistics.quote";

/// Hook event names this module listens to (literals repeated here
/// so the module stays decoupled from the store / assets modules).
pub(super) const ORDER_PAID: &str = "order.paid";
pub(super) const STORE_ORDER_CANCELLED: &str = "store.order.cancelled";
pub(super) const ASSETS_STOCK_LOW: &str = "assets.stock.low";
pub(super) const MAINTENANCE_REMIND_REQUESTED: &str = "logistics.maintenance.remind_requested";
