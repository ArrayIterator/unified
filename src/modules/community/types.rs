//! Community types: categories, topics, posts, votes, flags, members.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Maximum lengths enforced on user supplied text.
pub(crate) const MAX_NAME_LEN: usize = 200;

pub(crate) const MAX_TITLE_LEN: usize = 200;

pub(crate) const MAX_BODY_LEN: usize = 20000;

pub(crate) const MAX_DESCRIPTION_LEN: usize = 5000;

pub(crate) const MAX_REASON_LEN: usize = 1000;

pub(crate) const MAX_SLUG_LEN: usize = 128;

pub(crate) const MAX_TAG_LEN: usize = 64;

pub(crate) const MAX_TAGS: usize = 20;

/// Default rate limits: topics and posts per user per hour.
pub(crate) const DEFAULT_MAX_TOPICS_PER_HOUR: i64 = 10;

pub(crate) const DEFAULT_MAX_POSTS_PER_HOUR: i64 = 30;

/// Default number of open flags that auto-hides a post.
pub(crate) const DEFAULT_SPAM_FLAG_THRESHOLD: i64 = 5;

/// Capability names checked through the `role.check` hook.
pub(crate) const PERM_COMMUNITY_VIEW: &str = "community.view";

pub(crate) const PERM_COMMUNITY_MANAGE: &str = "community.manage";

/// Hook event name this module listens to.
pub(crate) const GAMIFICATION_BADGE_AWARDED: &str = "gamification.badge.awarded";

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

/// Who may see a category. `public` categories appear on the public
/// routes; `members_only` categories need a stored community
/// membership for the caller (the view capability alone is not
/// enough); `staff_only` categories need the manage capability and
/// answer 404 to everyone else.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CategoryVisibility {
    Public,
    MembersOnly,
    StaffOnly,
}

impl Default for CategoryVisibility {
    fn default() -> Self {
        Self::Public
    }
}

impl CategoryVisibility {
    pub(crate) fn as_str(&self) -> &'static str {
        match self {
            Self::Public => "public",
            Self::MembersOnly => "members_only",
            Self::StaffOnly => "staff_only",
        }
    }
}

/// One discussion category.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommunityCategory {
    pub id: Uuid,
    pub name: String,
    pub description: Option<String>,
    pub slug: String,
    pub sort_order: i64,
    pub visibility: CategoryVisibility,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// One discussion topic inside a category.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommunityTopic {
    pub id: Uuid,
    pub category_id: Uuid,
    pub title: String,
    pub author_id: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    pub pinned: bool,
    pub locked: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// One post. Top-level posts have `parent_post_id` None; a reply
/// names the post it answers, and that parent must itself be a
/// top-level post, so replies are exactly one level deep.
/// `hidden` is set by moderators or by the spam-flag auto-hide;
/// hidden posts are only visible to moderators. `edited` marks a
/// post whose body changed; the previous bodies live in
/// `CommunityPostEdit` rows.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommunityPost {
    pub id: Uuid,
    pub topic_id: Uuid,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_post_id: Option<Uuid>,
    pub author_id: String,
    pub body: String,
    pub hidden: bool,
    pub edited: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub edited_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// One kept edit of a post: the previous body, who edited it, and
/// when. Edits append a row here; the stored body is never
/// overwritten in place without a record.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommunityPostEdit {
    pub id: Uuid,
    pub post_id: Uuid,
    pub editor_id: String,
    pub previous_body: String,
    pub edited_at: DateTime<Utc>,
}

/// One vote on a post: +1 (up) or -1 (down). One row per
/// (post, user); changing a vote updates the row in place.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommunityVote {
    pub id: Uuid,
    pub post_id: Uuid,
    pub user_id: String,
    pub value: i8,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// One flag raised against a post. `status` is `open`, `dismissed`,
/// or `actioned` (a moderator acted on it).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommunityFlag {
    pub id: Uuid,
    pub post_id: Uuid,
    pub reporter_id: String,
    pub reason: String,
    pub status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reviewed_by: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reviewed_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// One audited moderator action: hide / unhide / pin / lock /
/// flag review, with who did it and why.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommunityModerationAction {
    pub id: Uuid,
    pub target_type: String,
    pub target_id: Uuid,
    pub action: String,
    pub moderator_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    pub created_at: DateTime<Utc>,
}

/// One community member. The directory exposes display name, join
/// date, badge counts, and activity counts only — never emails or
/// other private data. `badge_count` is maintained by the
/// `gamification.badge.awarded` listener.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommunityMember {
    pub id: Uuid,
    pub user_id: String,
    pub display_name: String,
    pub badge_count: i64,
    pub joined_at: DateTime<Utc>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// One announcement. Drafts are created unpublished; publishing
/// notifies every member and stamps `published_at`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommunityAnnouncement {
    pub id: Uuid,
    pub title: String,
    pub body: String,
    pub pinned: bool,
    pub published: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub published_at: Option<DateTime<Utc>>,
    pub author_id: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Rate limits and spam handling, configurable per installation.
/// The in-memory copy is authoritative until the migration lands.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommunitySettings {
    pub max_topics_per_hour: i64,
    pub max_posts_per_hour: i64,
    pub spam_flag_threshold: i64,
}

impl Default for CommunitySettings {
    fn default() -> Self {
        Self {
            max_topics_per_hour: DEFAULT_MAX_TOPICS_PER_HOUR,
            max_posts_per_hour: DEFAULT_MAX_POSTS_PER_HOUR,
            spam_flag_threshold: DEFAULT_SPAM_FLAG_THRESHOLD,
        }
    }
}

// ---------------------------------------------------------------------------
// Request bodies
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct CreateCategoryRequest {
    pub name: String,
    pub description: Option<String>,
    pub slug: Option<String>,
    pub sort_order: Option<i64>,
    pub visibility: Option<CategoryVisibility>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateCategoryRequest {
    pub name: Option<String>,
    pub description: Option<Option<String>>,
    pub slug: Option<String>,
    pub sort_order: Option<i64>,
    pub visibility: Option<CategoryVisibility>,
}

#[derive(Debug, Deserialize)]
pub struct CreateTopicRequest {
    pub category_id: Uuid,
    pub title: String,
    #[serde(default)]
    pub tags: Vec<String>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateTopicRequest {
    pub title: Option<String>,
    pub category_id: Option<Uuid>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tags: Option<Vec<String>>,
}

#[derive(Debug, Deserialize)]
pub struct CreatePostRequest {
    pub body: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_post_id: Option<Uuid>,
}

#[derive(Debug, Deserialize)]
pub struct UpdatePostRequest {
    pub body: String,
}

#[derive(Debug, Deserialize)]
pub struct VoteRequest {
    /// +1 for up, -1 for down.
    pub value: i8,
}

#[derive(Debug, Deserialize)]
pub struct FlagRequest {
    pub reason: String,
}

#[derive(Debug, Deserialize)]
pub struct ReviewFlagRequest {
    /// `dismiss` clears the flag; `hide` hides the post.
    pub action: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct JoinRequest {
    pub display_name: String,
}

#[derive(Debug, Deserialize)]
pub struct CreateAnnouncementRequest {
    pub title: String,
    pub body: String,
    #[serde(default)]
    pub pinned: bool,
}

#[derive(Debug, Deserialize)]
pub struct UpdateAnnouncementRequest {
    pub title: Option<String>,
    pub body: Option<String>,
    pub pinned: Option<bool>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateSettingsRequest {
    pub max_topics_per_hour: Option<i64>,
    pub max_posts_per_hour: Option<i64>,
    pub spam_flag_threshold: Option<i64>,
}

#[derive(Debug, Deserialize)]
pub struct ModerationReasonRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn category_visibility_default_is_public() {
        assert_eq!(CategoryVisibility::default(), CategoryVisibility::Public);
    }

    #[test]
    fn category_visibility_as_str_round_trips() {
        assert_eq!(CategoryVisibility::Public.as_str(), "public");
        assert_eq!(CategoryVisibility::MembersOnly.as_str(), "members_only");
        assert_eq!(CategoryVisibility::StaffOnly.as_str(), "staff_only");
    }
}
