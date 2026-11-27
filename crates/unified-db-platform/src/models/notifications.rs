// Diesel models for the notifications domain of the unified-db-platform bounded context.
//
// Split out of `src/models.rs` (see `src/lib.rs`); the structs below are
// byte-identical to the originals, only regrouped.

use crate::schema::*;
use chrono::{DateTime, Utc};
use diesel::prelude::*;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

// Search, notifications, and saved views (2026-11-22-000023_search_notifications_saved_views).

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = search_documents)]
pub struct SearchDocument {
    pub kind: String,
    pub id: String,
    pub title: String,
    pub snippet: String,
    pub url: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = search_documents)]
pub struct NewSearchDocument {
    pub kind: String,
    pub id: String,
    pub title: String,
    pub url: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = notifications)]
pub struct Notification {
    pub id: Uuid,
    pub user_id: String,
    pub title: String,
    pub body: String,
    pub kind: String,
    pub read: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = notifications)]
pub struct NewNotification {
    pub id: Uuid,
    pub user_id: String,
    pub title: String,
    pub body: String,
    pub kind: String,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = notification_preferences)]
pub struct NotificationPreference {
    pub user_id: String,
    pub channels: Value,
    pub categories: Value,
    pub channel_by_category: Value,
    pub digest: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = notification_preferences)]
pub struct NewNotificationPreference {
    pub user_id: String,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = notification_templates)]
pub struct NotificationTemplate {
    pub id: Uuid,
    pub event: String,
    pub locale: String,
    pub title_template: String,
    pub body_template: String,
    pub channels: Value,
    pub email_connection_id: Option<Uuid>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = notification_templates)]
pub struct NewNotificationTemplate {
    pub id: Uuid,
    pub event: String,
    pub title_template: String,
    pub body_template: String,
    pub email_connection_id: Option<Uuid>,
}
