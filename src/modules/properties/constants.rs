// Properties - shared constants.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::{HookBus, HookContext};
use unified_core::json;
use unified_core::modules::{Module, ModuleRegistry, RouteInfo, RouteVersionSupport};
use unified_core::pagination::{PageResult, PaginationParams};
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

use super::*;

/// Maximum lengths enforced on user supplied text.
pub(crate) const MAX_TITLE_LEN: usize = 200;
pub(crate) const MAX_DESCRIPTION_LEN: usize = 5000;
pub(crate) const MAX_SLUG_LEN: usize = 128;
pub(crate) const MAX_ADDRESS_LEN: usize = 500;
pub(crate) const MAX_NOTES_LEN: usize = 2000;
pub(crate) const MAX_REASON_LEN: usize = 500;
pub(crate) const MAX_NAME_LEN: usize = 200;
pub(crate) const MAX_STORAGE_KEY_LEN: usize = 512;
pub(crate) const MAX_SEARCH_QUERY_LEN: usize = 200;
/// Maximum positions (vertices) in one geometry, and maximum
/// rings in one Polygon / MultiPolygon, so a hostile geometry
/// can never allocate unbounded work.
pub(crate) const MAX_GEOMETRY_POSITIONS: usize = 10_000;
pub(crate) const MAX_GEOMETRY_RINGS: usize = 100;
/// Maximum search results returned by a geocoder provider.
pub(crate) const MAX_SEARCH_RESULTS: usize = 5;
/// Default and maximum geocoder HTTP timeout, in seconds.
pub(crate) const DEFAULT_GEOCODER_TIMEOUT_SECS: u64 = 10;
pub(crate) const MAX_GEOCODER_TIMEOUT_SECS: u64 = 30;
/// Fixed User-Agent sent to the geocoder provider (Nominatim
/// usage policy requires an identifying User-Agent).
pub(crate) const GEOCODER_USER_AGENT: &str = "Unified-Properties/1.0 (geocoder; contact: admin)";
/// SVG canvas: the plan is fitted into this box with padding.
pub(crate) const SVG_WIDTH: f64 = 1000.0;
pub(crate) const SVG_HEIGHT: f64 = 700.0;
pub(crate) const SVG_PADDING: f64 = 40.0;
/// Equirectangular projection constants (metres per degree).
pub(crate) const METRES_PER_DEGREE_LAT: f64 = 110_540.0;
pub(crate) const METRES_PER_DEGREE_LNG_AT_EQUATOR: f64 = 111_320.0;

/// Capability names checked through the `role.check` hook.
pub(crate) const PERM_PROPERTIES_VIEW: &str = "properties.view";
pub(crate) const PERM_PROPERTIES_CREATE: &str = "properties.create";
pub(crate) const PERM_PROPERTIES_MANAGE: &str = "properties.manage";

/// Hook event names this module listens to (literals repeated here
/// so the module stays decoupled from the payments / store modules,
/// exactly like the store module repeats the payments constants).
pub(crate) const PAYMENT_CAPTURED: &str = "payment.captured";
pub(crate) const BANK_TRANSFER_VERIFICATION_CREATED: &str = "bank_transfer.verification.created";
pub(crate) const BANK_TRANSFER_REJECTED: &str = "bank_transfer.rejected";
pub(crate) const PROPERTIES_ORDER_CREATED: &str = "properties.order.created";
