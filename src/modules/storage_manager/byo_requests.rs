// BYO request bodies.

use chrono::{DateTime, Utc};
use serde::Deserialize;
use serde_json::Value;
use uuid::Uuid;

// BYO request bodies.

#[derive(Debug, Deserialize)]
pub(crate) struct UpdateByoSettingsRequest {
    #[serde(default)]
    pub(crate) allowed_provider_kinds: Option<Vec<String>>,
    #[serde(default)]
    pub(crate) terms_required: Option<bool>,
    #[serde(default)]
    pub(crate) max_connections_per_user: Option<i64>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct CreateByoTermsRequest {
    pub(crate) version: String,
    pub(crate) title: String,
    pub(crate) body: String,
    #[serde(default)]
    pub(crate) is_current: Option<bool>,
    #[serde(default)]
    pub(crate) reacceptance_deadline: Option<DateTime<Utc>>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct UpdateByoTermsRequest {
    #[serde(default)]
    pub(crate) title: Option<String>,
    #[serde(default)]
    pub(crate) body: Option<String>,
    #[serde(default)]
    pub(crate) is_current: Option<bool>,
    #[serde(default)]
    pub(crate) reacceptance_deadline: Option<Option<DateTime<Utc>>>,
    #[serde(default)]
    pub(crate) enabled: Option<bool>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct CreateByoConnectionRequest {
    pub(crate) name: String,
    pub(crate) provider_kind: String,
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
    /// Path prefix scoping. When omitted, defaults to
    /// `byo/{owner_user}` so a connection is always scoped.
    #[serde(default)]
    pub(crate) path_prefix: Option<String>,
    #[serde(default)]
    pub(crate) company_id: Option<String>,
    #[serde(default)]
    pub(crate) allow_read: Option<bool>,
    #[serde(default)]
    pub(crate) allow_write: Option<bool>,
    #[serde(default)]
    pub(crate) allow_delete: Option<bool>,
    #[serde(default)]
    pub(crate) max_bytes: Option<i64>,
    #[serde(default)]
    pub(crate) max_files: Option<i64>,
    #[serde(default)]
    pub(crate) encryption_required: Option<bool>,
    #[serde(default)]
    pub(crate) allowed_extensions: Option<Vec<String>>,
    #[serde(default)]
    pub(crate) allowed_mime_types: Option<Vec<String>>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct UpdateByoConnectionRequest {
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
    /// Replacing credentials resets the connection to `draft`: it
    /// must be validated again before it can activate.
    #[serde(default)]
    pub(crate) credentials: Option<serde_json::Map<String, Value>>,
    #[serde(default)]
    pub(crate) path_prefix: Option<String>,
    #[serde(default)]
    pub(crate) allow_read: Option<bool>,
    #[serde(default)]
    pub(crate) allow_write: Option<bool>,
    #[serde(default)]
    pub(crate) allow_delete: Option<bool>,
    #[serde(default)]
    pub(crate) max_bytes: Option<Option<i64>>,
    #[serde(default)]
    pub(crate) max_files: Option<Option<i64>>,
    #[serde(default)]
    pub(crate) encryption_required: Option<bool>,
    #[serde(default)]
    pub(crate) allowed_extensions: Option<Vec<String>>,
    #[serde(default)]
    pub(crate) allowed_mime_types: Option<Vec<String>>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct ByoStatusRequest {
    #[serde(default)]
    pub(crate) reason: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct AcceptTermsRequest {
    #[serde(default)]
    pub(crate) connection_id: Option<Uuid>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct RunByoHealthRequest {
    #[serde(default)]
    pub(crate) connection_id: Option<Uuid>,
    /// Also ask the scheduler to enqueue a `storage.byo.health_check`
    /// job. The scheduler declines it until the kind is registered
    /// (see the module header); the run still executes in-process.
    #[serde(default)]
    pub(crate) enqueue: Option<bool>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct ByoUploadQuery {
    pub(crate) key: String,
    #[serde(default)]
    pub(crate) mime: Option<String>,
}
