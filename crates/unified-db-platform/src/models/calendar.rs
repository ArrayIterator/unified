// Diesel models for the calendar domain of the unified-db-platform bounded context.
//
// Split out of `src/models.rs` (see `src/lib.rs`); the structs below are
// byte-identical to the originals, only regrouped.

use crate::schema::*;
use chrono::{DateTime, Utc};
use diesel::prelude::*;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

// ---------------------------------------------------------------------------
// Calendar (migration 000101_calendar). `module` is a Rust keyword,
// so the struct field is `module_` (Diesel strips the underscore).
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = calendar_feeds)]
pub struct CalendarFeed {
    pub id: Uuid,
    pub tenant_id: String,
    pub company_id: Option<String>,
    pub user_id: String,
    pub token_hash: String,
    pub modules: Value,
    pub label: Option<String>,
    pub expires_at: DateTime<Utc>,
    pub revoked: bool,
    pub created_at: DateTime<Utc>,
    pub last_used_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = calendar_feeds)]
pub struct NewCalendarFeed {
    pub id: Uuid,
    pub tenant_id: String,
    pub company_id: Option<String>,
    pub user_id: String,
    pub token_hash: String,
    pub label: Option<String>,
    pub expires_at: DateTime<Utc>,
    pub last_used_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = calendar_settings)]
pub struct CalendarSetting {
    pub id: Uuid,
    pub tenant_id: String,
    pub scope: String,
    pub owner_id: String,
    pub weekend_days: Value,
    pub working_hours_start: String,
    pub working_hours_end: String,
    pub default_view: String,
    pub timezone: String,
    pub holiday_country: String,
    pub create_link_templates: Value,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = calendar_settings)]
pub struct NewCalendarSetting {
    pub id: Uuid,
    pub tenant_id: String,
    pub scope: String,
    pub owner_id: String,
    pub holiday_country: String,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = calendar_sources)]
pub struct CalendarSource {
    pub id: Uuid,
    pub tenant_id: String,
    pub user_id: String,
    pub module_: String,
    pub visible: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = calendar_sources)]
pub struct NewCalendarSource {
    pub id: Uuid,
    pub tenant_id: String,
    pub user_id: String,
    pub module_: String,
}
