// Diesel models for the meetings domain of the unified-db-platform bounded context.
//
// Split out of `src/models.rs` (see `src/lib.rs`); the structs below are
// byte-identical to the originals, only regrouped.

use crate::schema::*;
use chrono::{DateTime, Utc};
use diesel::prelude::*;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

// Meetings (2026-11-22-000027_meetings).

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = meeting_rooms)]
pub struct MeetingRoom {
    pub id: Uuid,
    pub title: String,
    pub description: Option<String>,
    pub host_id: String,
    pub status: String,
    pub access_model: String,
    pub product_id: Option<String>,
    pub max_participants: i32,
    pub waiting_room_enabled: bool,
    pub join_token: String,
    pub starts_at: Option<DateTime<Utc>>,
    pub ends_at: Option<DateTime<Utc>>,
    pub timezone: String,
    pub recurrence_rule: Option<String>,
    pub scheduler_job_id: Option<String>,
    pub scheduler_status: String,
    pub started_at: Option<DateTime<Utc>>,
    pub ended_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = meeting_rooms)]
pub struct NewMeetingRoom {
    pub id: Uuid,
    pub title: String,
    pub description: Option<String>,
    pub host_id: String,
    pub status: String,
    pub product_id: Option<String>,
    pub join_token: String,
    pub starts_at: Option<DateTime<Utc>>,
    pub ends_at: Option<DateTime<Utc>>,
    pub recurrence_rule: Option<String>,
    pub scheduler_job_id: Option<String>,
    pub started_at: Option<DateTime<Utc>>,
    pub ended_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = meeting_participants)]
pub struct MeetingParticipant {
    pub room_id: Uuid,
    pub user_id: String,
    pub role: String,
    pub status: String,
    pub joined_at: Option<DateTime<Utc>>,
    pub left_at: Option<DateTime<Utc>>,
    pub last_seen: DateTime<Utc>,
    pub audio_muted: bool,
    pub video_muted: bool,
    pub sharing_screen: bool,
    pub hand_raised: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = meeting_participants)]
pub struct NewMeetingParticipant {
    pub room_id: Uuid,
    pub user_id: String,
    pub joined_at: Option<DateTime<Utc>>,
    pub left_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = meeting_messages)]
pub struct MeetingMessage {
    pub id: Uuid,
    pub room_id: Uuid,
    pub author_id: String,
    pub body: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = meeting_messages)]
pub struct NewMeetingMessage {
    pub id: Uuid,
    pub room_id: Uuid,
    pub author_id: String,
    pub body: String,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = meeting_recordings)]
pub struct MeetingRecording {
    pub id: Uuid,
    pub room_id: Uuid,
    pub started_by: String,
    pub status: String,
    pub storage_target: Option<String>,
    pub storage_key: Option<String>,
    pub storage_url: Option<String>,
    pub size_bytes: Option<i64>,
    pub duration_secs: Option<i64>,
    pub started_at: DateTime<Utc>,
    pub stopped_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = meeting_recordings)]
pub struct NewMeetingRecording {
    pub id: Uuid,
    pub room_id: Uuid,
    pub started_by: String,
    pub storage_target: Option<String>,
    pub storage_key: Option<String>,
    pub storage_url: Option<String>,
    pub size_bytes: Option<i64>,
    pub duration_secs: Option<i64>,
    pub stopped_at: Option<DateTime<Utc>>,
}
