// Stored records.

use unified_core::storage::{sha256_hex, StorageKind};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::*;

// ---------------------------------------------------------------------------
// Stored records (in-memory DashMap stores are authoritative; the
// migration tables are persistence targets — see the module report).
// ---------------------------------------------------------------------------

/// One storage bucket (a container registered with this hub).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StorageBucket {
    pub id: Uuid,
    pub name: String,
    pub provider_kind: StorageKind,
    /// Endpoint URL (S3 / R2). Empty for local / Drive / SFTP.
    pub endpoint: String,
    /// Region (S3 / R2). `auto` for R2.
    pub region: String,
    /// Bucket / container name (S3 / R2). Empty for other kinds.
    pub container: String,
    /// Base directory (local provider) or base path (SFTP). Empty
    /// means the provider default.
    pub path: String,
    /// SFTP host. Must be allowlisted; see `validate_sftp_host`.
    pub sftp_host: String,
    /// SFTP port. `0` means 22.
    pub sftp_port: u16,
    /// SFTP username.
    pub sftp_username: String,
    /// SFTP auth kind: `"keyfile"` or `"password"`.
    pub sftp_auth_kind: String,
    /// SFTP private-key file path (keyfile auth only). A path, never
    /// key material.
    pub sftp_keyfile: String,
    /// Operator-configured allowlist of exact endpoint hosts, for
    /// self-hosted S3-compatible endpoints and SFTP hosts. Entries
    /// must already be on the operator allowlist
    /// (`storage.allowed_hosts` in the application config); a bucket
    /// cannot allowlist a host the operator has not approved, and
    /// the provider is built from the operator list, not this one.
    pub allowed_hosts: Vec<String>,
    /// Encrypted JSON object of the credential fields (access key,
    /// secret key, password, tokens, ...). Never serialized.
    #[serde(skip_serializing)]
    pub credentials_encrypted: Option<String>,
    /// Fingerprint of the encrypted credential blob (first 12 hex
    /// of the SHA-256 of the ciphertext). Lets an operator tell
    /// whether credentials changed; it is not a credential and
    /// cannot be used to recover one.
    pub key_fingerprint: Option<String>,
    /// Encryption-at-rest requirement flag. See the module header
    /// for the honest per-provider matrix: the flag is recorded, not
    /// enforced, for S3 / R2 / Drive / SFTP.
    pub encryption_at_rest: bool,
    pub enabled: bool,
    pub company_id: Option<String>,
    pub created_by: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Public bucket view: credentials are never included, only whether
/// any are configured.
#[derive(Debug, Clone, Serialize)]
pub struct BucketView {
    pub id: Uuid,
    pub name: String,
    pub provider_kind: StorageKind,
    pub endpoint: String,
    pub region: String,
    pub container: String,
    pub path: String,
    pub sftp_host: String,
    pub sftp_port: u16,
    pub sftp_username: String,
    pub sftp_auth_kind: String,
    pub sftp_keyfile: String,
    pub allowed_hosts: Vec<String>,
    pub credentials_configured: bool,
    pub key_fingerprint: Option<String>,
    pub encryption_at_rest: bool,
    /// Honest per-provider statement of what `encryption_at_rest`
    /// actually means for this bucket's kind (see module header).
    pub encryption_at_rest_note: String,
    pub enabled: bool,
    pub company_id: Option<String>,
    pub created_by: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

pub(crate) fn encryption_note(kind: StorageKind) -> String {
    match kind {
        StorageKind::S3 | StorageKind::R2 => {
            "Flag recorded only: the S3 provider sends no server-side-encryption header, so at-rest encryption must be enabled as the bucket default at the provider console."
                .to_string()
        }
        StorageKind::Local => {
            "OS / filesystem-level only: no application-level object encryption is applied."
                .to_string()
        }
        StorageKind::GoogleDrive => {
            "Transport encryption only (TLS + Google platform encryption); no customer-managed key support here."
                .to_string()
        }
        StorageKind::Sftp => {
            "Transport encryption only (SSH); at-rest encryption depends on the remote server filesystem."
                .to_string()
        }
    }
}

impl From<&StorageBucket> for BucketView {
    fn from(bucket: &StorageBucket) -> Self {
        Self {
            id: bucket.id,
            name: bucket.name.clone(),
            provider_kind: bucket.provider_kind,
            endpoint: bucket.endpoint.clone(),
            region: bucket.region.clone(),
            container: bucket.container.clone(),
            path: bucket.path.clone(),
            sftp_host: bucket.sftp_host.clone(),
            sftp_port: bucket.sftp_port,
            sftp_username: bucket.sftp_username.clone(),
            sftp_auth_kind: bucket.sftp_auth_kind.clone(),
            sftp_keyfile: bucket.sftp_keyfile.clone(),
            allowed_hosts: bucket.allowed_hosts.clone(),
            credentials_configured: bucket.credentials_encrypted.is_some(),
            key_fingerprint: bucket.key_fingerprint.clone(),
            encryption_at_rest: bucket.encryption_at_rest,
            encryption_at_rest_note: encryption_note(bucket.provider_kind),
            enabled: bucket.enabled,
            company_id: bucket.company_id.clone(),
            created_by: bucket.created_by.clone(),
            created_at: bucket.created_at,
            updated_at: bucket.updated_at,
        }
    }
}

/// One per-module binding: which module stores where.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StorageBinding {
    pub id: Uuid,
    /// Module id that stores here (for example `backup`, `files`).
    pub module_id: String,
    pub bucket_id: Uuid,
    /// Key prefix inside the bucket (no leading slash). Empty means
    /// the bucket root.
    pub key_prefix: String,
    /// `None` means this binding applies to every company; `Some`
    /// scopes it to one company.
    pub company_id: Option<String>,
    pub enabled: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// One quota: a soft (warn) and a hard (reject) limit on bytes and
/// object count, for one scope. `None` on a limit means unlimited.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StorageQuota {
    pub id: Uuid,
    /// `"company"`, `"user"`, or `"module"`.
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

/// One daily usage rollup row. The key is
/// `{date}|{bucket_id}|{company_id}|{user_id}|{module_id}`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UsageDaily {
    pub date: String,
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

/// One lifecycle rule for one bucket.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LifecycleRule {
    pub id: Uuid,
    pub bucket_id: Uuid,
    /// Objects older than this many days are candidates.
    pub retention_days: i64,
    /// True when a 0-day retention was explicitly confirmed at
    /// creation/update time (`confirm_zero_retention`). A 0-day
    /// rule without this flag never runs (see `run_one_rule`).
    #[serde(default)]
    pub zero_retention_confirmed: bool,
    /// Delete candidates.
    pub auto_delete: bool,
    /// Move candidates to the archive prefix instead of deleting.
    pub archive_transition: bool,
    /// Archive prefix inside the bucket (default `archive`).
    pub archive_prefix: String,
    pub enabled: bool,
    pub last_run_at: Option<DateTime<Utc>>,
    pub last_run_summary: Option<String>,
    pub last_error: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// One issued hub download token. The token itself is a bearer
/// secret: it is returned once at issuance and never listed again.
/// The record stores only its fingerprint-free id; listing shows
/// metadata, never the token value.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SignedUrlRecord {
    pub id: Uuid,
    /// The bearer token value. Never stored: kept empty on the
    /// record — lookup is by `token_hash` only (see below), so a
    /// copy of the store never leaks a usable token.
    #[serde(skip_serializing)]
    pub token: String,
    /// SHA-256 hex of the bearer token. Redemption hashes the
    /// presented token and compares hashes in constant time, so
    /// lookup is hash-keyed, not a linear `==` scan over secrets.
    #[serde(default)]
    pub token_hash: String,
    pub bucket_id: Uuid,
    pub key: String,
    pub issued_by: String,
    pub expires_at: DateTime<Utc>,
    pub revoked_at: Option<DateTime<Utc>>,
    pub revoke_reason: Option<String>,
    /// True when a provider (presigned) URL was also issued alongside
    /// the hub token. Provider URLs cannot be revoked by the hub;
    /// only the hub token is revocable (see the module header).
    pub provider_url_issued: bool,
    /// Set when the token was consumed by a successful download.
    /// Hub tokens are single-use: redemption revokes the record,
    /// so a leaked token cannot be replayed until expiry.
    #[serde(default)]
    pub used_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Public view of a signed-URL record: the token value is never
/// included.
#[derive(Debug, Clone, Serialize)]
pub struct SignedUrlView {
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

impl From<&SignedUrlRecord> for SignedUrlView {
    fn from(record: &SignedUrlRecord) -> Self {
        Self {
            id: record.id,
            bucket_id: record.bucket_id,
            key: record.key.clone(),
            issued_by: record.issued_by.clone(),
            expires_at: record.expires_at,
            revoked_at: record.revoked_at,
            revoke_reason: record.revoke_reason.clone(),
            provider_url_issued: record.provider_url_issued,
            created_at: record.created_at,
            updated_at: record.updated_at,
        }
    }
}

/// One health-check run for one bucket.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HealthCheck {
    pub id: Uuid,
    pub bucket_id: Uuid,
    pub ok: bool,
    pub latency_ms: i64,
    /// Short outcome text. Never contains credentials or provider
    /// error bodies.
    pub detail: String,
    pub checked_at: DateTime<Utc>,
}

/// One object recorded through this hub.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoredObject {
    pub bucket_id: Uuid,
    pub key: String,
    pub size_bytes: i64,
    /// SHA-256 hex digest of the bytes, recorded at upload time.
    /// `None` for objects recorded before checksums were kept, or
    /// uploaded outside the hub. Downloads verify against it when
    /// present and refuse corrupt bytes instead of serving them.
    pub sha256_hex: Option<String>,
    pub mime: String,
    pub owner_module: String,
    pub owner_user: String,
    pub company_id: Option<String>,
    pub archived: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Browser view of one object: recorded objects carry full metadata;
/// live-only keys (found by a provider `list()` but never recorded
/// through the hub) carry `recorded: false` and unknown ownership.
#[derive(Debug, Clone, Serialize)]
pub struct ObjectView {
    pub bucket_id: Uuid,
    pub key: String,
    pub size_bytes: Option<i64>,
    /// SHA-256 hex digest recorded at upload time, when known.
    pub sha256_hex: Option<String>,
    pub mime: Option<String>,
    pub owner_module: Option<String>,
    pub owner_user: Option<String>,
    pub company_id: Option<String>,
    pub archived: bool,
    pub recorded: bool,
    pub created_at: Option<DateTime<Utc>>,
}
