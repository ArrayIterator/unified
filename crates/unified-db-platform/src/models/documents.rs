// Diesel models for the documents domain of the unified-db-platform bounded context.
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
// Documents (migration 000065_documents; metadata columns 000124_documents_metadata).
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = documents)]
pub struct Document {
    pub id: Uuid,
    pub name: String,
    pub module_id: String,
    pub company_id: Option<Uuid>,
    pub owner_id: String,
    pub scope: String,
    pub mime: String,
    pub size: i64,
    pub checksum: Option<String>,
    pub storage_key: String,
    pub current_version: i32,
    pub version_count: i32,
    pub scan_status: String,
    pub scan_threat: Option<String>,
    pub visibility: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    /// Display title; the application layer defaults it to the
    /// uploaded file name (`name`) on upload.
    pub title: String,
    /// Alt text; the only image-specific metadata field.
    pub alt_text: Option<String>,
    pub caption: Option<String>,
    pub description: Option<String>,
    /// Caller-defined keys, merged by key at the application layer
    /// (a JSON null removes a key); `notes` is the worked example.
    pub custom_metadata: Value,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = documents)]
pub struct NewDocument {
    pub id: Uuid,
    pub name: String,
    pub module_id: String,
    pub company_id: Option<Uuid>,
    pub owner_id: String,
    pub mime: String,
    pub size: i64,
    pub checksum: Option<String>,
    pub storage_key: String,
    pub scan_threat: Option<String>,
    pub visibility: String,
    /// Display title; the caller supplies the uploaded file name
    /// here (there is no useful database-side default for it).
    /// The optional metadata columns keep their database defaults
    /// (`NULL`, `'{}'::jsonb`) at insert time.
    pub title: String,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = document_versions)]
pub struct DocumentVersion {
    pub id: Uuid,
    pub document_id: Uuid,
    pub version: i32,
    pub checksum: Option<String>,
    pub storage_key: String,
    pub size: i64,
    pub mime: String,
    pub uploader_id: String,
    pub scan_status: String,
    pub scan_threat: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = document_versions)]
pub struct NewDocumentVersion {
    pub id: Uuid,
    pub document_id: Uuid,
    pub version: i32,
    pub checksum: Option<String>,
    pub storage_key: String,
    pub size: i64,
    pub mime: String,
    pub uploader_id: String,
    pub scan_threat: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = document_upload_sessions)]
pub struct DocumentUploadSession {
    pub id: Uuid,
    pub document_id: Option<Uuid>,
    pub name: String,
    pub module_id: String,
    pub company_id: Option<Uuid>,
    pub owner_id: String,
    pub total_size: i64,
    pub expected_sha256: String,
    pub chunk_size: i64,
    pub part_count: i32,
    pub status: String,
    pub expires_at: DateTime<Utc>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = document_upload_sessions)]
pub struct NewDocumentUploadSession {
    pub id: Uuid,
    pub document_id: Option<Uuid>,
    pub name: String,
    pub module_id: String,
    pub company_id: Option<Uuid>,
    pub owner_id: String,
    pub total_size: i64,
    pub expected_sha256: String,
    pub chunk_size: i64,
    pub part_count: i32,
    pub expires_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = document_upload_parts)]
pub struct DocumentUploadPart {
    pub id: Uuid,
    pub session_id: Uuid,
    pub part_index: i32,
    pub size: i64,
    pub part_sha256: String,
    pub received_at: DateTime<Utc>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = document_upload_parts)]
pub struct NewDocumentUploadPart {
    pub id: Uuid,
    pub session_id: Uuid,
    pub part_index: i32,
    pub size: i64,
    pub part_sha256: String,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = document_allowlist_rules)]
pub struct DocumentAllowlistRule {
    pub id: Uuid,
    pub module_id: String,
    pub extensions: Vec<String>,
    pub mime_types: Vec<String>,
    pub max_bytes: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = document_allowlist_rules)]
pub struct NewDocumentAllowlistRule {
    pub id: Uuid,
    pub module_id: String,
    pub max_bytes: i64,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = document_downloads)]
pub struct DocumentDownload {
    pub id: Uuid,
    pub document_id: Option<Uuid>,
    pub version: i32,
    pub user_id: String,
    pub via: String,
    pub scan_status: String,
    pub bytes: i64,
    pub downloaded_at: DateTime<Utc>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = document_downloads)]
pub struct NewDocumentDownload {
    pub id: Uuid,
    pub document_id: Option<Uuid>,
    pub version: i32,
    pub user_id: String,
    pub scan_status: String,
    pub bytes: i64,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = document_quotas)]
pub struct DocumentQuota {
    pub id: Uuid,
    pub scope: String,
    pub scope_id: String,
    pub bytes_limit: Option<i64>,
    pub used_bytes: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = document_quotas)]
pub struct NewDocumentQuota {
    pub id: Uuid,
    pub scope: String,
    pub scope_id: String,
    pub bytes_limit: Option<i64>,
}

// ---------------------------------------------------------------------------
// Finalize jobs (migration 000123_storage_public_private).
// ---------------------------------------------------------------------------

/// One lazy-finalize job: the durable record of a pending whole-file
/// SHA-256 hash. Created when a chunked session completes (all parts
/// received); the worker claims it atomically, hashes the stored
/// object, and marks it `verified` (checksum stored on the document)
/// or `failed` (mismatch). The runtime `FinalizeJob` in
/// `src/modules/documents/` writes through to this table (see
/// `finalize.rs`, C11 task 271); the in-memory map stays the
/// runtime cache, the row is the restart-surviving record.
#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = finalize_jobs)]
pub struct FinalizeJob {
    pub id: Uuid,
    pub session_id: Uuid,
    pub document_id: Option<Uuid>,
    /// C11 (migration 000126): tenant scope of the job, copied
    /// from the upload session at creation. `None` for jobs
    /// created before the column existed.
    pub tenant_id: Option<Uuid>,
    pub status: String,
    pub attempts: i32,
    pub last_error: Option<String>,
    pub claimed_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = finalize_jobs)]
pub struct NewFinalizeJob {
    pub id: Uuid,
    pub session_id: Uuid,
    pub document_id: Option<Uuid>,
    pub tenant_id: Option<Uuid>,
}
