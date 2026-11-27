// Diesel models for the inbox domain of the unified-db-platform bounded context.
//
// Split out of `src/models.rs` (see `src/lib.rs`); the structs below are
// byte-identical to the originals, only regrouped.

use crate::schema::*;
use chrono::{DateTime, Utc};
use diesel::prelude::*;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

// Smart inbox (migration 000054_inbox).

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = inbox_threads)]
pub struct InboxThread {
    pub id: Uuid,
    pub source_channel: String,
    pub source_id: String,
    pub subject: String,
    pub customer_id: Option<String>,
    pub customer_label: Option<String>,
    pub participants: Value,
    pub department_id: Option<String>,
    pub assignee_id: Option<String>,
    pub status: String,
    pub priority: String,
    pub unread: bool,
    pub snoozed_until: Option<DateTime<Utc>>,
    pub snooze_scheduler_status: String,
    pub sla_due_at: Option<DateTime<Utc>>,
    pub sla_breached: bool,
    pub merged_into: Option<Uuid>,
    pub last_activity_at: DateTime<Utc>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = inbox_threads)]
pub struct NewInboxThread {
    pub id: Uuid,
    pub source_channel: String,
    pub source_id: String,
    pub subject: String,
    pub customer_id: Option<String>,
    pub customer_label: Option<String>,
    pub department_id: Option<String>,
    pub assignee_id: Option<String>,
    pub snoozed_until: Option<DateTime<Utc>>,
    pub sla_due_at: Option<DateTime<Utc>>,
    pub merged_into: Option<Uuid>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = inbox_assignments)]
pub struct InboxAssignment {
    pub id: Uuid,
    pub thread_id: Uuid,
    pub assignee_id: String,
    pub assigned_by: String,
    pub active: bool,
    pub assigned_at: DateTime<Utc>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = inbox_assignments)]
pub struct NewInboxAssignment {
    pub id: Uuid,
    pub thread_id: Uuid,
    pub assignee_id: String,
    pub assigned_by: String,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = inbox_canned_replies)]
pub struct InboxCannedReply {
    pub id: Uuid,
    pub department_id: Option<String>,
    pub title: String,
    pub body: String,
    pub created_by: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = inbox_canned_replies)]
pub struct NewInboxCannedReply {
    pub id: Uuid,
    pub department_id: Option<String>,
    pub title: String,
    pub body: String,
    pub created_by: String,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = inbox_thread_reads)]
pub struct InboxThreadRead {
    pub thread_id: Uuid,
    pub user_id: String,
    pub read_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = inbox_thread_reads)]
pub struct NewInboxThreadRead {
    pub thread_id: Uuid,
    pub user_id: String,
}
