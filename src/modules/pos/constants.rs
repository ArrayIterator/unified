// Pos module — limits, permission, and event constants.
// Split from the former single-file module; items keep their original names and behaviour. Private items are `pub(crate)` so sibling files in this module can use them.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::HookBus;
use unified_core::json;
use unified_core::modules::{Module, ModuleRegistry, RouteInfo, RouteVersionSupport};
use unified_core::pagination::{PageResult, PaginationParams};
use actix_web::{web, HttpRequest, HttpResponse};
use chrono::{DateTime, NaiveDate, Utc};
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::sync::{
    atomic::{AtomicU64, Ordering},
    Arc, RwLock,
};
use uuid::Uuid;
use super::*;

/// Maximum lengths enforced on user supplied text.
pub(crate) const MAX_NAME_LEN: usize = 200;

pub(crate) const MAX_CODE_LEN: usize = 64;

pub(crate) const MAX_LOCATION_LEN: usize = 200;

pub(crate) const MAX_NOTES_LEN: usize = 5000;

pub(crate) const MAX_REASON_LEN: usize = 1000;

pub(crate) const MAX_LABEL_LEN: usize = 120;

/// Default receipt width in characters. Configurable in settings.
pub(crate) const DEFAULT_RECEIPT_WIDTH: u32 = 42;

/// Maximum receipt width a caller may configure.
pub(crate) const MAX_RECEIPT_WIDTH: u32 = 80;

/// Maximum discount percent a sale or line may carry (configurable
/// in settings as `max_discount_percent`; this is the ceiling for
/// the setting itself).
pub(crate) const MAX_DISCOUNT_PERCENT: f64 = 100.0;

/// Capability names checked through the `role.check` hook.
pub(crate) const PERM_POS_VIEW: &str = "pos.view";

pub(crate) const PERM_POS_SELL: &str = "pos.sell";

pub(crate) const PERM_POS_MANAGE: &str = "pos.manage";

pub(crate) const PERM_POS_REFUND: &str = "pos.refund";

/// Events this module emits.
pub(crate) const EVENT_SALE_COMPLETED: &str = "pos.sale.completed";

pub(crate) const EVENT_STOCK_DECREMENT_REQUESTED: &str = "assets.stock.decrement_requested";

pub(crate) const EVENT_STOCK_INCREMENT_REQUESTED: &str = "assets.stock.increment_requested";


