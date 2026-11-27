// Diesel models for the webinars domain of the unified-db-platform bounded context.
//
// Split out of `src/models.rs` (see `src/lib.rs`); the structs below are
// byte-identical to the originals, only regrouped.

use crate::schema::*;
use chrono::{DateTime, Utc};
use diesel::prelude::*;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

// Webinars (migration 000081_webinars).

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = webinars)]
pub struct Webinar {
    pub id: Uuid,
    pub title: String,
    pub description: Option<String>,
    pub host_id: String,
    pub co_host_ids: Vec<String>,
    pub panelist_ids: Vec<String>,
    pub status: String,
    pub access_model: String,
    pub product_id: Option<String>,
    pub capacity: i32,
    pub requires_approval: bool,
    pub allow_waitlist: bool,
    pub chat_enabled: bool,
    pub qa_enabled: bool,
    pub polls_enabled: bool,
    pub recording_enabled: bool,
    pub meeting_room_id: Option<Uuid>,
    pub meeting_link_status: String,
    pub chat_channel_id: Option<String>,
    pub survey_id: Option<String>,
    pub survey_distribution_id: Option<String>,
    pub starts_at: Option<DateTime<Utc>>,
    pub ends_at: Option<DateTime<Utc>>,
    pub timezone: String,
    pub recurrence_rule: Option<String>,
    pub scheduler_job_id: Option<String>,
    pub scheduler_status: String,
    pub reminder_scheduler_status: String,
    pub started_at: Option<DateTime<Utc>>,
    pub ended_at: Option<DateTime<Utc>>,
    pub follow_up_sent_at: Option<DateTime<Utc>>,
    pub follow_up_count: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = webinars)]
pub struct NewWebinar {
    pub id: Uuid,
    pub title: String,
    pub description: Option<String>,
    pub host_id: String,
    pub product_id: Option<String>,
    pub meeting_room_id: Option<Uuid>,
    pub chat_channel_id: Option<String>,
    pub survey_id: Option<String>,
    pub survey_distribution_id: Option<String>,
    pub starts_at: Option<DateTime<Utc>>,
    pub ends_at: Option<DateTime<Utc>>,
    pub recurrence_rule: Option<String>,
    pub scheduler_job_id: Option<String>,
    pub started_at: Option<DateTime<Utc>>,
    pub ended_at: Option<DateTime<Utc>>,
    pub follow_up_sent_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = webinar_registrations)]
pub struct WebinarRegistration {
    pub id: Uuid,
    pub webinar_id: Uuid,
    pub user_id: String,
    pub status: String,
    pub payment_status: String,
    pub registered_at: DateTime<Utc>,
    pub approved_at: Option<DateTime<Utc>>,
    pub cancelled_at: Option<DateTime<Utc>>,
    pub promoted_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = webinar_registrations)]
pub struct NewWebinarRegistration {
    pub id: Uuid,
    pub webinar_id: Uuid,
    pub user_id: String,
    pub approved_at: Option<DateTime<Utc>>,
    pub cancelled_at: Option<DateTime<Utc>>,
    pub promoted_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = webinar_polls)]
pub struct WebinarPoll {
    pub id: Uuid,
    pub webinar_id: Uuid,
    pub question: String,
    pub options: Vec<String>,
    pub allow_multiple: bool,
    pub status: String,
    pub created_by: String,
    pub created_at: DateTime<Utc>,
    pub closed_at: Option<DateTime<Utc>>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = webinar_polls)]
pub struct NewWebinarPoll {
    pub id: Uuid,
    pub webinar_id: Uuid,
    pub question: String,
    pub options: Vec<String>,
    pub created_by: String,
    pub closed_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = webinar_poll_votes)]
pub struct WebinarPollVote {
    pub id: Uuid,
    pub poll_id: Uuid,
    pub webinar_id: Uuid,
    pub user_id: String,
    pub option_indexes: Vec<i32>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = webinar_poll_votes)]
pub struct NewWebinarPollVote {
    pub id: Uuid,
    pub poll_id: Uuid,
    pub webinar_id: Uuid,
    pub user_id: String,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = webinar_questions)]
pub struct WebinarQuestion {
    pub id: Uuid,
    pub webinar_id: Uuid,
    pub author_id: String,
    pub body: String,
    pub upvoted_by: Vec<String>,
    pub answered: bool,
    pub answered_by: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = webinar_questions)]
pub struct NewWebinarQuestion {
    pub id: Uuid,
    pub webinar_id: Uuid,
    pub author_id: String,
    pub body: String,
    pub answered_by: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = webinar_attendance)]
pub struct WebinarAttendance {
    pub id: Uuid,
    pub webinar_id: Uuid,
    pub user_id: String,
    pub joined_at: DateTime<Utc>,
    pub left_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = webinar_attendance)]
pub struct NewWebinarAttendance {
    pub id: Uuid,
    pub webinar_id: Uuid,
    pub user_id: String,
    pub left_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = webinar_recordings)]
pub struct WebinarRecording {
    pub id: Uuid,
    pub webinar_id: Uuid,
    pub meeting_recording_id: Option<Uuid>,
    pub title: Option<String>,
    pub status: String,
    pub storage_target: Option<String>,
    pub storage_key: Option<String>,
    pub storage_url: Option<String>,
    pub size_bytes: Option<i64>,
    pub duration_secs: Option<i64>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = webinar_recordings)]
pub struct NewWebinarRecording {
    pub id: Uuid,
    pub webinar_id: Uuid,
    pub meeting_recording_id: Option<Uuid>,
    pub title: Option<String>,
    pub storage_target: Option<String>,
    pub storage_key: Option<String>,
    pub storage_url: Option<String>,
    pub size_bytes: Option<i64>,
    pub duration_secs: Option<i64>,
}
