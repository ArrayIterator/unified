// Contracts - shared constants.

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
use sha2::Digest;
use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use uuid::Uuid;

use super::*;

/// Maximum lengths enforced on user supplied text.
pub(crate) const MAX_NAME_LEN: usize = 200;
pub(crate) const MAX_TITLE_LEN: usize = 200;
pub(crate) const MAX_BODY_LEN: usize = 100_000;
pub(crate) const MAX_TERMS_LEN: usize = 100_000;
pub(crate) const MAX_EMAIL_LEN: usize = 254;
pub(crate) const MAX_REASON_LEN: usize = 2000;
pub(crate) const MAX_PLACEHOLDERS: usize = 100;
pub(crate) const MAX_PLACEHOLDER_NAME_LEN: usize = 64;
pub(crate) const MAX_SIGNERS_PER_CONTRACT: usize = 20;
pub(crate) const MAX_ALERT_DAYS: usize = 10;
/// Signing token: 48 random alphanumeric characters (~285 bits).
pub(crate) const SIGNING_TOKEN_LEN: usize = 48;
/// Default signing-link lifetime in days.
pub(crate) const DEFAULT_TOKEN_TTL_DAYS: i64 = 30;
/// Default expiry alert offsets (days before the end date).
pub(crate) const DEFAULT_ALERT_DAYS: [i64; 4] = [30, 14, 7, 1];

/// Capability names checked through the `role.check` hook.
pub(crate) const PERM_CONTRACTS_VIEW: &str = "contracts.view";
pub(crate) const PERM_CONTRACTS_MANAGE: &str = "contracts.manage";

/// Hook event names this module listens to.
pub(crate) const EXPIRY_ALERT_REQUESTED: &str = "contracts.expiry_alert_requested";
