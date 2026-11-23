// Diesel models for the announcements domain of the unified-db-platform bounded context.
//
// Split out of `src/models.rs` (see `src/lib.rs`); the structs below are
// byte-identical to the originals, only regrouped.

use crate::schema::*;
use chrono::{DateTime, Utc};
use diesel::prelude::*;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = announcements)]
pub struct Announcement {
    pub id: Uuid,
    pub title: String,
    pub body: String,
    pub audience: String,
    pub status: String,
    pub publish_at: Option<DateTime<Utc>>,
    pub expires_at: Option<DateTime<Utc>>,
    pub pinned: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub published_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = announcements)]
pub struct NewAnnouncement {
    pub id: Uuid,
    pub title: String,
    pub body: String,
    pub status: String,
    pub publish_at: Option<DateTime<Utc>>,
    pub expires_at: Option<DateTime<Utc>>,
    pub published_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = changelog_entries)]
pub struct ChangelogEntry {
    pub id: Uuid,
    pub version: String,
    pub date: String,
    pub category: String,
    pub title: String,
    pub body: String,
    pub link: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = changelog_entries)]
pub struct NewChangelogEntry {
    pub id: Uuid,
    pub version: String,
    pub date: String,
    pub title: String,
    pub body: String,
    pub link: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = announcement_read_receipts)]
pub struct AnnouncementReadReceipt {
    pub announcement_id: Uuid,
    pub user_id: String,
    pub read_at: DateTime<Utc>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = announcement_read_receipts)]
pub struct NewAnnouncementReadReceipt {
    pub announcement_id: Uuid,
    pub user_id: String,
}
