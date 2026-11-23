// Request bodies.

use unified_core::storage::StorageKind;
use serde::Deserialize;
use serde_json::Value;
use uuid::Uuid;

use super::*;

// ---------------------------------------------------------------------------
// Request bodies.
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub(crate) struct CreateBucketRequest {
    pub(crate) name: String,
    pub(crate) provider_kind: StorageKind,
    #[serde(default)]
    pub(crate) endpoint: Option<String>,
    #[serde(default)]
    pub(crate) region: Option<String>,
    #[serde(default)]
    pub(crate) container: Option<String>,
    #[serde(default)]
    pub(crate) path: Option<String>,
    #[serde(default)]
    pub(crate) sftp_host: Option<String>,
    #[serde(default)]
    pub(crate) sftp_port: Option<u16>,
    #[serde(default)]
    pub(crate) sftp_username: Option<String>,
    #[serde(default)]
    pub(crate) sftp_auth_kind: Option<String>,
    #[serde(default)]
    pub(crate) sftp_keyfile: Option<String>,
    #[serde(default)]
    pub(crate) allowed_hosts: Option<Vec<String>>,
    /// Credential fields (access key, secret key, password, tokens).
    /// Write-only: accepted here, encrypted, never returned.
    #[serde(default)]
    pub(crate) credentials: Option<serde_json::Map<String, Value>>,
    #[serde(default)]
    pub(crate) encryption_at_rest: Option<bool>,
    #[serde(default)]
    pub(crate) company_id: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct UpdateBucketRequest {
    #[serde(default)]
    pub(crate) name: Option<String>,
    #[serde(default)]
    pub(crate) endpoint: Option<String>,
    #[serde(default)]
    pub(crate) region: Option<String>,
    #[serde(default)]
    pub(crate) container: Option<String>,
    #[serde(default)]
    pub(crate) path: Option<String>,
    #[serde(default)]
    pub(crate) sftp_host: Option<String>,
    #[serde(default)]
    pub(crate) sftp_port: Option<u16>,
    #[serde(default)]
    pub(crate) sftp_username: Option<String>,
    #[serde(default)]
    pub(crate) sftp_auth_kind: Option<String>,
    #[serde(default)]
    pub(crate) sftp_keyfile: Option<String>,
    #[serde(default)]
    pub(crate) allowed_hosts: Option<Vec<String>>,
    /// Replaces the stored credentials when present (an empty object
    /// clears them). Absent means "keep the stored credentials".
    #[serde(default)]
    pub(crate) credentials: Option<serde_json::Map<String, Value>>,
    #[serde(default)]
    pub(crate) encryption_at_rest: Option<bool>,
    #[serde(default)]
    pub(crate) enabled: Option<bool>,
    #[serde(default)]
    pub(crate) company_id: Option<Option<String>>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct CreateBindingRequest {
    pub(crate) module_id: String,
    pub(crate) bucket_id: Uuid,
    #[serde(default)]
    pub(crate) key_prefix: Option<String>,
    #[serde(default)]
    pub(crate) company_id: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct UpdateBindingRequest {
    #[serde(default)]
    pub(crate) bucket_id: Option<Uuid>,
    #[serde(default)]
    pub(crate) key_prefix: Option<String>,
    #[serde(default)]
    pub(crate) enabled: Option<bool>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct CreateQuotaRequest {
    pub(crate) scope: String,
    pub(crate) scope_id: String,
    #[serde(default)]
    pub(crate) bytes_limit: Option<i64>,
    #[serde(default)]
    pub(crate) objects_limit: Option<i64>,
    #[serde(default)]
    pub(crate) soft_bytes_limit: Option<i64>,
    #[serde(default)]
    pub(crate) soft_objects_limit: Option<i64>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct UpdateQuotaRequest {
    /// Outer `None` = unchanged; inner `None` = unlimited.
    #[serde(default)]
    pub(crate) bytes_limit: Option<Option<i64>>,
    #[serde(default)]
    pub(crate) objects_limit: Option<Option<i64>>,
    #[serde(default)]
    pub(crate) soft_bytes_limit: Option<Option<i64>>,
    #[serde(default)]
    pub(crate) soft_objects_limit: Option<Option<i64>>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct CreateLifecycleRequest {
    pub(crate) bucket_id: Uuid,
    pub(crate) retention_days: i64,
    /// Explicit confirmation that `retention_days: 0` is intended:
    /// 0 deletes everything older than now, i.e. every object.
    /// Required when `retention_days` is 0.
    #[serde(default)]
    pub(crate) confirm_zero_retention: bool,
    #[serde(default)]
    pub(crate) auto_delete: Option<bool>,
    #[serde(default)]
    pub(crate) archive_transition: Option<bool>,
    #[serde(default)]
    pub(crate) archive_prefix: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct UpdateLifecycleRequest {
    #[serde(default)]
    pub(crate) retention_days: Option<i64>,
    /// Explicit confirmation for setting `retention_days` to 0
    /// (deletes everything older than now). Required when the new
    /// value is 0.
    #[serde(default)]
    pub(crate) confirm_zero_retention: bool,
    #[serde(default)]
    pub(crate) auto_delete: Option<bool>,
    #[serde(default)]
    pub(crate) archive_transition: Option<bool>,
    #[serde(default)]
    pub(crate) archive_prefix: Option<String>,
    #[serde(default)]
    pub(crate) enabled: Option<bool>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct RunLifecycleRequest {
    #[serde(default)]
    pub(crate) rule_id: Option<Uuid>,
    #[serde(default)]
    pub(crate) bucket_id: Option<Uuid>,
    /// Also ask the scheduler to enqueue a `storage.lifecycle.run`
    /// job. The scheduler declines it until the kind is registered
    /// (see the module header); the run still executes in-process.
    #[serde(default)]
    pub(crate) enqueue: Option<bool>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct IssueSignedUrlRequest {
    pub(crate) key: String,
    #[serde(default)]
    pub(crate) expires_secs: Option<u64>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct MoveObjectRequest {
    pub(crate) key: String,
    pub(crate) new_key: String,
    /// Allow overwriting an existing destination key. Default
    /// false: a move onto an existing key is refused with 409 so
    /// a destination is never silently clobbered.
    #[serde(default)]
    pub(crate) overwrite: bool,
}

#[derive(Debug, Deserialize)]
pub(crate) struct ObjectQuery {
    pub(crate) key: String,
}

#[derive(Debug, Deserialize)]
pub(crate) struct UploadQuery {
    pub(crate) key: String,
    /// Owning module for quota + usage accounting (default `files`).
    #[serde(default)]
    pub(crate) module: Option<String>,
    #[serde(default)]
    pub(crate) company_id: Option<String>,
    #[serde(default)]
    pub(crate) mime: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct SetDefaultRequest {
    pub(crate) bucket_id: Uuid,
    #[serde(default)]
    pub(crate) company_id: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct RevokeSignedUrlRequest {
    #[serde(default)]
    pub(crate) reason: Option<String>,
}
