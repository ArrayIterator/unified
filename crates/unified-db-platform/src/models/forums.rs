// Diesel models for the forums domain of the unified-db-platform bounded context.
//
// Tables are defined by migration 2026-11-22-000110_forums. They had
// no Diesel bindings before C11: the forums module kept
// its live data in memory and the tables were persistence targets
// only. The structs below mirror that migration exactly; the
// runtime types live in `src/modules/forums/types.rs` and the
// hydrate path in `src/modules/forums/state.rs` converts between
// the two (the DB stores `visibility` as plain TEXT and the
// post `depth` / vote `value` as SMALLINT).

use crate::schema::*;
use chrono::{DateTime, Utc};
use diesel::prelude::*;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

// Forums (migration 000110_forums).

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = forums)]
pub struct Forum {
    pub id: Uuid,
    pub tenant_id: String,
    pub name: String,
    pub description: Option<String>,
    pub slug: String,
    pub icon: Option<String>,
    pub visibility: String,
    pub sort_order: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = forums)]
pub struct NewForum {
    pub id: Uuid,
    pub tenant_id: String,
    pub name: String,
    pub description: Option<String>,
    pub slug: String,
    pub icon: Option<String>,
    pub visibility: String,
    pub sort_order: i64,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = forum_categories)]
pub struct ForumCategory {
    pub id: Uuid,
    pub tenant_id: String,
    pub forum_id: Uuid,
    pub name: String,
    pub description: Option<String>,
    pub slug: String,
    pub sort_order: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = forum_categories)]
pub struct NewForumCategory {
    pub id: Uuid,
    pub tenant_id: String,
    pub forum_id: Uuid,
    pub name: String,
    pub description: Option<String>,
    pub slug: String,
    pub sort_order: i64,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = forum_threads)]
pub struct ForumThread {
    pub id: Uuid,
    pub tenant_id: String,
    pub forum_id: Uuid,
    pub category_id: Option<Uuid>,
    pub title: String,
    pub author_id: String,
    pub tags: Vec<String>,
    pub pinned: bool,
    pub locked: bool,
    pub solved: bool,
    pub best_answer_post_id: Option<Uuid>,
    pub view_count: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = forum_threads)]
pub struct NewForumThread {
    pub id: Uuid,
    pub tenant_id: String,
    pub forum_id: Uuid,
    pub category_id: Option<Uuid>,
    pub title: String,
    pub author_id: String,
    pub tags: Vec<String>,
    pub pinned: bool,
    pub locked: bool,
    pub solved: bool,
    pub best_answer_post_id: Option<Uuid>,
    pub view_count: i64,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = forum_posts)]
pub struct ForumPost {
    pub id: Uuid,
    pub tenant_id: String,
    pub thread_id: Uuid,
    pub parent_post_id: Option<Uuid>,
    pub depth: i16,
    pub author_id: String,
    pub body: String,
    pub hidden: bool,
    pub edited: bool,
    pub edited_at: Option<DateTime<Utc>>,
    pub quoted_post_id: Option<Uuid>,
    pub attachment_ids: Vec<String>,
    pub mentions: Vec<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = forum_posts)]
pub struct NewForumPost {
    pub id: Uuid,
    pub tenant_id: String,
    pub thread_id: Uuid,
    pub parent_post_id: Option<Uuid>,
    pub depth: i16,
    pub author_id: String,
    pub body: String,
    pub hidden: bool,
    pub edited: bool,
    pub edited_at: Option<DateTime<Utc>>,
    pub quoted_post_id: Option<Uuid>,
    pub attachment_ids: Vec<String>,
    pub mentions: Vec<String>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = forum_post_edits)]
pub struct ForumPostEdit {
    pub id: Uuid,
    pub tenant_id: String,
    pub post_id: Uuid,
    pub editor_id: String,
    pub previous_body: String,
    pub edited_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = forum_post_edits)]
pub struct NewForumPostEdit {
    pub id: Uuid,
    pub tenant_id: String,
    pub post_id: Uuid,
    pub editor_id: String,
    pub previous_body: String,
    pub edited_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = forum_votes)]
pub struct ForumVote {
    pub id: Uuid,
    pub tenant_id: String,
    pub post_id: Uuid,
    pub user_id: String,
    pub value: i16,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = forum_votes)]
pub struct NewForumVote {
    pub id: Uuid,
    pub tenant_id: String,
    pub post_id: Uuid,
    pub user_id: String,
    pub value: i16,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = forum_reports)]
pub struct ForumReport {
    pub id: Uuid,
    pub tenant_id: String,
    pub target_type: String,
    pub target_id: Uuid,
    pub reporter_id: String,
    pub reason: String,
    pub status: String,
    pub reviewed_by: Option<String>,
    pub reviewed_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = forum_reports)]
pub struct NewForumReport {
    pub id: Uuid,
    pub tenant_id: String,
    pub target_type: String,
    pub target_id: Uuid,
    pub reporter_id: String,
    pub reason: String,
    pub status: String,
    pub reviewed_by: Option<String>,
    pub reviewed_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = forum_moderation_actions)]
pub struct ForumModerationAction {
    pub id: Uuid,
    pub tenant_id: String,
    pub target_type: String,
    pub target_id: Uuid,
    pub action: String,
    pub moderator_id: String,
    pub reason: Option<String>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = forum_moderation_actions)]
pub struct NewForumModerationAction {
    pub id: Uuid,
    pub tenant_id: String,
    pub target_type: String,
    pub target_id: Uuid,
    pub action: String,
    pub moderator_id: String,
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = forum_subscriptions)]
pub struct ForumSubscription {
    pub id: Uuid,
    pub tenant_id: String,
    pub user_id: String,
    pub target_type: String,
    pub target_id: Uuid,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = forum_subscriptions)]
pub struct NewForumSubscription {
    pub id: Uuid,
    pub tenant_id: String,
    pub user_id: String,
    pub target_type: String,
    pub target_id: Uuid,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = forum_polls)]
pub struct ForumPoll {
    pub id: Uuid,
    pub tenant_id: String,
    pub thread_id: Uuid,
    pub question: String,
    pub closed: bool,
    pub closed_at: Option<DateTime<Utc>>,
    pub created_by: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = forum_polls)]
pub struct NewForumPoll {
    pub id: Uuid,
    pub tenant_id: String,
    pub thread_id: Uuid,
    pub question: String,
    pub closed: bool,
    pub closed_at: Option<DateTime<Utc>>,
    pub created_by: String,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = forum_poll_options)]
pub struct ForumPollOption {
    pub id: Uuid,
    pub tenant_id: String,
    pub poll_id: Uuid,
    pub text: String,
    pub sort_order: i64,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = forum_poll_options)]
pub struct NewForumPollOption {
    pub id: Uuid,
    pub tenant_id: String,
    pub poll_id: Uuid,
    pub text: String,
    pub sort_order: i64,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = forum_poll_votes)]
pub struct ForumPollVote {
    pub id: Uuid,
    pub tenant_id: String,
    pub poll_id: Uuid,
    pub option_id: Uuid,
    pub user_id: String,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = forum_poll_votes)]
pub struct NewForumPollVote {
    pub id: Uuid,
    pub tenant_id: String,
    pub poll_id: Uuid,
    pub option_id: Uuid,
    pub user_id: String,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = forum_reputation)]
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

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = forum_reputation)]
pub struct NewForumReputation {
    pub id: Uuid,
    pub tenant_id: String,
    pub user_id: String,
    pub score: i64,
    pub upvotes_received: i64,
    pub downvotes_received: i64,
    pub best_answers: i64,
    pub badge_count: i64,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = forum_settings)]
pub struct ForumSettings {
    pub tenant_id: String,
    pub max_threads_per_hour: i64,
    pub max_posts_per_hour: i64,
    pub spam_report_threshold: i64,
    pub new_user_link_limit: i64,
    pub new_user_age_hours: i64,
    pub max_attachments_per_post: i64,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = forum_settings)]
pub struct NewForumSettings {
    pub tenant_id: String,
    pub max_threads_per_hour: i64,
    pub max_posts_per_hour: i64,
    pub spam_report_threshold: i64,
    pub new_user_link_limit: i64,
    pub new_user_age_hours: i64,
    pub max_attachments_per_post: i64,
}
