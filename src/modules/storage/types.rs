//! Storage - status views and request types.
//!
//! Part of the `storage` module split; see `src/modules/storage.rs`.

use unified_core::storage::StorageKind;
use serde::Deserialize;
use serde::Serialize;
use serde_json::Value;

// ---------------------------------------------------------------------------
// Views and request types.
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize)]
pub(crate) struct StorageStatusView {
    pub(crate) kinds: Vec<String>,
    pub(crate) targets: usize,
    pub(crate) target_names: Vec<String>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct CreateTargetRequest {
    pub(crate) name: String,
    pub(crate) kind: StorageKind,
    /// Target configuration fields (endpoint, region, bucket,
    /// base_dir, access_key, secret_key, client_id, client_secret,
    /// refresh_token, access_token, token_url). Secrets are
    /// write-only: they are accepted here and never returned.
    #[serde(default)]
    pub(crate) config: serde_json::Map<String, Value>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct ObjectQuery {
    pub(crate) key: String,
    /// Optional expected SHA-256 hex digest: when present the
    /// download is verified against it and corrupt bytes are
    /// refused instead of served.
    #[serde(default)]
    pub(crate) expected_sha256: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct SignedUrlRequest {
    pub(crate) key: String,
    #[serde(default)]
    pub(crate) expires_secs: Option<u64>,
}
