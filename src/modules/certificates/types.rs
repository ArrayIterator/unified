//! Certificates - certificate records and settings types.
//!
//! Part of the `certificates` module split; see `src/modules/certificates.rs`.

use unified_core::config::AppConfig;
use crate::http::acme::AcmeConfig;
use crate::http::acme::ChallengeKind;
use crate::http::acme::KeyType;
use chrono::DateTime;
use chrono::Utc;
use serde::Deserialize;
use serde::Serialize;

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

/// One certificate known to this module: configured, renewed,
/// installed, or observed as a fallback.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CertificateRecord {
    pub id: String,
    pub domain: String,
    /// ACME provider key (`letsencrypt`, `google`, `zerossl`,
    /// `buypass`, `custom`) or `self-signed` for fallbacks.
    pub provider: String,
    /// `valid`, `expiring`, `expired`, `invalid`, `self-signed`,
    /// or `revoked`.
    pub status: String,
    pub issuer: String,
    pub sans: Vec<String>,
    pub fingerprint_sha256: String,
    pub not_before: Option<DateTime<Utc>>,
    pub expires_at: Option<DateTime<Utc>>,
    pub days_remaining: Option<i64>,
    pub self_signed: bool,
    pub wildcard: bool,
    /// `configured`, `dynamic`, `fallback`, or `custom`.
    pub source: String,
    pub revoked: bool,
    pub last_renewed_at: Option<DateTime<Utc>>,
    pub last_error: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// One renewal attempt, for the renewal history log.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RenewalAttempt {
    pub id: String,
    pub domain: String,
    pub provider: String,
    /// `started`, `succeeded`, `failed`, or `skipped` (not due yet).
    pub status: String,
    pub started_at: DateTime<Utc>,
    pub finished_at: Option<DateTime<Utc>>,
    pub error: Option<String>,
    pub new_expires_at: Option<DateTime<Utc>>,
    pub forced: bool,
}

/// Default published Cloudflare IP-range list URLs. These are the
/// SOURCE locations the ranges are fetched from, not the ranges
/// themselves: the ranges are stored data (see `CfIpRanges`) and are
/// never baked into the code. Overridable via `PUT /settings`
/// (`cf_ranges_v4_url`, `cf_ranges_v6_url`).
pub const DEFAULT_CF_RANGES_V4_URL: &str = "https://www.cloudflare.com/ips-v4";

pub const DEFAULT_CF_RANGES_V6_URL: &str = "https://www.cloudflare.com/ips-v6";

/// Default minimum remaining validity (days) a custom-uploaded
/// certificate must have. Overridable via `PUT /settings`
/// (`custom_min_validity_days`).
pub const DEFAULT_CUSTOM_MIN_VALIDITY_DAYS: i64 = 7;

/// Runtime certificate settings, seeded from the config file at
/// startup and adjustable through `PUT /settings`. EAB secrets are
/// held here and never serialized: the API only ever returns
/// `AcmeConfig::redacted()`.
#[derive(Debug, Clone)]
pub struct CertModuleSettings {
    /// `acme.enabled` from the config file.
    pub enabled: bool,
    pub acme: AcmeConfig,
    /// How often the expiry check runs, in seconds (default 86400).
    pub check_interval_secs: u64,
    /// Minimum remaining validity (days) for a custom-uploaded
    /// certificate; uploads expiring sooner are rejected. Default 7.
    pub custom_min_validity_days: i64,
    /// Cloudflare detection methods, individually enable/disable-able.
    /// `cf_detect_api` defaults to `false`: no module in the repo
    /// answers the `cloudflare.zone.lookup` hook yet, so the API
    /// method can only report "unknown" and every detection would
    /// carry the UNCONFIRMED warning. An admin may still enable it
    /// explicitly (for the day a listener lands); the detection
    /// result says so honestly when no handler answers.
    pub cf_detect_dns: bool,
    pub cf_detect_probe: bool,
    pub cf_detect_api: bool,
    /// Published Cloudflare IP-range list URLs (fetch sources).
    pub cf_ranges_v4_url: String,
    pub cf_ranges_v6_url: String,
}

impl Default for CertModuleSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            acme: AcmeConfig::default(),
            check_interval_secs: 86_400,
            custom_min_validity_days: DEFAULT_CUSTOM_MIN_VALIDITY_DAYS,
            cf_detect_dns: true,
            cf_detect_probe: true,
            cf_detect_api: false,
            cf_ranges_v4_url: DEFAULT_CF_RANGES_V4_URL.to_string(),
            cf_ranges_v6_url: DEFAULT_CF_RANGES_V6_URL.to_string(),
        }
    }
}

impl CertModuleSettings {
    pub(crate) fn from_app_config(config: &AppConfig) -> Self {
        Self {
            enabled: config.acme.enabled,
            acme: AcmeConfig::from_app_config(config),
            check_interval_secs: 86_400,
            custom_min_validity_days: DEFAULT_CUSTOM_MIN_VALIDITY_DAYS,
            cf_detect_dns: true,
            cf_detect_probe: true,
            cf_detect_api: false,
            cf_ranges_v4_url: DEFAULT_CF_RANGES_V4_URL.to_string(),
            cf_ranges_v6_url: DEFAULT_CF_RANGES_V6_URL.to_string(),
        }
    }
}

#[derive(Debug, Deserialize)]
pub(crate) struct RenewRequest {
    #[serde(default)]
    pub(crate) force: bool,
}

#[derive(Debug, Deserialize)]
pub(crate) struct RevokeRequest {
    #[serde(default)]
    pub(crate) reason: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct SwapRequest {
    /// New certificate chain in PEM format.
    pub(crate) cert_pem: String,
    /// Private key in PEM format. Never logged or returned.
    pub(crate) key_pem: String,
}

#[derive(Debug, Deserialize)]
pub(crate) struct InstallRequest {
    pub(crate) domain: String,
    /// Certificate chain in PEM format.
    pub(crate) cert_pem: String,
    /// Private key in PEM format. Never logged or returned.
    pub(crate) key_pem: String,
}

#[derive(Debug, Deserialize)]
pub(crate) struct UpdateSettingsRequest {
    #[serde(default)]
    pub(crate) enabled: Option<bool>,
    #[serde(default)]
    pub(crate) provider: Option<String>,
    #[serde(default)]
    pub(crate) directory_url: Option<String>,
    #[serde(default)]
    pub(crate) staging: Option<bool>,
    #[serde(default)]
    pub(crate) contacts: Option<Vec<String>>,
    #[serde(default)]
    pub(crate) eab_key_id: Option<String>,
    #[serde(default)]
    pub(crate) eab_hmac_key: Option<String>,
    #[serde(default)]
    pub(crate) renew_days_before_expiry: Option<i64>,
    #[serde(default)]
    pub(crate) key_type: Option<KeyType>,
    #[serde(default)]
    pub(crate) key_size: Option<u32>,
    #[serde(default)]
    pub(crate) curve: Option<String>,
    #[serde(default)]
    pub(crate) challenge: Option<ChallengeKind>,
    #[serde(default)]
    pub(crate) alert_thresholds_days: Option<Vec<i64>>,
    #[serde(default)]
    pub(crate) check_interval_secs: Option<u64>,
    /// Minimum remaining validity (days) for custom uploads.
    #[serde(default)]
    pub(crate) custom_min_validity_days: Option<i64>,
    /// Cloudflare detection toggles (DNS ranges / live probe / API).
    #[serde(default)]
    pub(crate) cf_detect_dns: Option<bool>,
    #[serde(default)]
    pub(crate) cf_detect_probe: Option<bool>,
    #[serde(default)]
    pub(crate) cf_detect_api: Option<bool>,
    /// Published Cloudflare IP-range list URLs (fetch sources).
    #[serde(default)]
    pub(crate) cf_ranges_v4_url: Option<String>,
    #[serde(default)]
    pub(crate) cf_ranges_v6_url: Option<String>,
}
