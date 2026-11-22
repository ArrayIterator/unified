// Notifications module.
//
// In-app notifications with read tracking and per-user channel,
// category and digest preferences. Notification templates render
// per-event titles and bodies with `{{placeholders}}` and carry the
// channel selection (email via an email-provider connection, in-app,
// webhook) on the template itself. Other modules request a
// notification through the `notification.requested` hook; email
// delivery goes through the Email Providers module
// (`email.send_requested`, falling back to the Mail Manager's
// `mail.send_requested` when no provider handler answers). The centre
// also listens directly to the failure and lifecycle events of the
// other modules (orders, payments, backups, clusters, tickets,
// licenses, incidents, webhooks, security) so alerts land here.
// Interconnection happens only through the hook bus.
//
// Audience rules (enforced server-side, see `resolve_recipients`):
// a notification is stored once per resolved recipient. Broadcast
// audiences (`all`, `users`) are stored once with `user_id` "all" and
// are visible to every user in list / unread-count queries. Role,
// department and segment audiences resolve ONLY through the audience
// registry (`POST /api/v1/notifications/audiences`); an unregistered
// audience is never delivered to anyone -- the request fails with an
// `audience unresolved` result and a `notification.audience.unresolved`
// event instead of silently dropping or leaking the notification.
// There is no user-directory hook in this codebase (the Users module
// exposes no audience-resolution event), so membership must be
// registered explicitly; that registry is the source of truth here.
//
// Delivery tracking: every channel dispatch records a
// `DeliveryAttempt` (queued / sent / delivered / failed, with the
// provider job id when one is returned). The providers report the
// outcome back through `email.sent` / `email.failed` (Email Providers)
// and `mail.sent` / `mail.failed` (Mail Manager); those listeners
// update the attempt. A failed delivery is never silent: the attempt
// keeps the error, `notification.delivery.failed` is emitted, and the
// attempt can be retried (see `RETRY_DELAYS_SECS` for the policy).
//
// Routes (all under `/api/v1/notifications`):
//   GET    /                              - paginated notifications
//   POST   /                              - create a notification
//   GET    /preferences                   - per-user preferences
//   PUT    /preferences                   - update per-user preferences
//   GET    /unread-count                  - unread/total counts for a user
//   POST   /read-all                      - mark all of a user's notifications read
//   POST   /digest/send                   - send a digest email for a user
//   GET    /audiences                     - registered audiences (paginated)
//   POST   /audiences                     - register an audience
//   GET    /audiences/{audience}          - one registered audience
//   DELETE /audiences/{audience}          - delete a registered audience
//   GET    /deliveries                    - delivery attempts (paginated)
//   GET    /{id}/deliveries               - delivery attempts for one notification
//   POST   /{id}/deliveries/{attempt_id}/retry - retry a failed delivery
//   GET    /templates                     - paginated notification templates
//   POST   /templates                     - create a notification template
//   GET    /templates/{id}                - one notification template
//   PUT    /templates/{id}                - update a notification template
//   DELETE /templates/{id}                - delete a notification template
//   POST   /templates/{id}/preview        - render a template with variables
//   POST   /templates/{id}/test           - render and dispatch a template once
//   GET    /{id}                          - one notification
//   DELETE /{id}                          - delete a notification
//   POST   /{id}/read                     - mark one notification read

use crate::core::context::AppContext;
use crate::core::error::{UnifiedError, UnifiedResult};
use crate::core::hooks::{HookBus, HookContext, HookHandler};
use crate::core::json;
use crate::core::modules::{Module, ModuleRegistry, RouteInfo};
use crate::core::pagination::{PageResult, PaginationParams};
use actix_web::{web, HttpResponse};
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use uuid::Uuid;

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

fn default_notification_category() -> String {
    "system".to_string()
}

fn default_priority() -> String {
    "normal".to_string()
}

fn default_audience() -> String {
    "user".to_string()
}

/// Normalise a priority value: anything outside the allow-list
/// becomes `normal` instead of failing the whole request, so a
/// misbehaving emitter can never lose a notification.
fn normalize_priority(priority: &str) -> String {
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
fn category_for_kind(kind: &str) -> &'static str {
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
fn module_for_kind(kind: &str) -> &'static str {
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

fn default_categories() -> Vec<String> {
    NOTIFICATION_CATEGORIES
        .iter()
        .map(|category| category.to_string())
        .collect()
}

fn default_digest() -> String {
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

fn next_retry_at(attempts: u32) -> Option<DateTime<Utc>> {
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

fn default_locale() -> Option<String> {
    Some("en".to_string())
}

// ---------------------------------------------------------------------------
// State
// ---------------------------------------------------------------------------

pub struct NotificationsState {
    hooks: RwLock<Arc<HookBus>>,
    notifications: DashMap<String, Notification>,
    preferences: DashMap<String, NotificationPreferences>,
    templates: DashMap<String, NotificationTemplate>,
    deliveries: DashMap<String, DeliveryAttempt>,
    audiences: DashMap<String, AudienceMembership>,
}

impl NotificationsState {
    fn new() -> Self {
        Self {
            hooks: RwLock::new(Arc::new(HookBus::new())),
            notifications: DashMap::new(),
            preferences: DashMap::new(),
            templates: DashMap::new(),
            deliveries: DashMap::new(),
            audiences: DashMap::new(),
        }
    }

    fn bus(&self) -> Arc<HookBus> {
        self.hooks.read().expect("hooks lock poisoned").clone()
    }

    fn emit(&self, event: &str, data: Value) {
        self.bus().emit(event, data);
    }

    fn create_notification(&self, input: CreateNotification) -> UnifiedResult<Notification> {
        if input.user_id.trim().is_empty() {
            return Err(UnifiedError::bad_request("user_id must not be empty"));
        }
        if input.title.trim().is_empty() {
            return Err(UnifiedError::bad_request("Title must not be empty"));
        }
        let kind = input.kind.unwrap_or_else(|| "general".to_string());
        let notification = Notification {
            id: new_id(),
            user_id: input.user_id.trim().to_string(),
            title: input.title.trim().to_string(),
            body: input.body,
            category: input
                .category
                .filter(|value| NOTIFICATION_CATEGORIES.contains(&value.as_str()))
                .unwrap_or_else(|| category_for_kind(&kind).to_string()),
            module: input
                .module
                .filter(|value| !value.trim().is_empty())
                .unwrap_or_else(|| module_for_kind(&kind).to_string()),
            priority: normalize_priority(input.priority.as_deref().unwrap_or("normal")),
            link: input.link.filter(|value| !value.trim().is_empty()),
            audience: input
                .audience
                .filter(|value| !value.trim().is_empty())
                .unwrap_or_else(|| {
                    if input.user_id.trim() == "all" {
                        "all".to_string()
                    } else {
                        "user".to_string()
                    }
                }),
            kind,
            read: false,
            created_at: now(),
        };
        self.notifications
            .insert(notification.id.clone(), notification.clone());
        Ok(notification)
    }

    fn preferences_for(&self, user_id: &str) -> NotificationPreferences {
        self.preferences
            .get(user_id)
            .map(|entry| entry.value().clone())
            .unwrap_or_else(|| NotificationPreferences {
                user_id: user_id.to_string(),
                channels: vec!["in_app".to_string()],
                categories: default_categories(),
                channel_by_category: HashMap::new(),
                digest: default_digest(),
                updated_at: now(),
            })
    }

    /// The template for an event and locale, falling back to the
    /// default locale (`en`) and then to any template for the event.
    fn template_for(&self, event: &str, locale: &str) -> Option<NotificationTemplate> {
        let mut fallback: Option<NotificationTemplate> = None;
        for entry in self.templates.iter() {
            let template = entry.value();
            if template.event != event {
                continue;
            }
            if template.locale == locale {
                return Some(template.clone());
            }
            if template.locale == "en" {
                fallback = Some(template.clone());
            } else if fallback.is_none() {
                fallback = Some(template.clone());
            }
        }
        fallback
    }

    /// Create a notification for a failure event, but only when the
    /// target user's category preferences include `category`. Returns
    /// `None` (without storing anything) when the user opted out of
    /// the category. This guard applies to the event-driven failure
    /// listeners below, not to explicit `notification.requested`
    /// calls, which are always stored.
    fn notify_for_category(
        &self,
        user_id: &str,
        category: &str,
        title: String,
        body: String,
        kind: &str,
    ) -> Option<Notification> {
        if !self
            .preferences_for(user_id)
            .categories
            .iter()
            .any(|subscribed| subscribed == category)
        {
            return None;
        }
        self.create_notification(CreateNotification {
            user_id: user_id.to_string(),
            user_ids: Vec::new(),
            emails: Vec::new(),
            audience: None,
            title,
            body,
            kind: Some(kind.to_string()),
            category: Some(category.to_string()),
            module: None,
            priority: None,
            link: None,
            channels: Vec::new(),
            email_template_id: None,
            email: None,
        })
        .ok()
    }

    /// Store one notification for an event-driven listener, applying
    /// the target user's category preferences first. Returns `None`
    /// (storing nothing) when the user opted out of the category.
    /// Explicit `notification.requested` calls do NOT go through here:
    /// they are always stored.
    fn store_notification(
        &self,
        user_id: &str,
        title: String,
        body: String,
        kind: &str,
        category: &str,
        priority: &str,
        link: Option<String>,
        audience: &str,
    ) -> Option<Notification> {
        if !self
            .preferences_for(user_id)
            .categories
            .iter()
            .any(|subscribed| subscribed == category)
        {
            return None;
        }
        self.create_notification(CreateNotification {
            user_id: user_id.to_string(),
            user_ids: Vec::new(),
            emails: Vec::new(),
            audience: Some(audience.to_string()),
            title,
            body,
            kind: Some(kind.to_string()),
            category: Some(category.to_string()),
            module: None,
            priority: Some(priority.to_string()),
            link,
            channels: Vec::new(),
            email_template_id: None,
            email: None,
        })
        .ok()
    }

    /// True when a notification addressed to `notification.user_id`
    /// may be shown to `user_id`. Broadcast notifications
    /// (`user_id` "all") are visible to every user; anything else is
    /// visible only to its own recipient. This is the server-side
    /// audience rule for list, unread-count and mark-all-read.
    fn is_visible_to(notification: &Notification, user_id: &str) -> bool {
        notification.user_id == user_id || notification.user_id == "all"
    }

    /// Effective channels for a user and category: the per-category
    /// override wins when one is set, otherwise the global list.
    fn effective_channels(&self, user_id: &str, category: &str) -> Vec<String> {
        let preferences = self.preferences_for(user_id);
        preferences
            .channel_by_category
            .get(category)
            .cloned()
            .unwrap_or(preferences.channels)
    }

    /// Registered members of a role / department / segment audience,
    /// or `None` when the audience was never registered.
    fn audience_members(&self, audience: &str) -> Option<Vec<AudienceMember>> {
        self.audiences
            .get(audience)
            .map(|entry| entry.value().members.clone())
    }

    /// Record one delivery attempt and return the stored copy.
    fn record_delivery(
        &self,
        notification_id: Option<String>,
        channel: &str,
        status: &str,
        provider: Option<String>,
        job_id: Option<String>,
        to: Option<String>,
        subject: Option<String>,
        email_template_id: Option<String>,
        error: Option<String>,
        attempts: u32,
    ) -> DeliveryAttempt {
        let attempt = DeliveryAttempt {
            id: new_id(),
            notification_id,
            channel: channel.to_string(),
            status: status.to_string(),
            provider,
            provider_message_id: None,
            to,
            subject,
            email_template_id,
            job_id,
            error,
            attempts,
            max_attempts: MAX_DELIVERY_ATTEMPTS,
            next_retry_at: if status == "failed" {
                next_retry_at(attempts)
            } else {
                None
            },
            created_at: now(),
            updated_at: now(),
        };
        self.deliveries
            .insert(attempt.id.clone(), attempt.clone());
        attempt
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn new_id() -> String {
    Uuid::new_v4().to_string()
}

fn now() -> DateTime<Utc> {
    Utc::now()
}

fn paginate<T: Serialize + Clone>(items: Vec<T>, params: &PaginationParams) -> PageResult<T> {
    let pagination = params.normalized();
    let total = items.len() as u64;
    let start = ((pagination.page - 1) * pagination.per_page) as usize;
    let page_items: Vec<T> = items
        .into_iter()
        .skip(start)
        .take(pagination.per_page as usize)
        .collect();
    PageResult::new(page_items, total, &pagination)
}

fn matches_search(haystack: &str, params: &PaginationParams) -> bool {
    match &params.search {
        Some(needle) if !needle.trim().is_empty() => haystack
            .to_lowercase()
            .contains(&needle.trim().to_lowercase()),
        _ => true,
    }
}

fn respond_created<T: Serialize>(result: UnifiedResult<T>) -> HttpResponse {
    match result {
        Ok(data) => json::success_response(data, 201),
        Err(error) => json::error_response(&error, false),
    }
}

/// Substitute `{{ name }}` and `{{ user.name }}` placeholders in a
/// template. Values come from the variables map (dotted paths resolve
/// into nested objects). Unknown placeholders are left untouched.
/// Templates are admin-authored, so values are substituted as plain
/// text without HTML escaping.
fn render_template_text(template: &str, variables: &HashMap<String, Value>) -> String {
    let mut out = String::with_capacity(template.len());
    let mut rest = template;
    while let Some(start) = rest.find("{{") {
        out.push_str(&rest[..start]);
        let after = &rest[start + 2..];
        match after.find("}}") {
            Some(end) => {
                let key = after[..end].trim();
                match lookup_variable(variables, key) {
                    Some(value) => out.push_str(&value_as_text(value)),
                    None => out.push_str(&format!("{{{{{key}}}}}")),
                }
                rest = &after[end + 2..];
            }
            None => {
                out.push_str(&rest[start..]);
                rest = "";
                break;
            }
        }
    }
    out.push_str(rest);
    out
}

fn lookup_variable<'a>(variables: &'a HashMap<String, Value>, path: &str) -> Option<&'a Value> {
    let mut current: Option<&Value> = None;
    for (index, part) in path.split('.').enumerate() {
        current = if index == 0 {
            variables.get(part)
        } else {
            current.and_then(|value| value.get(part))
        };
        if current.is_none() {
            return None;
        }
    }
    current
}

fn value_as_text(value: &Value) -> String {
    match value {
        Value::Null => String::new(),
        Value::String(text) => text.clone(),
        Value::Bool(flag) => flag.to_string(),
        Value::Number(number) => number.to_string(),
        other => serde_json::to_string(other).unwrap_or_default(),
    }
}

fn validate_channels(channels: &[String]) -> UnifiedResult<()> {
    if channels.is_empty() {
        return Err(UnifiedError::bad_request(
            "At least one channel must be enabled",
        ));
    }
    for channel in channels {
        if !NOTIFICATION_CHANNELS.contains(&channel.as_str()) {
            return Err(UnifiedError::bad_request(format!(
                "Channels must be a subset of: {}",
                NOTIFICATION_CHANNELS.join(", ")
            )));
        }
    }
    Ok(())
}

/// Send one digest email for a user: collect their unread
/// notifications, render them through the `digest` template when one
/// exists (otherwise a plain summary), and dispatch through the Email
/// Providers module (`email.send_requested`), falling back to the Mail
/// Manager (`mail.send_requested`) when no provider handler answers.
/// Returns the digest summary. Reading state is never changed: a
/// digest does not mark notifications read.
fn send_digest_for_user(
    state: &Arc<NotificationsState>,
    user_id: &str,
    email: Option<&str>,
) -> UnifiedResult<Value> {
    if user_id.trim().is_empty() {
        return Err(UnifiedError::bad_request("user_id must not be empty"));
    }
    let mut unread: Vec<Notification> = state
        .notifications
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|notification| {
            NotificationsState::is_visible_to(notification, user_id) && !notification.read
        })
        .collect();
    unread.sort_by(|a, b| b.created_at.cmp(&a.created_at));
    unread.truncate(50);
    if unread.is_empty() {
        return Ok(json!({ "user_id": user_id, "sent": false, "reason": "no unread notifications" }));
    }
    let items: Vec<Value> = unread
        .iter()
        .map(|notification| {
            json!({
                "id": notification.id,
                "title": notification.title,
                "body": notification.body,
                "kind": notification.kind,
                "created_at": notification.created_at.to_rfc3339(),
            })
        })
        .collect();
    let mut variables: HashMap<String, Value> = HashMap::new();
    variables.insert("user_id".to_string(), Value::String(user_id.to_string()));
    variables.insert(
        "count".to_string(),
        Value::from(unread.len() as u64),
    );
    variables.insert("notifications".to_string(), Value::Array(items.clone()));
    let (title, body, connection_id, email_template_id) = match state.template_for("digest", "en") {
        Some(template) => (
            render_template_text(&template.title_template, &variables),
            render_template_text(&template.body_template, &variables),
            template.email_connection_id.clone(),
            template.email_template_id.clone(),
        ),
        None => {
            let lines: Vec<String> = unread
                .iter()
                .map(|notification| format!("- {}: {}", notification.title, notification.body))
                .collect();
            (
                format!("Your notification digest ({} unread)", unread.len()),
                format!(
                    "You have {} unread notification(s):\n{}",
                    unread.len(),
                    lines.join("\n")
                ),
                None,
                None,
            )
        }
    };
    // Digest delivery respects the user's channels: email only when
    // the user has the email channel enabled.
    let preferences = state.preferences_for(user_id);
    if !preferences.channels.iter().any(|channel| channel == "email") {
        return Ok(json!({
            "user_id": user_id,
            "sent": false,
            "reason": "email channel is disabled for this user",
            "unread": unread.len(),
        }));
    }
    let Some(to) = email.map(str::to_string) else {
        return Ok(json!({
            "user_id": user_id,
            "sent": false,
            "reason": "no recipient email address was supplied",
            "unread": unread.len(),
        }));
    };
    // Send through the shared chain (Email Providers first, Mail
    // Manager fallback) and record the attempt, so a digest that
    // cannot be delivered is visible instead of silent.
    let outcome = send_email(
        &state,
        &to,
        &title,
        &body,
        email_template_id.as_deref(),
        &variables,
        connection_id,
        json!({ "digest": "true", "user_id": user_id }),
    );
    if outcome.queued {
        state.record_delivery(
            None,
            "email",
            "queued",
            Some(outcome.via.to_string()),
            outcome.job_id,
            Some(to.clone()),
            Some(title.clone()),
            email_template_id.clone(),
            None,
            1,
        );
        state.emit(
            "notification.digest.sent",
            json!({
                "user_id": user_id,
                "unread": unread.len(),
                "via": outcome.via,
                "sent_at": now().to_rfc3339(),
            }),
        );
        Ok(json!({
            "user_id": user_id,
            "sent": true,
            "via": outcome.via,
            "unread": unread.len(),
            "notifications": items,
        }))
    } else {
        let reason = outcome
            .error
            .unwrap_or_else(|| "digest email was not queued".to_string());
        let attempt = state.record_delivery(
            None,
            "email",
            "failed",
            Some(outcome.via.to_string()),
            None,
            Some(to.clone()),
            Some(title.clone()),
            email_template_id.clone(),
            Some(reason.clone()),
            1,
        );
        state.emit(
            "notification.delivery.failed",
            json!({
                "attempt_id": attempt.id,
                "notification_id": Value::Null,
                "channel": "email",
                "digest": true,
                "user_id": user_id,
                "error": reason,
                "attempts": 1,
                "retryable": true,
            }),
        );
        Ok(json!({
            "user_id": user_id,
            "sent": false,
            "reason": reason,
            "unread": unread.len(),
            "notifications": items,
        }))
    }
}

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

/// Ownership check for single-notification actions. The acting
/// user id comes from the `user_id` query parameter when the caller
/// supplies one; broadcast notifications (`user_id` "all") may be
/// acted on by any user. When no user id is supplied the check is
/// skipped (legacy callers), so full enforcement needs the caller --
/// normally the auth layer -- to pass the acting user id.
fn check_notification_access(
    notification: &Notification,
    user_id: Option<&str>,
) -> UnifiedResult<()> {
    match user_id {
        Some(user_id)
            if !user_id.trim().is_empty()
                && notification.user_id != "all"
                && notification.user_id != user_id =>
        {
            Err(UnifiedError::forbidden(
                "Notification belongs to another user",
            ))
        }
        _ => Ok(()),
    }
}

async fn list_notifications(
    state: web::Data<Arc<NotificationsState>>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let params = query.into_inner();
    let mut items: Vec<Notification> = state
        .notifications
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|notification| {
            matches_search(
                &format!("{} {}", notification.title, notification.body),
                &params,
            ) && params
                .filters
                .get("user_id")
                .is_none_or(|user_id| NotificationsState::is_visible_to(notification, user_id))
                && params.filters.get("kind").is_none_or(|kind| notification.kind == *kind)
                && params.filters.get("category").is_none_or(|category| notification.category == *category)
                && params.filters.get("module").is_none_or(|module| notification.module == *module)
                && params.filters.get("priority").is_none_or(|priority| notification.priority == *priority)
                && params.filters.get("audience").is_none_or(|audience| notification.audience == *audience)
                && params.filters.get("read").is_none_or(|read| {
                    notification.read == (*read == "true" || *read == "1")
                })
        })
        .collect();
    items.sort_by(|a, b| b.created_at.cmp(&a.created_at));
    if params.normalized().order == "asc" {
        items.reverse();
    }
    let page = paginate(items, &params);
    json::success_response(page, 200)
}

async fn create_notification(
    state: web::Data<Arc<NotificationsState>>,
    body: web::Json<CreateNotification>,
) -> HttpResponse {
    let result = state.create_notification(body.into_inner());
    respond_created(result)
}

async fn get_notification(
    state: web::Data<Arc<NotificationsState>>,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let user_id = query.filters.get("user_id").cloned();
    let result: UnifiedResult<Notification> = (|| {
        let notification = state
            .notifications
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Notification not found"))?;
        check_notification_access(&notification, user_id.as_deref())?;
        Ok(notification)
    })();
    json::respond(result, false)
}

async fn delete_notification(
    state: web::Data<Arc<NotificationsState>>,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let user_id = query.filters.get("user_id").cloned();
    let result: UnifiedResult<Value> = (|| {
        let existing = state
            .notifications
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Notification not found"))?;
        check_notification_access(&existing, user_id.as_deref())?;
        state
            .notifications
            .remove(path.as_str())
            .map(|_| json!({ "deleted": true, "id": path.as_str() }))
            .ok_or_else(|| UnifiedError::not_found("Notification not found"))
    })();
    json::respond(result, false)
}

async fn mark_read(
    state: web::Data<Arc<NotificationsState>>,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let user_id = query.filters.get("user_id").cloned();
    let result: UnifiedResult<Notification> = (|| {
        let existing = state
            .notifications
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Notification not found"))?;
        check_notification_access(&existing, user_id.as_deref())?;
        let mut notification = state
            .notifications
            .get_mut(path.as_str())
            .ok_or_else(|| UnifiedError::not_found("Notification not found"))?;
        notification.read = true;
        Ok(notification.clone())
    })();
    if let Ok(notification) = &result {
        state.emit(
            "notification.read",
            json!({
                "notification_id": notification.id,
                "user_id": notification.user_id,
                "read": true,
                "read_at": now().to_rfc3339(),
            }),
        );
    }
    json::respond(result, false)
}

async fn mark_unread(
    state: web::Data<Arc<NotificationsState>>,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let user_id = query.filters.get("user_id").cloned();
    let result: UnifiedResult<Notification> = (|| {
        let existing = state
            .notifications
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Notification not found"))?;
        check_notification_access(&existing, user_id.as_deref())?;
        let mut notification = state
            .notifications
            .get_mut(path.as_str())
            .ok_or_else(|| UnifiedError::not_found("Notification not found"))?;
        notification.read = false;
        Ok(notification.clone())
    })();
    if let Ok(notification) = &result {
        state.emit(
            "notification.read",
            json!({
                "notification_id": notification.id,
                "user_id": notification.user_id,
                "read": false,
                "read_at": now().to_rfc3339(),
            }),
        );
    }
    json::respond(result, false)
}

async fn mark_all_read(
    state: web::Data<Arc<NotificationsState>>,
    body: web::Json<ReadAllRequest>,
) -> HttpResponse {
    let result: UnifiedResult<Value> = (|| {
        let user_id = body.into_inner().user_id;
        if user_id.trim().is_empty() {
            return Err(UnifiedError::bad_request("user_id must not be empty"));
        }
        let mut marked = 0u64;
        for mut entry in state.notifications.iter_mut() {
            if NotificationsState::is_visible_to(entry.value(), &user_id) && !entry.value().read {
                entry.value_mut().read = true;
                marked += 1;
            }
        }
        Ok(json!({ "user_id": user_id, "marked_read": marked }))
    })();
    if let Ok(value) = &result {
        state.emit(
            "notification.read",
            json!({
                "user_id": value.get("user_id"),
                "marked_read": value.get("marked_read"),
                "read_all": true,
                "read_at": now().to_rfc3339(),
            }),
        );
    }
    json::respond(result, false)
}

async fn unread_count(
    state: web::Data<Arc<NotificationsState>>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let user_id = query
        .filters
        .get("user_id")
        .cloned()
        .unwrap_or_default();
    if user_id.trim().is_empty() {
        return json::error_response(
            &UnifiedError::bad_request("Query parameter user_id is required"),
            false,
        );
    }
    // Broadcast notifications (user_id "all") count for every user.
    let visible: Vec<Notification> = state
        .notifications
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|notification| NotificationsState::is_visible_to(notification, &user_id))
        .collect();
    let unread = visible.iter().filter(|notification| !notification.read).count() as u64;
    let total = visible.len() as u64;
    let mut by_category: HashMap<String, u64> = HashMap::new();
    for notification in visible.iter().filter(|notification| !notification.read) {
        *by_category.entry(notification.category.clone()).or_insert(0) += 1;
    }
    json::success_response(
        json!({ "user_id": user_id, "unread": unread, "total": total, "by_category": by_category }),
        200,
    )
}

async fn get_preferences(
    state: web::Data<Arc<NotificationsState>>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    // The user is selected with the `user_id` query parameter.
    let user_id = query
        .filters
        .get("user_id")
        .cloned()
        .unwrap_or_default();
    if user_id.trim().is_empty() {
        return json::error_response(
            &UnifiedError::bad_request("Query parameter user_id is required"),
            false,
        );
    }
    let preferences = state.preferences_for(&user_id);
    json::success_response(preferences, 200)
}

async fn update_preferences(
    state: web::Data<Arc<NotificationsState>>,
    query: web::Query<PaginationParams>,
    body: web::Json<UpdatePreferences>,
) -> HttpResponse {
    let result: UnifiedResult<NotificationPreferences> = (|| {
        let user_id = query
            .filters
            .get("user_id")
            .cloned()
            .unwrap_or_default();
        if user_id.trim().is_empty() {
            return Err(UnifiedError::bad_request("Query parameter user_id is required"));
        }
        let input = body.into_inner();
        let channels = input.channels;
        validate_channels(&channels)?;
        let current = state.preferences_for(&user_id);
        let categories = match input.categories {
            Some(categories) => {
                if categories.is_empty() {
                    return Err(UnifiedError::bad_request(
                        "At least one category must be enabled",
                    ));
                }
                for category in &categories {
                    if !NOTIFICATION_CATEGORIES.contains(&category.as_str()) {
                        return Err(UnifiedError::bad_request(format!(
                            "Categories must be a subset of: {}",
                            NOTIFICATION_CATEGORIES.join(", ")
                        )));
                    }
                }
                categories
            }
            None => current.categories,
        };
        let channel_by_category = match input.channel_by_category {
            Some(overrides) => {
                for (category, category_channels) in &overrides {
                    if !NOTIFICATION_CATEGORIES.contains(&category.as_str()) {
                        return Err(UnifiedError::bad_request(format!(
                            "channel_by_category keys must be a subset of: {}",
                            NOTIFICATION_CATEGORIES.join(", ")
                        )));
                    }
                    validate_channels(category_channels)?;
                }
                overrides
            }
            None => current.channel_by_category,
        };
        let digest = match input.digest {
            Some(digest) => {
                if !DIGEST_OPTIONS.contains(&digest.as_str()) {
                    return Err(UnifiedError::bad_request(format!(
                        "digest must be one of: {}",
                        DIGEST_OPTIONS.join(", ")
                    )));
                }
                digest
            }
            None => current.digest,
        };
        let preferences = NotificationPreferences {
            user_id: user_id.clone(),
            channels,
            categories,
            channel_by_category,
            digest,
            updated_at: now(),
        };
        state
            .preferences
            .insert(user_id.clone(), preferences.clone());
        state.emit(
            "notification.preferences.updated",
            json!({
                "user_id": user_id,
                "channels": preferences.channels,
                "categories": preferences.categories,
                "channel_by_category": preferences.channel_by_category,
                "digest": preferences.digest,
                "updated_at": preferences.updated_at.to_rfc3339(),
            }),
        );
        // A daily or weekly digest is delivered by the scheduler, not
        // by this module: signal the scheduler that this user now wants
        // a digest so it can pick the cadence up on its next run.
        // `immediate` and `off` need no scheduler job.
        if preferences.digest == "daily" || preferences.digest == "weekly" {
            state.emit(
                "notification.digest.requested",
                json!({
                    "user_id": user_id,
                    "digest": preferences.digest,
                }),
            );
        }
        Ok(preferences)
    })();
    json::respond(result, false)
}

async fn send_digest(
    state: web::Data<Arc<NotificationsState>>,
    body: web::Json<DigestSendRequest>,
) -> HttpResponse {
    let state: Arc<NotificationsState> = state.into_inner();
    let input = body.into_inner();
    let result = send_digest_for_user(&state, &input.user_id, input.email.as_deref());
    json::respond(result, false)
}

// ---------------------------------------------------------------------------
// Delivery and audience handlers.
// ---------------------------------------------------------------------------

async fn list_deliveries(
    state: web::Data<Arc<NotificationsState>>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let params = query.into_inner();
    let mut items: Vec<DeliveryAttempt> = state
        .deliveries
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|attempt| {
            params
                .filters
                .get("notification_id")
                .is_none_or(|id| attempt.notification_id.as_deref() == Some(id.as_str()))
                && params
                    .filters
                    .get("channel")
                    .is_none_or(|channel| attempt.channel == *channel)
                && params
                    .filters
                    .get("status")
                    .is_none_or(|status| attempt.status == *status)
                && params.filters.get("job_id").is_none_or(|job_id| {
                    attempt.job_id.as_deref() == Some(job_id.as_str())
                })
        })
        .collect();
    items.sort_by(|a, b| b.created_at.cmp(&a.created_at));
    if params.normalized().order == "asc" {
        items.reverse();
    }
    let page = paginate(items, &params);
    json::success_response(page, 200)
}

async fn list_notification_deliveries(
    state: web::Data<Arc<NotificationsState>>,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let params = query.into_inner();
    let result: UnifiedResult<PageResult<DeliveryAttempt>> = (|| {
        if !state.notifications.contains_key(path.as_str()) {
            return Err(UnifiedError::not_found("Notification not found"));
        }
        let mut items: Vec<DeliveryAttempt> = state
            .deliveries
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|attempt| attempt.notification_id.as_deref() == Some(path.as_str()))
            .collect();
        items.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        if params.normalized().order == "asc" {
            items.reverse();
        }
        Ok(paginate(items, &params))
    })();
    json::respond(result, false)
}

/// Retry one failed delivery attempt (email or webhook) under the
/// retry policy: at most `MAX_DELIVERY_ATTEMPTS` attempts, with the
/// backoff recorded as `next_retry_at`. The retry re-sends through
/// the same channel with the stored recipient, subject and template.
async fn retry_delivery(
    state: web::Data<Arc<NotificationsState>>,
    path: web::Path<(String, String)>,
) -> HttpResponse {
    let state: Arc<NotificationsState> = state.into_inner();
    let (notification_id, attempt_id) = path.into_inner();
    let result: UnifiedResult<DeliveryAttempt> = (|| {
        let notification = state
            .notifications
            .get(notification_id.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Notification not found"))?;
        // Snapshot first and drop the guard before sending: the send
        // emits hook events, and no hook handler may run while this
        // module holds a write guard on the deliveries map.
        let snapshot = state
            .deliveries
            .get(attempt_id.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Delivery attempt not found"))?;
        if snapshot.notification_id.as_deref() != Some(notification_id.as_str()) {
            return Err(UnifiedError::not_found("Delivery attempt not found"));
        }
        if snapshot.attempts >= snapshot.max_attempts {
            return Err(UnifiedError::unprocessable(
                "Delivery has reached the maximum number of attempts",
            ));
        }
        match snapshot.channel.as_str() {
            "email" => {
                let to = snapshot
                    .to
                    .clone()
                    .filter(|value| !value.trim().is_empty())
                    .ok_or_else(|| {
                        UnifiedError::bad_request("No recipient email address was recorded for this attempt")
                    })?;
                let variables: HashMap<String, Value> = HashMap::new();
                let outcome = send_email(
                    &state,
                    &to,
                    &notification.title,
                    &notification.body,
                    snapshot.email_template_id.as_deref(),
                    &variables,
                    None,
                    json!({ "notification_id": notification.id, "event": notification.kind, "retry": true }),
                );
                let mut attempt = state
                    .deliveries
                    .get_mut(attempt_id.as_str())
                    .ok_or_else(|| UnifiedError::not_found("Delivery attempt not found"))?;
                attempt.attempts += 1;
                if outcome.queued {
                    attempt.status = "queued".to_string();
                    attempt.job_id = outcome.job_id;
                    attempt.provider = Some(outcome.via.to_string());
                    attempt.error = None;
                    attempt.next_retry_at = None;
                    attempt.updated_at = now();
                    Ok(attempt.clone())
                } else {
                    let reason = outcome
                        .error
                        .unwrap_or_else(|| "email send was not queued".to_string());
                    attempt.status = "failed".to_string();
                    attempt.error = Some(reason.clone());
                    attempt.next_retry_at = next_retry_at(attempt.attempts);
                    let failed = attempt.clone();
                    drop(attempt);
                    state.emit(
                        "notification.delivery.failed",
                        json!({
                            "attempt_id": failed.id,
                            "notification_id": notification.id,
                            "channel": "email",
                            "error": reason,
                            "attempts": failed.attempts,
                            "retryable": failed.attempts < failed.max_attempts,
                        }),
                    );
                    Ok(failed)
                }
            }
            "webhook" => {
                state.emit(
                    "notification.webhook",
                    json!({
                        "event": notification.kind,
                        "notification_id": notification.id,
                        "user_id": notification.user_id,
                        "title": notification.title,
                        "body": notification.body,
                        "kind": notification.kind,
                        "retry": true,
                    }),
                );
                let mut attempt = state
                    .deliveries
                    .get_mut(attempt_id.as_str())
                    .ok_or_else(|| UnifiedError::not_found("Delivery attempt not found"))?;
                attempt.attempts += 1;
                attempt.status = "queued".to_string();
                attempt.next_retry_at = None;
                attempt.updated_at = now();
                Ok(attempt.clone())
            }
            other => Err(UnifiedError::unprocessable(format!(
                "Channel {other} is already delivered and cannot be retried"
            ))),
        }
    })();
    json::respond(result, false)
}

fn valid_audience_address(audience: &str) -> bool {
    audience.starts_with("role:")
        && audience.len() > "role:".len()
        || audience.starts_with("department:")
        && audience.len() > "department:".len()
        || audience.starts_with("segment:")
        && audience.len() > "segment:".len()
}

async fn list_audiences(
    state: web::Data<Arc<NotificationsState>>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let params = query.into_inner();
    let mut items: Vec<AudienceMembership> = state
        .audiences
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|membership| {
            matches_search(
                &format!(
                    "{} {}",
                    membership.audience,
                    membership
                        .members
                        .iter()
                        .map(|member| member.user_id.clone())
                        .collect::<Vec<_>>()
                        .join(" ")
                ),
                &params,
            )
        })
        .collect();
    items.sort_by(|a, b| a.audience.cmp(&b.audience));
    if params.normalized().order == "desc" {
        items.reverse();
    }
    let page = paginate(items, &params);
    json::success_response(page, 200)
}

async fn create_audience(
    state: web::Data<Arc<NotificationsState>>,
    body: web::Json<CreateAudience>,
) -> HttpResponse {
    let result: UnifiedResult<AudienceMembership> = (|| {
        let input = body.into_inner();
        let audience = input.audience.trim().to_string();
        if !valid_audience_address(&audience) {
            return Err(UnifiedError::bad_request(
                "audience must be role:<id>, department:<name> or segment:<name>",
            ));
        }
        let mut members: Vec<AudienceMember> = Vec::new();
        for member in input.members {
            let user_id = member.user_id.trim().to_string();
            if user_id.is_empty() || members.iter().any(|item| item.user_id == user_id) {
                continue;
            }
            members.push(AudienceMember {
                user_id,
                email: member.email.filter(|value| !value.trim().is_empty()),
            });
        }
        for user_id in input.user_ids {
            let user_id = user_id.trim().to_string();
            if user_id.is_empty() || members.iter().any(|item| item.user_id == user_id) {
                continue;
            }
            members.push(AudienceMember {
                user_id,
                email: None,
            });
        }
        if members.is_empty() {
            return Err(UnifiedError::bad_request(
                "At least one member is required",
            ));
        }
        let membership = AudienceMembership {
            audience: audience.clone(),
            members,
            updated_at: now(),
        };
        state
            .audiences
            .insert(audience, membership.clone());
        Ok(membership)
    })();
    match result {
        Ok(membership) => json::success_response(membership, 201),
        Err(error) => json::error_response(&error, false),
    }
}

async fn get_audience(
    state: web::Data<Arc<NotificationsState>>,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<AudienceMembership> = state
        .audiences
        .get(path.as_str())
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found("Audience not found"));
    json::respond(result, false)
}

async fn delete_audience(
    state: web::Data<Arc<NotificationsState>>,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<Value> = (|| {
        state
            .audiences
            .remove(path.as_str())
            .map(|_| json!({ "deleted": true, "audience": path.as_str() }))
            .ok_or_else(|| UnifiedError::not_found("Audience not found"))
    })();
    json::respond(result, false)
}

// ---------------------------------------------------------------------------
// Notification template handlers.
// ---------------------------------------------------------------------------

async fn list_templates(
    state: web::Data<Arc<NotificationsState>>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let params = query.into_inner();
    let mut items: Vec<NotificationTemplate> = state
        .templates
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|template| {
            matches_search(
                &format!("{} {} {}", template.event, template.title_template, template.body_template),
                &params,
            ) && params
                .filters
                .get("event")
                .is_none_or(|event| template.event == *event)
                && params
                    .filters
                    .get("locale")
                    .is_none_or(|locale| template.locale == *locale)
                && params
                    .filters
                    .get("channel")
                    .is_none_or(|channel| template.channels.iter().any(|c| c == channel))
        })
        .collect();
    items.sort_by(|a, b| a.event.cmp(&b.event).then(a.locale.cmp(&b.locale)));
    if params.normalized().order == "desc" {
        items.reverse();
    }
    let page = paginate(items, &params);
    json::success_response(page, 200)
}

async fn create_template(
    state: web::Data<Arc<NotificationsState>>,
    body: web::Json<CreateNotificationTemplate>,
) -> HttpResponse {
    let debug = false;
    let input = body.into_inner();
    if input.event.trim().is_empty() {
        return json::error_response(
            &UnifiedError::bad_request("Template event must not be empty"),
            debug,
        );
    }
    if input.title_template.trim().is_empty() {
        return json::error_response(
            &UnifiedError::bad_request("title_template must not be empty"),
            debug,
        );
    }
    if input.body_template.trim().is_empty() {
        return json::error_response(
            &UnifiedError::bad_request("body_template must not be empty"),
            debug,
        );
    }
    if let Err(error) = validate_channels(&input.channels) {
        return json::error_response(&error, debug);
    }
    // No email_connection_id is not an error: the provider's global
    // default connection is used when a template has none.
    let now = now();
    let template = NotificationTemplate {
        id: new_id(),
        event: input.event.trim().to_string(),
        locale: input
            .locale
            .unwrap_or_else(|| "en".to_string())
            .trim()
            .to_string(),
        title_template: input.title_template,
        body_template: input.body_template,
        channels: input.channels,
        email_connection_id: input.email_connection_id.filter(|value| !value.trim().is_empty()),
        email_template_id: input.email_template_id.filter(|value| !value.trim().is_empty()),
        created_at: now,
        updated_at: now,
    };
    state
        .templates
        .insert(template.id.clone(), template.clone());
    state.emit(
        "notification.template.created",
        json!({
            "template_id": template.id,
            "event": template.event,
            "locale": template.locale,
        }),
    );
    respond_created(Ok(template))
}

async fn get_template(
    state: web::Data<Arc<NotificationsState>>,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<NotificationTemplate> = state
        .templates
        .get(path.as_str())
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found("Notification template not found"));
    json::respond(result, false)
}

async fn update_template(
    state: web::Data<Arc<NotificationsState>>,
    path: web::Path<String>,
    body: web::Json<UpdateNotificationTemplate>,
) -> HttpResponse {
    let result: UnifiedResult<NotificationTemplate> = (|| {
        let input = body.into_inner();
        let mut template = state
            .templates
            .get_mut(path.as_str())
            .ok_or_else(|| UnifiedError::not_found("Notification template not found"))?;
        if let Some(channels) = &input.channels {
            validate_channels(channels)?;
        }
        if let Some(event) = input.event {
            if event.trim().is_empty() {
                return Err(UnifiedError::bad_request("Template event must not be empty"));
            }
            template.event = event.trim().to_string();
        }
        if let Some(locale) = input.locale {
            template.locale = locale.trim().to_string();
        }
        if let Some(title_template) = input.title_template {
            template.title_template = title_template;
        }
        if let Some(body_template) = input.body_template {
            template.body_template = body_template;
        }
        if let Some(channels) = input.channels {
            template.channels = channels;
        }
        if let Some(email_connection_id) = input.email_connection_id {
            template.email_connection_id = if email_connection_id.trim().is_empty() {
                None
            } else {
                Some(email_connection_id)
            };
        }
        if let Some(email_template_id) = input.email_template_id {
            template.email_template_id = if email_template_id.trim().is_empty() {
                None
            } else {
                Some(email_template_id)
            };
        }
        template.updated_at = now();
        Ok(template.clone())
    })();
    json::respond(result, false)
}

async fn delete_template(
    state: web::Data<Arc<NotificationsState>>,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<Value> = (|| {
        state
            .templates
            .remove(path.as_str())
            .map(|_| json!({ "deleted": true, "id": path.as_str() }))
            .ok_or_else(|| UnifiedError::not_found("Notification template not found"))
    })();
    json::respond(result, false)
}

async fn preview_template(
    state: web::Data<Arc<NotificationsState>>,
    path: web::Path<String>,
    body: web::Json<TemplateRenderInput>,
) -> HttpResponse {
    let result: UnifiedResult<Value> = (|| {
        let template = state
            .templates
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Notification template not found"))?;
        let variables = body.into_inner().variables;
        Ok(json!({
            "template_id": template.id,
            "event": template.event,
            "locale": template.locale,
            "title": render_template_text(&template.title_template, &variables),
            "body": render_template_text(&template.body_template, &variables),
            "channels": template.channels,
        }))
    })();
    json::respond(result, false)
}

async fn test_template(
    state: web::Data<Arc<NotificationsState>>,
    path: web::Path<String>,
    body: web::Json<TemplateTestInput>,
) -> HttpResponse {
    let state: Arc<NotificationsState> = state.into_inner();
    let result: UnifiedResult<Value> = (|| {
        let input = body.into_inner();
        let template = state
            .templates
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Notification template not found"))?;
        let mut variables = input.variables.clone();
        variables.insert(
            "user_id".to_string(),
            Value::String(input.user_id.clone()),
        );
        let title = render_template_text(&template.title_template, &variables);
        let body_text = render_template_text(&template.body_template, &variables);
        let notification = Notification {
            id: new_id(),
            user_id: input.user_id.clone(),
            title: title.clone(),
            body: body_text.clone(),
            kind: template.event.clone(),
            category: category_for_kind(&template.event).to_string(),
            module: module_for_kind(&template.event).to_string(),
            priority: "normal".to_string(),
            link: None,
            audience: "user".to_string(),
            read: false,
            created_at: now(),
        };
        // A test render is not stored; it only dispatches through the
        // template's channels so the operator can see the real output.
        dispatch_template_channels(&state, &template, &notification, input.email, &variables);
        Ok(json!({
            "template_id": template.id,
            "event": template.event,
            "title": title,
            "body": body_text,
            "channels": template.channels,
            "dispatched": true,
        }))
    })();
    json::respond(result, false)
}

// ---------------------------------------------------------------------------
// Hook handlers
// ---------------------------------------------------------------------------

fn on_notification_requested(state: Arc<NotificationsState>, ctx: &mut HookContext) {
    let data = ctx.data.clone();
    let kind = data
        .get("kind")
        .and_then(|v| v.as_str())
        .unwrap_or("general")
        .to_string();
    let (recipients, audience_label) = match resolve_recipients(&state, &data) {
        Ok(resolved) => resolved,
        Err(error) => {
            state.emit(
                "notification.audience.unresolved",
                json!({
                    "audience": data.get("audience"),
                    "kind": kind,
                    "error": error,
                }),
            );
            ctx.add_result(json!({ "error": error, "delivered": 0 }));
            return;
        }
    };
    // Chat mentions carry no title/body, so synthesize readable ones
    // from the kind instead of storing an empty notification.
    let title = data
        .get("title")
        .and_then(|v| v.as_str())
        .map(str::to_string)
        .unwrap_or_else(|| match kind.as_str() {
            "chat.mention" => "You were mentioned in chat".to_string(),
            "ticket" => "Ticket update".to_string(),
            "invoice" => "Invoice update".to_string(),
            "license" => "License update".to_string(),
            other => format!("Notification: {other}"),
        });
    let body = data
        .get("body")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let category = data
        .get("category")
        .and_then(|v| v.as_str())
        .filter(|value| NOTIFICATION_CATEGORIES.contains(value))
        .map(str::to_string);
    let module = data
        .get("module")
        .and_then(|v| v.as_str())
        .map(str::to_string);
    let priority = data
        .get("priority")
        .and_then(|v| v.as_str())
        .map(str::to_string);
    let link = data
        .get("link")
        .and_then(|v| v.as_str())
        .map(str::to_string);
    let email_template_id = data
        .get("email_template_id")
        .and_then(|v| v.as_str())
        .map(str::to_string);
    let mut requested_channels: Vec<String> = data
        .get("channels")
        .and_then(|v| v.as_array())
        .map(|items| {
            items
                .iter()
                .filter_map(|item| item.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    if requested_channels.is_empty() {
        if let Some(channel) = data.get("channel").and_then(|v| v.as_str()) {
            requested_channels.push(channel.to_string());
        }
    }
    let mut variables: HashMap<String, Value> = HashMap::new();
    if let Value::Object(map) = &data {
        for (key, value) in map {
            variables.insert(key.clone(), value.clone());
        }
    }
    variables.insert("title".to_string(), Value::String(title.clone()));
    variables.insert("body".to_string(), Value::String(body.clone()));
    let mut delivered = 0usize;
    let mut notification_ids: Vec<String> = Vec::new();
    for (user_id, email) in recipients {
        variables.insert("user_id".to_string(), Value::String(user_id.clone()));
        let input = CreateNotification {
            user_id: user_id.clone(),
            user_ids: Vec::new(),
            emails: Vec::new(),
            audience: Some(audience_label.clone()),
            title: title.clone(),
            body: body.clone(),
            kind: Some(kind.clone()),
            category: category.clone(),
            module: module.clone(),
            priority: priority.clone(),
            link: link.clone(),
            channels: requested_channels.clone(),
            email_template_id: email_template_id.clone(),
            email: email.clone(),
        };
        match state.create_notification(input) {
            Ok(notification) => {
                notification_ids.push(notification.id.clone());
                delivered += 1;
                render_and_dispatch(
                    &state,
                    &notification,
                    &variables,
                    email,
                    email_template_id.as_deref(),
                    &requested_channels,
                );
            }
            Err(error) => {
                ctx.add_result(json!({ "error": error.message, "user_id": user_id }));
            }
        }
    }
    ctx.add_result(json!({ "delivered": delivered, "notification_ids": notification_ids }));
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

/// Send one email through the real chain, in order:
/// `email.send_requested` (Email Providers module, per-connection id;
/// payload `{ connection_id?, to, subject, body_text?, template_id?,
/// variables?, metadata? }` -- exactly the shape its hook handler
/// answers) and, only when no provider handler queues it, the Mail
/// Manager fallback `mail.send_requested` (`{ to, subject, body_text?,
/// template_id?, variables? }`). A `template_id` hands rendering to
/// the GrapeJS Email Templates module via `template.render_requested`
/// inside the provider / mail worker; this function never renders a
/// template itself. Returns the outcome; the caller records it as a
/// delivery attempt. Nothing here is silent: when neither handler
/// answers, the outcome carries the error.
fn send_email(
    state: &Arc<NotificationsState>,
    to: &str,
    subject: &str,
    body_text: &str,
    template_id: Option<&str>,
    variables: &HashMap<String, Value>,
    connection_id: Option<String>,
    metadata: Value,
) -> EmailSendOutcome {
    let mut payload = json!({
        "connection_id": connection_id,
        "to": to,
        "subject": subject,
        "body_text": body_text,
        "metadata": metadata,
    });
    if let Some(template_id) = template_id {
        payload["template_id"] = Value::String(template_id.to_string());
        payload["variables"] =
            serde_json::to_value(variables).unwrap_or(Value::Object(Default::default()));
    }
    let provider_answer = state.bus().emit("email.send_requested", payload);
    if let Some(result) = provider_answer.results.first() {
        if result.get("error").is_none()
            && result
                .get("queued")
                .and_then(|value| value.as_bool())
                .unwrap_or(false)
        {
            return EmailSendOutcome {
                queued: true,
                job_id: result
                    .get("job_id")
                    .and_then(|value| value.as_str())
                    .map(str::to_string),
                via: "email_providers",
                error: None,
            };
        }
    }
    let mut mail_payload = json!({
        "to": to,
        "subject": subject,
        "body_text": body_text,
    });
    if let Some(template_id) = template_id {
        mail_payload["template_id"] = Value::String(template_id.to_string());
        mail_payload["variables"] =
            serde_json::to_value(variables).unwrap_or(Value::Object(Default::default()));
    }
    let mail_answer = state.bus().emit("mail.send_requested", mail_payload);
    if let Some(result) = mail_answer.results.first() {
        if result.get("error").is_none()
            && result
                .get("queued")
                .and_then(|value| value.as_bool())
                .unwrap_or(false)
        {
            return EmailSendOutcome {
                queued: true,
                job_id: result
                    .get("job_id")
                    .and_then(|value| value.as_str())
                    .map(str::to_string),
                via: "mail",
                error: None,
            };
        }
        if let Some(error) = result.get("error").and_then(|value| value.as_str()) {
            return EmailSendOutcome {
                queued: false,
                job_id: None,
                via: "mail",
                error: Some(error.to_string()),
            };
        }
    }
    EmailSendOutcome {
        queued: false,
        job_id: None,
        via: "none",
        error: Some("No email handler answered: neither an email provider nor the Mail Manager is registered".to_string()),
    }
}

/// Dispatch one stored notification through an explicit channel list
/// (used when no notification template exists for the event).
/// Email goes through `send_email` (providers first, Mail Manager
/// fallback); a webhook delivery is emitted as
/// `notification.webhook` for the webhooks module; in-app delivery
/// already happened (the notification is stored) and is recorded as
/// `delivered`. Every channel records a delivery attempt. Broadcast
/// notifications (`user_id` "all") have no single user whose
/// preferences could apply, so their channels are used as given;
/// per-recipient notifications respect the recipient's effective
/// channels (per-category override, else global). A user who disabled
/// a channel gets a `skipped` attempt for it, never a delivery.
fn dispatch_channels(
    state: &Arc<NotificationsState>,
    notification: &Notification,
    channels: &[String],
    email: Option<String>,
    email_template_id: Option<&str>,
    variables: &HashMap<String, Value>,
    connection_id: Option<String>,
) {
    let effective = if notification.user_id == "all" {
        channels.to_vec()
    } else {
        state.effective_channels(&notification.user_id, &notification.category)
    };
    let notification_id = Some(notification.id.clone());
    for channel in channels {
        match channel.as_str() {
            "email" => {
                if !effective.iter().any(|item| item == "email") {
                    state.record_delivery(
                        notification_id.clone(),
                        "email",
                        "skipped",
                        None,
                        None,
                        email.clone(),
                        Some(notification.title.clone()),
                        email_template_id.map(str::to_string),
                        Some("email channel is disabled for this user".to_string()),
                        0,
                    );
                    continue;
                }
                let Some(to) = email.clone() else {
                    let attempt = state.record_delivery(
                        notification_id.clone(),
                        "email",
                        "failed",
                        None,
                        None,
                        None,
                        Some(notification.title.clone()),
                        email_template_id.map(str::to_string),
                        Some("no recipient email address was supplied".to_string()),
                        1,
                    );
                    state.emit(
                        "notification.delivery.failed",
                        json!({
                            "attempt_id": attempt.id,
                            "notification_id": notification.id,
                            "channel": "email",
                            "error": "no recipient email address was supplied",
                            "attempts": 1,
                            "retryable": true,
                        }),
                    );
                    continue;
                };
                let outcome = send_email(
                    state,
                    &to,
                    &notification.title,
                    &notification.body,
                    email_template_id,
                    variables,
                    connection_id.clone(),
                    json!({ "notification_id": notification.id, "event": notification.kind }),
                );
                if outcome.queued {
                    state.record_delivery(
                        notification_id.clone(),
                        "email",
                        "queued",
                        Some(outcome.via.to_string()),
                        outcome.job_id,
                        Some(to),
                        Some(notification.title.clone()),
                        email_template_id.map(str::to_string),
                        None,
                        1,
                    );
                } else {
                    let reason = outcome
                        .error
                        .unwrap_or_else(|| "email send was not queued".to_string());
                    let attempt = state.record_delivery(
                        notification_id.clone(),
                        "email",
                        "failed",
                        Some(outcome.via.to_string()),
                        None,
                        Some(to),
                        Some(notification.title.clone()),
                        email_template_id.map(str::to_string),
                        Some(reason.clone()),
                        1,
                    );
                    state.emit(
                        "notification.delivery.failed",
                        json!({
                            "attempt_id": attempt.id,
                            "notification_id": notification.id,
                            "channel": "email",
                            "error": reason,
                            "attempts": 1,
                            "retryable": true,
                        }),
                    );
                }
            }
            "webhook" => {
                state.emit(
                    "notification.webhook",
                    json!({
                        "event": notification.kind,
                        "notification_id": notification.id,
                        "user_id": notification.user_id,
                        "title": notification.title,
                        "body": notification.body,
                        "kind": notification.kind,
                    }),
                );
                // The webhooks module owns the final delivery; its own
                // delivery records live there. This attempt stays
                // `queued` until a webhook listener reports back.
                // Note: no handler is registered for
                // `notification.webhook` today -- a webhook endpoint
                // must subscribe to that event for this channel to
                // deliver anywhere (see the report).
                state.record_delivery(
                    notification_id.clone(),
                    "webhook",
                    "queued",
                    None,
                    None,
                    None,
                    Some(notification.title.clone()),
                    None,
                    None,
                    1,
                );
            }
            _ => {
                // `in_app` (and anything else) needs no dispatch: the
                // notification is already stored, which IS the delivery.
                state.record_delivery(
                    notification_id.clone(),
                    "in_app",
                    "delivered",
                    None,
                    None,
                    None,
                    Some(notification.title.clone()),
                    None,
                    None,
                    1,
                );
            }
        }
    }
}

/// Dispatch a rendered notification through the channels stored on
/// its template. Email goes through the Email Providers module with
/// the template's provider connection, falling back to the Mail
/// Manager when no provider handler answers. Webhook delivery is
/// emitted as `notification.webhook` for the webhooks module to
/// deliver. In-app delivery already happened (the notification is
/// stored). The user's channel preferences still gate email: a user
/// who disabled email never receives one.
fn dispatch_template_channels(
    state: &Arc<NotificationsState>,
    template: &NotificationTemplate,
    notification: &Notification,
    email: Option<String>,
    variables: &HashMap<String, Value>,
) {
    dispatch_channels(
        state,
        notification,
        &template.channels,
        email,
        template.email_template_id.as_deref(),
        variables,
        template.email_connection_id.clone(),
    );
}

/// Render one stored notification through its event template (when
/// one exists) and dispatch it. Without a template the stored
/// title/body are dispatched through the requested channels instead.
/// `email_template_id` is the GrapeJS template id supplied with the
/// request; a notification template's own id wins over it.
fn render_and_dispatch(
    state: &Arc<NotificationsState>,
    notification: &Notification,
    variables: &HashMap<String, Value>,
    email: Option<String>,
    email_template_id: Option<&str>,
    requested_channels: &[String],
) {
    let template = state.template_for(&notification.kind, "en");
    match template {
        Some(template) => {
            let mut rendered = notification.clone();
            rendered.title = render_template_text(&template.title_template, variables);
            rendered.body = render_template_text(&template.body_template, variables);
            state
                .notifications
                .insert(rendered.id.clone(), rendered.clone());
            dispatch_template_channels(state, &template, &rendered, email, variables);
        }
        None => {
            let channels: Vec<String> = if requested_channels.is_empty() {
                vec!["in_app".to_string()]
            } else {
                requested_channels.to_vec()
            };
            dispatch_channels(
                state,
                notification,
                &channels,
                email,
                email_template_id,
                variables,
                None,
            );
        }
    }
}

/// Resolve the recipients of a `notification.requested` payload.
/// Returns the resolved recipients as `(user_id, email)` pairs plus
/// the audience label stored on each notification. Resolution order:
/// explicit `user_ids` first, then broadcast (`all` / `users` / a
/// `user_id` of "all"), then a registered role / department / segment
/// audience (`admins` resolves through the registered `role:admin`
/// audience), then a single `user_id`. An unregistered role /
/// department / segment audience is an error: the notification is
/// delivered to nobody rather than to the wrong people.
fn resolve_recipients(
    state: &Arc<NotificationsState>,
    data: &Value,
) -> Result<(Vec<(String, Option<String>)>, String), String> {
    let clean = |value: &Value| {
        value
            .as_str()
            .map(str::trim)
            .filter(|text| !text.is_empty())
            .map(str::to_string)
    };
    let user_ids: Vec<String> = data
        .get("user_ids")
        .and_then(|value| value.as_array())
        .map(|items| items.iter().filter_map(clean).collect())
        .unwrap_or_default();
    let emails: Vec<String> = data
        .get("emails")
        .and_then(|value| value.as_array())
        .map(|items| items.iter().filter_map(clean).collect())
        .unwrap_or_default();
    let single_email = data.get("email").and_then(clean);
    let audience = data
        .get("audience")
        .and_then(|value| value.as_str())
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(str::to_string)
        .or_else(|| {
            data.get("role")
                .and_then(|value| value.as_str())
                .map(|role| format!("role:{}", role.trim()))
        })
        .or_else(|| {
            data.get("department")
                .and_then(|value| value.as_str())
                .map(|department| format!("department:{}", department.trim()))
        })
        .or_else(|| {
            data.get("segment")
                .and_then(|value| value.as_str())
                .map(|segment| format!("segment:{}", segment.trim()))
        });
    let single_user = data
        .get("user_id")
        .and_then(|value| value.as_str())
        .map(str::trim)
        .unwrap_or("")
        .to_string();
    if !user_ids.is_empty() {
        let mut recipients: Vec<(String, Option<String>)> = Vec::new();
        let mut seen: Vec<String> = Vec::new();
        for (index, user_id) in user_ids.into_iter().enumerate() {
            if seen.contains(&user_id) {
                continue;
            }
            seen.push(user_id.clone());
            let email = emails
                .get(index)
                .cloned()
                .or_else(|| single_email.clone());
            recipients.push((user_id, email));
        }
        let label = audience.unwrap_or_else(|| "user".to_string());
        return Ok((recipients, label));
    }
    if audience.as_deref() == Some("all")
        || audience.as_deref() == Some("users")
        || single_user == "all"
    {
        return Ok((vec![("all".to_string(), None)], "all".to_string()));
    }
    if let Some(audience) = audience {
        let registry_key = if audience == "admins" {
            "role:admin".to_string()
        } else {
            audience.clone()
        };
        if audience == "admins"
            || audience.starts_with("role:")
            || audience.starts_with("department:")
            || audience.starts_with("segment:")
        {
            return match state.audience_members(&registry_key) {
                Some(members) if !members.is_empty() => Ok((
                    members
                        .into_iter()
                        .map(|member| (member.user_id, member.email))
                        .collect(),
                    registry_key,
                )),
                _ => Err(format!(
                    "Audience {audience} has no registered members; register them with POST /api/v1/notifications/audiences before sending"
                )),
            };
        }
        return Err(format!("Unknown audience {audience}"));
    }
    if !single_user.is_empty() {
        return Ok((vec![(single_user, single_email)], "user".to_string()));
    }
    Err("A notification.requested payload needs user_id, user_ids or audience".to_string())
}

// ---------------------------------------------------------------------------
// Failure-event listeners.
//
// The centre subscribes to the verified failure events so alerts land
// here instead of only in logs:
//
//   - `security.auth.failed` — emitted by the hardening layer
//     (`crate::core::hardening::security_events::AUTH_FAILED`) when a
//     failed-auth cluster reaches its threshold. Payload: `key`, `ip`,
//     `account`, `count`, `threshold` (no `user_id`). The notification
//     goes to the `account` when one is present, otherwise `system`.
//   - `cluster.sync.failed` — emitted by the clusters module
//     (`CLUSTER_SYNC_FAILED`) with `plan_id`, `cluster_id`, `node_id`,
//     `direction`, `errors`. The clusters module already forwards the
//     same failure through `notification.requested` (user `system`),
//     so this listener only creates a notification when the payload
//     carries an explicit `user_id`: without one, creating here would
//     duplicate the notification the clusters module already sent.
//   - `payment.failed` — defined by the payments module
//     (`PAYMENT_FAILED`) for chargeback webhooks, which also forward a
//     `notification.requested`. No emit call site for the raw event
//     exists in the tree yet, so this listener is currently dormant;
//     it creates a billing notification when a payload with a
//     `user_id` arrives. The same user-id guard prevents duplicates
//     from emitters that already forward via `notification.requested`.
//
// All three listeners respect the target user's category preferences
// through `notify_for_category` (security / sync / billing).

fn on_security_auth_failed(state: Arc<NotificationsState>, ctx: &mut HookContext) {
    let data = ctx.data.clone();
    let account = data
        .get("account")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let user_id = if account.is_empty() {
        "system".to_string()
    } else {
        account
    };
    let ip = data.get("ip").and_then(|v| v.as_str()).unwrap_or("unknown");
    let count = data.get("count").and_then(|v| v.as_u64()).unwrap_or(0);
    let threshold = data
        .get("threshold")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    let title = "Repeated failed sign-in attempts detected".to_string();
    let body = format!(
        "{count} failed sign-in attempts (threshold {threshold}) from IP {ip} for account \"{user_id}\""
    );
    match state.notify_for_category(&user_id, "security", title, body, "security.auth.failed") {
        Some(notification) => {
            ctx.add_result(json!({ "notified": true, "notification_id": notification.id }));
        }
        None => {
            ctx.add_result(json!({ "notified": false, "reason": "security category disabled" }));
        }
    }
}

fn on_cluster_sync_failed(state: Arc<NotificationsState>, ctx: &mut HookContext) {
    let data = ctx.data.clone();
    let Some(user_id) = data
        .get("user_id")
        .and_then(|v| v.as_str())
        .map(str::to_string)
    else {
        // No explicit user: the clusters module already forwards this
        // failure via `notification.requested`, so do not duplicate it.
        ctx.add_result(json!({ "notified": false, "reason": "no user_id; already forwarded via notification.requested" }));
        return;
    };
    let cluster_id = data
        .get("cluster_id")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown");
    let plan_id = data
        .get("plan_id")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown");
    let errors = data
        .get("errors")
        .and_then(|v| v.as_array())
        .map(|items| {
            items
                .iter()
                .filter_map(|item| item.as_str())
                .collect::<Vec<_>>()
                .join("; ")
        })
        .unwrap_or_default();
    let title = "Cluster sync failed".to_string();
    let body = format!("Sync plan {plan_id} for cluster {cluster_id} failed: {errors}");
    match state.notify_for_category(&user_id, "sync", title, body, "cluster.sync.failed") {
        Some(notification) => {
            ctx.add_result(json!({ "notified": true, "notification_id": notification.id }));
        }
        None => {
            ctx.add_result(json!({ "notified": false, "reason": "sync category disabled" }));
        }
    }
}

fn on_payment_failed(state: Arc<NotificationsState>, ctx: &mut HookContext) {
    let data = ctx.data.clone();
    let Some(user_id) = data
        .get("user_id")
        .or_else(|| data.get("customer_id"))
        .and_then(|v| v.as_str())
        .map(str::to_string)
    else {
        // No explicit user: chargeback webhooks already forward via
        // `notification.requested`, so do not duplicate it.
        ctx.add_result(json!({ "notified": false, "reason": "no user_id; already forwarded via notification.requested" }));
        return;
    };
    let payment_id = data
        .get("payment_id")
        .or_else(|| data.get("id"))
        .and_then(|v| v.as_str())
        .unwrap_or("unknown");
    let reason = data
        .get("reason")
        .or_else(|| data.get("error"))
        .and_then(|v| v.as_str())
        .unwrap_or("payment failed");
    let title = "Payment failed".to_string();
    let body = format!("Payment {payment_id} failed: {reason}");
    match state.notify_for_category(&user_id, "billing", title, body, "payment.failed") {
        Some(notification) => {
            ctx.add_result(json!({ "notified": true, "notification_id": notification.id }));
        }
        None => {
            ctx.add_result(json!({ "notified": false, "reason": "billing category disabled" }));
        }
    }
}

// ---------------------------------------------------------------------------
// Domain-event listeners.
//
// Every other module's important events that were NOT already covered
// get a listener here. The event names below are the ones the emitting
// modules actually emit (verified by reading them); no name was
// invented. Coverage decisions:
//
// Covered by a direct listener (the emitter does NOT forward to
// `notification.requested` itself):
//   order.paid (store), invoice.paid (billing),
//   payment.captured / payment.refunded (payments),
//   store.order.completed / refunded / cancelled (store),
//   ticket.commented (ticket replied) / ticket.assigned (tickets),
//   backup.failed / completed / restored (backup),
//   cluster.health.changed (node unhealthy) / cluster.node.unhealthy
//   (contract event) / lb.backend.unhealthy (load balancer) -- the
//   "server down" path; no generic `server.down` event exists,
//   cluster.sync.completed / conflict (clusters),
//   license.issued / activated / deactivated / suspended / revoked
//   (licensing; payloads carry no user id, so these go to `system`),
//   statuspage.incident.created / resolved (status page),
//   vcs.sync.failed (version control), webhook.failed (webhooks),
//   security.bruteforce.locked / security.ip.flagged (hardening).
//
// Deliberately NOT subscribed here because the emitter already
// forwards through `notification.requested` (a second listener would
// duplicate the notification):
//   payment.failed (payments notify on every failure path),
//   payment.chargeback (payments notify the customer),
//   cluster.sync.failed (clusters forward it),
//   ssl.expiring / ssl.renew_failed (certificates notify `system`),
//   ticket.created (tickets notify the owner),
//   chat mentions (chat notifies the mentioned user),
//   maintenance.started / ended (maintenance notifies `all`),
//   announcement.published (announcements fan out on publish).
// `payment.completed` is also not subscribed: the store emits it in
// the same transition as `order.paid`, so listening to both would
// create two notifications for one payment.
//
// Gaps found while auditing (no event exists to subscribe to):
//   - No `license.expiring` event exists anywhere in the tree: the
//     licensing module has no expiry scan, so a license can expire
//     without any hook firing. The licensing owner needs an expiry
//     scan that emits `license.expiring` (with `license_id`,
//     `license_key`, `days_remaining`); this module will then pick it
//     up with the same pattern as `ssl.expiring`.
//   - The service monitor probes services over HTTP but emits no
//     event when one goes down; `cluster.health.changed` and
//     `lb.backend.unhealthy` above are the only down-events today.

fn domain_handler(state: Arc<NotificationsState>, event: &'static str) -> HookHandler {
    Arc::new(move |ctx: &mut HookContext| on_domain_event(state.clone(), event, ctx))
}

/// Routing for one domain event: (category, priority, needs a user).
/// Events that need a user are skipped (with a result explaining why)
/// when the payload carries no usable user id; the rest go to the
/// `system` user.
fn event_routing(event: &str) -> (&'static str, &'static str, bool) {
    match event {
        "order.paid" | "store.order.completed" | "store.order.refunded" | "store.order.cancelled" => {
            ("orders", "normal", true)
        }
        "invoice.paid" => ("billing", "normal", true),
        "payment.captured" | "payment.refunded" => ("payments", "normal", true),
        "ticket.commented" | "ticket.assigned" => ("tickets", "normal", true),
        "backup.failed" => ("backup", "high", false),
        "backup.completed" | "backup.restored" => ("backup", "normal", false),
        "cluster.health.changed" | "cluster.node.unhealthy" | "lb.backend.unhealthy" => {
            ("monitoring", "high", false)
        }
        "cluster.sync.completed" => ("sync", "normal", false),
        "cluster.sync.conflict" => ("sync", "high", false),
        "license.issued" | "license.activated" | "license.deactivated" => {
            ("licensing", "normal", false)
        }
        "license.suspended" | "license.revoked" => ("licensing", "high", false),
        "statuspage.incident.created" => ("monitoring", "high", false),
        "statuspage.incident.resolved" => ("monitoring", "normal", false),
        "vcs.sync.failed" => ("sync", "high", false),
        "webhook.failed" => ("system", "high", false),
        "security.bruteforce.locked" | "security.ip.flagged" => ("security", "high", false),
        _ => ("system", "normal", false),
    }
}

fn event_text(data: &Value, key: &str) -> String {
    data.get(key)
        .and_then(|value| value.as_str())
        .unwrap_or("unknown")
        .to_string()
}

fn event_nested<'a>(data: &'a Value, key: &str) -> Option<&'a Value> {
    data.get(key).filter(|value| value.is_object())
}

/// Pick the recipient user id out of a domain-event payload. The
/// payload shapes are the emitters' real ones: an order / invoice is
/// the object itself (`user_id`), a refunded store order nests it
/// under `order`, a payment carries `customer_id`, a ticket carries
/// `user_id` (owner) or `assigned_to` (agent).
fn event_user_id(event: &str, data: &Value) -> Option<String> {
    let pick = |value: &Value| {
        value
            .get("user_id")
            .or_else(|| value.get("customer_id"))
            .and_then(|item| item.as_str())
            .filter(|text| !text.trim().is_empty())
            .map(str::to_string)
    };
    match event {
        "ticket.assigned" => data
            .get("assigned_to")
            .and_then(|value| value.as_str())
            .filter(|text| !text.trim().is_empty())
            .map(str::to_string),
        "ticket.commented" => pick(data),
        "store.order.refunded" => event_nested(data, "order")
            .and_then(pick)
            .or_else(|| pick(data)),
        "security.bruteforce.locked" => {
            if data.get("kind").and_then(|value| value.as_str()) == Some("account") {
                data.get("key")
                    .and_then(|value| value.as_str())
                    .filter(|text| !text.trim().is_empty())
                    .map(str::to_string)
            } else {
                None
            }
        }
        _ => pick(data).or_else(|| event_nested(data, "order").and_then(pick)),
    }
}

fn truncate_text(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_string();
    }
    let kept: String = text.chars().take(max_chars).collect();
    format!("{kept}...")
}

/// Human-readable title and body for one domain event, built from
/// the fields the emitter actually puts in the payload.
fn event_message(event: &str, data: &Value) -> (String, String) {
    let order = event_nested(data, "order").unwrap_or(data);
    let id = event_text(data, "id");
    match event {
        "order.paid" => (
            "Order paid".to_string(),
            format!(
                "Order {} for {} {} was paid.",
                event_text(data, "id"),
                event_text(data, "total"),
                event_text(data, "currency")
            ),
        ),
        "invoice.paid" => (
            "Invoice paid".to_string(),
            format!(
                "Invoice {} was paid in full.",
                event_text(data, "number")
            ),
        ),
        "payment.captured" => (
            "Payment captured".to_string(),
            format!(
                "Payment of {} {} for order {} was captured.",
                event_text(data, "amount_minor"),
                event_text(data, "currency"),
                event_text(data, "order_id")
            ),
        ),
        "payment.refunded" => (
            "Payment refunded".to_string(),
            format!(
                "Payment of {} {} for order {} was refunded.",
                event_text(data, "amount_minor"),
                event_text(data, "currency"),
                event_text(data, "order_id")
            ),
        ),
        "store.order.completed" => (
            "Order completed".to_string(),
            format!("Order {} is complete.", event_text(order, "id")),
        ),
        "store.order.refunded" => (
            "Order refunded".to_string(),
            format!("Order {} was refunded.", event_text(order, "id")),
        ),
        "store.order.cancelled" => (
            "Order cancelled".to_string(),
            format!("Order {} was cancelled.", event_text(order, "id")),
        ),
        "ticket.commented" => {
            let last = data
                .get("comments")
                .and_then(|value| value.as_array())
                .and_then(|items| items.last());
            let comment = last
                .and_then(|item| item.get("body"))
                .and_then(|value| value.as_str())
                .unwrap_or("");
            (
                format!("Ticket reply: {}", event_text(data, "title")),
                format!(
                    "New reply on ticket {}: {}",
                    event_text(data, "id"),
                    truncate_text(comment, 200)
                ),
            )
        }
        "ticket.assigned" => (
            "Ticket assigned to you".to_string(),
            format!(
                "Ticket {} ({}) was assigned to you.",
                event_text(data, "id"),
                event_text(data, "title")
            ),
        ),
        "backup.failed" => (
            format!("Backup failed: {}", event_text(data, "job_name")),
            format!(
                "Backup job {} ({}) failed: {}",
                event_text(data, "job_name"),
                event_text(data, "job_id"),
                event_text(data, "error")
            ),
        ),
        "backup.completed" => (
            format!("Backup completed: {}", event_text(data, "job_name")),
            format!(
                "Backup job {} ({}) completed.",
                event_text(data, "job_name"),
                event_text(data, "job_id")
            ),
        ),
        "backup.restored" => (
            format!("Backup restored: {}", event_text(data, "job_name")),
            format!(
                "Backup job {} ({}) was restored.",
                event_text(data, "job_name"),
                event_text(data, "job_id")
            ),
        ),
        "cluster.health.changed" => (
            "Cluster node unhealthy".to_string(),
            format!(
                "Node {} in cluster {} is {} ({} consecutive failures).",
                event_text(data, "node_id"),
                event_text(data, "cluster_id"),
                event_text(data, "health"),
                data.get("consecutive_failures")
                    .and_then(|value| value.as_u64())
                    .unwrap_or(0)
            ),
        ),
        "cluster.node.unhealthy" => (
            "Cluster node unhealthy".to_string(),
            format!(
                "Node {} in cluster {} is unhealthy.",
                event_text(data, "node_id"),
                event_text(data, "cluster_id")
            ),
        ),
        "lb.backend.unhealthy" => (
            "Load balancer backend unhealthy".to_string(),
            format!(
                "Backend {} ({}) is unhealthy.",
                event_text(data, "backend_name"),
                event_text(data, "address")
            ),
        ),
        "cluster.sync.completed" => (
            "Cluster sync completed".to_string(),
            format!(
                "Sync plan {} for cluster {} completed.",
                event_text(data, "plan_id"),
                event_text(data, "cluster_id")
            ),
        ),
        "cluster.sync.conflict" => (
            "Cluster sync conflict".to_string(),
            format!(
                "Sync plan {} for cluster {} has conflicts that need review.",
                event_text(data, "plan_id"),
                event_text(data, "cluster_id")
            ),
        ),
        "license.issued" => (
            "License issued".to_string(),
            format!(
                "License {} was issued for {}.",
                event_text(data, "license_key"),
                event_text(data, "product")
            ),
        ),
        "license.activated" => (
            "License activated".to_string(),
            format!("License {} was activated.", event_text(data, "license_key")),
        ),
        "license.deactivated" => (
            "License deactivated".to_string(),
            format!("License {} was deactivated.", event_text(data, "license_key")),
        ),
        "license.suspended" => (
            "License suspended".to_string(),
            format!("License {} was suspended.", event_text(data, "license_key")),
        ),
        "license.revoked" => (
            "License revoked".to_string(),
            format!("License {} was revoked.", event_text(data, "license_key")),
        ),
        "statuspage.incident.created" => (
            format!("Incident: {}", event_text(data, "title")),
            format!(
                "A status page incident was created (severity {}).",
                event_text(data, "severity")
            ),
        ),
        "statuspage.incident.resolved" => (
            format!("Incident resolved: {}", event_text(data, "title")),
            "The status page incident was resolved.".to_string(),
        ),
        "vcs.sync.failed" => (
            "Version control sync failed".to_string(),
            format!(
                "Sync for connection {} ({}) failed: {}",
                event_text(data, "connection_id"),
                event_text(data, "provider"),
                event_text(data, "error")
            ),
        ),
        "webhook.failed" => (
            "Webhook delivery failed".to_string(),
            format!(
                "Webhook {} to endpoint {} failed: {}",
                event_text(data, "event"),
                event_text(data, "endpoint_id"),
                event_text(data, "error")
            ),
        ),
        "security.bruteforce.locked" => (
            "Account locked after failed sign-ins".to_string(),
            format!(
                "Sign-in attempts for {} {} were locked for {} seconds after {} lockouts.",
                event_text(data, "kind"),
                event_text(data, "key"),
                data.get("lockout_secs")
                    .and_then(|value| value.as_u64())
                    .unwrap_or(0),
                data.get("lock_count")
                    .and_then(|value| value.as_u64())
                    .unwrap_or(0)
            ),
        ),
        "security.ip.flagged" => (
            "IP address flagged".to_string(),
            format!(
                "IP {} was flagged (score {}): {}",
                event_text(data, "ip"),
                data.get("score")
                    .and_then(|value| value.as_u64())
                    .unwrap_or(0),
                event_text(data, "reason")
            ),
        ),
        _ => (
            format!("Notification: {event}"),
            format!("Event {event} occurred (id {id})."),
        ),
    }
}

fn on_domain_event(state: Arc<NotificationsState>, event: &str, ctx: &mut HookContext) {
    let data = ctx.data.clone();
    // A node that recovered (or is still healthy) needs no alert:
    // only the unhealthy transition notifies.
    if event == "cluster.health.changed" {
        let health = data
            .get("health")
            .and_then(|value| value.as_str())
            .unwrap_or("")
            .to_lowercase();
        if !matches!(health.as_str(), "unhealthy" | "down" | "failed" | "critical") {
            ctx.add_result(json!({ "notified": false, "reason": "node is not unhealthy; no alert" }));
            return;
        }
    }
    // A ticket reply notifies the ticket owner -- unless the owner
    // wrote the reply themselves.
    if event == "ticket.commented" {
        let owner = data.get("user_id").and_then(|value| value.as_str());
        let author = data
            .get("comments")
            .and_then(|value| value.as_array())
            .and_then(|items| items.last())
            .and_then(|item| item.get("author_id"))
            .and_then(|value| value.as_str());
        if let (Some(owner), Some(author)) = (owner, author) {
            if owner == author {
                ctx.add_result(json!({ "notified": false, "reason": "comment author is the ticket owner" }));
                return;
            }
        }
    }
    let (category, mut priority, needs_user) = event_routing(event);
    if event == "statuspage.incident.created" {
        let severity = data
            .get("severity")
            .and_then(|value| value.as_str())
            .unwrap_or("");
        if !matches!(severity, "major" | "critical") {
            priority = "normal";
        }
    }
    let user_id = match event_user_id(event, &data) {
        Some(user_id) => user_id,
        None if needs_user => {
            ctx.add_result(json!({ "notified": false, "reason": "no user_id in payload" }));
            return;
        }
        None => "system".to_string(),
    };
    let (title, body) = event_message(event, &data);
    match state.store_notification(
        &user_id,
        title,
        body,
        event,
        category,
        priority,
        None,
        "user",
    ) {
        Some(notification) => {
            // Dispatch through the event template when one exists and
            // an email address was supplied (or the template needs no
            // email). Without an address, email is skipped rather than
            // recorded as failed: system events have no recipient
            // address by design, and in-app delivery already happened.
            let email = data.get("email").and_then(|value| value.as_str()).map(str::to_string);
            let template = state.template_for(event, "en");
            let dispatchable = match &template {
                Some(template) => !template.channels.iter().any(|channel| channel == "email") || email.is_some(),
                None => false,
            };
            if dispatchable {
                let mut variables: HashMap<String, Value> = HashMap::new();
                if let Value::Object(map) = &data {
                    for (key, value) in map {
                        variables.insert(key.clone(), value.clone());
                    }
                }
                variables.insert(
                    "title".to_string(),
                    Value::String(notification.title.clone()),
                );
                variables.insert(
                    "body".to_string(),
                    Value::String(notification.body.clone()),
                );
                variables.insert(
                    "user_id".to_string(),
                    Value::String(notification.user_id.clone()),
                );
                render_and_dispatch(&state, &notification, &variables, email, None, &[]);
            }
            ctx.add_result(json!({ "notified": true, "notification_id": notification.id }));
        }
        None => {
            ctx.add_result(json!({ "notified": false, "reason": format!("{category} category disabled") }));
        }
    }
}

/// Update a recorded delivery attempt from a provider outcome:
/// `email.sent` / `mail.sent` mark it delivered (with the provider
/// message id when one is reported); `email.failed` / `mail.failed`
/// mark it failed with the error and the next retry time from the
/// retry policy. The match is by `job_id`, which `send_email`
/// records when the send is queued. An unmatched `job_id` (a send
/// that did not go through this module) is reported, not invented.
fn on_email_delivery(state: Arc<NotificationsState>, delivered: bool, ctx: &mut HookContext) {
    let data = ctx.data.clone();
    let job_id = data
        .get("job_id")
        .and_then(|value| value.as_str())
        .unwrap_or("")
        .to_string();
    let mut matched: Option<String> = None;
    let mut failed_report: Option<Value> = None;
    for mut entry in state.deliveries.iter_mut() {
        if entry.value().job_id.as_deref() == Some(job_id.as_str()) && !job_id.is_empty() {
            let attempt = entry.value_mut();
            if delivered {
                attempt.status = "delivered".to_string();
                attempt.provider_message_id = data
                    .get("provider_message_id")
                    .and_then(|value| value.as_str())
                    .map(str::to_string)
                    .or_else(|| attempt.provider_message_id.clone());
                attempt.error = None;
                attempt.next_retry_at = None;
            } else {
                let reason = data
                    .get("error")
                    .and_then(|value| value.as_str())
                    .unwrap_or("delivery failed")
                    .to_string();
                attempt.status = "failed".to_string();
                attempt.error = Some(reason.clone());
                attempt.next_retry_at = next_retry_at(attempt.attempts);
                // Emit after the map guard is dropped (below): a hook
                // handler must never run while this module holds a
                // write guard on the same map.
                failed_report = Some(json!({
                    "attempt_id": attempt.id,
                    "notification_id": attempt.notification_id,
                    "channel": attempt.channel,
                    "error": reason,
                    "attempts": attempt.attempts,
                    "retryable": attempt.attempts < attempt.max_attempts,
                }));
            }
            attempt.updated_at = now();
            matched = Some(attempt.id.clone());
            break;
        }
    }
    if let Some(report) = failed_report {
        state.emit("notification.delivery.failed", report);
    }
    ctx.add_result(json!({ "matched": matched.is_some(), "attempt_id": matched }));
}

// ---------------------------------------------------------------------------
// Module
// ---------------------------------------------------------------------------

pub struct NotificationsModule {
    state: Arc<NotificationsState>,
}

impl NotificationsModule {
    pub fn new() -> Self {
        Self {
            state: Arc::new(NotificationsState::new()),
        }
    }
}

impl Default for NotificationsModule {
    fn default() -> Self {
        Self::new()
    }
}

impl Module for NotificationsModule {
    fn id(&self) -> &'static str {
        "notifications"
    }

    fn name(&self) -> &'static str {
        "Notifications"
    }

    fn description(&self) -> &'static str {
        "Notifications with read tracking, per-user channel, category and digest preferences (including off), per-event notification templates with channel selection (email via an email-provider connection and GrapeJS email templates, in-app, webhook), delivery tracking with retries, audience targeting (per-user, role, department, segment, broadcast) enforced server-side, and listeners for the other modules' important events (orders, payments, backups, clusters, tickets, licenses, incidents, webhooks, security)."
    }

    fn dependencies(&self) -> Vec<&'static str> {
        vec!["mail", "email_providers"]
    }

    fn init(&self, ctx: &AppContext) -> UnifiedResult<()> {
        *self
            .state
            .hooks
            .write()
            .expect("hooks lock poisoned") = ctx.hooks.clone();
        Ok(())
    }

    fn register_hooks(&self, bus: &HookBus) {
        let state = self.state.clone();
        let handler: HookHandler =
            Arc::new(move |ctx: &mut HookContext| on_notification_requested(state.clone(), ctx));
        bus.register("notification.requested", "notifications", handler);
        let state = self.state.clone();
        let handler: HookHandler =
            Arc::new(move |ctx: &mut HookContext| on_security_auth_failed(state.clone(), ctx));
        bus.register("security.auth.failed", "notifications", handler);
        let state = self.state.clone();
        let handler: HookHandler =
            Arc::new(move |ctx: &mut HookContext| on_cluster_sync_failed(state.clone(), ctx));
        bus.register("cluster.sync.failed", "notifications", handler);
        let state = self.state.clone();
        let handler: HookHandler =
            Arc::new(move |ctx: &mut HookContext| on_payment_failed(state.clone(), ctx));
        bus.register("payment.failed", "notifications", handler);
        // Direct listeners for the other modules' important events.
        // See the "Domain-event listeners" section above for the
        // coverage table and the events deliberately NOT subscribed
        // here (their emitters already forward via
        // `notification.requested`).
        for event in [
            "order.paid",
            "invoice.paid",
            "payment.captured",
            "payment.refunded",
            "store.order.completed",
            "store.order.refunded",
            "store.order.cancelled",
            "ticket.commented",
            "ticket.assigned",
            "backup.failed",
            "backup.completed",
            "backup.restored",
            "cluster.health.changed",
            "cluster.node.unhealthy",
            "lb.backend.unhealthy",
            "cluster.sync.completed",
            "cluster.sync.conflict",
            "license.issued",
            "license.activated",
            "license.deactivated",
            "license.suspended",
            "license.revoked",
            "statuspage.incident.created",
            "statuspage.incident.resolved",
            "vcs.sync.failed",
            "webhook.failed",
            "security.bruteforce.locked",
            "security.ip.flagged",
        ] {
            bus.register(event, "notifications", domain_handler(self.state.clone(), event));
        }
        // Provider outcomes update the recorded delivery attempts
        // (matched by the job id `send_email` recorded when queuing).
        for (event, delivered) in [
            ("email.sent", true),
            ("email.failed", false),
            ("mail.sent", true),
            ("mail.failed", false),
        ] {
            let state = self.state.clone();
            let handler: HookHandler = Arc::new(move |ctx: &mut HookContext| {
                on_email_delivery(state.clone(), delivered, ctx)
            });
            bus.register(event, "notifications", handler);
        }
        // Digest delivery entry point for the scheduler (or any other
        // module): `{ "user_id", "email"? }`. The digest itself is
        // sent through the Email Providers module, falling back to the
        // Mail Manager.
        let state = self.state.clone();
        let handler: HookHandler = Arc::new(move |ctx: &mut HookContext| {
            let user_id = ctx
                .get("user_id")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let email = ctx
                .get("email")
                .and_then(|v| v.as_str())
                .map(str::to_string);
            // A user who set the digest to `off` never gets a
            // scheduler-driven digest. The manual POST /digest/send
            // endpoint is an explicit operator action and bypasses
            // this guard.
            if state.preferences_for(&user_id).digest == "off" {
                ctx.add_result(json!({
                    "user_id": user_id,
                    "sent": false,
                    "reason": "digest is disabled for this user",
                }));
                return;
            }
            match send_digest_for_user(&state, &user_id, email.as_deref()) {
                Ok(result) => ctx.add_result(result),
                Err(error) => ctx.add_result(json!({ "error": error.message })),
            }
        });
        bus.register("notification.digest.send", "notifications", handler);
    }

    fn route_info(&self) -> Vec<RouteInfo> {
        vec![
            RouteInfo { method: "GET", path: "/api/v1/notifications", owner: "notifications", description: "List notifications" },
            RouteInfo { method: "POST", path: "/api/v1/notifications", owner: "notifications", description: "Create notification" },
            RouteInfo { method: "GET", path: "/api/v1/notifications/preferences", owner: "notifications", description: "Get preferences" },
            RouteInfo { method: "PUT", path: "/api/v1/notifications/preferences", owner: "notifications", description: "Update preferences" },
            RouteInfo { method: "GET", path: "/api/v1/notifications/unread-count", owner: "notifications", description: "Unread count" },
            RouteInfo { method: "POST", path: "/api/v1/notifications/read-all", owner: "notifications", description: "Mark all read" },
            RouteInfo { method: "POST", path: "/api/v1/notifications/digest/send", owner: "notifications", description: "Send digest" },
            RouteInfo { method: "GET", path: "/api/v1/notifications/audiences", owner: "notifications", description: "List registered audiences (paginated)." },
            RouteInfo { method: "POST", path: "/api/v1/notifications/audiences", owner: "notifications", description: "Register an audience (role/department/segment members)." },
            RouteInfo { method: "GET", path: "/api/v1/notifications/audiences/{audience}", owner: "notifications", description: "Get one registered audience." },
            RouteInfo { method: "DELETE", path: "/api/v1/notifications/audiences/{audience}", owner: "notifications", description: "Delete a registered audience." },
            RouteInfo { method: "GET", path: "/api/v1/notifications/deliveries", owner: "notifications", description: "List delivery attempts (paginated, filter by notification_id, channel, status, job_id)." },
            RouteInfo { method: "GET", path: "/api/v1/notifications/{id}/deliveries", owner: "notifications", description: "List delivery attempts for one notification (paginated)." },
            RouteInfo { method: "POST", path: "/api/v1/notifications/{id}/deliveries/{attempt_id}/retry", owner: "notifications", description: "Retry one failed delivery attempt (max 3 attempts)." },
            RouteInfo { method: "GET", path: "/api/v1/notifications/templates", owner: "notifications", description: "List templates" },
            RouteInfo { method: "POST", path: "/api/v1/notifications/templates", owner: "notifications", description: "Create template" },
            RouteInfo { method: "GET", path: "/api/v1/notifications/templates/{id}", owner: "notifications", description: "Get template" },
            RouteInfo { method: "PUT", path: "/api/v1/notifications/templates/{id}", owner: "notifications", description: "Update template" },
            RouteInfo { method: "DELETE", path: "/api/v1/notifications/templates/{id}", owner: "notifications", description: "Delete template" },
            RouteInfo { method: "POST", path: "/api/v1/notifications/templates/{id}/preview", owner: "notifications", description: "Preview template" },
            RouteInfo { method: "POST", path: "/api/v1/notifications/templates/{id}/test", owner: "notifications", description: "Test template" },
            RouteInfo { method: "GET", path: "/api/v1/notifications/{id}", owner: "notifications", description: "Get notification" },
            RouteInfo { method: "DELETE", path: "/api/v1/notifications/{id}", owner: "notifications", description: "Delete notification" },
            RouteInfo { method: "POST", path: "/api/v1/notifications/{id}/read", owner: "notifications", description: "Mark read" },
            RouteInfo { method: "POST", path: "/api/v1/notifications/{id}/unread", owner: "notifications", description: "Mark unread" },
        ]
    }

    fn configure_routes(&self, cfg: &mut web::ServiceConfig) {
        cfg.app_data(web::Data::new(self.state.clone()));
        cfg.service(
            web::scope("/api/v1/notifications")
                .route("", web::get().to(list_notifications))
                .route("", web::post().to(create_notification))
                .route("/preferences", web::get().to(get_preferences))
                .route("/preferences", web::put().to(update_preferences))
                .route("/unread-count", web::get().to(unread_count))
                .route("/read-all", web::post().to(mark_all_read))
                .route("/digest/send", web::post().to(send_digest))
                .route("/audiences", web::get().to(list_audiences))
                .route("/audiences", web::post().to(create_audience))
                .route("/audiences/{audience}", web::get().to(get_audience))
                .route("/audiences/{audience}", web::delete().to(delete_audience))
                .route("/deliveries", web::get().to(list_deliveries))
                .route("/templates", web::get().to(list_templates))
                .route("/templates", web::post().to(create_template))
                .route("/templates/{id}", web::get().to(get_template))
                .route("/templates/{id}", web::put().to(update_template))
                .route("/templates/{id}", web::delete().to(delete_template))
                .route("/templates/{id}/preview", web::post().to(preview_template))
                .route("/templates/{id}/test", web::post().to(test_template))
                .route("/{id}", web::get().to(get_notification))
                .route("/{id}", web::delete().to(delete_notification))
                .route("/{id}/read", web::post().to(mark_read))
                .route("/{id}/unread", web::post().to(mark_unread))
                .route("/{id}/deliveries", web::get().to(list_notification_deliveries))
                .route("/{id}/deliveries/{attempt_id}/retry", web::post().to(retry_delivery)),
        );
    }
}

pub fn register(registry: &ModuleRegistry) {
    registry.register(Arc::new(NotificationsModule::new()));
}
