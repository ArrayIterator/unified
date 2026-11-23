// Diesel models for the sessions domain of the unified-db-infra bounded context.
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
#[diesel(table_name = export_schedules)]
pub struct ExportSchedule {
    pub id: Uuid,
    pub name: String,
    pub entity: String,
    pub format: String,
    pub fields: Value,
    pub filters: Value,
    pub redact_pii: bool,
    pub redact_names: bool,
    pub pii_allowed: bool,
    pub schedule: String,
    pub enabled: bool,
    pub company_id: Option<Uuid>,
    pub actor: String,
    pub last_run_at: Option<DateTime<Utc>>,
    pub last_export_id: Option<Uuid>,
    pub last_error: Option<String>,
    pub run_count: i64,
    pub scheduling: Value,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = export_schedules)]
pub struct NewExportSchedule {
    pub id: Uuid,
    pub name: String,
    pub entity: String,
    pub format: String,
    pub schedule: String,
    pub company_id: Option<Uuid>,
    pub last_run_at: Option<DateTime<Utc>>,
    pub last_export_id: Option<Uuid>,
    pub last_error: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = import_sessions)]
pub struct ImportSession {
    pub id: Uuid,
    pub entity: String,
    pub format: String,
    pub status: String,
    pub total_chunks: Option<i32>,
    pub received_chunks: Value,
    pub total_rows: i64,
    pub valid_rows: i64,
    pub invalid_rows: i64,
    pub row_errors: Value,
    pub mapping: Value,
    pub actor: String,
    pub company_id: Option<Uuid>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = import_sessions)]
pub struct NewImportSession {
    pub id: Uuid,
    pub entity: String,
    pub format: String,
    pub status: String,
    pub total_chunks: Option<i32>,
    pub company_id: Option<Uuid>,
}
