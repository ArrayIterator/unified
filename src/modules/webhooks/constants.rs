// Webhooks - Constants.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::{events, HookBus};
use unified_core::json;
use unified_core::modules::{Module, ModuleRegistry};
use unified_core::pagination::{PageResult, PaginationParams};
use actix_web::{web, HttpResponse};
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use hmac::{Hmac, Mac};
use rand::{distr::Alphanumeric, Rng};
use reqwest::header;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::net::{IpAddr, ToSocketAddrs};
use std::sync::{Arc, OnceLock};
use std::time::Duration;
use url::Url;
use uuid::Uuid;

use super::*;

pub(crate) type HmacSha256 = Hmac<Sha256>;

// Constants.

/// Default retry attempts per delivery (1..=10).
pub(crate) const DEFAULT_MAX_ATTEMPTS: u32 = 3;
/// Maximum retry attempts per delivery.
pub(crate) const MAX_MAX_ATTEMPTS: u32 = 10;
/// Default base backoff in seconds; doubled per attempt, capped.
pub(crate) const DEFAULT_BACKOFF_BASE_SECS: u64 = 5;
pub(crate) const MAX_BACKOFF_BASE_SECS: u64 = 300;
/// Backoff ceiling in seconds.
pub(crate) const MAX_BACKOFF_SECS: u64 = 300;
/// Default per-delivery request timeout in seconds.
pub(crate) const DEFAULT_TIMEOUT_SECS: u64 = 30;
pub(crate) const MAX_TIMEOUT_SECS: u64 = 120;
/// User-Agent sent with every delivery.
pub(crate) const USER_AGENT: &str = concat!("unified-webhooks/", env!("CARGO_PKG_VERSION"));

/// Capability names checked through the `role.check` hook
/// (fail-closed when no handler answers): `webhooks.view` for
/// reads, `webhooks.manage` for writes.
pub(crate) const PERM_WEBHOOKS_VIEW: &str = "webhooks.view";
pub(crate) const PERM_WEBHOOKS_MANAGE: &str = "webhooks.manage";

/// Well-known events emitted by this module.
pub mod webhook_events {
    pub const DELIVERED: &str = "webhook.delivered";
    pub const FAILED: &str = "webhook.failed";
    pub const ENDPOINT_CREATED: &str = "webhook.endpoint.created";
    pub const ENDPOINT_UPDATED: &str = "webhook.endpoint.updated";
    pub const ENDPOINT_DELETED: &str = "webhook.endpoint.deleted";
    pub const ENDPOINT_PAUSED: &str = "webhook.endpoint.paused";
    pub const PING: &str = "webhook.ping";
}

/// Every event name an endpoint may subscribe to: the core events plus
/// the module events this application defines.
pub const KNOWN_EVENTS: &[&str] = &[
    events::APP_START,
    events::APP_STOP,
    events::APP_RELOAD,
    events::APP_PAUSE,
    events::APP_RESUME,
    events::APP_MAINTENANCE,
    events::CONFIG_VALIDATE,
    events::CONFIG_RELOAD,
    events::REQUEST_BEFORE,
    events::REQUEST_AFTER,
    events::AUTH_LOGIN,
    events::AUTH_LOGOUT,
    events::AUTH_FAILED,
    events::USER_CREATED,
    events::USER_UPDATED,
    events::ROLE_CREATED,
    events::MODULE_ENABLED,
    events::MODULE_DISABLED,
    events::SSL_RENEW,
    events::SSL_SWAP,
    events::GEO_LOOKUP,
    events::LICENSE_ISSUED,
    events::LICENSE_REVOKED,
    events::DOWNLOAD_STARTED,
    events::DOWNLOAD_FINISHED,
    events::SYNC_CONNECTED,
    "apikey.created",
    "apikey.rotated",
    "apikey.revoked",
    "apikey.used",
    "webhook.delivered",
    "webhook.failed",
    "webhook.endpoint.created",
    "webhook.endpoint.updated",
    "webhook.endpoint.deleted",
    "webhook.endpoint.paused",
    "webhook.ping",
    "import.completed",
    "import.failed",
    "export.completed",
    "export.requested",
    "security.bruteforce.locked",
    "security.bruteforce.unlocked",
    "security.auth.failed",
    "security.ip.flagged",
    "security.ip.unflagged",
    "security.settings.updated",
    "security.rate_limited",
    "module.auto_activated",
    "notification.requested",
    "notification.webhook",
    "license.validate_requested",
    // Batch 18 (cards / reference ids): only the outcome events
    // are subscribable. `cards.validate_requested` is deliberately
    // absent - its payload carries the card number, expiry, and
    // CVV, which must never leave the process to a webhook
    // endpoint. `cards.custom_rule.changed` is not listed either:
    // the cards module does not emit it yet.
    "cards.validated",
    "reference.generated",
    "reference.settings_changed",
];
