// records — Shared types: constants, enums, records, and request/response shapes.

use unified_core::hooks::events;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Default retention in days, used when the `records` config
/// section carries no value (zero). Config validation rejects zero,
/// so this only guards against a hand-built config that skipped
/// validation.
pub const DEFAULT_RETENTION_DAYS: u32 = 365;

/// Read records, counts, exports, and the retention view.
pub const PERM_RECORDS_READ: &str = "records:read";

/// Purge records under the retention policy.
pub const PERM_RECORDS_MANAGE: &str = "records:manage";

// ---------------------------------------------------------------------------
// Types.
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
pub struct SystemRecord {
    pub id: String,
    pub seq: u64,
    pub actor: String,
    pub action: String,
    pub entity_type: String,
    pub entity_id: String,
    pub summary: String,
    pub details: Value,
    pub ip: Option<String>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize)]
pub struct CountEntry {
    pub key: String,
    pub count: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct RetentionView {
    pub retention_days: u32,
    pub configured: bool,
    pub total_records: u64,
    pub oldest_record_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct PurgeInput {
    /// Must be exactly `true`; anything else is rejected with 422 so
    /// a purge can never happen by accident.
    pub confirm: bool,
    /// Records older than this many days are removed. When omitted,
    /// the default retention (365 days) is used. Must be at least 1.
    #[serde(default)]
    pub keep_days: Option<u32>,
}

// ---------------------------------------------------------------------------
// State.
// ---------------------------------------------------------------------------

/// Events this module subscribes to. The audit-subscribed events are
/// listed first, then the task events and `user.deleted`.
pub(crate) const RECORDED_EVENTS: &[&str] = &[
    events::USER_CREATED,
    events::USER_UPDATED,
    events::ROLE_CREATED,
    events::LICENSE_ISSUED,
    events::LICENSE_REVOKED,
    events::MODULE_ENABLED,
    events::MODULE_DISABLED,
    "order.created",
    "invoice.paid",
    events::SSL_RENEW,
    events::AUTH_FAILED,
    events::GEO_LOOKUP,
    "cloudflare.dns.changed",
    "cloudflare.cache.purged",
    "cloudflare.connected",
    "google.sso.completed",
    "google.drive.link",
    "google.drive.unlink",
    "user.deleted",
    "task.created",
    "task.assigned",
    "task.status.changed",
    "task.completed",
    "task.comment.added",
    "task.deleted",
    "task.unassigned",
];
