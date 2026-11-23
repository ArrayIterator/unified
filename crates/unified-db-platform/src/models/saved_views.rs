// Diesel models for the saved_views domain of the unified-db-platform bounded context.
//
// Split out of `src/models.rs` (see `src/lib.rs`); the structs below are
// byte-identical to the originals, only regrouped.

use crate::schema::*;
use chrono::{DateTime, Utc};
use diesel::prelude::*;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = saved_views)]
pub struct SavedView {
    pub id: Uuid,
    pub user_id: String,
    pub module_id: String,
    pub name: String,
    pub filter: Value,
    pub sort: Option<String>,
    pub order: Option<String>,
    pub visibility: String,
    pub is_default: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = saved_views)]
pub struct NewSavedView {
    pub id: Uuid,
    pub user_id: String,
    pub module_id: String,
    pub name: String,
    pub sort: Option<String>,
    pub order: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = timeline_entries)]
pub struct TimelineEntry {
    pub id: Uuid,
    pub seq: i64,
    pub actor: String,
    pub action: String,
    pub entity_type: String,
    pub entity_id: String,
    pub summary: String,
    pub source: String,
    pub details: Value,
    pub ip: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = timeline_entries)]
pub struct NewTimelineEntry {
    pub id: Uuid,
    pub seq: i64,
    pub actor: String,
    pub action: String,
    pub entity_type: String,
    pub entity_id: String,
    pub source: String,
    pub ip: Option<String>,
}
