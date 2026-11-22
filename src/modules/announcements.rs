// Announcements and Changelog module.
//
// Announcements (draft / published / archived, with audience,
// channels, scheduled publish time, expiry, and pinning) and
// changelog entries (version, date, category, body, link).
//
// Audience rules (enforced server-side, see `audience_matches`):
// an announcement is addressed to `all`, `users`, `admins`, one
// `role:<id>`, one `department:<name>`, one `segment:<name>`, or an
// explicit list of users (`user` + `audience_user_ids`). The public
// list, the unread list and the dashboard feed only ever return
// announcements addressed to the calling user: a caller who does not
// identify themselves with `user_id` sees broadcast (`all`)
// announcements only. `users` means every user (there is no
// user-directory hook to distinguish users from admins, so it
// collapses to broadcast); `admins` means callers whose declared
// roles include `admin`.
//
// Publishing and scheduling: `POST /{id}/publish` (or setting the
// status to `published`) only fans the announcement out when its
// publish time has arrived. A future `publish_at` leaves the
// announcement scheduled (`published_at` stays empty) until
// `POST /announcements/publish-due` -- or the
// `announcement.publish_due` hook, which the scheduler can drive --
// publishes everything due. An announcement whose expiry passed
// before its publish time never goes out.
//
// Fan-out: on publish the module emits `announcement.published` and
// a `notification.requested` (kind `announcement`) carrying the full
// audience, the channels and the optional GrapeJS email-template id,
// so the Notifications module can fan it out into the notification
// centre and, when the email channel is selected, send it by email
// (recipient addresses come from `audience_emails`, aligned with
// `audience_user_ids`, or from the registered audience members in
// the Notifications module). The event name was verified against
// `src/modules/notifications.rs`, which registers its handler on
// exactly `notification.requested`.
//
// Read tracking: one receipt per (announcement, user). Marking read
// is idempotent. `GET /api/v1/announcements/unread?user_id=...`
// lists the published, currently visible announcements a user has
// not read yet. `GET /api/v1/announcements/dashboard?user_id=...`
// is the user-dashboard feed: the same audience-filtered list plus
// the unread count.
//
// Events emitted on the hook bus:
//   - `announcement.published` — an announcement was published.
//   - `changelog.created`      — a changelog entry was created.
//   - `notification.requested` — emitted on publish (kind
//     `announcement`) so the Notifications module can fan the
//     announcement out to its audience.

use crate::core::context::AppContext;
use crate::core::error::{UnifiedError, UnifiedResult};
use crate::core::hooks::{HookBus, HookContext};
use crate::core::json;
use crate::core::modules::{Module, ModuleRegistry};
use crate::core::pagination::{PageResult, PaginationParams};
use actix_web::{web, HttpResponse};
use chrono::{DateTime, NaiveDate, Utc};
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::sync::{Arc, RwLock};
use uuid::Uuid;

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

pub const AUDIENCE_ALL: &str = "all";
pub const AUDIENCE_USERS: &str = "users";
pub const AUDIENCE_ADMINS: &str = "admins";
pub const AUDIENCE_ROLE: &str = "role";
pub const AUDIENCE_DEPARTMENT: &str = "department";
pub const AUDIENCE_SEGMENT: &str = "segment";
pub const AUDIENCE_USER: &str = "user";

fn valid_audience(audience: &str) -> bool {
    matches!(
        audience,
        AUDIENCE_ALL
            | AUDIENCE_USERS
            | AUDIENCE_ADMINS
            | AUDIENCE_ROLE
            | AUDIENCE_DEPARTMENT
            | AUDIENCE_SEGMENT
            | AUDIENCE_USER
    )
}

/// Channels an announcement may use: `in_app`, `email`, `webhook`.
/// The email channel needs recipient addresses (see
/// `audience_emails`): without any address the Notifications module
/// records the email delivery as failed instead of dropping it.
pub const ANNOUNCEMENT_CHANNELS: &[&str] = &["in_app", "email", "webhook"];

fn validate_channels(channels: &[String]) -> UnifiedResult<()> {
    if channels.is_empty() {
        return Err(UnifiedError::bad_request(
            "At least one channel must be enabled",
        ));
    }
    for channel in channels {
        if !ANNOUNCEMENT_CHANNELS.contains(&channel.as_str()) {
            return Err(UnifiedError::bad_request(format!(
                "Channels must be a subset of: {}",
                ANNOUNCEMENT_CHANNELS.join(", ")
            )));
        }
    }
    Ok(())
}

/// Validate the audience trio: role / department / segment need an
/// `audience_value`; `user` needs at least one user id; email
/// addresses only make sense alongside user ids.
fn validate_audience(
    audience: &str,
    audience_value: Option<&str>,
    audience_user_ids: &[String],
    audience_emails: &[String],
) -> UnifiedResult<()> {
    if !valid_audience(audience) {
        return Err(UnifiedError::bad_request(format!(
            "Invalid audience \"{audience}\": expected all, users, admins, role, department, segment, or user"
        )));
    }
    match audience {
        AUDIENCE_ROLE | AUDIENCE_DEPARTMENT | AUDIENCE_SEGMENT => {
            if audience_value.map(str::trim).unwrap_or("").is_empty() {
                return Err(UnifiedError::bad_request(format!(
                    "audience_value is required for audience \"{audience}\""
                )));
            }
        }
        AUDIENCE_USER => {
            if audience_user_ids.is_empty() {
                return Err(UnifiedError::bad_request(
                    "audience_user_ids must not be empty for audience \"user\"",
                ));
            }
        }
        _ => {}
    }
    if !audience_emails.is_empty() && audience_user_ids.is_empty() {
        return Err(UnifiedError::bad_request(
            "audience_emails requires audience_user_ids",
        ));
    }
    for email in audience_emails {
        if !email.contains('@') || !email.contains('.') {
            return Err(UnifiedError::bad_request(format!(
                "Invalid email address \"{email}\""
            )));
        }
    }
    Ok(())
}

/// True when an announcement addressed this way may be shown to a
/// caller with this identity. `roles`, `departments` and `segments`
/// are the caller's declared memberships (comma-separated `roles`,
/// `departments`, `segments` query parameters). This is the
/// server-side audience rule for the public list, the unread list
/// and the dashboard feed: a user only ever sees announcements
/// addressed to them.
fn audience_matches(
    announcement: &Announcement,
    user_id: &str,
    roles: &[String],
    departments: &[String],
    segments: &[String],
) -> bool {
    match announcement.audience.as_str() {
        AUDIENCE_ALL | AUDIENCE_USERS => true,
        AUDIENCE_ADMINS => roles.iter().any(|role| role == "admin" || role == "administrator"),
        AUDIENCE_ROLE => announcement
            .audience_value
            .as_deref()
            .is_some_and(|value| roles.iter().any(|role| role == value)),
        AUDIENCE_DEPARTMENT => announcement
            .audience_value
            .as_deref()
            .is_some_and(|value| departments.iter().any(|department| department == value)),
        AUDIENCE_SEGMENT => announcement
            .audience_value
            .as_deref()
            .is_some_and(|value| segments.iter().any(|segment| segment == value)),
        AUDIENCE_USER => announcement
            .audience_user_ids
            .iter()
            .any(|id| id == user_id),
        _ => false,
    }
}

/// Split one comma-separated query value into trimmed, non-empty parts.
fn split_list(raw: Option<&String>) -> Vec<String> {
    raw.map(|value| {
        value
            .split(',')
            .map(str::trim)
            .filter(|part| !part.is_empty())
            .map(str::to_string)
            .collect()
    })
    .unwrap_or_default()
}

pub const STATUS_DRAFT: &str = "draft";
pub const STATUS_PUBLISHED: &str = "published";
pub const STATUS_ARCHIVED: &str = "archived";

fn valid_announcement_status(status: &str) -> bool {
    matches!(status, STATUS_DRAFT | STATUS_PUBLISHED | STATUS_ARCHIVED)
}

pub const CATEGORY_FEATURE: &str = "feature";
pub const CATEGORY_FIX: &str = "fix";
pub const CATEGORY_SECURITY: &str = "security";

fn valid_category(category: &str) -> bool {
    matches!(category, CATEGORY_FEATURE | CATEGORY_FIX | CATEGORY_SECURITY)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Announcement {
    pub id: Uuid,
    pub title: String,
    pub body: String,
    pub audience: String,
    /// Role id, department name or segment name for the `role`,
    /// `department` and `segment` audiences. Unused otherwise.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audience_value: Option<String>,
    /// Explicit recipients for the `user` audience.
    #[serde(default)]
    pub audience_user_ids: Vec<String>,
    /// Recipient email addresses for the email channel, aligned with
    /// `audience_user_ids` by index. Broadcast audiences have no
    /// addresses here; their email delivery resolves through the
    /// registered audience members in the Notifications module.
    #[serde(default)]
    pub audience_emails: Vec<String>,
    /// Channels the announcement goes out on: a subset of `in_app`,
    /// `email`, `webhook`. `in_app` lands in the notification centre.
    #[serde(default = "default_announcement_channels")]
    pub channels: Vec<String>,
    /// GrapeJS email-template id (Email Templates module) used to
    /// render the email body when the email channel is selected.
    /// When unset, the plain title/body are sent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub email_template_id: Option<String>,
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub publish_at: Option<DateTime<Utc>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<DateTime<Utc>>,
    pub pinned: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub published_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChangelogEntry {
    pub id: Uuid,
    pub version: String,
    /// Release date in `YYYY-MM-DD`.
    pub date: String,
    pub category: String,
    pub title: String,
    pub body: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub link: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReadReceipt {
    pub announcement_id: Uuid,
    pub user_id: String,
    pub read_at: DateTime<Utc>,
}

fn default_announcement_channels() -> Vec<String> {
    vec!["in_app".to_string()]
}

#[derive(Debug, Clone, Deserialize)]
pub struct CreateAnnouncementRequest {
    pub title: String,
    pub body: String,
    #[serde(default)]
    pub audience: Option<String>,
    #[serde(default)]
    pub audience_value: Option<String>,
    #[serde(default)]
    pub audience_user_ids: Vec<String>,
    #[serde(default)]
    pub audience_emails: Vec<String>,
    #[serde(default)]
    pub channels: Option<Vec<String>>,
    #[serde(default)]
    pub email_template_id: Option<String>,
    #[serde(default)]
    pub publish_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub expires_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub pinned: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct UpdateAnnouncementRequest {
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub body: Option<String>,
    #[serde(default)]
    pub audience: Option<String>,
    #[serde(default)]
    pub audience_value: Option<String>,
    #[serde(default)]
    pub audience_user_ids: Option<Vec<String>>,
    #[serde(default)]
    pub audience_emails: Option<Vec<String>>,
    #[serde(default)]
    pub channels: Option<Vec<String>>,
    #[serde(default)]
    pub email_template_id: Option<String>,
    #[serde(default)]
    pub publish_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub expires_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub pinned: Option<bool>,
    #[serde(default)]
    pub status: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct MarkReadRequest {
    pub user_id: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CreateChangelogRequest {
    pub version: String,
    pub date: String,
    #[serde(default)]
    pub category: Option<String>,
    pub title: String,
    pub body: String,
    #[serde(default)]
    pub link: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct UpdateChangelogRequest {
    #[serde(default)]
    pub version: Option<String>,
    #[serde(default)]
    pub date: Option<String>,
    #[serde(default)]
    pub category: Option<String>,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub body: Option<String>,
    #[serde(default)]
    pub link: Option<String>,
}

// ---------------------------------------------------------------------------
// State
// ---------------------------------------------------------------------------

pub struct AnnouncementsState {
    hooks: RwLock<Arc<HookBus>>,
    announcements: DashMap<Uuid, Announcement>,
    changelog: DashMap<Uuid, ChangelogEntry>,
    /// Keyed by `{announcement_id}:{user_id}`.
    reads: DashMap<String, ReadReceipt>,
}

impl AnnouncementsState {
    fn new() -> Self {
        Self {
            hooks: RwLock::new(Arc::new(HookBus::new())),
            announcements: DashMap::new(),
            changelog: DashMap::new(),
            reads: DashMap::new(),
        }
    }

    fn bus(&self) -> Arc<HookBus> {
        self.hooks.read().expect("hooks lock poisoned").clone()
    }

    fn emit(&self, event: &str, data: Value) {
        self.bus().emit(event, data);
    }

    /// True when an announcement is visible on the public list right now.
    fn is_visible_now(announcement: &Announcement, now: DateTime<Utc>) -> bool {
        if announcement.status != STATUS_PUBLISHED {
            return false;
        }
        if let Some(publish_at) = announcement.publish_at {
            if now < publish_at {
                return false;
            }
        }
        if let Some(expires_at) = announcement.expires_at {
            if now >= expires_at {
                return false;
            }
        }
        true
    }

    fn publish(&self, announcement: &Announcement) {
        self.emit(
            "announcement.published",
            json!({
                "announcement_id": announcement.id,
                "title": announcement.title,
                "audience": announcement.audience,
                "audience_value": announcement.audience_value,
                "channels": announcement.channels,
                "published_at": announcement.published_at,
            }),
        );
        // Fan out through the Notifications module with the full
        // audience, so the centre only ever delivers the announcement
        // to its addressees: `all` / `users` broadcast, `admins`
        // through the registered `role:admin` audience, `role` /
        // `department` / `segment` through their registered
        // audiences, and `user` through the explicit user ids.
        // Email addresses ride along for the email channel, aligned
        // with the user ids by index.
        let mut payload = json!({
            "title": announcement.title,
            "body": announcement.body,
            "kind": "announcement",
            "category": "announcements",
            "channels": announcement.channels,
            "audience": announcement.audience,
        });
        match announcement.audience.as_str() {
            AUDIENCE_ALL | AUDIENCE_USERS => {
                payload["user_id"] = Value::String("all".to_string());
                payload["audience"] = Value::String("all".to_string());
            }
            AUDIENCE_ADMINS => {
                payload["audience"] = Value::String("role:admin".to_string());
            }
            AUDIENCE_ROLE | AUDIENCE_DEPARTMENT | AUDIENCE_SEGMENT => {
                if let Some(value) = &announcement.audience_value {
                    payload["audience"] = Value::String(format!(
                        "{}:{value}",
                        announcement.audience
                    ));
                }
            }
            AUDIENCE_USER => {
                if let Some(first) = announcement.audience_user_ids.first() {
                    payload["user_id"] = Value::String(first.clone());
                }
            }
            _ => {}
        }
        if !announcement.audience_user_ids.is_empty() {
            payload["user_ids"] = serde_json::to_value(&announcement.audience_user_ids)
                .unwrap_or(Value::Array(Vec::new()));
        }
        if !announcement.audience_emails.is_empty() {
            payload["emails"] = serde_json::to_value(&announcement.audience_emails)
                .unwrap_or(Value::Array(Vec::new()));
        }
        if let Some(template_id) = &announcement.email_template_id {
            payload["email_template_id"] = Value::String(template_id.clone());
        }
        self.emit("notification.requested", payload);
    }

    /// Publish one announcement whose time has come: stamp
    /// `published_at` and fan it out. Returns false when the
    /// announcement is not due yet (future `publish_at`), already
    /// published, or already expired -- in all three cases nothing is
    /// emitted, so a scheduled announcement can never leak early.
    fn maybe_publish(&self, announcement: &Announcement) -> bool {
        if announcement.published_at.is_some() {
            return false;
        }
        if !Self::is_visible_now(announcement, Utc::now()) {
            return false;
        }
        let mut updated = announcement.clone();
        updated.published_at = Some(Utc::now());
        updated.updated_at = Utc::now();
        self.announcements
            .insert(updated.id, updated.clone());
        self.publish(&updated);
        true
    }

    /// Publish every announcement whose publish time has arrived and
    /// which has not been fanned out yet. Returns the announcements
    /// that were published by this call.
    fn publish_due(&self) -> Vec<Announcement> {
        let now = Utc::now();
        let due: Vec<Announcement> = self
            .announcements
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|announcement| {
                announcement.status == STATUS_PUBLISHED
                    && announcement.published_at.is_none()
                    && Self::is_visible_now(announcement, now)
            })
            .collect();
        let mut published = Vec::new();
        for announcement in due {
            if self.maybe_publish(&announcement) {
                published.push(announcement);
            }
        }
        published
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn parse_uuid(raw: &str, what: &str) -> UnifiedResult<Uuid> {
    Uuid::parse_str(raw)
        .map_err(|_| UnifiedError::bad_request(format!("Invalid {what} id \"{raw}\"")))
}

fn validate_title_body(title: &str, body: &str) -> UnifiedResult<()> {
    if title.trim().is_empty() {
        return Err(UnifiedError::bad_request("title must not be empty"));
    }
    if title.chars().count() > 200 {
        return Err(UnifiedError::bad_request(
            "title must be at most 200 characters",
        ));
    }
    if body.trim().is_empty() {
        return Err(UnifiedError::bad_request("body must not be empty"));
    }
    if body.chars().count() > 20000 {
        return Err(UnifiedError::bad_request(
            "body must be at most 20000 characters",
        ));
    }
    Ok(())
}

fn validate_window(
    publish_at: Option<DateTime<Utc>>,
    expires_at: Option<DateTime<Utc>>,
) -> UnifiedResult<()> {
    if let (Some(publish_at), Some(expires_at)) = (publish_at, expires_at) {
        if expires_at <= publish_at {
            return Err(UnifiedError::bad_request(
                "expires_at must be after publish_at",
            ));
        }
    }
    Ok(())
}

/// Validate a version string: `1.2.3`, `v1.2.3`, or with a pre-release
/// suffix such as `1.2.3-beta.1`.
fn validate_version(version: &str) -> UnifiedResult<()> {
    let version = version.trim();
    if version.is_empty() {
        return Err(UnifiedError::bad_request("version must not be empty"));
    }
    if version.chars().count() > 50 {
        return Err(UnifiedError::bad_request(
            "version must be at most 50 characters",
        ));
    }
    let core = version.strip_prefix('v').unwrap_or(version);
    let core = core.split(['-', '+']).next().unwrap_or(core);
    let parts: Vec<&str> = core.split('.').collect();
    if parts.len() != 3
        || parts
            .iter()
            .any(|part| part.is_empty() || !part.bytes().all(|byte| byte.is_ascii_digit()))
    {
        return Err(UnifiedError::bad_request(format!(
            "Invalid version \"{version}\": expected semantic versioning such as 1.2.3"
        )));
    }
    Ok(())
}

fn validate_date(raw: &str) -> UnifiedResult<()> {
    NaiveDate::parse_from_str(raw, "%Y-%m-%d").map_err(|_| {
        UnifiedError::bad_request(format!("Invalid date \"{raw}\": expected YYYY-MM-DD"))
    })?;
    Ok(())
}

fn validate_user_id(user_id: &str) -> UnifiedResult<()> {
    if user_id.trim().is_empty() {
        return Err(UnifiedError::bad_request("user_id must not be empty"));
    }
    if user_id.chars().count() > 256 {
        return Err(UnifiedError::bad_request(
            "user_id must be at most 256 characters",
        ));
    }
    Ok(())
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

fn sort_announcements(
    mut items: Vec<Announcement>,
    params: &PaginationParams,
) -> Vec<Announcement> {
    let field = params.allowed_sort(&["created_at", "publish_at", "title"], "created_at");
    match field.as_str() {
        "publish_at" => items.sort_by(|a, b| {
            b.publish_at.cmp(&a.publish_at).then_with(|| b.created_at.cmp(&a.created_at))
        }),
        "title" => items.sort_by(|a, b| a.title.to_lowercase().cmp(&b.title.to_lowercase())),
        _ => items.sort_by(|a, b| b.created_at.cmp(&a.created_at)),
    }
    // Pinned first on the public ordering, then the chosen sort.
    if params.normalized().order == "asc" {
        items.reverse();
    }
    items
}

// ---------------------------------------------------------------------------
// Announcement handlers
// ---------------------------------------------------------------------------

async fn list_announcements(
    state: web::Data<Arc<AnnouncementsState>>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let params = query.into_inner();
    let mut items: Vec<Announcement> = state
        .announcements
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|announcement| {
            matches_search(
                &format!("{} {}", announcement.title, announcement.body),
                &params,
            ) && params.filters.get("status").map_or(true, |status| {
                announcement.status == *status
            }) && params.filters.get("audience").map_or(true, |audience| {
                announcement.audience == *audience
            }) && params.filters.get("pinned").map_or(true, |pinned| {
                announcement.pinned == (*pinned == "true" || *pinned == "1")
            })
        })
        .collect();
    items = sort_announcements(items, &params);
    let page = paginate(items, &params);
    json::success_response(page, 200)
}

/// PUBLIC: published, currently visible announcements only.
async fn list_public_announcements(
    state: web::Data<Arc<AnnouncementsState>>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let params = query.into_inner();
    let now = Utc::now();
    // Caller identity for the audience rule: `user_id` plus the
    // caller's declared `roles`, `departments` and `segments`
    // (comma-separated). A caller who does not identify themselves
    // sees broadcast announcements only.
    let user_id = params.filters.get("user_id").cloned().unwrap_or_default();
    let roles = split_list(params.filters.get("roles"));
    let departments = split_list(params.filters.get("departments"));
    let segments = split_list(params.filters.get("segments"));
    let mut items: Vec<Announcement> = state
        .announcements
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|announcement| {
            AnnouncementsState::is_visible_now(announcement, now)
                && matches_search(
                    &format!("{} {}", announcement.title, announcement.body),
                    &params,
                )
                && (if user_id.trim().is_empty() {
                    announcement.audience == AUDIENCE_ALL
                } else {
                    audience_matches(announcement, &user_id, &roles, &departments, &segments)
                })
                && params.filters.get("audience").map_or(true, |audience| {
                    announcement.audience == *audience
                })
        })
        .collect();
    // Pinned first, then newest publish time.
    items.sort_by(|a, b| {
        b.pinned
            .cmp(&a.pinned)
            .then_with(|| b.created_at.cmp(&a.created_at))
    });
    if params.normalized().order == "asc" {
        items.reverse();
    }
    let page = paginate(items, &params);
    json::success_response(page, 200)
}

async fn create_announcement(
    state: web::Data<Arc<AnnouncementsState>>,
    body: web::Json<CreateAnnouncementRequest>,
) -> HttpResponse {
    let result: UnifiedResult<Announcement> = (|| {
        let input = body.into_inner();
        validate_title_body(&input.title, &input.body)?;
        let audience = input.audience.clone().unwrap_or_else(|| AUDIENCE_ALL.to_string());
        let audience_value = input
            .audience_value
            .clone()
            .filter(|value| !value.trim().is_empty());
        validate_audience(
            &audience,
            audience_value.as_deref(),
            &input.audience_user_ids,
            &input.audience_emails,
        )?;
        let channels = input.channels.clone().unwrap_or_else(default_announcement_channels);
        validate_channels(&channels)?;
        validate_window(input.publish_at, input.expires_at)?;
        let now = Utc::now();
        let announcement = Announcement {
            id: Uuid::now_v7(),
            title: input.title.trim().to_string(),
            body: input.body.trim().to_string(),
            audience,
            audience_value,
            audience_user_ids: input.audience_user_ids.clone(),
            audience_emails: input.audience_emails.clone(),
            channels,
            email_template_id: input
                .email_template_id
                .clone()
                .filter(|value| !value.trim().is_empty()),
            status: STATUS_DRAFT.to_string(),
            publish_at: input.publish_at,
            expires_at: input.expires_at,
            pinned: input.pinned,
            created_at: now,
            updated_at: now,
            published_at: None,
        };
        state
            .announcements
            .insert(announcement.id, announcement.clone());
        Ok(announcement)
    })();
    match result {
        Ok(announcement) => json::success_response(announcement, 201),
        Err(error) => json::error_response(&error, false),
    }
}

async fn get_announcement(
    state: web::Data<Arc<AnnouncementsState>>,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<Announcement> = (|| {
        let id = parse_uuid(path.as_str(), "announcement")?;
        state
            .announcements
            .get(&id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Announcement not found"))
    })();
    json::respond(result, false)
}

async fn update_announcement(
    state: web::Data<Arc<AnnouncementsState>>,
    path: web::Path<String>,
    body: web::Json<UpdateAnnouncementRequest>,
) -> HttpResponse {
    let result: UnifiedResult<Announcement> = (|| {
        let id = parse_uuid(path.as_str(), "announcement")?;
        let input = body.into_inner();
        let mut announcement = state
            .announcements
            .get_mut(&id)
            .ok_or_else(|| UnifiedError::not_found("Announcement not found"))?;
        if let Some(title) = input.title {
            if title.trim().is_empty() {
                return Err(UnifiedError::bad_request("title must not be empty"));
            }
            announcement.title = title.trim().to_string();
        }
        if let Some(body) = input.body {
            if body.trim().is_empty() {
                return Err(UnifiedError::bad_request("body must not be empty"));
            }
            announcement.body = body.trim().to_string();
        }
        if let Some(audience) = input.audience {
            if !valid_audience(&audience) {
                return Err(UnifiedError::bad_request(format!(
                    "Invalid audience \"{audience}\""
                )));
            }
            announcement.audience = audience;
        }
        if let Some(audience_value) = input.audience_value {
            announcement.audience_value = if audience_value.trim().is_empty() {
                None
            } else {
                Some(audience_value.trim().to_string())
            };
        }
        if let Some(user_ids) = input.audience_user_ids {
            announcement.audience_user_ids = user_ids;
        }
        if let Some(emails) = input.audience_emails {
            announcement.audience_emails = emails;
        }
        if let Some(channels) = input.channels {
            validate_channels(&channels)?;
            announcement.channels = channels;
        }
        if let Some(email_template_id) = input.email_template_id {
            announcement.email_template_id = if email_template_id.trim().is_empty() {
                None
            } else {
                Some(email_template_id)
            };
        }
        if input.publish_at.is_some() {
            announcement.publish_at = input.publish_at;
        }
        if input.expires_at.is_some() {
            announcement.expires_at = input.expires_at;
        }
        validate_window(announcement.publish_at, announcement.expires_at)?;
        if let Some(pinned) = input.pinned {
            announcement.pinned = pinned;
        }
        validate_audience(
            &announcement.audience,
            announcement.audience_value.as_deref(),
            &announcement.audience_user_ids,
            &announcement.audience_emails,
        )?;
        if let Some(status) = input.status {
            if !valid_announcement_status(&status) {
                return Err(UnifiedError::bad_request(format!(
                    "Invalid status \"{status}\": expected draft, published, or archived"
                )));
            }
            announcement.status = status;
        }
        announcement.updated_at = Utc::now();
        let snapshot = announcement.clone();
        drop(announcement);
        // Publishing only fans out when the publish time has arrived;
        // a future publish_at stays scheduled until publish-due runs.
        state.maybe_publish(&snapshot);
        let snapshot = state
            .announcements
            .get(&id)
            .map(|entry| entry.value().clone())
            .unwrap_or(snapshot);
        Ok(snapshot)
    })();
    json::respond(result, false)
}

async fn delete_announcement(
    state: web::Data<Arc<AnnouncementsState>>,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<Value> = (|| {
        let id = parse_uuid(path.as_str(), "announcement")?;
        if !state.announcements.contains_key(&id) {
            return Err(UnifiedError::not_found("Announcement not found"));
        }
        state.announcements.remove(&id);
        state.reads.retain(|_, receipt| receipt.announcement_id != id);
        Ok(json!({ "deleted": true, "id": id }))
    })();
    json::respond(result, false)
}

async fn publish_announcement(
    state: web::Data<Arc<AnnouncementsState>>,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<Announcement> = (|| {
        let id = parse_uuid(path.as_str(), "announcement")?;
        let mut announcement = state
            .announcements
            .get_mut(&id)
            .ok_or_else(|| UnifiedError::not_found("Announcement not found"))?;
        if announcement.status == STATUS_PUBLISHED {
            return Err(UnifiedError::conflict(
                "Announcement is already published",
            ));
        }
        if announcement.status == STATUS_ARCHIVED {
            return Err(UnifiedError::conflict(
                "An archived announcement cannot be published; create a new one instead",
            ));
        }
        let now = Utc::now();
        announcement.status = STATUS_PUBLISHED.to_string();
        announcement.updated_at = now;
        let snapshot = announcement.clone();
        drop(announcement);
        // Only fans out when the publish time has arrived. A future
        // publish_at leaves the announcement scheduled
        // (`published_at` stays empty) until publish-due runs.
        state.maybe_publish(&snapshot);
        let snapshot = state
            .announcements
            .get(&id)
            .map(|entry| entry.value().clone())
            .unwrap_or(snapshot);
        Ok(snapshot)
    })();
    json::respond(result, false)
}

/// Publish every announcement whose publish time has arrived.
/// Driven by the scheduler through the `announcement.publish_due`
/// hook (see the scheduler arm in the report), or by an operator.
async fn publish_due(
    state: web::Data<Arc<AnnouncementsState>>,
) -> HttpResponse {
    let published = state.publish_due();
    let ids: Vec<Uuid> = published.iter().map(|announcement| announcement.id).collect();
    json::success_response(
        json!({ "published": published.len(), "announcement_ids": ids }),
        200,
    )
}

async fn archive_announcement(
    state: web::Data<Arc<AnnouncementsState>>,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<Announcement> = (|| {
        let id = parse_uuid(path.as_str(), "announcement")?;
        let mut announcement = state
            .announcements
            .get_mut(&id)
            .ok_or_else(|| UnifiedError::not_found("Announcement not found"))?;
        if announcement.status == STATUS_ARCHIVED {
            return Err(UnifiedError::conflict("Announcement is already archived"));
        }
        announcement.status = STATUS_ARCHIVED.to_string();
        announcement.updated_at = Utc::now();
        Ok(announcement.clone())
    })();
    json::respond(result, false)
}

async fn mark_announcement_read(
    state: web::Data<Arc<AnnouncementsState>>,
    path: web::Path<String>,
    body: web::Json<MarkReadRequest>,
) -> HttpResponse {
    let result: UnifiedResult<ReadReceipt> = (|| {
        let id = parse_uuid(path.as_str(), "announcement")?;
        let user_id = body.user_id.trim().to_string();
        validate_user_id(&user_id)?;
        if !state.announcements.contains_key(&id) {
            return Err(UnifiedError::not_found("Announcement not found"));
        }
        let key = format!("{id}:{user_id}");
        if let Some(existing) = state.reads.get(&key) {
            return Ok(existing.value().clone());
        }
        let receipt = ReadReceipt {
            announcement_id: id,
            user_id: user_id.clone(),
            read_at: Utc::now(),
        };
        state.reads.insert(key, receipt.clone());
        Ok(receipt)
    })();
    json::respond(result, false)
}

async fn list_announcement_reads(
    state: web::Data<Arc<AnnouncementsState>>,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let params = query.into_inner();
    let result: UnifiedResult<PageResult<ReadReceipt>> = (|| {
        let id = parse_uuid(path.as_str(), "announcement")?;
        if !state.announcements.contains_key(&id) {
            return Err(UnifiedError::not_found("Announcement not found"));
        }
        let mut items: Vec<ReadReceipt> = state
            .reads
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|receipt| receipt.announcement_id == id)
            .collect();
        items.sort_by(|a, b| b.read_at.cmp(&a.read_at));
        if params.normalized().order == "asc" {
            items.reverse();
        }
        Ok(paginate(items, &params))
    })();
    json::respond(result, false)
}

/// Published, visible announcements the user has not read yet.
async fn list_unread(
    state: web::Data<Arc<AnnouncementsState>>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let params = query.into_inner();
    let user_id = params.filters.get("user_id").cloned().unwrap_or_default();
    if let Err(error) = validate_user_id(&user_id) {
        return json::error_response(&error, false);
    }
    let now = Utc::now();
    let roles = split_list(params.filters.get("roles"));
    let departments = split_list(params.filters.get("departments"));
    let segments = split_list(params.filters.get("segments"));
    let mut items: Vec<Announcement> = state
        .announcements
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|announcement| {
            AnnouncementsState::is_visible_now(announcement, now)
                && audience_matches(announcement, &user_id, &roles, &departments, &segments)
                && !state
                    .reads
                    .contains_key(&format!("{}:{user_id}", announcement.id))
        })
        .collect();
    items.sort_by(|a, b| {
        b.pinned
            .cmp(&a.pinned)
            .then_with(|| b.created_at.cmp(&a.created_at))
    });
    let page = paginate(items, &params);
    json::success_response(page, 200)
}

/// User-dashboard feed: the audience-filtered, currently visible
/// announcements for one user plus the unread count. The dashboards
/// module does not read announcements itself, so this endpoint is the
/// feed it (or any client) renders.
async fn dashboard_feed(
    state: web::Data<Arc<AnnouncementsState>>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let params = query.into_inner();
    let user_id = params.filters.get("user_id").cloned().unwrap_or_default();
    if let Err(error) = validate_user_id(&user_id) {
        return json::error_response(&error, false);
    }
    let roles = split_list(params.filters.get("roles"));
    let departments = split_list(params.filters.get("departments"));
    let segments = split_list(params.filters.get("segments"));
    let now = Utc::now();
    let mut items: Vec<Announcement> = state
        .announcements
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|announcement| {
            AnnouncementsState::is_visible_now(announcement, now)
                && audience_matches(announcement, &user_id, &roles, &departments, &segments)
        })
        .collect();
    items.sort_by(|a, b| {
        b.pinned
            .cmp(&a.pinned)
            .then_with(|| b.created_at.cmp(&a.created_at))
    });
    let unread = items
        .iter()
        .filter(|announcement| {
            !state
                .reads
                .contains_key(&format!("{}:{user_id}", announcement.id))
        })
        .count() as u64;
    let page = paginate(items, &params);
    json::success_response(
        json!({
            "user_id": user_id,
            "unread_announcements": unread,
            "announcements": page.items,
            "pagination": page.pagination,
        }),
        200,
    )
}

// ---------------------------------------------------------------------------
// Changelog handlers
// ---------------------------------------------------------------------------

/// PUBLIC: changelog entries, newest first.
async fn list_changelog(
    state: web::Data<Arc<AnnouncementsState>>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let params = query.into_inner();
    let mut items: Vec<ChangelogEntry> = state
        .changelog
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|entry| {
            matches_search(&format!("{} {} {}", entry.version, entry.title, entry.body), &params)
                && params.filters.get("category").map_or(true, |category| {
                    entry.category == *category
                })
                && params.filters.get("version").map_or(true, |version| {
                    entry.version == *version
                })
        })
        .collect();
    items.sort_by(|a, b| {
        b.date
            .cmp(&a.date)
            .then_with(|| b.version.cmp(&a.version))
            .then_with(|| b.created_at.cmp(&a.created_at))
    });
    if params.normalized().order == "asc" {
        items.reverse();
    }
    let page = paginate(items, &params);
    json::success_response(page, 200)
}

async fn create_changelog(
    state: web::Data<Arc<AnnouncementsState>>,
    body: web::Json<CreateChangelogRequest>,
) -> HttpResponse {
    let result: UnifiedResult<ChangelogEntry> = (|| {
        let input = body.into_inner();
        validate_version(&input.version)?;
        validate_date(&input.date)?;
        let category = input.category.clone().unwrap_or_else(|| CATEGORY_FEATURE.to_string());
        if !valid_category(&category) {
            return Err(UnifiedError::bad_request(format!(
                "Invalid category \"{category}\": expected feature, fix, or security"
            )));
        }
        validate_title_body(&input.title, &input.body)?;
        if let Some(link) = &input.link {
            if link.trim().is_empty() {
                return Err(UnifiedError::bad_request("link must not be empty"));
            }
            if !(link.starts_with("https://") || link.starts_with("http://")) {
                return Err(UnifiedError::bad_request(
                    "link must be an http(s) URL",
                ));
            }
        }
        let now = Utc::now();
        let entry = ChangelogEntry {
            id: Uuid::now_v7(),
            version: input.version.trim().to_string(),
            date: input.date.trim().to_string(),
            category,
            title: input.title.trim().to_string(),
            body: input.body.trim().to_string(),
            link: input.link.map(|link| link.trim().to_string()),
            created_at: now,
            updated_at: now,
        };
        state.changelog.insert(entry.id, entry.clone());
        state.emit(
            "changelog.created",
            json!({
                "entry_id": entry.id,
                "version": entry.version,
                "category": entry.category,
                "title": entry.title,
            }),
        );
        Ok(entry)
    })();
    match result {
        Ok(entry) => json::success_response(entry, 201),
        Err(error) => json::error_response(&error, false),
    }
}

async fn get_changelog(
    state: web::Data<Arc<AnnouncementsState>>,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<ChangelogEntry> = (|| {
        let id = parse_uuid(path.as_str(), "changelog entry")?;
        state
            .changelog
            .get(&id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Changelog entry not found"))
    })();
    json::respond(result, false)
}

async fn update_changelog(
    state: web::Data<Arc<AnnouncementsState>>,
    path: web::Path<String>,
    body: web::Json<UpdateChangelogRequest>,
) -> HttpResponse {
    let result: UnifiedResult<ChangelogEntry> = (|| {
        let id = parse_uuid(path.as_str(), "changelog entry")?;
        let input = body.into_inner();
        let mut entry = state
            .changelog
            .get_mut(&id)
            .ok_or_else(|| UnifiedError::not_found("Changelog entry not found"))?;
        if let Some(version) = input.version {
            validate_version(&version)?;
            entry.version = version.trim().to_string();
        }
        if let Some(date) = input.date {
            validate_date(&date)?;
            entry.date = date.trim().to_string();
        }
        if let Some(category) = input.category {
            if !valid_category(&category) {
                return Err(UnifiedError::bad_request(format!(
                    "Invalid category \"{category}\""
                )));
            }
            entry.category = category;
        }
        if let Some(title) = input.title {
            if title.trim().is_empty() {
                return Err(UnifiedError::bad_request("title must not be empty"));
            }
            entry.title = title.trim().to_string();
        }
        if let Some(body) = input.body {
            if body.trim().is_empty() {
                return Err(UnifiedError::bad_request("body must not be empty"));
            }
            entry.body = body.trim().to_string();
        }
        if let Some(link) = input.link {
            if !(link.starts_with("https://") || link.starts_with("http://")) {
                return Err(UnifiedError::bad_request(
                    "link must be an http(s) URL",
                ));
            }
            entry.link = Some(link.trim().to_string());
        }
        entry.updated_at = Utc::now();
        Ok(entry.clone())
    })();
    json::respond(result, false)
}

async fn delete_changelog(
    state: web::Data<Arc<AnnouncementsState>>,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<Value> = (|| {
        let id = parse_uuid(path.as_str(), "changelog entry")?;
        if !state.changelog.contains_key(&id) {
            return Err(UnifiedError::not_found("Changelog entry not found"));
        }
        state.changelog.remove(&id);
        Ok(json!({ "deleted": true, "id": id }))
    })();
    json::respond(result, false)
}

// ---------------------------------------------------------------------------
// Module
// ---------------------------------------------------------------------------

pub struct AnnouncementsModule {
    state: Arc<AnnouncementsState>,
}

impl AnnouncementsModule {
    pub fn new() -> Self {
        Self {
            state: Arc::new(AnnouncementsState::new()),
        }
    }
}

impl Default for AnnouncementsModule {
    fn default() -> Self {
        Self::new()
    }
}

impl Module for AnnouncementsModule {
    fn id(&self) -> &'static str {
        "announcements"
    }

    fn name(&self) -> &'static str {
        "Announcements and Changelog"
    }

    fn description(&self) -> &'static str {
        "Announcements with audience targeting (all, users, admins, role, department, segment, explicit users), channels (in-app, email, webhook), scheduling with publish-due, expiry, pinning, and read tracking, plus a public changelog and a user-dashboard feed."
    }

    fn requires(&self) -> &[&'static str] {
        &["notifications"]
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
        // Publish-due entry point for the scheduler (or any other
        // module): `{}`. The scheduler needs the
        // `announcement.publish_due` job kind to drive this on a
        // schedule (see the report); the endpoint
        // `POST /api/v1/announcements/publish-due` does the same
        // thing on demand.
        let state = self.state.clone();
        bus.register(
            "announcement.publish_due",
            "announcements",
            Arc::new(move |ctx: &mut HookContext| {
                let published = state.publish_due();
                ctx.add_result(json!({
                    "published": published.len(),
                    "announcement_ids": published
                        .iter()
                        .map(|announcement| announcement.id)
                        .collect::<Vec<_>>(),
                }));
            }),
        );
    }

    fn configure_routes(&self, cfg: &mut web::ServiceConfig) {
        cfg.app_data(web::Data::new(self.state.clone()));
        cfg.service(
            web::scope("/api/v1/announcements")
                .route("", web::get().to(list_announcements))
                .route("", web::post().to(create_announcement))
                // Static sub-paths must be registered before `{id}` so
                // Actix does not match them as an announcement id.
                .route("/public", web::get().to(list_public_announcements))
                .route("/unread", web::get().to(list_unread))
                .route("/dashboard", web::get().to(dashboard_feed))
                .route("/publish-due", web::post().to(publish_due))
                .route("/changelog", web::get().to(list_changelog))
                .route("/changelog", web::post().to(create_changelog))
                .route("/changelog/{id}", web::get().to(get_changelog))
                .route("/changelog/{id}", web::put().to(update_changelog))
                .route("/changelog/{id}", web::delete().to(delete_changelog))
                .route("/{id}", web::get().to(get_announcement))
                .route("/{id}", web::put().to(update_announcement))
                .route("/{id}", web::delete().to(delete_announcement))
                .route("/{id}/publish", web::post().to(publish_announcement))
                .route("/{id}/archive", web::post().to(archive_announcement))
                .route("/{id}/read", web::post().to(mark_announcement_read))
                .route("/{id}/reads", web::get().to(list_announcement_reads)),
        );
    }

    fn route_info(&self) -> Vec<crate::core::modules::RouteInfo> {
        use crate::core::modules::RouteInfo;
        vec![
            RouteInfo { method: "GET", path: "/api/v1/announcements", owner: "announcements", description: "List announcements, all statuses (paginated)." },
            RouteInfo { method: "POST", path: "/api/v1/announcements", owner: "announcements", description: "Create an announcement draft." },
            RouteInfo { method: "GET", path: "/api/v1/announcements/public", owner: "announcements", description: "PUBLIC: published, currently visible announcements, audience-filtered for the caller (?user_id=, roles=, departments=, segments=)." },
            RouteInfo { method: "GET", path: "/api/v1/announcements/unread", owner: "announcements", description: "Published announcements a user has not read yet (?user_id=)." },
            RouteInfo { method: "GET", path: "/api/v1/announcements/dashboard", owner: "announcements", description: "User-dashboard feed: audience-filtered announcements plus the unread count (?user_id=)." },
            RouteInfo { method: "POST", path: "/api/v1/announcements/publish-due", owner: "announcements", description: "Publish every announcement whose publish time has arrived (also via the announcement.publish_due hook)." },
            RouteInfo { method: "GET", path: "/api/v1/announcements/{id}", owner: "announcements", description: "Get one announcement." },
            RouteInfo { method: "PUT", path: "/api/v1/announcements/{id}", owner: "announcements", description: "Update an announcement." },
            RouteInfo { method: "DELETE", path: "/api/v1/announcements/{id}", owner: "announcements", description: "Delete an announcement and its read receipts." },
            RouteInfo { method: "POST", path: "/api/v1/announcements/{id}/publish", owner: "announcements", description: "Publish an announcement (fans out immediately when its publish time has arrived; otherwise stays scheduled until publish-due)." },
            RouteInfo { method: "POST", path: "/api/v1/announcements/{id}/archive", owner: "announcements", description: "Archive an announcement." },
            RouteInfo { method: "POST", path: "/api/v1/announcements/{id}/read", owner: "announcements", description: "Mark an announcement read for a user." },
            RouteInfo { method: "GET", path: "/api/v1/announcements/{id}/reads", owner: "announcements", description: "List read receipts for an announcement (paginated)." },
            RouteInfo { method: "GET", path: "/api/v1/announcements/changelog", owner: "announcements", description: "PUBLIC: changelog entries, newest first (paginated)." },
            RouteInfo { method: "POST", path: "/api/v1/announcements/changelog", owner: "announcements", description: "Create a changelog entry." },
            RouteInfo { method: "GET", path: "/api/v1/announcements/changelog/{id}", owner: "announcements", description: "Get one changelog entry." },
            RouteInfo { method: "PUT", path: "/api/v1/announcements/changelog/{id}", owner: "announcements", description: "Update a changelog entry." },
            RouteInfo { method: "DELETE", path: "/api/v1/announcements/changelog/{id}", owner: "announcements", description: "Delete a changelog entry." },
        ]
    }
}

pub fn register(registry: &ModuleRegistry) {
    registry.register(Arc::new(AnnouncementsModule::new()));
}
