//! Hook event names and permission strings.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::{HookBus, HookContext};
use unified_core::json;
use unified_core::modules::{Module, ModuleRegistry, ProductCategory, RouteInfo, RouteVersionSupport};
use unified_core::tenancy::{schema_name_for_str, TenantContext as TenancyContext};
use unified_core::pagination::{PageResult, PaginationParams};
use actix_web::{web, HttpRequest, HttpResponse};
use base64::Engine as _;
use chrono::{DateTime, Datelike, NaiveDate, Utc};
use dashmap::DashMap;
use hmac::{Hmac, Mac};
use reqwest::header::RETRY_AFTER;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet, VecDeque};
use std::net::{IpAddr, ToSocketAddrs};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};
use uuid::Uuid;

use super::*;

// Events.

pub const EVENT_IS_HOLIDAY: &str = "holidays.is_holiday";
pub const EVENT_NON_WORKING_DAYS: &str = "holidays.non_working_days";
pub const EVENT_SYNC_REQUESTED: &str = "holidays.sync_requested";
pub const EVENT_SYNC_ALL_REQUESTED: &str = "holidays.sync_all_requested";
pub const EVENT_PROVIDER_CREATED: &str = "holidays.provider.created";
pub const EVENT_PROVIDER_UPDATED: &str = "holidays.provider.updated";
pub const EVENT_PROVIDER_DELETED: &str = "holidays.provider.deleted";
pub const EVENT_CONFIG_CREATED: &str = "holidays.config.created";
pub const EVENT_CONFIG_UPDATED: &str = "holidays.config.updated";
pub const EVENT_CONFIG_DELETED: &str = "holidays.config.deleted";
pub const EVENT_SYNC_STARTED: &str = "holidays.sync.started";
pub const EVENT_SYNC_FINISHED: &str = "holidays.sync.finished";
pub const EVENT_SYNC_FAILED: &str = "holidays.sync.failed";
pub const EVENT_OVERRIDE_CREATED: &str = "holidays.override.created";
pub const EVENT_OVERRIDE_UPDATED: &str = "holidays.override.updated";
pub const EVENT_OVERRIDE_DELETED: &str = "holidays.override.deleted";

// Capabilities (PERM_* const pattern, as in todos.rs).

pub const PERM_HOLIDAYS_READ: &str = "holidays:read";
pub const PERM_HOLIDAYS_MANAGE: &str = "holidays:manage";
pub const PERM_HOLIDAYS_SYNC: &str = "holidays:sync";
