// notifications module -- Shared types, records, request bodies, and constants.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;

use super::*;

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Notification {
    pub id: String,
    pub user_id: String,
    pub title: String,
    pub body: String,
    pub kind: String,
    /// Event category, derived from `kind` when the caller does not
    /// supply one (see `category_for_kind`). Used for list filtering
    /// and for the per-user category preferences.
    #[serde(default = "default_notification_category")]
    pub category: String,
    /// Emitting module, derived from `kind` when not supplied (see
    /// `module_for_kind`). Used for list filtering.
    #[serde(default)]
    pub module: String,
    /// `low`, `normal`, `high` or `urgent`. Unknown values are
    /// normalised to `normal` by `normalize_priority`.
    #[serde(default = "default_priority")]
    pub priority: String,
    /// Optional deep link the client opens for this notification
    /// (for example an order or ticket URL).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub link: Option<String>,
    /// How this notification was addressed: `user` (one recipient),
    /// `all` (broadcast, stored once with `user_id` "all"),
    /// `role:<id>`, `department:<name>` or `segment:<name>`.
    #[serde(default = "default_audience")]
    pub audience: String,
    pub read: bool,
    pub created_at: DateTime<Utc>,
}

pub(crate) fn default_notification_category() -> String {
    "system".to_string()
}

pub(crate) fn default_priority() -> String {
    "normal".to_string()
}

pub(crate) fn default_audience() -> String {
    "user".to_string()
}

/// Normalise a priority value: anything outside the allow-list
/// becomes `normal` instead of failing the whole request, so a
/// misbehaving emitter can never lose a notification.
pub(crate) fn normalize_priority(priority: &str) -> String {
    if NOTIFICATION_PRIORITIES.contains(&priority) {
        priority.to_string()
    } else {
        "normal".to_string()
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct CreateNotification {
    pub user_id: String,
    /// Explicit recipients. When non-empty, one notification is
    /// stored per id (paired with `emails` by index when supplied).
    #[serde(default)]
    pub user_ids: Vec<String>,
    /// Recipient email addresses, aligned with `user_ids` by index.
    /// Needed for the email channel: this module has no user
    /// directory to look an address up in.
    #[serde(default)]
    pub emails: Vec<String>,
    /// Audience address: `all`, `users`, `admins`, `role:<id>`,
    /// `department:<name>` or `segment:<name>`. Takes precedence over
    /// a single `user_id` (except `user_id` "all", which is broadcast).
    #[serde(default)]
    pub audience: Option<String>,
    pub title: String,
    pub body: String,
    pub kind: Option<String>,
    #[serde(default)]
    pub category: Option<String>,
    #[serde(default)]
    pub module: Option<String>,
    #[serde(default)]
    pub priority: Option<String>,
    #[serde(default)]
    pub link: Option<String>,
    /// Channels for this notification when no template exists for its
    /// event: a subset of `in_app`, `email`, `webhook`. A template, when
    /// one exists, always wins over this list.
    #[serde(default)]
    pub channels: Vec<String>,
    /// GrapeJS email-template id (Email Templates module) to render
    /// the email body with, when the email channel is used and no
    /// notification template supplies one.
    #[serde(default)]
    pub email_template_id: Option<String>,
    /// Recipient email for a single-recipient request.
    #[serde(default)]
    pub email: Option<String>,
}

/// Channels a notification may use: `in_app`, `email`, `webhook`.
pub const NOTIFICATION_CHANNELS: &[&str] = &["in_app", "email", "webhook"];

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NotificationPreferences {
    pub user_id: String,
    /// Enabled channels: `in_app`, `email`, `webhook`.
    pub channels: Vec<String>,
    /// Event categories the user wants notifications for: a subset of
    /// `NOTIFICATION_CATEGORIES`. Defaults to all of them.
    #[serde(default = "default_categories")]
    pub categories: Vec<String>,
    /// Per-category channel override: when a category is present here,
    /// its channels replace the global `channels` list for events of
    /// that category. Absent categories fall back to `channels`.
    #[serde(default)]
    pub channel_by_category: HashMap<String, Vec<String>>,
    /// Digest cadence: `immediate`, `daily`, `weekly` or `off`.
    /// The setting is stored here; delivery itself is a scheduler hook
    /// (`notification.digest.requested` is emitted when a daily or
    /// weekly digest is saved, and the scheduler owns the send).
    /// `off` disables digest email entirely.
    #[serde(default = "default_digest")]
    pub digest: String,
    pub updated_at: DateTime<Utc>,
}

/// Event categories a user can subscribe to. The first four are the
/// original set; the rest were added as the event listeners grew so
/// every important event has a category a user can opt out of.
/// `category_for_kind` maps each event kind onto one of these.
pub const NOTIFICATION_CATEGORIES: &[&str] = &[
    "billing",
    "security",
    "sync",
    "system",
    "orders",
    "payments",
    "licensing",
    "certificates",
    "backup",
    "tickets",
    "chat",
    "monitoring",
    "maintenance",
    "announcements",
    "software",
    "store",
];

/// Digest cadences. `off` means no digest email is ever sent for the
/// user (in-app notifications still arrive); `immediate` means no
/// digest is needed because notifications arrive as they happen.

pub const DIGEST_OPTIONS: &[&str] = &["immediate", "daily", "weekly", "off"];

/// Priorities a notification may carry.
pub const NOTIFICATION_PRIORITIES: &[&str] = &["low", "normal", "high", "urgent"];

/// Map an event kind onto its category. Unknown kinds fall back to
/// `system` so they are never uncategorised.
pub(crate) fn category_for_kind(kind: &str) -> &'static str {
    if kind.starts_with("order.") || kind.starts_with("store.order.") {
        return "orders";
    }
    if kind.starts_with("payment") || kind == "payment_failed" || kind == "payment_chargeback" {
        return "payments";
    }
    if kind.starts_with("invoice") || kind.starts_with("billing.") {
        return "billing";
    }
    if kind.starts_with("license") {
        return "licensing";
    }
    if kind.starts_with("ssl.") {
        return "certificates";
    }
    if kind.starts_with("backup.") {
        return "backup";
    }
    if kind.starts_with("ticket") {
        return "tickets";
    }
    if kind.starts_with("chat.") {
        return "chat";
    }
    if kind.starts_with("cluster.sync") || kind.starts_with("vcs.") {
        return "sync";
    }
    if kind.starts_with("cluster.") || kind.starts_with("lb.") || kind.starts_with("statuspage.") {
        return "monitoring";
    }
    if kind.starts_with("maintenance") {
        return "maintenance";
    }
    if kind.starts_with("announcement") {
        return "announcements";
    }
    if kind.starts_with("security.") || kind.starts_with("auth.") {
        return "security";
    }
    if kind.starts_with("software.") || kind.starts_with("store.") {
        return "software";
    }
    "system"
}

/// Map an event kind onto the module that emits it. Used for the
/// `module` list filter only; delivery never depends on it.
pub(crate) fn module_for_kind(kind: &str) -> &str {
    let first = kind.split('.').next().unwrap_or(kind);
    match first {
        "order" | "store" => "store",
        "payment" | "payment_failed" | "payment_chargeback" => "payments",
        "invoice" | "billing" => "billing",
        "license" => "licensing",
        "ssl" => "certificates",
        "backup" => "backup",
        "ticket" => "tickets",
        "chat" => "chat",
        "cluster" => "clusters",
        "lb" => "load_balancer",
        "statuspage" => "statuspage",
        "maintenance" => "maintenance",
        "announcement" => "announcements",
        "security" | "auth" => "security",
        "vcs" => "vcs",
        "software" => "software",
        "webhook" => "webhooks",
        other => other,
    }
}

pub(crate) fn default_categories() -> Vec<String> {
    NOTIFICATION_CATEGORIES
        .iter()
        .map(|category| category.to_string())
        .collect()
}

pub(crate) fn default_digest() -> String {
    "immediate".to_string()
}

#[derive(Debug, Clone, Deserialize)]
pub struct UpdatePreferences {
    pub channels: Vec<String>,
    /// Optional replacement category list (subset of
    /// `NOTIFICATION_CATEGORIES`). Omitted means "keep the current
    /// categories".
    #[serde(default)]
    pub categories: Option<Vec<String>>,
    /// Optional per-category channel overrides (keys must be in
    /// `NOTIFICATION_CATEGORIES`, values subsets of
    /// `NOTIFICATION_CHANNELS`). Omitted means "keep the current
    /// overrides".
    #[serde(default)]
    pub channel_by_category: Option<HashMap<String, Vec<String>>>,
    /// Optional digest cadence (`immediate`, `daily`, `weekly`, `off`).
    /// Omitted means "keep the current digest".
    #[serde(default)]
    pub digest: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ReadAllRequest {
    pub user_id: String,
}

/// One member of a registered audience: a user id plus the email
/// address the email channel uses for that member, when known.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AudienceMember {
    pub user_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub email: Option<String>,
}

/// A registered audience: the members of one `role:<id>`,
/// `department:<name>` or `segment:<name>` address. Broadcast
/// audiences (`all`, `users`) need no registration. This registry is
/// the only way the module can resolve a role / department / segment
/// audience, because no user-directory hook exists to ask.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AudienceMembership {
    pub audience: String,
    pub members: Vec<AudienceMember>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CreateAudience {
    pub audience: String,
    /// Plain user ids (members without a known email address).
    #[serde(default)]
    pub user_ids: Vec<String>,
    /// Members with an optional email address. When both lists are
    /// given, `members` wins for ids present in both.
    #[serde(default)]
    pub members: Vec<AudienceMember>,
}

/// Delivery retry policy (recorded here and enforced by the retry
/// endpoint and the `email.sent` / `email.failed` listeners):
/// at most 3 attempts per channel. Email and webhook attempts retry
/// with exponential backoff -- 60s, then 300s, then 900s -- and only
/// while the attempt count is below the maximum. In-app delivery never
/// retries: storing the notification IS the delivery. A failure that
/// is not retryable (validation error, unknown template, missing
/// recipient address on an explicit request) fails immediately with no
/// `next_retry_at`. There is no background worker inside this module:
/// retries happen through `POST /{id}/deliveries/{attempt_id}/retry`
/// or by re-emitting the event; `next_retry_at` is the earliest time a
/// retry may run, and `notification.delivery.failed` is emitted on
/// every failure so the scheduler or an operator can act on it.
pub const MAX_DELIVERY_ATTEMPTS: u32 = 3;

pub const RETRY_DELAYS_SECS: &[u64] = &[60, 300, 900];

pub(crate) fn next_retry_at(attempts: u32) -> Option<DateTime<Utc>> {
    if attempts >= MAX_DELIVERY_ATTEMPTS {
        return None;
    }
    let delay = RETRY_DELAYS_SECS
        .get(attempts.saturating_sub(1) as usize)
        .copied()
        .unwrap_or(900);
    Some(now() + chrono::Duration::seconds(delay as i64))
}

/// One recorded delivery attempt for one notification and channel.
/// `status` is `queued`, `sent`, `delivered`, `failed` or `skipped`
/// (skipped means the user disabled that channel -- a preference, not
/// a failure). `notification_id` is `None` for digest emails, which
/// cover many notifications at once.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeliveryAttempt {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notification_id: Option<String>,
    pub channel: String,
    pub status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_message_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub to: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subject: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub email_template_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub job_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    pub attempts: u32,
    pub max_attempts: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_retry_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// A notification template: per-event title and body templates with
/// `{{placeholders}}`, a locale, and the channels the event is
/// delivered through. The channel choice lives on the template, not
/// in a global setting. `email_connection_id` selects the email
/// provider connection for the email channel; when it is empty, the
/// provider's global default connection is used.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NotificationTemplate {
    pub id: String,
    /// Event name this template renders, for example
    /// `security.auth.failed` or `billing.invoice.due`. The special
    /// event `digest` renders digest emails.
    pub event: String,
    pub locale: String,
    pub title_template: String,
    pub body_template: String,
    /// Channels for this event: a subset of `in_app`, `email`,
    /// `webhook`.
    pub channels: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub email_connection_id: Option<String>,
    /// GrapeJS email-template id (Email Templates module) used to
    /// render the email body for this event. When set, the send goes
    /// through `email.send_requested` (or the Mail Manager fallback)
    /// with `template_id` and the event variables, so the rendered
    /// HTML comes from the template builder instead of the plain-text
    /// notification body. When unset, the plain title/body are sent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub email_template_id: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CreateNotificationTemplate {
    pub event: String,
    #[serde(default = "default_locale")]
    pub locale: Option<String>,
    pub title_template: String,
    pub body_template: String,
    pub channels: Vec<String>,
    #[serde(default)]
    pub email_connection_id: Option<String>,
    #[serde(default)]
    pub email_template_id: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct UpdateNotificationTemplate {
    #[serde(default)]
    pub event: Option<String>,
    #[serde(default)]
    pub locale: Option<String>,
    #[serde(default)]
    pub title_template: Option<String>,
    #[serde(default)]
    pub body_template: Option<String>,
    #[serde(default)]
    pub channels: Option<Vec<String>>,
    #[serde(default)]
    pub email_connection_id: Option<String>,
    #[serde(default)]
    pub email_template_id: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct TemplateRenderInput {
    #[serde(default)]
    pub variables: HashMap<String, Value>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct TemplateTestInput {
    pub user_id: String,
    #[serde(default)]
    pub email: Option<String>,
    #[serde(default)]
    pub variables: HashMap<String, Value>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct DigestSendRequest {
    pub user_id: String,
    #[serde(default)]
    pub email: Option<String>,
}

pub(crate) fn default_locale() -> Option<String> {
    Some("en".to_string())
}

/// Outcome of one email send attempt through the providers / mail
/// chain. `queued` is true only when a handler answered with
/// `queued: true` and no `error`; `job_id` is that handler's job id
/// (the Email Providers job id or the Mail Manager job id), which the
/// `email.sent` / `email.failed` / `mail.sent` / `mail.failed`
/// listeners later use to update the recorded delivery attempt.
pub struct EmailSendOutcome {
    pub queued: bool,
    pub job_id: Option<String>,
    pub via: &'static str,
    pub error: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_priority_falls_back_to_normal() {
        assert_eq!(normalize_priority("high"), "high");
        assert_eq!(normalize_priority("urgent"), "urgent");
        assert_eq!(normalize_priority("bogus"), "normal");
        assert_eq!(normalize_priority(""), "normal");
    }

    #[test]
    fn category_for_kind_routes_domains() {
        assert_eq!(category_for_kind("order.paid"), "orders");
        assert_eq!(category_for_kind("store.order.completed"), "orders");
        assert_eq!(category_for_kind("payment_failed"), "payments");
        assert_eq!(category_for_kind("invoice.paid"), "billing");
        assert_eq!(category_for_kind("license.issued"), "licensing");
        assert_eq!(category_for_kind("security.bruteforce.locked"), "security");
        assert_eq!(category_for_kind("announcement.published"), "announcements");
        assert_eq!(category_for_kind("mystery.event"), "system");
    }

    #[test]
    fn module_for_kind_uses_first_segment() {
        assert_eq!(module_for_kind("order.paid"), "store");
        assert_eq!(module_for_kind("ssl.expiring"), "certificates");
        assert_eq!(module_for_kind("payment_failed"), "payments");
        assert_eq!(module_for_kind("frobnicate"), "frobnicate");
    }

    #[test]
    fn default_categories_cover_all_categories() {
        let defaults = default_categories();
        assert_eq!(defaults.len(), NOTIFICATION_CATEGORIES.len());
        assert!(defaults.contains(&"system".to_string()));
    }

    #[test]
    fn default_digest_is_immediate() {
        assert_eq!(default_digest(), "immediate");
    }

    #[test]
    fn next_retry_at_stops_after_max_attempts() {
        assert!(next_retry_at(0).is_some());
        assert!(next_retry_at(2).is_some());
        assert!(next_retry_at(MAX_DELIVERY_ATTEMPTS).is_none());
        assert!(next_retry_at(MAX_DELIVERY_ATTEMPTS + 1).is_none());
    }
}
