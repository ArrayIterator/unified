// Diesel models for the storage_buckets domain of the unified-db-operations bounded context.
//
// Split out of `src/models.rs` (see `src/lib.rs`); the structs below are
// byte-identical to the originals, only regrouped.

use crate::schema::*;
use chrono::{DateTime, NaiveDate, Utc};
use diesel::prelude::*;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

// ---------------------------------------------------------------------------
// Storage manager (migration 000033_storage_manager; batch 2 notes §5).
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = storage_buckets)]
pub struct StorageBucket {
    pub id: Uuid,
    pub name: String,
    pub provider_kind: String,
    pub endpoint: String,
    pub region: String,
    pub container: String,
    pub path: String,
    pub sftp_host: String,
    pub sftp_port: i32,
    pub sftp_username: String,
    pub sftp_auth_kind: String,
    pub sftp_keyfile: String,
    pub allowed_hosts: Vec<String>,
    #[serde(skip_serializing)]
    pub credentials_encrypted: Option<String>,
    pub key_fingerprint: Option<String>,
    pub encryption_at_rest: bool,
    pub enabled: bool,
    pub company_id: Option<String>,
    pub created_by: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = storage_buckets)]
pub struct NewStorageBucket {
    pub id: Uuid,
    pub name: String,
    pub provider_kind: String,
    #[serde(skip_serializing)]
    pub credentials_encrypted: Option<String>,
    pub key_fingerprint: Option<String>,
    pub company_id: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = storage_bindings)]
pub struct StorageBinding {
    pub id: Uuid,
    pub module_id: String,
    pub bucket_id: Uuid,
    pub key_prefix: String,
    pub company_id: Option<String>,
    pub enabled: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = storage_bindings)]
pub struct NewStorageBinding {
    pub id: Uuid,
    pub module_id: String,
    pub bucket_id: Uuid,
    pub company_id: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = storage_quotas)]
pub struct StorageQuota {
    pub id: Uuid,
    pub scope: String,
    pub scope_id: String,
    pub bytes_limit: Option<i64>,
    pub objects_limit: Option<i64>,
    pub soft_bytes_limit: Option<i64>,
    pub soft_objects_limit: Option<i64>,
    pub used_bytes: i64,
    pub used_objects: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = storage_quotas)]
pub struct NewStorageQuota {
    pub id: Uuid,
    pub scope: String,
    pub scope_id: String,
    pub bytes_limit: Option<i64>,
    pub objects_limit: Option<i64>,
    pub soft_bytes_limit: Option<i64>,
    pub soft_objects_limit: Option<i64>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = storage_usage_daily)]
pub struct UsageDaily {
    pub id: Uuid,
    pub date: NaiveDate,
    pub bucket_id: Uuid,
    pub company_id: Option<String>,
    pub user_id: String,
    pub module_id: String,
    pub bytes_total: i64,
    pub objects_total: i64,
    pub bytes_delta: i64,
    pub objects_delta: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = storage_usage_daily)]
pub struct NewUsageDaily {
    pub id: Uuid,
    pub date: NaiveDate,
    pub bucket_id: Uuid,
    pub company_id: Option<String>,
    pub user_id: String,
    pub module_id: String,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = storage_lifecycle_rules)]
pub struct LifecycleRule {
    pub id: Uuid,
    pub bucket_id: Uuid,
    pub retention_days: i64,
    pub auto_delete: bool,
    pub archive_transition: bool,
    pub archive_prefix: String,
    pub enabled: bool,
    pub last_run_at: Option<DateTime<Utc>>,
    pub last_run_summary: Option<String>,
    pub last_error: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = storage_lifecycle_rules)]
pub struct NewLifecycleRule {
    pub id: Uuid,
    pub bucket_id: Uuid,
    pub last_run_at: Option<DateTime<Utc>>,
    pub last_run_summary: Option<String>,
    pub last_error: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = storage_signed_url_revocations)]
pub struct SignedUrlRecord {
    pub id: Uuid,
    pub bucket_id: Uuid,
    pub key: String,
    pub issued_by: String,
    pub expires_at: DateTime<Utc>,
    pub revoked_at: Option<DateTime<Utc>>,
    pub revoke_reason: Option<String>,
    pub provider_url_issued: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = storage_signed_url_revocations)]
pub struct NewSignedUrlRecord {
    pub id: Uuid,
    pub bucket_id: Uuid,
    pub key: String,
    pub expires_at: DateTime<Utc>,
    pub revoked_at: Option<DateTime<Utc>>,
    pub revoke_reason: Option<String>,
}
