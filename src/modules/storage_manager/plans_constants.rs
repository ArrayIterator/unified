// Storage plans and BYO constants and types.

use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::storage::{sha256_hex, StorageKind};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::*;

// Storage plans (storage as a product) + bring-your-own storage (BYO).
//
// Plans are products: the price itself is owned by Product
// Management / billing, so a plan stores only a `product_id`
// billing reference and a billing-cycle hint - never a money
// column. A buyer (company and/or user) holds at most one active
// assignment. Changing a plan re-checks current usage against the
// new quota and refuses with 409 when usage exceeds it. Usage is
// metered from the recorded hub objects plus the BYO objects and
// reported with `storage.metering.reported` for billing.
//
// BYO connections let a buyer connect their own external storage.
// Only provider kinds on the admin-managed allowlist may connect;
// every connection is validated (connect, list a test prefix,
// write + read + delete a probe object, then remove it) before it
// can activate; the owner must accept the current terms version;
// and every object operation is scoped to the connection prefix,
// checked against its allowed operations, file-type allowlists, and
// quotas, with ownership checked on every access (cross-owner
// access answers 404, never 403, so existence does not leak).
// Credentials follow the bucket pattern exactly: write-only,
// encrypted with the same versioned `v1.` construction, never
// returned, logged, or placed in a hook payload.

/// Canonical provider-kind names accepted by plans and the BYO
/// allowlist. `drive` is the Google Drive kind; `s3-compatible` is
/// the S3 kind with a custom endpoint (an endpoint is required).
pub(crate) const BYO_KIND_NAMES: &[&str] = &["local", "s3", "r2", "drive", "sftp", "s3-compatible"];

/// Billing-cycle hints a plan may carry. The cycle itself is
/// interpreted by billing; this module only records the hint.
pub(crate) const BILLING_CYCLES: &[&str] = &["monthly", "yearly", "one_time", "usage_based"];

/// Module id used for the licensing entitlement gate.
pub(crate) const STORAGE_MODULE_ID: &str = "storage_manager";

pub(crate) fn canonical_provider_kind_name(name: &str) -> UnifiedResult<String> {
    let normalized = name.trim().to_ascii_lowercase();
    if BYO_KIND_NAMES.contains(&normalized.as_str()) {
        Ok(normalized)
    } else {
        Err(UnifiedError::bad_request(format!(
            "Unknown provider kind \"{name}\" (expected one of: {})",
            BYO_KIND_NAMES.join(", ")
        )))
    }
}

/// Map a canonical allowlist name to the `StorageKind` it means.
pub(crate) fn storage_kind_for_name(name: &str) -> UnifiedResult<StorageKind> {
    match name {
        "local" => Ok(StorageKind::Local),
        "s3" | "s3-compatible" => Ok(StorageKind::S3),
        "r2" => Ok(StorageKind::R2),
        "drive" => Ok(StorageKind::GoogleDrive),
        "sftp" => Ok(StorageKind::Sftp),
        other => Err(UnifiedError::bad_request(format!(
            "Unknown provider kind \"{other}\""
        ))),
    }
}

pub(crate) fn validate_provider_kind_list(values: &[String]) -> UnifiedResult<Vec<String>> {
    let mut out: Vec<String> = Vec::new();
    for value in values {
        let canonical = canonical_provider_kind_name(value)?;
        if !out.contains(&canonical) {
            out.push(canonical);
        }
    }
    if out.is_empty() {
        return Err(UnifiedError::bad_request(
            "At least one included provider kind is required",
        ));
    }
    Ok(out)
}

pub(crate) fn validate_billing_cycle(value: &Option<String>) -> UnifiedResult<Option<String>> {
    match value {
        None => Ok(None),
        Some(raw) => {
            let normalized = raw.trim().to_ascii_lowercase();
            if BILLING_CYCLES.contains(&normalized.as_str()) {
                Ok(Some(normalized))
            } else {
                Err(UnifiedError::bad_request(format!(
                    "Unknown billing cycle \"{raw}\" (expected one of: {})",
                    BILLING_CYCLES.join(", ")
                )))
            }
        }
    }
}

/// One storage plan: a sellable tier. Price is owned by Product
/// Management / billing; only the `product_id` reference and the
/// billing-cycle hint live here. `max_files` 0 means unlimited.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoragePlan {
    pub id: Uuid,
    pub name: String,
    pub description: String,
    /// Quota in bytes (GB is a display unit only; see `PlanView`).
    pub quota_bytes: i64,
    /// Maximum file count; 0 means unlimited.
    pub max_files: i64,
    /// Canonical provider-kind names included in this plan.
    pub included_provider_kinds: Vec<String>,
    /// Billing reference owned by Product Management / billing.
    pub product_id: Option<String>,
    /// Billing-cycle hint (`monthly`, `yearly`, `one_time`,
    /// `usage_based`); interpreted by billing, not by this module.
    pub billing_cycle: Option<String>,
    pub is_public: bool,
    pub sort_order: i64,
    pub enabled: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Public plan view: the quota is shown in bytes and in GB.
#[derive(Debug, Clone, Serialize)]
pub struct PlanView {
    pub id: Uuid,
    pub name: String,
    pub description: String,
    pub quota_bytes: i64,
    pub quota_gb: f64,
    pub max_files: i64,
    pub included_provider_kinds: Vec<String>,
    pub product_id: Option<String>,
    pub billing_cycle: Option<String>,
    pub is_public: bool,
    pub sort_order: i64,
    pub enabled: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl From<&StoragePlan> for PlanView {
    fn from(plan: &StoragePlan) -> Self {
        Self {
            id: plan.id,
            name: plan.name.clone(),
            description: plan.description.clone(),
            quota_bytes: plan.quota_bytes,
            quota_gb: plan.quota_bytes as f64 / 1_073_741_824.0,
            max_files: plan.max_files,
            included_provider_kinds: plan.included_provider_kinds.clone(),
            product_id: plan.product_id.clone(),
            billing_cycle: plan.billing_cycle.clone(),
            is_public: plan.is_public,
            sort_order: plan.sort_order,
            enabled: plan.enabled,
            created_at: plan.created_at,
            updated_at: plan.updated_at,
        }
    }
}

/// One buyer plan assignment. A buyer is a company, a user, or
/// both; at least one of the two is always set.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlanAssignment {
    pub id: Uuid,
    pub plan_id: Uuid,
    pub company_id: Option<String>,
    pub user_id: String,
    /// `active`, `cancelled`, or `expired`.
    pub status: String,
    pub assigned_by: String,
    pub assigned_at: DateTime<Utc>,
    pub expires_at: Option<DateTime<Utc>>,
    pub cancelled_at: Option<DateTime<Utc>>,
    pub cancel_reason: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// One metering report: bytes + files stored by one buyer in one
/// period, reported for billing. `period` is `YYYY-MM`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MeteringReport {
    pub id: Uuid,
    pub company_id: Option<String>,
    pub user_id: String,
    pub plan_id: Option<Uuid>,
    pub period: String,
    pub bytes_stored: i64,
    pub files_stored: i64,
    pub reported_by: String,
    pub reported_at: DateTime<Utc>,
}

/// Admin-managed BYO settings: the provider allowlist and limits.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ByoSettings {
    /// Canonical provider-kind names an owner may connect.
    pub allowed_provider_kinds: Vec<String>,
    /// When true, a connection cannot activate until its owner has
    /// accepted the current terms version.
    pub terms_required: bool,
    /// Maximum active connections per user; 0 means unlimited.
    pub max_connections_per_user: i64,
}

impl Default for ByoSettings {
    fn default() -> Self {
        Self {
            allowed_provider_kinds: BYO_KIND_NAMES
                .iter()
                .map(|name| (*name).to_string())
                .collect(),
            terms_required: true,
            max_connections_per_user: 0,
        }
    }
}

/// One bring-your-own storage connection: a buyer's own external
/// storage, used as a backend under admin rules and terms.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ByoConnection {
    pub id: Uuid,
    pub name: String,
    pub provider_kind: StorageKind,
    /// True when the kind was connected as `s3-compatible`
    /// (S3 kind with a custom endpoint); recorded so the view and
    /// the audit trail keep the distinction.
    pub s3_compatible: bool,
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
    /// Encrypted credential JSON. Never serialized.
    #[serde(skip_serializing)]
    pub credentials_encrypted: Option<String>,
    pub key_fingerprint: Option<String>,
    /// Path prefix inside the target: the connection may ONLY touch
    /// keys under this prefix. Never empty.
    pub path_prefix: String,
    pub owner_user: String,
    pub company_id: Option<String>,
    /// `draft`, `validated`, `active`, `suspended`, `revoked`.
    pub status: String,
    pub status_reason: Option<String>,
    /// Allowed operations through this connection.
    pub allow_read: bool,
    pub allow_write: bool,
    pub allow_delete: bool,
    /// Per-connection quotas; `None` means unlimited.
    pub max_bytes: Option<i64>,
    pub max_files: Option<i64>,
    pub used_bytes: i64,
    pub used_files: i64,
    /// Encryption-at-rest requirement flag. Same honest matrix as
    /// buckets: recorded, not enforced, for S3 / R2 / Drive / SFTP;
    /// see the module header.
    pub encryption_required: bool,
    /// Allowed file extensions (lowercase, no dot) and MIME types.
    /// Empty means no restriction on that dimension.
    pub allowed_extensions: Vec<String>,
    pub allowed_mime_types: Vec<String>,
    /// Terms version the owner accepted for this connection.
    pub terms_version: Option<String>,
    pub terms_accepted_at: Option<DateTime<Utc>>,
    pub last_validated_at: Option<DateTime<Utc>>,
    pub last_validation_ok: Option<bool>,
    pub last_validation_error: Option<String>,
    pub last_health_at: Option<DateTime<Utc>>,
    pub last_health_ok: Option<bool>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Public BYO view: credentials are never included, only whether
/// any are configured.
#[derive(Debug, Clone, Serialize)]
pub struct ByoConnectionView {
    pub id: Uuid,
    pub name: String,
    pub provider_kind: StorageKind,
    pub s3_compatible: bool,
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
    pub path_prefix: String,
    pub owner_user: String,
    pub company_id: Option<String>,
    pub status: String,
    pub status_reason: Option<String>,
    pub allow_read: bool,
    pub allow_write: bool,
    pub allow_delete: bool,
    pub max_bytes: Option<i64>,
    pub max_files: Option<i64>,
    pub used_bytes: i64,
    pub used_files: i64,
    pub encryption_required: bool,
    pub encryption_required_note: String,
    pub allowed_extensions: Vec<String>,
    pub allowed_mime_types: Vec<String>,
    pub terms_version: Option<String>,
    pub terms_accepted_at: Option<DateTime<Utc>>,
    pub last_validated_at: Option<DateTime<Utc>>,
    pub last_validation_ok: Option<bool>,
    pub last_validation_error: Option<String>,
    pub last_health_at: Option<DateTime<Utc>>,
    pub last_health_ok: Option<bool>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl From<&ByoConnection> for ByoConnectionView {
    fn from(connection: &ByoConnection) -> Self {
        Self {
            id: connection.id,
            name: connection.name.clone(),
            provider_kind: connection.provider_kind,
            s3_compatible: connection.s3_compatible,
            endpoint: connection.endpoint.clone(),
            region: connection.region.clone(),
            container: connection.container.clone(),
            path: connection.path.clone(),
            sftp_host: connection.sftp_host.clone(),
            sftp_port: connection.sftp_port,
            sftp_username: connection.sftp_username.clone(),
            sftp_auth_kind: connection.sftp_auth_kind.clone(),
            sftp_keyfile: connection.sftp_keyfile.clone(),
            allowed_hosts: connection.allowed_hosts.clone(),
            credentials_configured: connection.credentials_encrypted.is_some(),
            key_fingerprint: connection.key_fingerprint.clone(),
            path_prefix: connection.path_prefix.clone(),
            owner_user: connection.owner_user.clone(),
            company_id: connection.company_id.clone(),
            status: connection.status.clone(),
            status_reason: connection.status_reason.clone(),
            allow_read: connection.allow_read,
            allow_write: connection.allow_write,
            allow_delete: connection.allow_delete,
            max_bytes: connection.max_bytes,
            max_files: connection.max_files,
            used_bytes: connection.used_bytes,
            used_files: connection.used_files,
            encryption_required: connection.encryption_required,
            encryption_required_note: encryption_note(connection.provider_kind),
            allowed_extensions: connection.allowed_extensions.clone(),
            allowed_mime_types: connection.allowed_mime_types.clone(),
            terms_version: connection.terms_version.clone(),
            terms_accepted_at: connection.terms_accepted_at,
            last_validated_at: connection.last_validated_at,
            last_validation_ok: connection.last_validation_ok,
            last_validation_error: connection.last_validation_error,
            last_health_at: connection.last_health_at,
            last_health_ok: connection.last_health_ok,
            created_at: connection.created_at,
            updated_at: connection.updated_at,
        }
    }
}

/// One object recorded through a BYO connection. The full key
/// (prefix + relative key) is what the provider stores.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ByoObject {
    pub connection_id: Uuid,
    pub key: String,
    pub size_bytes: i64,
    /// SHA-256 hex digest of the bytes, recorded at upload time.
    /// `None` for objects recorded before checksums were kept.
    /// Downloads verify against it when present.
    pub sha256_hex: Option<String>,
    pub mime: String,
    pub owner_user: String,
    pub company_id: Option<String>,
    pub created_at: DateTime<Utc>,
}

/// One version of the BYO terms of use. Only one version is
/// current at a time; changing the current version requires
/// re-acceptance (see `enforce_byo_reacceptance`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ByoTerms {
    pub id: Uuid,
    /// Version label, for example `1.0`. Unique among versions.
    pub version: String,
    pub title: String,
    /// Full terms text: data residency, liability limits, and the
    /// admin's right to suspend a misbehaving connection.
    pub body: String,
    pub is_current: bool,
    /// After this instant, connections whose owner has not accepted
    /// this version are suspended by the enforcement run.
    pub reacceptance_deadline: Option<DateTime<Utc>>,
    pub enabled: bool,
    pub created_by: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// One recorded acceptance of a BYO terms version by one user.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ByoTermsAcceptance {
    pub id: Uuid,
    pub terms_id: Uuid,
    pub terms_version: String,
    pub user_id: String,
    pub company_id: Option<String>,
    pub connection_id: Option<Uuid>,
    pub accepted_at: DateTime<Utc>,
}

/// One BYO health-check run for one connection.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ByoHealthCheck {
    pub id: Uuid,
    pub connection_id: Uuid,
    pub ok: bool,
    pub latency_ms: i64,
    /// Short outcome text. Never contains credentials or provider
    /// error bodies.
    pub detail: String,
    pub checked_at: DateTime<Utc>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonical_provider_kind_name_normalizes_case_and_trims() {
        assert_eq!(canonical_provider_kind_name("S3").unwrap(), "s3");
        assert_eq!(canonical_provider_kind_name("  s3-compatible ").unwrap(), "s3-compatible");
        assert_eq!(canonical_provider_kind_name("DRIVE").unwrap(), "drive");
        assert!(canonical_provider_kind_name("bogus").is_err());
        assert!(canonical_provider_kind_name("").is_err());
    }

    #[test]
    fn storage_kind_for_name_maps_all_kinds() {
        assert_eq!(storage_kind_for_name("local").unwrap(), StorageKind::Local);
        assert_eq!(storage_kind_for_name("s3").unwrap(), StorageKind::S3);
        assert_eq!(storage_kind_for_name("s3-compatible").unwrap(), StorageKind::S3);
        assert_eq!(storage_kind_for_name("r2").unwrap(), StorageKind::R2);
        assert_eq!(storage_kind_for_name("drive").unwrap(), StorageKind::GoogleDrive);
        assert_eq!(storage_kind_for_name("sftp").unwrap(), StorageKind::Sftp);
        assert!(storage_kind_for_name("bogus").is_err());
    }

    #[test]
    fn validate_provider_kind_list_dedupes_and_requires_one() {
        let kinds = vec!["S3".to_string(), "drive".to_string(), "s3".to_string()];
        assert_eq!(
            validate_provider_kind_list(&kinds).unwrap(),
            vec!["s3".to_string(), "drive".to_string()]
        );
        assert!(validate_provider_kind_list(&[]).is_err());
        assert!(validate_provider_kind_list(&["bogus".to_string()]).is_err());
    }

    #[test]
    fn validate_billing_cycle_accepts_closed_vocabulary() {
        assert_eq!(validate_billing_cycle(&None).unwrap(), None);
        assert_eq!(
            validate_billing_cycle(&Some("Monthly".to_string())).unwrap(),
            Some("monthly".to_string())
        );
        assert_eq!(
            validate_billing_cycle(&Some("usage_based".to_string())).unwrap(),
            Some("usage_based".to_string())
        );
        assert!(validate_billing_cycle(&Some("weekly".to_string())).is_err());
    }
}
