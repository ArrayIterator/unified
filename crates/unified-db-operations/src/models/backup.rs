// Diesel models for the backup domain of the unified-db-operations bounded context.
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
// Backup and storage targets (2026-11-22-000009_backup_storage).
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = backup_jobs)]
pub struct BackupJob {
    pub id: Uuid,
    pub name: String,
    pub kind: String,
    pub database_name: String,
    pub paths: Value,
    pub config_keys: Value,
    pub target: String,
    pub schedule: String,
    pub retention_keep_last: i32,
    pub retention_keep_days: i32,
    pub enabled: bool,
    pub scheduling: Value,
    pub status: String,
    pub checksum: Option<String>,
    pub size_bytes: Option<i64>,
    pub storage_key: Option<String>,
    pub last_run_id: Option<Uuid>,
    pub started_at: Option<DateTime<Utc>>,
    pub finished_at: Option<DateTime<Utc>>,
    pub error: Option<String>,
    pub company_id: Option<Uuid>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = backup_jobs)]
pub struct NewBackupJob {
    pub id: Uuid,
    pub name: String,
    pub kind: String,
    pub target: String,
    pub checksum: Option<String>,
    pub size_bytes: Option<i64>,
    pub storage_key: Option<String>,
    pub last_run_id: Option<Uuid>,
    pub started_at: Option<DateTime<Utc>>,
    pub finished_at: Option<DateTime<Utc>>,
    pub error: Option<String>,
    pub company_id: Option<Uuid>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = backup_runs)]
pub struct BackupRun {
    pub id: Uuid,
    pub job_id: Option<Uuid>,
    pub job_name: String,
    pub kind: String,
    pub target: String,
    pub storage_key: Option<String>,
    pub status: String,
    pub checksum: Option<String>,
    pub size_bytes: Option<i64>,
    pub file_count: Option<i64>,
    pub started_at: DateTime<Utc>,
    pub finished_at: Option<DateTime<Utc>>,
    pub error: Option<String>,
    pub company_id: Option<Uuid>,
    pub verified: Option<bool>,
    pub last_verified_at: Option<DateTime<Utc>>,
    pub manifest_verified: Option<bool>,
    pub verification_error: Option<String>,
    pub last_drill_at: Option<DateTime<Utc>>,
    pub drill_verified: Option<bool>,
    pub encryption: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = backup_runs)]
pub struct NewBackupRun {
    pub id: Uuid,
    pub job_id: Option<Uuid>,
    pub job_name: String,
    pub kind: String,
    pub target: String,
    pub storage_key: Option<String>,
    pub checksum: Option<String>,
    pub size_bytes: Option<i64>,
    pub file_count: Option<i64>,
    pub finished_at: Option<DateTime<Utc>>,
    pub error: Option<String>,
    pub company_id: Option<Uuid>,
    pub verified: Option<bool>,
    pub last_verified_at: Option<DateTime<Utc>>,
    pub manifest_verified: Option<bool>,
    pub verification_error: Option<String>,
    pub last_drill_at: Option<DateTime<Utc>>,
    pub drill_verified: Option<bool>,
    pub encryption: Option<String>,
}
