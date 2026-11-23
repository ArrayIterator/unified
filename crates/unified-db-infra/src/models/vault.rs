// Diesel models for the vault domain of the unified-db-infra bounded context.
//
// Split out of `src/models.rs` (see `src/lib.rs`); the structs below are
// byte-identical to the originals, only regrouped.

use crate::schema::*;
use chrono::{DateTime, Utc};
use diesel::prelude::*;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

// ---------------------------------------------------------------------------
// Document vault (migration 000052_vault).
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = vault_folders)]
pub struct VaultFolder {
    pub id: Uuid,
    pub name: String,
    pub parent_id: Option<Uuid>,
    pub owner_id: String,
    pub company_id: Option<String>,
    pub retention_days: Option<i64>,
    pub auto_delete: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = vault_folders)]
pub struct NewVaultFolder {
    pub id: Uuid,
    pub name: String,
    pub parent_id: Option<Uuid>,
    pub owner_id: String,
    pub company_id: Option<String>,
    pub retention_days: Option<i64>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = vault_documents)]
pub struct VaultDocument {
    pub id: Uuid,
    pub folder_id: Uuid,
    pub name: String,
    pub description: Option<String>,
    pub owner_id: String,
    pub company_id: Option<String>,
    pub current_version_id: Option<Uuid>,
    pub storage_target: String,
    pub storage_key: String,
    pub checksum_sha256: String,
    pub size_bytes: i64,
    pub mime_type: String,
    pub shared_with_client_ids: Vec<String>,
    pub status: String,
    pub flagged_for_review: bool,
    pub flagged_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = vault_documents)]
pub struct NewVaultDocument {
    pub id: Uuid,
    pub folder_id: Uuid,
    pub name: String,
    pub description: Option<String>,
    pub owner_id: String,
    pub company_id: Option<String>,
    pub current_version_id: Option<Uuid>,
    pub storage_target: String,
    pub storage_key: String,
    pub checksum_sha256: String,
    pub flagged_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = vault_document_versions)]
pub struct VaultDocumentVersion {
    pub id: Uuid,
    pub document_id: Uuid,
    pub version_number: i64,
    pub storage_target: String,
    pub storage_key: String,
    pub checksum_sha256: String,
    pub size_bytes: i64,
    pub mime_type: String,
    pub uploaded_by: String,
    pub note: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = vault_document_versions)]
pub struct NewVaultDocumentVersion {
    pub id: Uuid,
    pub document_id: Uuid,
    pub version_number: i64,
    pub storage_target: String,
    pub storage_key: String,
    pub checksum_sha256: String,
    pub uploaded_by: String,
    pub note: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = vault_access_log)]
pub struct VaultAccessLog {
    pub id: Uuid,
    pub document_id: Uuid,
    pub actor_id: String,
    pub action: String,
    pub detail: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = vault_access_log)]
pub struct NewVaultAccessLog {
    pub id: Uuid,
    pub document_id: Uuid,
    pub actor_id: String,
    pub action: String,
    pub detail: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = vault_share_links)]
pub struct VaultShareLink {
    pub id: Uuid,
    pub document_id: Uuid,
    pub token: String,
    pub expires_at: DateTime<Utc>,
    pub max_downloads: i64,
    pub download_count: i64,
    pub revoked: bool,
    pub created_by: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = vault_share_links)]
pub struct NewVaultShareLink {
    pub id: Uuid,
    pub document_id: Uuid,
    pub token: String,
    pub expires_at: DateTime<Utc>,
    pub created_by: String,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = vault_access_requests)]
pub struct VaultAccessRequest {
    pub id: Uuid,
    pub document_id: Uuid,
    pub requester_id: String,
    pub reason: Option<String>,
    pub status: String,
    pub decided_by: Option<String>,
    pub decided_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = vault_access_requests)]
pub struct NewVaultAccessRequest {
    pub id: Uuid,
    pub document_id: Uuid,
    pub requester_id: String,
    pub reason: Option<String>,
    pub decided_by: Option<String>,
    pub decided_at: Option<DateTime<Utc>>,
}
