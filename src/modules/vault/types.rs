//! vault module - Folders, documents, versions, logs, share links and requests.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

// Types

/// One folder. `parent_id` is `None` for a top-level folder.
/// `retention_days` is the retention rule for the folder: documents
/// older than that many days are flagged for review (never
/// auto-deleted). `auto_delete` is stored for the migration target
/// only and is always `false` in this module - deletion is never
/// automatic; see the module header.
/// `tenant_id` is the owning tenant, bound server-side from the
/// resolved tenant context at creation (never taken from the
/// client); `company_id` likewise comes from the tenant context,
/// never from the request body.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VaultFolder {
    pub id: Uuid,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_id: Option<Uuid>,
    pub owner_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tenant_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub company_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retention_days: Option<i64>,
    #[serde(default)]
    pub auto_delete: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// One document. Metadata only: the file bytes live in the storage
/// module, referenced by `storage_target` + `storage_key`.
/// `checksum_sha256` is the hex SHA-256 of the bytes (format
/// validated on write); `size_bytes` is the byte size.
/// `shared_with_client_ids` lists the client user ids the document
/// is shared with; `status` is `active` or `archived`.
/// `tenant_id` is the owning tenant, bound server-side from the
/// resolved tenant context at creation (never taken from the
/// client); `company_id` likewise comes from the tenant context,
/// never from the request body.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VaultDocument {
    pub id: Uuid,
    pub folder_id: Uuid,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub owner_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tenant_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub company_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current_version_id: Option<Uuid>,
    pub storage_target: String,
    pub storage_key: String,
    pub checksum_sha256: String,
    pub size_bytes: i64,
    pub mime_type: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub shared_with_client_ids: Vec<String>,
    pub status: String,
    #[serde(default)]
    pub flagged_for_review: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub flagged_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// One version of a document. Every upload creates a new version;
/// old versions are kept and can be restored. `version_number` is
/// 1-based and strictly increasing per document.
#[derive(Debug, Clone, Serialize, Deserialize)]
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    pub created_at: DateTime<Utc>,
}

/// One append-only access-log row. Written on every view, download,
/// share, request, approve, deny, restore, and delete action.
/// Rows are never updated or deleted by this module.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VaultAccessLog {
    pub id: Uuid,
    pub document_id: Uuid,
    pub actor_id: String,
    pub action: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    pub created_at: DateTime<Utc>,
}

/// One share link. `token` is unguessable (two UUIDv7 values, hex,
/// no separators) and is the only credential needed to resolve the
/// link; it is never written to logs. `expires_at` is required;
/// `max_downloads` caps downloads (0 means unlimited is not
/// allowed - a cap is always set, defaulting to 1); `revoked`
/// disables the link immediately. `tenant_id` is the owning tenant,
/// bound server-side from the resolved tenant context at creation;
/// resolving the link requires a request resolved to the same
/// tenant. The token itself is looked up through a SHA-256 hash
/// index (see `VaultState::share_link_tokens`), never by scanning
/// links and comparing tokens.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VaultShareLink {
    pub id: Uuid,
    pub document_id: Uuid,
    pub token: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tenant_id: Option<String>,
    pub expires_at: DateTime<Utc>,
    pub max_downloads: i64,
    pub download_count: i64,
    pub revoked: bool,
    pub created_by: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// One access request: a client asks the owner for access to a
/// document. `status` is `pending`, `approved`, or `denied`.
/// Approval grants visibility through `can_see_document`; the
/// decision itself is written to the access log. `tenant_id` is
/// the owning tenant, bound server-side from the resolved tenant
/// context at creation; request listings are scoped to it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VaultAccessRequest {
    pub id: Uuid,
    pub document_id: Uuid,
    pub requester_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tenant_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    pub status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decided_by: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decided_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

// Request bodies

#[derive(Debug, Deserialize)]
pub struct CreateFolderRequest {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_id: Option<Uuid>,
    /// Accepted for API compatibility but IGNORED: the folder's
    /// company is always taken from the resolved tenant context,
    /// never from the request body (a client must not be able to
    /// file a folder under another company).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub company_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retention_days: Option<i64>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateFolderRequest {
    pub name: Option<String>,
    pub parent_id: Option<Option<Uuid>>,
    pub retention_days: Option<Option<i64>>,
}

#[derive(Debug, Deserialize)]
pub struct SetRetentionRequest {
    pub retention_days: i64,
}

#[derive(Debug, Deserialize)]
pub struct CreateDocumentRequest {
    pub folder_id: Uuid,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Accepted for API compatibility but IGNORED: the document's
    /// company is always taken from the resolved tenant context,
    /// never from the request body (a client must not be able to
    /// create or list documents under another company).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub company_id: Option<String>,
    pub storage_target: String,
    pub storage_key: String,
    pub checksum_sha256: String,
    pub size_bytes: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mime_type: Option<String>,
    #[serde(default)]
    pub shared_with_client_ids: Vec<String>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateDocumentRequest {
    pub name: Option<String>,
    pub description: Option<Option<String>>,
    pub folder_id: Option<Uuid>,
    pub status: Option<String>,
    pub shared_with_client_ids: Option<Vec<String>>,
}

#[derive(Debug, Deserialize)]
pub struct UploadVersionRequest {
    pub storage_target: String,
    pub storage_key: String,
    pub checksum_sha256: String,
    pub size_bytes: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mime_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct CreateShareLinkRequest {
    /// Seconds from now until the link expires. Required and
    /// bounded: at least 60 seconds, at most 365 days.
    pub expires_in_seconds: i64,
    #[serde(default = "default_max_downloads")]
    pub max_downloads: i64,
}

pub(crate) fn default_max_downloads() -> i64 {
    1
}

#[derive(Debug, Deserialize)]
pub struct RequestAccessRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct DecideAccessRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}
