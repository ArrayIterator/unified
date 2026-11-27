// Security hardening enforcement engine.
//
// All enforcement is driven by `SecuritySettings`: every limit, window,
// threshold, and header value is a real configuration value (see the
// `security` section snippet for `AppConfig` in the coordinator report;
// `SecuritySettings` is also the shape of that config section). Named
// constants below exist only as fallbacks for `Default`, never as
// enforcement values buried in code.
//
// The engine is split into four Actix middlewares (registered by
// `the server HTTP layer`, ordered by priority):
//   - `SecurityHeadersMiddleware` (priority 1000): response headers.
//   - `IpFilterMiddleware` (priority 900): CIDR allow/block lists.
//   - `RateLimitMiddleware` (priority 800): rate limiting, flood
//     detection, and per-IP concurrent-connection limits.
//   - `RequestSizeMiddleware` (priority 700): per-route-group body limits.
//
// State is shared between the middleware and the Security module HTTP
// handlers through one `HardeningState` behind `shared_state()` /
// `install_shared_state()`: the server installs the config-derived
// state at startup, and the module handlers read the same instance on
// every request, so settings updates and lockouts are visible on both
// sides immediately. Nothing here is persisted to disk or to the
// database: IP lists, attempt counters, and reputation scores are
// in-memory and reset on restart. Operators who need persistence
// should re-apply the lists from configuration on startup.
//
// Events emitted on the hook bus (so the Notifications and Audit
// modules can pick them up without a direct dependency):
//   - `security.bruteforce.locked` / `security.bruteforce.unlocked`
//   - `security.auth.failed` (failed-auth cluster reached its threshold)
//   - `security.ip.flagged` / `security.ip.unflagged`
//   - `security.settings.updated`
//   - `security.rate_limited`

use crate::error::UnifiedError;
use crate::hooks::HookBus;
use crate::json;
use crate::middleware::{Middleware, MiddlewareResult, NextFn};
use actix_web::dev::ServiceRequest;
use actix_web::http::header::{self, HeaderValue};
use actix_web::HttpResponse;
use base64::Engine;
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

pub mod brute_force;
pub mod helpers;
pub mod ip_lists;
pub mod middleware;
pub mod rate_limit;
pub mod reputation;
pub mod settings;
pub mod state;

pub(crate) use self::brute_force::AttemptState;
pub use self::brute_force::*;
pub use self::helpers::*;
pub use self::ip_lists::*;
pub use self::middleware::*;
pub(crate) use self::rate_limit::BurstBucket;
pub(crate) use self::rate_limit::WindowBucket;
pub use self::rate_limit::*;
pub use self::reputation::*;
pub use self::settings::*;
pub use self::state::*;

/// Well-known security events emitted on the hook bus.
pub mod security_events {
    pub const BRUTEFORCE_LOCKED: &str = "security.bruteforce.locked";
    pub const BRUTEFORCE_UNLOCKED: &str = "security.bruteforce.unlocked";
    pub const AUTH_FAILED: &str = "security.auth.failed";
    pub const IP_FLAGGED: &str = "security.ip.flagged";
    pub const IP_UNFLAGGED: &str = "security.ip.unflagged";
    pub const SETTINGS_UPDATED: &str = "security.settings.updated";
    pub const RATE_LIMITED: &str = "security.rate_limited";
}

// Shared enforcement state

/// The shared hardening engine. One instance is shared by the
/// middleware and the Security module handlers (see `shared_state`).
pub struct HardeningState {
    settings: RwLock<SecuritySettings>,
    hooks: Arc<HookBus>,
    brute_ip: DashMap<String, AttemptState>,
    brute_account: DashMap<String, AttemptState>,
    rate_ip: DashMap<String, WindowBucket>,
    rate_user: DashMap<String, WindowBucket>,
    rate_endpoint: DashMap<String, WindowBucket>,
    burst: DashMap<String, BurstBucket>,
    concurrent: DashMap<String, Arc<AtomicU64>>,
    allowlist: DashMap<String, IpListEntry>,
    blocklist: DashMap<String, IpListEntry>,
    reputation: DashMap<String, ReputationEntry>,
    failed_auth: DashMap<String, FailedAuthCluster>,
}
