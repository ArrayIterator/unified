//! Announcement and changelog types and vocabulary constants.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
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

/// Channels an announcement may use: `in_app`, `email`, `webhook`.
/// The email channel needs recipient addresses (see
/// `audience_emails`): without any address the Notifications module
/// records the email delivery as failed instead of dropping it.
pub const ANNOUNCEMENT_CHANNELS: &[&str] = &["in_app", "email", "webhook"];

pub const STATUS_DRAFT: &str = "draft";

pub const STATUS_PUBLISHED: &str = "published";

pub const STATUS_ARCHIVED: &str = "archived";

pub const CATEGORY_FEATURE: &str = "feature";

pub const CATEGORY_FIX: &str = "fix";

pub const CATEGORY_SECURITY: &str = "security";

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

pub(crate) fn default_announcement_channels() -> Vec<String> {
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
