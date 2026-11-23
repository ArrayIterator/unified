// affiliates module — Module constants.
//
// Split from the former single-file module `src/modules/affiliates.rs`.
// The module root (`affiliates.rs`) re-exports the public items, so the
// public API is unchanged.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::{HookBus, HookContext};
use unified_core::json;
use unified_core::modules::{Module, ModuleRegistry, RouteInfo, RouteVersionSupport};
use unified_core::pagination::{PageResult, PaginationParams};
use actix_web::{web, HttpRequest, HttpResponse};
use chrono::{DateTime, Duration, Utc};
use dashmap::DashMap;
use rand::{distr::Alphanumeric, Rng};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::sync::{Arc, RwLock};
use uuid::Uuid;

use super::helpers::created;

/// Maximum lengths enforced on user supplied text.
pub(crate) const MAX_NAME_LEN: usize = 200;
pub(crate) const MAX_EMAIL_LEN: usize = 254;
pub(crate) const MAX_CODE_LEN: usize = 64;
pub(crate) const MAX_CAMPAIGN_LEN: usize = 128;
pub(crate) const MAX_REASON_LEN: usize = 1000;
pub(crate) const MAX_METHOD_LEN: usize = 64;
pub(crate) const MAX_PRODUCT_ID_LEN: usize = 128;
pub(crate) const MAX_USER_AGENT_LEN: usize = 512;
pub(crate) const MAX_REFERRER_LEN: usize = 2048;
pub(crate) const MAX_USER_ID_LEN: usize = 128;
pub(crate) const MAX_COMPANY_ID_LEN: usize = 128;
/// Default attribution window in days (last-click wins inside it).
pub(crate) const DEFAULT_ATTRIBUTION_WINDOW_DAYS: i64 = 30;
/// Largest money amount accepted in a request, in minor units.
/// NUMERIC(19,4) holds at most 15 integer digits, so anything at
/// or above 1e15 major units (1e17 minor units) is rejected.
/// Mirrors the admissions `MAX_MONEY` bound.
pub(crate) const MAX_MONEY_MINOR: i64 = 100_000_000_000_000_000;
/// Fraud thresholds. Crossing one only flags the click for review.
pub(crate) const DUPLICATE_IP_WINDOW_MINUTES: i64 = 60;
pub(crate) const DUPLICATE_IP_THRESHOLD: usize = 10;
pub(crate) const VELOCITY_WINDOW_MINUTES: i64 = 10;
pub(crate) const VELOCITY_THRESHOLD: usize = 20;
/// Random suffix length for auto-generated referral codes.
pub(crate) const CODE_RANDOM_LEN: usize = 8;

/// Capability names checked through the `role.check` hook.
pub(crate) const PERM_AFFILIATES_VIEW: &str = "affiliates.view";
pub(crate) const PERM_AFFILIATES_MANAGE: &str = "affiliates.manage";

/// Hook event names this module listens to (literals repeated here
/// so the module stays decoupled from the users / store modules).
pub(crate) const USER_CREATED: &str = "user.created";
pub(crate) const STORE_ORDER_COMPLETED: &str = "store.order.completed";
pub(crate) const STORE_ORDER_REFUNDED: &str = "store.order.refunded";
