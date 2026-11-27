// Diesel models for the events domain of the unified-db-platform bounded context.
//
// Split out of `src/models.rs` (see `src/lib.rs`); the structs below are
// byte-identical to the originals, only regrouped.

use crate::schema::*;
use chrono::{DateTime, Utc};
use diesel::data_types::PgNumeric;
use diesel::prelude::*;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

// Events (migration 000034_events; batch 2 notes sec. 6). `New*` structs
// added per the notes' convention (the notes give Queryable only).

#[derive(Debug, Clone, Queryable, Selectable)]
#[diesel(table_name = events)]
pub struct EventRow {
    pub id: Uuid,
    pub title: String,
    pub description: Option<String>,
    pub slug: String,
    pub category: Vec<String>,
    pub tags: Vec<String>,
    pub venue_name: Option<String>,
    pub venue_address: Option<String>,
    pub venue_latitude: Option<f64>,
    pub venue_longitude: Option<f64>,
    pub venue_capacity: Option<i64>,
    pub starts_at: DateTime<Utc>,
    pub ends_at: DateTime<Utc>,
    pub timezone: String,
    pub visibility: String,
    pub status: String,
    pub organizer_id: String,
    pub company_id: Option<String>,
    pub cover_storage_target: Option<String>,
    pub cover_storage_key: Option<String>,
    pub recurrence: Option<Value>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = events)]
pub struct NewEventRow {
    pub id: Uuid,
    pub title: String,
    pub description: Option<String>,
    pub slug: String,
    pub venue_name: Option<String>,
    pub venue_address: Option<String>,
    pub venue_latitude: Option<f64>,
    pub venue_longitude: Option<f64>,
    pub venue_capacity: Option<i64>,
    pub starts_at: DateTime<Utc>,
    pub ends_at: DateTime<Utc>,
    pub organizer_id: String,
    pub company_id: Option<String>,
    pub cover_storage_target: Option<String>,
    pub cover_storage_key: Option<String>,
    pub recurrence: Option<Value>,
}

#[derive(Debug, Clone, Queryable, Selectable)]
#[diesel(table_name = event_occurrences)]
pub struct EventOccurrenceRow {
    pub id: Uuid,
    pub event_id: Uuid,
    pub sequence: i32,
    pub starts_at: DateTime<Utc>,
    pub ends_at: DateTime<Utc>,
    pub status: String,
    pub venue_name: Option<String>,
    pub venue_address: Option<String>,
    pub venue_latitude: Option<f64>,
    pub venue_longitude: Option<f64>,
    pub venue_capacity: Option<i64>,
    pub overridden: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = event_occurrences)]
pub struct NewEventOccurrenceRow {
    pub id: Uuid,
    pub event_id: Uuid,
    pub sequence: i32,
    pub starts_at: DateTime<Utc>,
    pub ends_at: DateTime<Utc>,
    pub venue_name: Option<String>,
    pub venue_address: Option<String>,
    pub venue_latitude: Option<f64>,
    pub venue_longitude: Option<f64>,
    pub venue_capacity: Option<i64>,
}

#[derive(Debug, Clone, Queryable, Selectable)]
#[diesel(table_name = event_ticket_types)]
pub struct EventTicketTypeRow {
    pub id: Uuid,
    pub event_id: Uuid,
    pub name: String,
    pub description: Option<String>,
    pub price: PgNumeric,
    pub currency: String,
    pub quantity: i64,
    pub sold: i64,
    pub sales_start: Option<DateTime<Utc>>,
    pub sales_end: Option<DateTime<Utc>>,
    pub per_order_limit: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable)]
#[diesel(table_name = event_ticket_types)]
pub struct NewEventTicketTypeRow {
    pub id: Uuid,
    pub event_id: Uuid,
    pub name: String,
    pub description: Option<String>,
    pub sales_start: Option<DateTime<Utc>>,
    pub sales_end: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Queryable, Selectable)]
#[diesel(table_name = event_registrations)]
pub struct EventRegistrationRow {
    pub id: Uuid,
    pub event_id: Uuid,
    pub occurrence_id: Option<Uuid>,
    pub ticket_type_id: Uuid,
    pub user_id: String,
    pub quantity: i64,
    pub total_amount: PgNumeric,
    pub currency: String,
    pub status: String,
    pub order_id: Option<String>,
    pub payment_reference: Option<String>,
    pub payment_provider: Option<String>,
    pub check_in_code: String,
    pub checked_in_at: Option<DateTime<Utc>>,
    pub cancelled_at: Option<DateTime<Utc>>,
    pub refund_reason: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable)]
#[diesel(table_name = event_registrations)]
pub struct NewEventRegistrationRow {
    pub id: Uuid,
    pub event_id: Uuid,
    pub occurrence_id: Option<Uuid>,
    pub ticket_type_id: Uuid,
    pub user_id: String,
    pub order_id: Option<String>,
    pub payment_reference: Option<String>,
    pub payment_provider: Option<String>,
    pub check_in_code: String,
    pub checked_in_at: Option<DateTime<Utc>>,
    pub cancelled_at: Option<DateTime<Utc>>,
    pub refund_reason: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = event_attendees)]
pub struct EventAttendeeRow {
    pub id: Uuid,
    pub event_id: Uuid,
    pub registration_id: Uuid,
    pub name: String,
    pub email: String,
    pub custom_answers: Value,
    pub checked_in_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = event_attendees)]
pub struct NewEventAttendeeRow {
    pub id: Uuid,
    pub event_id: Uuid,
    pub registration_id: Uuid,
    pub name: String,
    pub email: String,
    pub checked_in_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = event_waitlist)]
pub struct EventWaitlistRow {
    pub id: Uuid,
    pub event_id: Uuid,
    pub ticket_type_id: Uuid,
    pub user_id: String,
    pub quantity: i64,
    pub position: i64,
    pub status: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = event_waitlist)]
pub struct NewEventWaitlistRow {
    pub id: Uuid,
    pub event_id: Uuid,
    pub ticket_type_id: Uuid,
    pub user_id: String,
    pub position: i64,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = event_reminders)]
pub struct EventReminderRow {
    pub id: Uuid,
    pub event_id: Uuid,
    pub offset_minutes: i64,
    pub occurrence_id: Option<Uuid>,
    pub title_template: Option<String>,
    pub body_template: Option<String>,
    pub status: String,
    pub scheduler_job_id: Option<String>,
    pub scheduler_status: String,
    pub sent_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = event_reminders)]
pub struct NewEventReminderRow {
    pub id: Uuid,
    pub event_id: Uuid,
    pub offset_minutes: i64,
    pub occurrence_id: Option<Uuid>,
    pub title_template: Option<String>,
    pub body_template: Option<String>,
    pub scheduler_job_id: Option<String>,
    pub sent_at: Option<DateTime<Utc>>,
}

// Sharing / auditors (migration 000068_sharing_auditor; batch 5 notes sec. d).

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = public_links)]
pub struct PublicLinkRow {
    pub id: Uuid,
    pub token_hash: String,
    pub scope_kind: String,
    pub scope_id: Uuid,
    pub scope_view: String,
    pub created_by: String,
    pub expires_at: DateTime<Utc>,
    pub revoked_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = public_links)]
pub struct NewPublicLinkRow {
    pub id: Uuid,
    pub token_hash: String,
    pub scope_kind: String,
    pub scope_id: Uuid,
    pub scope_view: String,
    pub created_by: String,
    pub expires_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = public_link_access_log)]
pub struct PublicLinkAccessLogRow {
    pub id: Uuid,
    pub link_id: Option<Uuid>,
    pub accessed_at: DateTime<Utc>,
    pub ip_hash: Option<String>,
    pub user_agent_hash: Option<String>,
    pub allowed: bool,
    pub reason: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = public_link_access_log)]
pub struct NewPublicLinkAccessLogRow {
    pub id: Uuid,
    pub link_id: Option<Uuid>,
    pub ip_hash: Option<String>,
    pub user_agent_hash: Option<String>,
    pub allowed: bool,
    pub reason: String,
}
