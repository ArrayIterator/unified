// Diesel models for the community domain of the unified-db-platform bounded context.
//
// Split out of `src/models.rs` (see `src/lib.rs`); the structs below are
// byte-identical to the originals, only regrouped.

use crate::schema::*;
use chrono::{DateTime, Utc};
use diesel::prelude::*;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

// Community (migration 000056_community).

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = community_categories)]
pub struct CommunityCategory {
    pub id: Uuid,
    pub name: String,
    pub description: Option<String>,
    pub slug: String,
    pub sort_order: i64,
    pub visibility: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = community_categories)]
pub struct NewCommunityCategory {
    pub id: Uuid,
    pub name: String,
    pub description: Option<String>,
    pub slug: String,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = community_topics)]
pub struct CommunityTopic {
    pub id: Uuid,
    pub category_id: Uuid,
    pub title: String,
    pub author_id: String,
    pub tags: Vec<String>,
    pub pinned: bool,
    pub locked: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = community_topics)]
pub struct NewCommunityTopic {
    pub id: Uuid,
    pub category_id: Uuid,
    pub title: String,
    pub author_id: String,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = community_posts)]
pub struct CommunityPost {
    pub id: Uuid,
    pub topic_id: Uuid,
    pub parent_post_id: Option<Uuid>,
    pub author_id: String,
    pub body: String,
    pub hidden: bool,
    pub edited: bool,
    pub edited_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = community_posts)]
pub struct NewCommunityPost {
    pub id: Uuid,
    pub topic_id: Uuid,
    pub parent_post_id: Option<Uuid>,
    pub author_id: String,
    pub body: String,
    pub edited_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = community_post_edits)]
pub struct CommunityPostEdit {
    pub id: Uuid,
    pub post_id: Uuid,
    pub editor_id: String,
    pub previous_body: String,
    pub edited_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = community_post_edits)]
pub struct NewCommunityPostEdit {
    pub id: Uuid,
    pub post_id: Uuid,
    pub editor_id: String,
    pub previous_body: String,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = community_votes)]
pub struct CommunityVote {
    pub id: Uuid,
    pub post_id: Uuid,
    pub user_id: String,
    pub value: i16,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = community_votes)]
pub struct NewCommunityVote {
    pub id: Uuid,
    pub post_id: Uuid,
    pub user_id: String,
    pub value: i16,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = community_flags)]
pub struct CommunityFlag {
    pub id: Uuid,
    pub post_id: Uuid,
    pub reporter_id: String,
    pub reason: String,
    pub status: String,
    pub reviewed_by: Option<String>,
    pub reviewed_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = community_flags)]
pub struct NewCommunityFlag {
    pub id: Uuid,
    pub post_id: Uuid,
    pub reporter_id: String,
    pub reason: String,
    pub reviewed_by: Option<String>,
    pub reviewed_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = community_moderation_actions)]
pub struct CommunityModerationAction {
    pub id: Uuid,
    pub target_type: String,
    pub target_id: Uuid,
    pub action: String,
    pub moderator_id: String,
    pub reason: Option<String>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = community_moderation_actions)]
pub struct NewCommunityModerationAction {
    pub id: Uuid,
    pub target_type: String,
    pub target_id: Uuid,
    pub action: String,
    pub moderator_id: String,
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = community_members)]
pub struct CommunityMember {
    pub id: Uuid,
    pub user_id: String,
    pub display_name: String,
    pub badge_count: i64,
    pub joined_at: DateTime<Utc>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = community_members)]
pub struct NewCommunityMember {
    pub id: Uuid,
    pub user_id: String,
    pub display_name: String,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = community_announcements)]
pub struct CommunityAnnouncement {
    pub id: Uuid,
    pub title: String,
    pub body: String,
    pub pinned: bool,
    pub published: bool,
    pub published_at: Option<DateTime<Utc>>,
    pub author_id: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = community_announcements)]
pub struct NewCommunityAnnouncement {
    pub id: Uuid,
    pub title: String,
    pub body: String,
    pub author_id: String,
    pub published_at: Option<DateTime<Utc>>,
}
