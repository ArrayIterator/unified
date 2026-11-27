//! Forums types: forums, categories, threads, posts, votes, reports,
//! subscriptions, polls, reputation, settings.
//!
//! The forums module is the public-facing forum product. It is
//! separate from the community module (members-community engine):
//! separate stores, separate routes, separate events. It has no
//! member directory and no announcements.

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

pub(crate) const MAX_POLL_OPTIONS: usize = 10;

pub(crate) const MAX_POLL_QUESTION_LEN: usize = 500;

/// Default rate limits: threads and posts per user per hour.
pub(crate) const DEFAULT_MAX_THREADS_PER_HOUR: i64 = 10;

pub(crate) const DEFAULT_MAX_POSTS_PER_HOUR: i64 = 30;

/// Default number of open reports that auto-hides a post or thread.
pub(crate) const DEFAULT_SPAM_REPORT_THRESHOLD: i64 = 5;

/// Default link limit for new users (0 means a new user may post no
/// links at all) and the age window that defines a new user.
pub(crate) const DEFAULT_NEW_USER_LINK_LIMIT: i64 = 0;

pub(crate) const DEFAULT_NEW_USER_AGE_HOURS: i64 = 24;

/// Default maximum attachments per post.
pub(crate) const DEFAULT_MAX_ATTACHMENTS_PER_POST: i64 = 5;

/// Capability names checked through the `role.check` hook.
pub(crate) const PERM_FORUMS_VIEW: &str = "forums.view";

pub(crate) const PERM_FORUMS_MANAGE: &str = "forums.manage";

pub(crate) const PERM_FORUMS_MODERATE: &str = "forums.moderate";

/// Hook event names this module listens to.
pub(crate) const EVENT_USER_DELETED: &str = "user.deleted";

pub(crate) const EVENT_GAMIFICATION_BADGE_AWARDED: &str = "gamification.badge.awarded";

pub(crate) const EVENT_SEARCH_REINDEX_REQUESTED: &str = "search.reindex.requested";

/// Reputation points awarded per event.
pub(crate) const REP_POINTS_UPVOTE_RECEIVED: i64 = 10;

pub(crate) const REP_POINTS_DOWNVOTE_RECEIVED: i64 = -2;

pub(crate) const REP_POINTS_BEST_ANSWER: i64 = 15;

/// Reputation score from raw counts: 10 per upvote received, -2
/// per downvote received, 15 per best answer.
pub(crate) fn reputation_score(upvotes: i64, downvotes: i64, best_answers: i64) -> i64 {
    REP_POINTS_UPVOTE_RECEIVED * upvotes
        + REP_POINTS_DOWNVOTE_RECEIVED * downvotes
        + REP_POINTS_BEST_ANSWER * best_answers
}

// Types

/// Who may see a forum. `public` forums appear on the public
/// routes; `members_only` forums need an authenticated caller
/// holding the `forums.view` capability; `staff_only` forums need
/// the manage capability and answer 404 to everyone else.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ForumVisibility {
    Public,
    MembersOnly,
    StaffOnly,
}

impl Default for ForumVisibility {
    fn default() -> Self {
        Self::Public
    }
}

impl ForumVisibility {
    pub(crate) fn as_str(&self) -> &'static str {
        match self {
            Self::Public => "public",
            Self::MembersOnly => "members_only",
            Self::StaffOnly => "staff_only",
        }
    }
}

/// One forum. `slug` is unique per tenant.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Forum {
    pub id: Uuid,
    pub tenant_id: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub slug: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon: Option<String>,
    pub visibility: ForumVisibility,
    pub sort_order: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// One category inside a forum. `slug` is unique per forum.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ForumCategory {
    pub id: Uuid,
    pub tenant_id: String,
    pub forum_id: Uuid,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub slug: String,
    pub sort_order: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// One discussion thread inside a forum. The opening post is a
/// `ForumPost` whose `thread_id` is this thread; creating a thread
/// also creates its opening post from the request `body`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ForumThread {
    pub id: Uuid,
    pub tenant_id: String,
    pub forum_id: Uuid,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub category_id: Option<Uuid>,
    pub title: String,
    pub author_id: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    pub pinned: bool,
    pub locked: bool,
    pub solved: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub best_answer_post_id: Option<Uuid>,
    pub view_count: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// One post. Top-level posts have `parent_post_id` None and
/// `depth` 0; a reply names the post it answers (`depth` 1); a
/// reply to a reply is `depth` 2, the maximum nesting depth.
/// `hidden` is set by moderators or by the spam-report auto-hide;
/// hidden posts are only visible to their author and to
/// moderators. `edited` marks a post whose body changed; the
/// previous bodies live in `ForumPostEdit` rows.
/// `attachment_ids` holds document/storage references only -
/// metadata, never file bytes.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ForumPost {
    pub id: Uuid,
    pub tenant_id: String,
    pub thread_id: Uuid,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_post_id: Option<Uuid>,
    pub depth: i8,
    pub author_id: String,
    pub body: String,
    pub hidden: bool,
    pub edited: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub edited_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quoted_post_id: Option<Uuid>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub attachment_ids: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub mentions: Vec<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// One kept edit of a post: the previous body, who edited it, and
/// when. Edits append a row here; the stored body is never
/// overwritten in place without a record.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ForumPostEdit {
    pub id: Uuid,
    pub tenant_id: String,
    pub post_id: Uuid,
    pub editor_id: String,
    pub previous_body: String,
    pub edited_at: DateTime<Utc>,
}

/// One vote on a post: +1 (up) or -1 (down). One row per
/// (post, user); changing a vote updates the row in place.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ForumVote {
    pub id: Uuid,
    pub tenant_id: String,
    pub post_id: Uuid,
    pub user_id: String,
    pub value: i8,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// One report raised against a post or a thread. `target_type` is
/// `post` or `thread`; `status` is `open`, `dismissed`, or
/// `actioned` (a moderator acted on it).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ForumReport {
    pub id: Uuid,
    pub tenant_id: String,
    pub target_type: String,
    pub target_id: Uuid,
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
/// solve / report review, with who did it and why.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ForumModerationAction {
    pub id: Uuid,
    pub tenant_id: String,
    pub target_type: String,
    pub target_id: Uuid,
    pub action: String,
    pub moderator_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    pub created_at: DateTime<Utc>,
}

/// One subscription: a user following a thread or a whole forum.
/// `target_type` is `thread` or `forum`. One row per
/// (user, target).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ForumSubscription {
    pub id: Uuid,
    pub tenant_id: String,
    pub user_id: String,
    pub target_type: String,
    pub target_id: Uuid,
    pub created_at: DateTime<Utc>,
}

/// One poll attached to a thread. One poll per thread.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ForumPoll {
    pub id: Uuid,
    pub tenant_id: String,
    pub thread_id: Uuid,
    pub question: String,
    pub closed: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub closed_at: Option<DateTime<Utc>>,
    pub created_by: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// One option of a poll.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ForumPollOption {
    pub id: Uuid,
    pub tenant_id: String,
    pub poll_id: Uuid,
    pub text: String,
    pub sort_order: i64,
    pub created_at: DateTime<Utc>,
}

/// One vote in a poll. One row per (poll, user); a second vote by
/// the same user in the same poll is rejected.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ForumPollVote {
    pub id: Uuid,
    pub tenant_id: String,
    pub poll_id: Uuid,
    pub option_id: Uuid,
    pub user_id: String,
    pub created_at: DateTime<Utc>,
}

/// Reputation totals for one user in one tenant. One row per
/// (tenant, user); `score` is derived from the counters by
/// `reputation_score`, and `badge_count` is maintained by the
/// `gamification.badge.awarded` listener.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ForumReputation {
    pub id: Uuid,
    pub tenant_id: String,
    pub user_id: String,
    pub score: i64,
    pub upvotes_received: i64,
    pub downvotes_received: i64,
    pub best_answers: i64,
    pub badge_count: i64,
    pub updated_at: DateTime<Utc>,
}

/// Rate limits, spam handling, and new-user restrictions,
/// configurable per installation. The in-memory copy is
/// authoritative until the migration lands.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ForumSettings {
    pub max_threads_per_hour: i64,
    pub max_posts_per_hour: i64,
    pub spam_report_threshold: i64,
    pub new_user_link_limit: i64,
    pub new_user_age_hours: i64,
    pub max_attachments_per_post: i64,
}

impl Default for ForumSettings {
    fn default() -> Self {
        Self {
            max_threads_per_hour: DEFAULT_MAX_THREADS_PER_HOUR,
            max_posts_per_hour: DEFAULT_MAX_POSTS_PER_HOUR,
            spam_report_threshold: DEFAULT_SPAM_REPORT_THRESHOLD,
            new_user_link_limit: DEFAULT_NEW_USER_LINK_LIMIT,
            new_user_age_hours: DEFAULT_NEW_USER_AGE_HOURS,
            max_attachments_per_post: DEFAULT_MAX_ATTACHMENTS_PER_POST,
        }
    }
}

// Request bodies

#[derive(Debug, Deserialize)]
pub struct CreateForumRequest {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub slug: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub visibility: Option<ForumVisibility>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sort_order: Option<i64>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateForumRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<Option<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub slug: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon: Option<Option<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub visibility: Option<ForumVisibility>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sort_order: Option<i64>,
}

#[derive(Debug, Deserialize)]
pub struct CreateCategoryRequest {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub slug: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sort_order: Option<i64>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateCategoryRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<Option<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub slug: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sort_order: Option<i64>,
}

/// Creating a thread ALSO creates its opening post from `body`:
/// the thread has no body of its own, so the first post carries
/// the opening text.
#[derive(Debug, Deserialize)]
pub struct CreateThreadRequest {
    pub forum_id: Uuid,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub category_id: Option<Uuid>,
    pub title: String,
    #[serde(default)]
    pub tags: Vec<String>,
    pub body: String,
}

#[derive(Debug, Deserialize)]
pub struct UpdateThreadRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub category_id: Option<Uuid>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tags: Option<Vec<String>>,
}

#[derive(Debug, Deserialize)]
pub struct SolveThreadRequest {
    pub best_answer_post_id: Uuid,
}

#[derive(Debug, Deserialize)]
pub struct CreatePostRequest {
    pub body: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_post_id: Option<Uuid>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quoted_post_id: Option<Uuid>,
    #[serde(default)]
    pub attachment_ids: Vec<String>,
    #[serde(default)]
    pub mentions: Vec<String>,
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
pub struct ReportRequest {
    pub reason: String,
}

#[derive(Debug, Deserialize)]
pub struct ReviewReportRequest {
    /// `dismiss` clears the report; `hide` hides the target;
    /// `delete` deletes the target.
    pub action: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct CreatePollRequest {
    pub question: String,
    pub options: Vec<String>,
}

#[derive(Debug, Deserialize)]
pub struct VotePollRequest {
    pub option_id: Uuid,
}

#[derive(Debug, Deserialize)]
pub struct UpdateSettingsRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_threads_per_hour: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_posts_per_hour: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spam_report_threshold: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub new_user_link_limit: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub new_user_age_hours: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_attachments_per_post: Option<i64>,
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
    fn forum_visibility_default_is_public() {
        assert_eq!(ForumVisibility::default(), ForumVisibility::Public);
    }

    #[test]
    fn forum_visibility_as_str_round_trips() {
        assert_eq!(ForumVisibility::Public.as_str(), "public");
        assert_eq!(ForumVisibility::MembersOnly.as_str(), "members_only");
        assert_eq!(ForumVisibility::StaffOnly.as_str(), "staff_only");
    }

    #[test]
    fn reputation_score_formula() {
        assert_eq!(reputation_score(0, 0, 0), 0);
        assert_eq!(reputation_score(1, 0, 0), REP_POINTS_UPVOTE_RECEIVED);
        assert_eq!(reputation_score(0, 1, 0), REP_POINTS_DOWNVOTE_RECEIVED);
        assert_eq!(reputation_score(0, 0, 1), REP_POINTS_BEST_ANSWER);
        assert_eq!(reputation_score(3, 1, 2), 10 * 3 - 2 * 1 + 15 * 2);
    }

    #[test]
    fn poll_option_limit_constant_sanity() {
        assert!(MAX_POLL_OPTIONS > 1);
        assert_eq!(MAX_POLL_OPTIONS, 10);
        assert!(MAX_POLL_QUESTION_LEN > 0);
        assert_eq!(MAX_POLL_QUESTION_LEN, 500);
    }

    #[test]
    fn forum_settings_default_uses_constants() {
        let settings = ForumSettings::default();
        assert_eq!(settings.max_threads_per_hour, DEFAULT_MAX_THREADS_PER_HOUR);
        assert_eq!(settings.max_posts_per_hour, DEFAULT_MAX_POSTS_PER_HOUR);
        assert_eq!(
            settings.spam_report_threshold,
            DEFAULT_SPAM_REPORT_THRESHOLD
        );
        assert_eq!(settings.new_user_link_limit, DEFAULT_NEW_USER_LINK_LIMIT);
        assert_eq!(settings.new_user_age_hours, DEFAULT_NEW_USER_AGE_HOURS);
        assert_eq!(
            settings.max_attachments_per_post,
            DEFAULT_MAX_ATTACHMENTS_PER_POST
        );
    }
}
