// Tax module - permission and vocabulary constants.
// Split from the former single-file module; items keep their original names and behaviour. Private items are `pub(crate)` so sibling files in this module can use them.

use super::*;
use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::{HookBus, HookContext, HookHandler};
use unified_core::json;
use unified_core::modules::{Module, ModuleRegistry, RouteInfo, RouteVersionSupport};
use unified_core::pagination::{PageResult, PaginationParams};
use actix_web::{HttpRequest, HttpResponse, web};
use base64::Engine as _;
use chrono::{DateTime, Datelike, NaiveDate, Utc};
use dashmap::DashMap;
use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::HashSet;
use std::net::{IpAddr, ToSocketAddrs};
use std::sync::{Arc, RwLock};
use std::time::Duration;
use url::Url;
use uuid::Uuid;

// Constants

/// Capabilities checked through the `role.check` hook.
pub const PERM_VIEW: &str = "tax:view";

pub const PERM_MANAGE: &str = "tax:manage";

pub const PERM_FILE: &str = "tax:file";

pub const PERM_MANAGE_CONNECTORS: &str = "tax:manage_connectors";

/// Tax types a rule may carry.
pub(crate) const TAX_TYPES: &[&str] = &[
    "vat",
    "gst",
    "sales",
    "use",
    "withholding",
    "excise",
    "other",
];

/// Filing frequencies a company may configure.
pub(crate) const FILING_FREQUENCIES: &[&str] = &["monthly", "quarterly", "yearly"];

/// Connector kinds. `example` is a clearly-marked example provider
/// mapping: it never performs a network call and its endpoint and
/// field names are obviously fake.
pub(crate) const CONNECTOR_KINDS: &[&str] = &["generic_http", "example"];

/// Authentication schemes a generic HTTP connector may use.
pub(crate) const AUTH_SCHEMES: &[&str] = &["none", "bearer", "api_key", "hmac"];

/// Development fallback when `app.secret_key` is not configured.
/// Production config validation requires a real secret key.
pub(crate) const DEV_ENCRYPTION_SECRET: &str = "unified-tax-development-secret";

/// Maximum bytes of a provider response body returned by the test
/// endpoint (a preview only; the full body is never stored).
pub(crate) const RESPONSE_PREVIEW_LIMIT: usize = 2048;

/// Default days after a period ends before its filing is due.
pub(crate) const DEFAULT_DUE_DAYS_AFTER_PERIOD: u32 = 20;
