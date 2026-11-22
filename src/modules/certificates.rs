// Certificates module — advanced SSL certificate management.
//
// Config-first certificate management built on `crate::http::tls`
// (SNI storage, self-signed fallback, swap with rollback) and
// `crate::http::acme` (multi-provider ACME, EAB, HTTP-01 renewal).
// The configuration file defines everything (see the `acme` and
// `app.ssl` sections); this module is the API and dashboard surface
// over it, plus the automation around it.
//
// Routes under `/api/v1/certificates`:
//
//   GET    /                        list certificates (paginated)
//   GET    /config                  effective config + provider matrix + validation
//   GET    /validate                validate every configured certificate
//   GET    /history                 renewal history (paginated)
//   POST   /install                 install a custom certificate (validated first)
//   POST   /renew-all               renew every certificate near expiry
//   POST   /check-expiry            run the expiry check now (alerts + summary)
//   GET    /settings                runtime certificate settings (secrets redacted)
//   PUT    /settings                update runtime certificate settings
//   GET    /{domain}                one certificate: detail + recent history
//   POST   /{domain}/validate       test/validate one certificate
//   POST   /{domain}/renew          renew now (body: {"force": bool})
//   POST   /{domain}/force-renew    renew now, regardless of expiry
//   POST   /{domain}/revoke         revoke a certificate (body: {"reason": ...})
//   POST   /{domain}/swap           swap the live certificate (validated first)
//   POST   /{domain}/rollback       restore the previous live certificate
//   GET    /cert-modes              list per-domain certificate modes (paginated)
//   GET    /cert-modes/definitions  certificate-mode registry (admin-extensible)
//   PUT    /cert-modes/definitions  replace the certificate-mode registry (admin)
//   GET    /cloudflare-ranges       stored Cloudflare IP-range snapshot (data + date)
//   POST   /cloudflare-ranges/refresh  refresh the IP-range snapshot from Cloudflare
//   GET    /{domain}/cert-mode      one domain's certificate mode + mode history
//   PUT    /{domain}/cert-mode      set a domain's certificate mode
//   POST   /{domain}/cert-mode/custom-upload  upload a custom cert (validated first)
//   POST   /{domain}/detect-cloudflare        detect Cloudflare for a domain
//   GET    /{domain}/cloudflare-detection     stored detection result
//   POST   /{domain}/cloudflare-detection/recheck  re-run detection
//
// Certificate modes are a data-driven registry, not a hard-coded list:
// the built-in definitions (`letsencrypt`, `custom`, `cloudflare`) are
// seed data in `default_cert_mode_definitions`, editable through
// `PUT /cert-modes/definitions`. The custom-upload minimum remaining
// validity (`custom_min_validity_days`, default 7) and the Cloudflare
// detection toggles / range-list URLs are settings (`PUT /settings`),
// not constants. The Cloudflare IP ranges are stored data with a
// fetch date (`GET /cloudflare-ranges`) and are refreshable; no range
// list is baked into the code. ACME provider choice keeps using the
// existing provider registry (`AcmeConfig::resolve_provider`).
//
// Events emitted (see `cert_events` below): `ssl.renewed`,
// `ssl.renew_failed`, `ssl.revoked`, `ssl.swapped`, `ssl.expiring`,
// `ssl.installed`, `ssl.cert_mode.changed`, `ssl.cloudflare.detected`.
// The module also listens to the core `ssl.renew` event (dispatched by
// the scheduler and the Domains module), to `ssl.swap` completions
// (emitted by the server ACME listener), and to
// `ssl.cloudflare.recheck` (scheduled re-check of stored detections).
// Hook contracts this module RELIES on (answered by other modules):
// `cloudflare.zone.lookup` (the Cloudflare module answers whether a
// domain's zone exists there and which SSL mode it uses) and
// `domain.ownership.check` (the SaaS/domains side answers whether a
// tenant user owns a domain); both fail closed when unanswered.
// Expiry alerts go out as `notification.requested` (user `system`)
// and as `ssl.expiring` events, so the audit log and the
// Notifications module observe them through the hook bus.
//
// Secrets (private keys, EAB credentials) live only in settings /
// config, are never logged, and are never returned by any response:
// responses carry fingerprints and metadata only.
//
// CLI commands (see `register_cli`):
//   cert-list, cert-validate, cert-renew, cert-renew-all,
//   cert-revoke, cert-check-expiry

use crate::core::config::AppConfig;
use crate::core::context::AppContext;
use crate::core::error::{UnifiedError, UnifiedResult};
use crate::core::hooks::{events, HookBus, HookContext};
use crate::core::json;
use crate::core::modules::{CliCommandDef, CliRegistry, Module, ModuleRegistry, RouteInfo};
use crate::core::pagination::{PageResult, PaginationParams};
use crate::http::acme::{
    provider_matrix, AcmeClient, AcmeConfig, ChallengeKind, KeyType,
};
use crate::http::tls::{
    status_for, validate_config_entry, validate_pem_pair, validate_ssl_config,
    CertificateInfo, CertificateValidation, SslStorage,
};
use crate::modules::network::DnsVerifier;
use actix_web::{web, HttpRequest, HttpResponse};
use chrono::{DateTime, Utc};
use clap::{Arg, ArgAction, ArgMatches, Command};
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::net::IpAddr;
use std::sync::{Arc, RwLock};
use std::time::Duration;
use uuid::Uuid;

// ---------------------------------------------------------------------------
// Events.
//
// The core bus (`crate::core::hooks::events`) defines only `ssl.renew`
// and `ssl.swap`. The certificate lifecycle events below are defined
// here, in the module that owns them, so no shared file has to change
// for this module to work. For the core definitions to live in
// `crate::core::hooks::events` instead, add one constant per name
// there (for example `pub const SSL_RENEWED: &str = "ssl.renewed";`)
// and switch the emits below to those constants; the audit module
// would then also need the new names in its `AUDITED_EVENTS` list.
// ---------------------------------------------------------------------------

pub mod cert_events {
    /// Emitted after a renewal succeeds. Payload: `domain`,
    /// `provider`, `ssl_expires_at`, `completed: true`.
    pub const SSL_RENEWED: &str = "ssl.renewed";
    /// Emitted after a renewal fails. Payload: `domain`, `provider`,
    /// `error`.
    pub const SSL_RENEW_FAILED: &str = "ssl.renew_failed";
    /// Emitted after a certificate is revoked. Payload: `domain`,
    /// `reason`, `fingerprint_sha256`.
    pub const SSL_REVOKED: &str = "ssl.revoked";
    /// Emitted after a live certificate swap (or rollback). Payload:
    /// `domain`, `ssl_expires_at`, `fingerprint_sha256`,
    /// `previous_fingerprint_sha256`, `rolled_back: bool`.
    pub const SSL_SWAPPED: &str = "ssl.swapped";
    /// Emitted when a certificate approaches expiry (or already
    /// expired). Payload: `domain`, `days_remaining`, `threshold_days`,
    /// `expired: bool`.
    pub const SSL_EXPIRING: &str = "ssl.expiring";
    /// Emitted after a custom certificate is installed. Payload:
    /// `domain`, `fingerprint_sha256`, `ssl_expires_at`, `source`.
    pub const SSL_INSTALLED: &str = "ssl.installed";
    /// Emitted after a domain's certificate mode changes. Payload:
    /// `domain`, `mode`, `previous_mode`, `provider`, `status`,
    /// `actor`.
    pub const SSL_CERT_MODE_CHANGED: &str = "ssl.cert_mode.changed";
    /// Emitted after a Cloudflare detection run for a domain.
    /// Payload: `domain`, `detected`, `ssl_mode`,
    /// `end_to_end_encrypted`, `checked_at`.
    pub const SSL_CF_DETECTED: &str = "ssl.cloudflare.detected";
    /// A scheduler (or admin) asks for a Cloudflare re-check.
    /// Payload: `{"domain"}` for one domain, or no domain to re-check
    /// every domain with a stored mode. Handled by this module.
    pub const SSL_CF_RECHECK: &str = "ssl.cloudflare.recheck";
    /// Hook contract: ask the Cloudflare module whether a domain's
    /// zone exists there. Request payload: `{"domain", "found": false,
    /// "zone_id": null, "ssl_mode": null}`; the answering handler sets
    /// `found` / `zone_id` / `ssl_mode` (`flexible`, `full`, or
    /// `full_strict`). No answer means "unknown", never "found".
    pub const CLOUDFLARE_ZONE_LOOKUP: &str = "cloudflare.zone.lookup";
    /// Hook contract: ask the SaaS/domains side whether a tenant user
    /// owns a domain. Request payload: `{"domain", "user_id",
    /// "owned": false}`; the answering handler sets `owned: true`
    /// when the user owns the (verified) domain. No answer means "not
    /// owned": tenant-scoped access fails closed.
    pub const DOMAIN_OWNERSHIP_CHECK: &str = "domain.ownership.check";
}

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
            cf_detect_api: true,
            cf_ranges_v4_url: DEFAULT_CF_RANGES_V4_URL.to_string(),
            cf_ranges_v6_url: DEFAULT_CF_RANGES_V6_URL.to_string(),
        }
    }
}

impl CertModuleSettings {
    fn from_app_config(config: &AppConfig) -> Self {
        Self {
            enabled: config.acme.enabled,
            acme: AcmeConfig::from_app_config(config),
            check_interval_secs: 86_400,
            custom_min_validity_days: DEFAULT_CUSTOM_MIN_VALIDITY_DAYS,
            cf_detect_dns: true,
            cf_detect_probe: true,
            cf_detect_api: true,
            cf_ranges_v4_url: DEFAULT_CF_RANGES_V4_URL.to_string(),
            cf_ranges_v6_url: DEFAULT_CF_RANGES_V6_URL.to_string(),
        }
    }
}

#[derive(Debug, Deserialize)]
struct RenewRequest {
    #[serde(default)]
    force: bool,
}

#[derive(Debug, Deserialize)]
struct RevokeRequest {
    #[serde(default)]
    reason: Option<String>,
}

#[derive(Debug, Deserialize)]
struct SwapRequest {
    /// New certificate chain in PEM format.
    cert_pem: String,
    /// Private key in PEM format. Never logged or returned.
    key_pem: String,
}

#[derive(Debug, Deserialize)]
struct InstallRequest {
    domain: String,
    /// Certificate chain in PEM format.
    cert_pem: String,
    /// Private key in PEM format. Never logged or returned.
    key_pem: String,
}

#[derive(Debug, Deserialize)]
struct UpdateSettingsRequest {
    #[serde(default)]
    enabled: Option<bool>,
    #[serde(default)]
    provider: Option<String>,
    #[serde(default)]
    directory_url: Option<String>,
    #[serde(default)]
    staging: Option<bool>,
    #[serde(default)]
    contacts: Option<Vec<String>>,
    #[serde(default)]
    eab_key_id: Option<String>,
    #[serde(default)]
    eab_hmac_key: Option<String>,
    #[serde(default)]
    renew_days_before_expiry: Option<i64>,
    #[serde(default)]
    key_type: Option<KeyType>,
    #[serde(default)]
    key_size: Option<u32>,
    #[serde(default)]
    curve: Option<String>,
    #[serde(default)]
    challenge: Option<ChallengeKind>,
    #[serde(default)]
    alert_thresholds_days: Option<Vec<i64>>,
    #[serde(default)]
    check_interval_secs: Option<u64>,
    /// Minimum remaining validity (days) for custom uploads.
    #[serde(default)]
    custom_min_validity_days: Option<i64>,
    /// Cloudflare detection toggles (DNS ranges / live probe / API).
    #[serde(default)]
    cf_detect_dns: Option<bool>,
    #[serde(default)]
    cf_detect_probe: Option<bool>,
    #[serde(default)]
    cf_detect_api: Option<bool>,
    /// Published Cloudflare IP-range list URLs (fetch sources).
    #[serde(default)]
    cf_ranges_v4_url: Option<String>,
    #[serde(default)]
    cf_ranges_v6_url: Option<String>,
}

// ---------------------------------------------------------------------------
// Certificate modes (data-driven registry) and Cloudflare detection.
// ---------------------------------------------------------------------------

/// One certificate-mode definition in the admin-extensible registry.
/// The built-in modes are seed data only (see
/// `default_cert_mode_definitions`); an admin may add, rename, or
/// disable modes through `PUT /cert-modes/definitions` without a code
/// change. Behaviour is driven by the flags, never by matching the
/// mode id in code: `requires_acme` triggers the existing ACME
/// renewal, `requires_upload` expects a PEM upload, and
/// `requires_cloudflare` marks the mode as a Cloudflare origin mode.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CertModeDefinition {
    /// Mode id, for example `letsencrypt`, `custom`, `cloudflare`.
    pub id: String,
    pub label: String,
    pub description: String,
    /// Setting this mode expects a certificate/key PEM upload.
    pub requires_upload: bool,
    /// Setting this mode issues/renews through the ACME provider
    /// registry (`AcmeConfig::resolve_provider`).
    pub requires_acme: bool,
    /// This mode is a Cloudflare origin-certificate mode.
    pub requires_cloudflare: bool,
    /// Disabled definitions cannot be newly selected; existing mode
    /// records that reference one keep their stored value.
    pub enabled: bool,
}

/// Seed definitions for the certificate-mode registry. Seed data, not
/// a closed list: the registry is replaced through
/// `PUT /cert-modes/definitions`.
pub fn default_cert_mode_definitions() -> Vec<CertModeDefinition> {
    vec![
        CertModeDefinition {
            id: "letsencrypt".to_string(),
            label: "Let's Encrypt (automatic)".to_string(),
            description: "Automatic issuance and renewal through the configured ACME provider.".to_string(),
            requires_upload: false,
            requires_acme: true,
            requires_cloudflare: false,
            enabled: true,
        },
        CertModeDefinition {
            id: "custom".to_string(),
            label: "Custom certificate upload".to_string(),
            description: "Upload a certificate PEM and private key PEM; validated before install.".to_string(),
            requires_upload: true,
            requires_acme: false,
            requires_cloudflare: false,
            enabled: true,
        },
        CertModeDefinition {
            id: "cloudflare".to_string(),
            label: "Cloudflare origin certificate".to_string(),
            description: "Origin-certificate mode for domains served through Cloudflare; an origin certificate is expected and Full (Strict) mode is required for end-to-end encryption.".to_string(),
            requires_upload: false,
            requires_acme: false,
            requires_cloudflare: true,
            enabled: true,
        },
    ]
}

/// Validate a replacement mode registry: non-empty, unique ids, ids
/// are lowercase slugs, every definition has a label.
fn validate_mode_definitions(definitions: &[CertModeDefinition]) -> Result<(), String> {
    if definitions.is_empty() {
        return Err("at least one certificate mode definition is required".to_string());
    }
    let mut seen: HashSet<String> = HashSet::new();
    for definition in definitions {
        let id = definition.id.trim();
        if id.is_empty()
            || !id
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        {
            return Err(format!(
                "invalid mode id \"{id}\": use lowercase letters, digits, and hyphens"
            ));
        }
        if !seen.insert(id.to_string()) {
            return Err(format!("duplicate mode id \"{id}\""));
        }
        if definition.label.trim().is_empty() {
            return Err(format!("mode \"{id}\" has an empty label"));
        }
    }
    Ok(())
}

/// One domain's certificate mode: the current row of the
/// `domain_cert_modes` table (mode history is the earlier rows for
/// the same domain). Private keys and PEM material are never stored
/// here: only fingerprints and metadata.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DomainCertMode {
    pub domain: String,
    /// Mode id from the registry (`letsencrypt`, `custom`,
    /// `cloudflare`, or an admin-defined id).
    pub mode: String,
    /// ACME provider key, `custom`, `cloudflare`, or the mode id.
    pub provider: String,
    /// SHA-256 fingerprint of the installed certificate, if any.
    pub fingerprint_sha256: String,
    pub installed_at: Option<DateTime<Utc>>,
    pub previous_fingerprint: Option<String>,
    /// `active`, `pending`, `awaiting_upload`, or `error`.
    pub status: String,
    pub last_error: Option<String>,
    pub updated_at: DateTime<Utc>,
    pub updated_by: String,
}

/// Stored Cloudflare IP-range snapshot: DATA, not code. The ranges
/// are fetched from the published lists (see the settings URLs),
/// stored with their fetch date, and refreshed through
/// `POST /cloudflare-ranges/refresh` (or automatically on first use).
/// Until the first successful fetch the lists are empty and the DNS
/// detection method honestly reports `ranges_unavailable` instead of
/// pretending a bundled snapshot is current.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CfIpRanges {
    pub v4: Vec<String>,
    pub v6: Vec<String>,
    pub fetched_at: Option<DateTime<Utc>>,
    pub source_v4: String,
    pub source_v6: String,
    pub fetch_error: Option<String>,
}

impl Default for CfIpRanges {
    fn default() -> Self {
        Self {
            v4: Vec::new(),
            v6: Vec::new(),
            fetched_at: None,
            source_v4: DEFAULT_CF_RANGES_V4_URL.to_string(),
            source_v6: DEFAULT_CF_RANGES_V6_URL.to_string(),
            fetch_error: None,
        }
    }
}

/// One stored Cloudflare detection result for a domain.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CloudflareDetection {
    pub domain: String,
    pub detected: bool,
    /// Per-method results (`dns`, `probe`, `api`), each honest about
    /// what it checked, found, or could not check.
    pub methods: Value,
    /// Cloudflare SSL mode reported by the API check, when known:
    /// `flexible`, `full`, or `full_strict`.
    pub ssl_mode: Option<String>,
    /// `false` in Flexible mode (no origin encryption), `true` in
    /// Full / Full (Strict) mode, `None` when unconfirmed.
    pub end_to_end_encrypted: Option<bool>,
    /// Set whenever end-to-end encryption is absent or unconfirmed,
    /// so the system NEVER claims full encryption in Flexible mode.
    pub flexible_warning: Option<String>,
    pub checked_at: DateTime<Utc>,
}

#[derive(Debug, Deserialize)]
struct SetCertModeRequest {
    mode: String,
    #[serde(default)]
    provider: Option<String>,
}

#[derive(Debug, Deserialize)]
struct CustomUploadRequest {
    /// Certificate chain in PEM format.
    cert_pem: String,
    /// Private key in PEM format. Never logged or returned.
    key_pem: String,
}

#[derive(Debug, Deserialize)]
struct UpdateModeDefinitionsRequest {
    modes: Vec<CertModeDefinition>,
}

/// Capability names checked through the `role.check` hook.
const PERM_CERTIFICATES_VIEW: &str = "certificates.view";
const PERM_CERTIFICATES_MANAGE: &str = "certificates.manage";
const PERM_CERTIFICATES_MANAGE_OWN: &str = "certificates.manage_own";

/// Caller identity: `X-User-Id` (required) plus `X-Role` (required
/// for capability checks), exactly like the properties module.
struct Caller {
    actor: String,
    role: String,
}

fn caller(req: &HttpRequest) -> UnifiedResult<Caller> {
    let actor = req
        .headers()
        .get("x-user-id")
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .ok_or_else(|| UnifiedError::unauthorized("Missing X-User-Id header"))?;
    let role = req
        .headers()
        .get("x-role")
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .ok_or_else(|| UnifiedError::unauthorized("Missing X-Role header"))?;
    Ok(Caller { actor, role })
}

/// Ask the roles module whether `role` holds `permission` through
/// the `role.check` hook. Fails closed: no answer means not allowed.
fn check_capability(ctx: &AppContext, role: &str, permission: &str) -> bool {
    let context = ctx.hooks.emit(
        "role.check",
        json!({ "role": role, "permission": permission, "allowed": false }),
    );
    context
        .get("allowed")
        .and_then(|value| value.as_bool())
        .unwrap_or(false)
}

fn require_permission(ctx: &AppContext, role: &str, permission: &str) -> UnifiedResult<()> {
    if check_capability(ctx, role, permission) {
        Ok(())
    } else {
        Err(UnifiedError::forbidden(format!(
            "Role \"{role}\" does not hold permission \"{permission}\""
        )))
    }
}

/// Ask the SaaS/domains side (through the `domain.ownership.check`
/// hook) whether `actor` owns the verified `domain`. Fails closed:
/// no answer means not owned.
fn domain_owned(ctx: &AppContext, actor: &str, domain: &str) -> bool {
    let context = ctx.hooks.emit(
        cert_events::DOMAIN_OWNERSHIP_CHECK,
        json!({ "domain": domain, "user_id": actor, "owned": false }),
    );
    context
        .get("owned")
        .and_then(|value| value.as_bool())
        .unwrap_or(false)
}

/// Authorize one domain-scoped call. An admin (role holding
/// `certificates.manage`) may act on any domain. A tenant (role
/// holding `certificates.manage_own` for writes, or
/// `certificates.view` for reads) may only act on their OWN verified
/// domains, confirmed through the `domain.ownership.check` hook;
/// without a confirming answer the call is denied. Fail closed.
fn authorize_domain(
    ctx: &AppContext,
    req: &HttpRequest,
    domain: &str,
    write: bool,
) -> UnifiedResult<Caller> {
    let caller = caller(req)?;
    if check_capability(ctx, &caller.role, PERM_CERTIFICATES_MANAGE) {
        return Ok(caller);
    }
    if write {
        require_permission(ctx, &caller.role, PERM_CERTIFICATES_MANAGE_OWN)?;
    } else {
        require_permission(ctx, &caller.role, PERM_CERTIFICATES_VIEW)?;
    }
    if domain_owned(ctx, &caller.actor, domain) {
        Ok(caller)
    } else {
        Err(UnifiedError::forbidden(format!(
            "Domain \"{domain}\" is not owned by the caller; tenant access requires a verified domain"
        )))
    }
}

/// Parse one CIDR notation entry (`192.0.2.0/24`,
/// `2001:db8::/32`) into (network address, prefix length).
fn parse_cidr(cidr: &str) -> Option<(IpAddr, u8)> {
    let (address, prefix) = cidr.trim().split_once('/')?;
    let network: IpAddr = address.trim().parse().ok()?;
    let prefix: u8 = prefix.trim().parse().ok()?;
    match network {
        IpAddr::V4(_) if prefix > 32 => None,
        IpAddr::V6(_) if prefix > 128 => None,
        _ => Some((network, prefix)),
    }
}

/// True when `ip` lies inside the `cidr` range. Address families
/// must match; a malformed range never matches.
fn cidr_contains(cidr: &str, ip: &IpAddr) -> bool {
    let Some((network, prefix)) = parse_cidr(cidr) else {
        return false;
    };
    match (network, ip) {
        (IpAddr::V4(network), IpAddr::V4(address)) => {
            let mask = if prefix == 0 {
                0u32
            } else {
                u32::MAX << (32 - prefix)
            };
            (u32::from(network) & mask) == (u32::from(*address) & mask)
        }
        (IpAddr::V6(network), IpAddr::V6(address)) => {
            let mask = if prefix == 0 {
                0u128
            } else {
                u128::MAX << (128 - prefix)
            };
            (u128::from(network) & mask) == (u128::from(*address) & mask)
        }
        _ => false,
    }
}

// ---------------------------------------------------------------------------
// State
// ---------------------------------------------------------------------------

pub struct CertificatesState {
    hooks: RwLock<Arc<HookBus>>,
    storage: RwLock<Option<Arc<SslStorage>>>,
    records: DashMap<String, CertificateRecord>,
    history: DashMap<String, RenewalAttempt>,
    settings: RwLock<CertModuleSettings>,
    /// Thresholds (days before expiry, plus -1 for "already
    /// expired") already alerted for, per domain, so the expiry
    /// check does not repeat the same alert on every run.
    alerted_thresholds: DashMap<String, Vec<i64>>,
    /// Configured cert/key file paths per domain (`default` for the
    /// default pair), so validation can re-read the files on demand.
    cert_paths: RwLock<HashMap<String, (String, String)>>,
    /// Per-domain certificate modes (`domain_cert_modes` table).
    cert_modes: DashMap<String, DomainCertMode>,
    /// Mode history per domain: earlier `DomainCertMode` rows, most
    /// recent first (capped at 20 entries per domain).
    mode_history: DashMap<String, Vec<DomainCertMode>>,
    /// The certificate-mode registry (admin-extensible definitions).
    mode_definitions: RwLock<Vec<CertModeDefinition>>,
    /// Stored Cloudflare detection results per domain.
    detections: DashMap<String, CloudflareDetection>,
    /// Stored Cloudflare IP-range snapshot (fetched data + date).
    cf_ranges: RwLock<CfIpRanges>,
}

fn new_id() -> String {
    Uuid::new_v4().to_string()
}

fn now() -> DateTime<Utc> {
    Utc::now()
}

fn normalize_domain(domain: &str) -> String {
    crate::http::tls::to_ascii_name(domain)
}

impl CertificatesState {
    pub fn new() -> Self {
        Self {
            hooks: RwLock::new(Arc::new(HookBus::new())),
            storage: RwLock::new(None),
            records: DashMap::new(),
            history: DashMap::new(),
            settings: RwLock::new(CertModuleSettings::default()),
            alerted_thresholds: DashMap::new(),
            cert_paths: RwLock::new(HashMap::new()),
            cert_modes: DashMap::new(),
            mode_history: DashMap::new(),
            mode_definitions: RwLock::new(default_cert_mode_definitions()),
            detections: DashMap::new(),
            cf_ranges: RwLock::new(CfIpRanges::default()),
        }
    }

    fn bus(&self) -> Arc<HookBus> {
        self.hooks.read().expect("hooks lock poisoned").clone()
    }

    fn emit(&self, event: &str, data: Value) {
        self.bus().emit(event, data);
    }

    /// Ask the Notifications module (through the hook bus) to alert
    /// user `system` about a certificate event.
    fn notify(&self, kind: &str, title: String, body: String) {
        self.emit(
            "notification.requested",
            json!({
                "user_id": "system",
                "title": title,
                "body": body,
                "kind": kind,
            }),
        );
    }

    fn storage(&self) -> Option<Arc<SslStorage>> {
        self.storage.read().expect("storage lock poisoned").clone()
    }

    fn settings(&self) -> CertModuleSettings {
        self.settings.read().expect("settings lock poisoned").clone()
    }

    /// Seed the state from the config file: use the shared storage
    /// when one is attached (`shared_storage`, the handle the HTTP
    /// server built and attached to the context), otherwise build a
    /// private storage with the lenient loader. Either way the
    /// lenient behaviour holds: invalid entries are reported, never
    /// loaded, and never fail startup. One record is created per
    /// certificate, and `ssl.expiring` / `ssl.renew_failed` alerts
    /// are emitted for the entries that could not be used, so a
    /// missing or expired certificate falls back to the self-signed
    /// certificate for that domain instead of taking the server down.
    ///
    /// The shared path seeds records from the live storage
    /// (`list_certificates`, which also covers renewed and dynamic
    /// certificates) and only re-validates the configured entries to
    /// catch invalid ones the shared storage does not hold. The
    /// fallback path seeds from the lenient loader's reports instead.
    /// Callers pass `ctx.tls()` in the server path; the CLI passes
    /// `None` because it has no server context.
    pub fn seed_from_config(
        &self,
        config: &AppConfig,
        hooks: Arc<HookBus>,
        shared_storage: Option<Arc<SslStorage>>,
    ) {
        *self.hooks.write().expect("hooks lock poisoned") = hooks.clone();
        *self
            .settings
            .write()
            .expect("settings lock poisoned") = CertModuleSettings::from_app_config(config);
        {
            let mut paths = self.cert_paths.write().expect("paths lock poisoned");
            if !config.app.ssl.cert.is_empty() && !config.app.ssl.key.is_empty() {
                paths.insert(
                    "default".to_string(),
                    (config.app.ssl.cert.clone(), config.app.ssl.key.clone()),
                );
            }
            for (domain, entry) in &config.app.ssl.domains {
                if !entry.cert.is_empty() && !entry.key.is_empty() {
                    paths.insert(
                        normalize_domain(domain),
                        (entry.cert.clone(), entry.key.clone()),
                    );
                }
            }
        }
        match shared_storage {
            Some(storage) => {
                // Shared handle from the context: the same storage the
                // HTTPS listener serves, so renewals and swaps through
                // the server are visible here (and vice versa) instead
                // of this module serving a stale private copy.
                *self.storage.write().expect("storage lock poisoned") = Some(storage.clone());
                self.seed_records_from_storage(&storage);
                self.report_invalid_config_entries(config);
            }
            None => {
                // Fallback: no shared handle is attached yet. Module
                // init runs before the HTTP server builds the storage
                // (init_all precedes run_server in the start command),
                // and the CLI has no server at all, so this is the
                // normal startup path today: build a private storage
                // with the lenient loader, exactly as before.
                let (storage, reports) = SslStorage::from_config_lenient(&config.app.ssl);
                *self.storage.write().expect("storage lock poisoned") = Some(storage);
                for report in &reports {
                    self.upsert_record_from_validation(report, "custom", "configured");
                    if report.valid {
                        continue;
                    }
                    self.report_invalid_entry(report);
                }
            }
        }
    }

    /// Seed records from the live certificates in a shared storage:
    /// configured, renewed, and dynamic entries. Self-signed fallback
    /// entries are skipped, matching the refresh paths in
    /// `check_expiry` and `list_certificates`.
    fn seed_records_from_storage(&self, storage: &SslStorage) {
        for info in storage.list_certificates() {
            if info.source != "fallback" {
                self.upsert_record_from_info(&info, "acme");
            }
        }
    }

    /// Re-validate the configured entries and report the invalid ones
    /// (record plus alert). Valid entries are already seeded from the
    /// shared storage, so only invalid reports are processed here. A
    /// storage built by the strict loader holds every configured
    /// entry, so this normally finds nothing; it covers handles
    /// attached by another path that skipped an entry.
    fn report_invalid_config_entries(&self, config: &AppConfig) {
        for report in validate_ssl_config(&config.app.ssl) {
            if report.valid {
                continue;
            }
            self.upsert_record_from_validation(&report, "custom", "configured");
            self.report_invalid_entry(&report);
        }
    }

    /// Emit the alert (and notification) for one invalid configured
    /// certificate: an expiry alert when the certificate expired, a
    /// renewal-failure alert otherwise.
    fn report_invalid_entry(&self, report: &CertificateValidation) {
        let reasons = report.reasons.join("; ");
        let expired = report
            .info
            .as_ref()
            .is_some_and(|info| info.days_remaining < 0);
        if expired {
            self.emit(
                cert_events::SSL_EXPIRING,
                json!({
                    "domain": &report.domain,
                    "days_remaining": report.info.as_ref().map(|info| info.days_remaining),
                    "expired": true,
                    "reasons": &report.reasons,
                }),
            );
            self.notify(
                "ssl.expiring",
                format!("Certificate for {} has expired", report.domain),
                format!(
                    "The configured certificate for {} expired and is not being served; the domain falls back to the self-signed certificate. Reasons: {reasons}",
                    report.domain
                ),
            );
        } else {
            self.emit(
                cert_events::SSL_RENEW_FAILED,
                json!({
                    "domain": &report.domain,
                    "error": &reasons,
                    "stage": "config-validation",
                }),
            );
            self.notify(
                "ssl.renew_failed",
                format!("Certificate for {} is invalid", report.domain),
                format!(
                    "The configured certificate for {} could not be loaded and is not being served; the domain falls back to the self-signed certificate. Reasons: {reasons}",
                    report.domain
                ),
            );
        }
    }

    fn upsert_record_from_validation(
        &self,
        report: &CertificateValidation,
        provider: &str,
        source: &str,
    ) {
        let domain = normalize_domain(&report.domain);
        let now = now();
        let (status, issuer, sans, fingerprint, not_before, expires_at, days_remaining, self_signed) =
            match &report.info {
                Some(info) => {
                    let status = if report.valid {
                        info.status.clone()
                    } else {
                        "invalid".to_string()
                    };
                    (
                        status,
                        info.issuer.clone(),
                        info.sans.clone(),
                        info.fingerprint_sha256.clone(),
                        DateTime::parse_from_rfc3339(&info.not_before)
                            .ok()
                            .map(|parsed| parsed.with_timezone(&Utc)),
                        DateTime::parse_from_rfc3339(&info.not_after)
                            .ok()
                            .map(|parsed| parsed.with_timezone(&Utc)),
                        Some(info.days_remaining),
                        info.self_signed,
                    )
                }
                None => (
                    "invalid".to_string(),
                    String::new(),
                    Vec::new(),
                    String::new(),
                    None,
                    None,
                    None,
                    false,
                ),
            };
        let record = CertificateRecord {
            id: self
                .records
                .get(&domain)
                .map(|entry| entry.value().id.clone())
                .unwrap_or_else(new_id),
            domain: domain.clone(),
            provider: provider.to_string(),
            status,
            issuer,
            sans,
            fingerprint_sha256: fingerprint,
            not_before,
            expires_at,
            days_remaining,
            self_signed,
            wildcard: domain.starts_with("*."),
            source: source.to_string(),
            revoked: false,
            last_renewed_at: self
                .records
                .get(&domain)
                .and_then(|entry| entry.value().last_renewed_at),
            last_error: if report.valid {
                None
            } else {
                Some(report.reasons.join("; "))
            },
            created_at: self
                .records
                .get(&domain)
                .map(|entry| entry.value().created_at)
                .unwrap_or(now),
            updated_at: now,
        };
        self.records.insert(domain, record);
    }

    fn upsert_record_from_info(&self, info: &CertificateInfo, provider: &str) {
        let domain = normalize_domain(&info.domain);
        let now = now();
        let record = CertificateRecord {
            id: self
                .records
                .get(&domain)
                .map(|entry| entry.value().id.clone())
                .unwrap_or_else(new_id),
            domain: domain.clone(),
            provider: provider.to_string(),
            status: info.status.clone(),
            issuer: info.issuer.clone(),
            sans: info.sans.clone(),
            fingerprint_sha256: info.fingerprint_sha256.clone(),
            not_before: DateTime::parse_from_rfc3339(&info.not_before)
                .ok()
                .map(|parsed| parsed.with_timezone(&Utc)),
            expires_at: DateTime::parse_from_rfc3339(&info.not_after)
                .ok()
                .map(|parsed| parsed.with_timezone(&Utc)),
            days_remaining: Some(info.days_remaining),
            self_signed: info.self_signed,
            wildcard: domain.starts_with("*."),
            source: info.source.clone(),
            revoked: self
                .records
                .get(&domain)
                .is_some_and(|entry| entry.value().revoked),
            last_renewed_at: self
                .records
                .get(&domain)
                .and_then(|entry| entry.value().last_renewed_at),
            last_error: self
                .records
                .get(&domain)
                .and_then(|entry| entry.value().last_error.clone()),
            created_at: self
                .records
                .get(&domain)
                .map(|entry| entry.value().created_at)
                .unwrap_or(now),
            updated_at: now,
        };
        self.records.insert(domain, record);
    }

    /// Run the expiry check over every known certificate: for each
    /// configured alert threshold (30/14/7/1 days by default) that a
    /// certificate has newly crossed, emit `ssl.expiring` and a
    /// notification; expired certificates alert once as well. Returns
    /// a summary of what was checked.
    pub fn check_expiry(&self) -> Value {
        let settings = self.settings();
        let mut checked = 0u64;
        let mut expiring: Vec<Value> = Vec::new();
        let mut expired: Vec<Value> = Vec::new();
        // Refresh live data from the storage first, so the check sees
        // renewals that arrived through the server ACME listener.
        if let Some(storage) = self.storage() {
            for info in storage.list_certificates() {
                if info.source != "fallback" {
                    self.upsert_record_from_info(&info, "acme");
                }
            }
        }
        let domains: Vec<String> = self
            .records
            .iter()
            .map(|entry| entry.key().clone())
            .collect();
        for domain in domains {
            let Some(record) = self.records.get(&domain).map(|entry| entry.value().clone())
            else {
                continue;
            };
            if record.revoked || record.status == "invalid" {
                continue;
            }
            let Some(days) = record.days_remaining else {
                continue;
            };
            checked += 1;
            let mut alerted = self
                .alerted_thresholds
                .get(&domain)
                .map(|entry| entry.value().clone())
                .unwrap_or_default();
            let mut crossed: Vec<i64> = Vec::new();
            if days < 0 {
                if !alerted.contains(&-1) {
                    crossed.push(-1);
                }
                expired.push(json!({
                    "domain": &domain,
                    "days_remaining": days,
                    "expires_at": record.expires_at,
                }));
            } else {
                for threshold in &settings.acme.alert_thresholds_days {
                    if days <= *threshold && !alerted.contains(threshold) {
                        crossed.push(*threshold);
                    }
                }
                if !crossed.is_empty() {
                    expiring.push(json!({
                        "domain": &domain,
                        "days_remaining": days,
                        "expires_at": record.expires_at,
                        "threshold_days": &crossed,
                    }));
                }
            }
            for threshold in &crossed {
                alerted.push(*threshold);
                let expired_flag = *threshold == -1;
                self.emit(
                    cert_events::SSL_EXPIRING,
                    json!({
                        "domain": &domain,
                        "days_remaining": days,
                        "threshold_days": threshold,
                        "expired": expired_flag,
                    }),
                );
                if expired_flag {
                    self.notify(
                        "ssl.expiring",
                        format!("Certificate for {domain} has expired"),
                        format!(
                            "The certificate for {domain} expired {} day(s) ago; the domain falls back to the self-signed certificate until it is renewed.",
                            -days
                        ),
                    );
                } else {
                    self.notify(
                        "ssl.expiring",
                        format!("Certificate for {domain} expires in {days} day(s)"),
                        format!(
                            "The certificate for {domain} expires on {} (in {days} day(s)); renewal is due within the configured window.",
                            record
                                .expires_at
                                .map(|expiry| expiry.to_rfc3339())
                                .unwrap_or_default()
                        ),
                    );
                }
            }
            if !crossed.is_empty() {
                self.alerted_thresholds.insert(domain, alerted);
            }
        }
        json!({
            "checked": checked,
            "expiring": expiring,
            "expired": expired,
        })
    }

    /// Renew one domain through ACME, recording the attempt in the
    /// renewal history. On success the record is updated and both
    /// `ssl.renewed` and the core `ssl.swap` completion event are
    /// emitted (the Domains module listens to the latter); on failure
    /// `ssl.renew_failed` is emitted and a notification is requested.
    pub async fn renew_domain(
        state: &Arc<Self>,
        domain: &str,
        force: bool,
    ) -> UnifiedResult<RenewalAttempt> {
        let domain = normalize_domain(domain);
        let settings = state.settings();
        if !settings.enabled {
            return Err(UnifiedError::unprocessable(
                "ACME is disabled (acme.enabled is false); enable it in the configuration to renew certificates",
            ));
        }
        let storage = state.storage().ok_or_else(|| {
            UnifiedError::internal("certificate storage is not initialized")
        })?;
        let provider = settings.acme.resolve_provider()?;
        let provider_name = settings.acme.provider.clone();
        let attempt = RenewalAttempt {
            id: new_id(),
            domain: domain.clone(),
            provider: provider_name.clone(),
            status: "started".to_string(),
            started_at: now(),
            finished_at: None,
            error: None,
            new_expires_at: None,
            forced: force,
        };
        state.history.insert(attempt.id.clone(), attempt.clone());
        let outcome: UnifiedResult<bool> = async {
            let client = AcmeClient::new_with_provider(
                &provider,
                settings.acme.staging,
                &settings.acme.contacts,
                settings.acme.eab(),
            )
            .await?;
            let key_type = settings.acme.key_type;
            let curve = settings.acme.curve.clone();
            if force {
                client
                    .renew_certificate_with_key_type(&storage, &domain, key_type, &curve)
                    .await?;
                Ok(true)
            } else {
                client
                    .renew_if_expiring_with_key_type(
                        &storage,
                        &domain,
                        settings.acme.renew_days_before_expiry,
                        key_type,
                        &curve,
                    )
                    .await
            }
        }
        .await;
        let mut attempt = attempt;
        attempt.finished_at = Some(now());
        match outcome {
            Ok(true) => {
                let new_expires_at = storage.cert_expiry(&domain);
                attempt.status = "succeeded".to_string();
                attempt.new_expires_at = new_expires_at;
                state.history.insert(attempt.id.clone(), attempt.clone());
                if let Some(info) = storage.certificate_info(&domain) {
                    state.upsert_record_from_info(&info, &provider_name);
                }
                if let Some(mut record) = state.records.get_mut(&domain) {
                    record.last_renewed_at = Some(now());
                    record.last_error = None;
                    record.updated_at = now();
                }
                // Clear the alerted thresholds: the new certificate
                // starts a fresh expiry countdown.
                state.alerted_thresholds.remove(&domain);
                let expires_at = new_expires_at.map(|expiry| expiry.to_rfc3339());
                state.emit(
                    cert_events::SSL_RENEWED,
                    json!({
                        "domain": &domain,
                        "provider": &provider_name,
                        "ssl_expires_at": &expires_at,
                        "completed": true,
                    }),
                );
                // Core completion event, so the Domains module updates
                // its SSL status. The server ACME listener ignores
                // completion payloads, so this cannot re-trigger a
                // renewal.
                state.emit(
                    events::SSL_SWAP,
                    json!({
                        "domain": &domain,
                        "ssl_expires_at": &expires_at,
                        "renewed": true,
                    }),
                );
                Ok(attempt)
            }
            Ok(false) => {
                attempt.status = "skipped".to_string();
                state.history.insert(attempt.id.clone(), attempt.clone());
                Ok(attempt)
            }
            Err(error) => {
                attempt.status = "failed".to_string();
                attempt.error = Some(error.message.clone());
                state.history.insert(attempt.id.clone(), attempt.clone());
                if let Some(mut record) = state.records.get_mut(&domain) {
                    record.last_error = Some(error.message.clone());
                    record.updated_at = now();
                }
                state.emit(
                    cert_events::SSL_RENEW_FAILED,
                    json!({
                        "domain": &domain,
                        "provider": &provider_name,
                        "error": &error.message,
                    }),
                );
                state.notify(
                    "ssl.renew_failed",
                    format!("Certificate renewal failed for {domain}"),
                    format!("Renewal of the certificate for {domain} via {provider_name} failed: {}", error.message),
                );
                Err(error)
            }
        }
    }

    /// Renew every certificate that is due (or every certificate when
    /// `force` is true). Each domain is attempted independently: one
    /// failure never stops the others.
    pub async fn renew_all(
        state: &Arc<Self>,
        force: bool,
    ) -> Vec<RenewalAttempt> {
        let domains: Vec<String> = state
            .records
            .iter()
            .filter(|entry| !entry.value().revoked)
            .map(|entry| entry.key().clone())
            .collect();
        let mut attempts = Vec::new();
        for domain in domains {
            match Self::renew_domain(state, &domain, force).await {
                Ok(attempt) => attempts.push(attempt),
                Err(error) => attempts.push(RenewalAttempt {
                    id: new_id(),
                    domain,
                    provider: state.settings().acme.provider.clone(),
                    status: "failed".to_string(),
                    started_at: now(),
                    finished_at: Some(now()),
                    error: Some(error.message),
                    new_expires_at: None,
                    forced: force,
                }),
            }
        }
        attempts
    }
}

// ---------------------------------------------------------------------------
// Certificate modes and Cloudflare detection (state methods).
// ---------------------------------------------------------------------------

impl CertificatesState {
    fn mode_definitions(&self) -> Vec<CertModeDefinition> {
        self.mode_definitions
            .read()
            .expect("mode definitions lock poisoned")
            .clone()
    }

    fn mode_definition(&self, mode: &str) -> Option<CertModeDefinition> {
        self.mode_definitions()
            .into_iter()
            .find(|definition| definition.id == mode)
    }

    fn cf_ranges_snapshot(&self) -> CfIpRanges {
        self.cf_ranges
            .read()
            .expect("cloudflare ranges lock poisoned")
            .clone()
    }

    /// Record a mode change: the previous row moves into the per-domain
    /// history (capped at 20 entries) and the new row becomes current.
    fn record_cert_mode(&self, mode: DomainCertMode) {
        if let Some(previous) = self
            .cert_modes
            .get(&mode.domain)
            .map(|entry| entry.value().clone())
        {
            let mut history = self
                .mode_history
                .get(&mode.domain)
                .map(|entry| entry.value().clone())
                .unwrap_or_default();
            history.insert(0, previous);
            history.truncate(20);
            self.mode_history.insert(mode.domain.clone(), history);
        }
        self.cert_modes.insert(mode.domain.clone(), mode);
    }

    fn mode_history_for(&self, domain: &str) -> Vec<DomainCertMode> {
        self.mode_history
            .get(domain)
            .map(|entry| entry.value().clone())
            .unwrap_or_default()
    }

    /// Fetch the published Cloudflare IP-range lists and store them as
    /// data with the fetch date. The configured source URLs are the
    /// only fixed values; the ranges themselves are never baked into
    /// the code. On failure the previously stored snapshot is kept
    /// untouched and the error is recorded on it.
    pub async fn refresh_cf_ranges(state: &Arc<Self>) -> UnifiedResult<CfIpRanges> {
        let settings = state.settings();
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(10))
            .user_agent("Unified-CF-Ranges/1.0")
            .build()
            .map_err(|error| {
                UnifiedError::internal(format!("cannot build HTTP client: {error}"))
            })?;
        let fetch_list = async |url: &str| -> UnifiedResult<Vec<String>> {
            let text = client
                .get(url)
                .send()
                .await
                .map_err(|error| {
                    UnifiedError::new(
                        format!("cannot fetch Cloudflare IP range list from {url}: {error}"),
                        502,
                    )
                })?
                .error_for_status()
                .map_err(|error| {
                    UnifiedError::new(
                        format!("Cloudflare IP range list at {url} returned an error: {error}"),
                        502,
                    )
                })?
                .text()
                .await
                .map_err(|error| {
                    UnifiedError::new(
                        format!("cannot read Cloudflare IP range list from {url}: {error}"),
                        502,
                    )
                })?;
            let mut ranges: Vec<String> = Vec::new();
            for line in text.lines() {
                let line = line.trim();
                if line.is_empty() || line.starts_with('#') {
                    continue;
                }
                if parse_cidr(line).is_none() {
                    return Err(UnifiedError::new(
                        format!("Cloudflare IP range list at {url} contains an invalid entry: \"{line}\""),
                        502,
                    ));
                }
                ranges.push(line.to_string());
            }
            if ranges.is_empty() {
                return Err(UnifiedError::new(
                    format!("Cloudflare IP range list at {url} is empty"),
                    502,
                ));
            }
            Ok(ranges)
        };
        let v4 = fetch_list(&settings.cf_ranges_v4_url).await;
        let v6 = fetch_list(&settings.cf_ranges_v6_url).await;
        let (v4, v6) = match (v4, v6) {
            (Ok(v4), Ok(v6)) => (v4, v6),
            (Err(error), _) | (_, Err(error)) => {
                let mut snapshot = state.cf_ranges_snapshot();
                snapshot.fetch_error = Some(error.message.clone());
                *state
                    .cf_ranges
                    .write()
                    .expect("cloudflare ranges lock poisoned") = snapshot;
                return Err(error);
            }
        };
        let snapshot = CfIpRanges {
            v4,
            v6,
            fetched_at: Some(now()),
            source_v4: settings.cf_ranges_v4_url.clone(),
            source_v6: settings.cf_ranges_v6_url.clone(),
            fetch_error: None,
        };
        *state
            .cf_ranges
            .write()
            .expect("cloudflare ranges lock poisoned") = snapshot.clone();
        Ok(snapshot)
    }

    /// Detect Cloudflare usage for `domain` and store the result.
    /// Three methods, each individually enable/disable-able through
    /// settings, each recorded honestly:
    ///
    /// (a) DNS: resolve A/AAAA records and compare them against the
    /// stored Cloudflare IP ranges (fetched live on first use; when
    /// the ranges cannot be fetched the method reports
    /// `ranges_unavailable` instead of guessing).
    /// (b) Live probe: request the domain over HTTPS and look for
    /// `CF-Ray`, `CF-Cache-Status`, and `Server: cloudflare`.
    /// (c) API check: ask the Cloudflare module through the
    /// `cloudflare.zone.lookup` hook whether the domain's zone exists
    /// there and which SSL mode it uses; no answer means "unknown".
    ///
    /// When Cloudflare is detected the result records the origin
    /// expectation, and when the SSL mode is Flexible (or unconfirmed)
    /// it records an explicit warning: end-to-end encryption is NOT
    /// present between Cloudflare and the origin in Flexible mode.
    pub async fn detect_cloudflare(state: &Arc<Self>, domain: &str) -> CloudflareDetection {
        let domain = normalize_domain(domain);
        let settings = state.settings();
        let mut methods = serde_json::Map::new();

        // (a) DNS check against the stored Cloudflare IP ranges.
        if !settings.cf_detect_dns {
            methods.insert(
                "dns".to_string(),
                json!({ "enabled": false, "result": "disabled" }),
            );
        } else {
            let verifier = DnsVerifier::new();
            match verifier.lookup_ip(&domain).await {
                Ok(addresses) => {
                    let mut snapshot = state.cf_ranges_snapshot();
                    let ranges_available = !snapshot.v4.is_empty() || !snapshot.v6.is_empty();
                    if !ranges_available {
                        // First use (or an expired snapshot): fetch the
                        // published lists now and store them.
                        match Self::refresh_cf_ranges(state).await {
                            Ok(fetched) => snapshot = fetched,
                            Err(error) => {
                                methods.insert(
                                    "dns".to_string(),
                                    json!({
                                        "enabled": true,
                                        "checked": true,
                                        "addresses": addresses.iter().map(|ip| ip.to_string()).collect::<Vec<_>>(),
                                        "result": "ranges_unavailable",
                                        "error": error.message,
                                    }),
                                );
                            }
                        }
                    }
                    if !methods.contains_key("dns") {
                        let mut matched: Vec<String> = Vec::new();
                        for address in &addresses {
                            let in_ranges = snapshot
                                .v4
                                .iter()
                                .chain(snapshot.v6.iter())
                                .any(|range| cidr_contains(range, address));
                            if in_ranges {
                                matched.push(address.to_string());
                            }
                        }
                        methods.insert(
                            "dns".to_string(),
                            json!({
                                "enabled": true,
                                "checked": true,
                                "addresses": addresses.iter().map(|ip| ip.to_string()).collect::<Vec<_>>(),
                                "matched_addresses": matched,
                                "detected": !matched.is_empty(),
                                "ranges_fetched_at": snapshot.fetched_at.map(|at| at.to_rfc3339()),
                                "ranges_source_v4": snapshot.source_v4,
                                "ranges_source_v6": snapshot.source_v6,
                            }),
                        );
                    }
                }
                Err(error) => {
                    methods.insert(
                        "dns".to_string(),
                        json!({
                            "enabled": true,
                            "checked": false,
                            "result": "lookup_failed",
                            "error": error.message,
                        }),
                    );
                }
            }
        }

        // (b) Live HTTPS probe for Cloudflare response headers.
        if !settings.cf_detect_probe {
            methods.insert(
                "probe".to_string(),
                json!({ "enabled": false, "result": "disabled" }),
            );
        } else {
            let client = reqwest::Client::builder()
                .timeout(Duration::from_secs(10))
                .user_agent("Unified-CF-Detect/1.0")
                .build();
            match client {
                Ok(client) => {
                    match client
                        .get(format!("https://{domain}/"))
                        .send()
                        .await
                    {
                        Ok(response) => {
                            let headers = response.headers();
                            let mut found: Vec<String> = Vec::new();
                            if headers.contains_key("cf-ray") {
                                found.push("cf-ray".to_string());
                            }
                            if headers.contains_key("cf-cache-status") {
                                found.push("cf-cache-status".to_string());
                            }
                            if headers
                                .get("server")
                                .and_then(|value| value.to_str().ok())
                                .is_some_and(|value| value.eq_ignore_ascii_case("cloudflare"))
                            {
                                found.push("server: cloudflare".to_string());
                            }
                            let status = response.status().as_u16();
                            methods.insert(
                                "probe".to_string(),
                                json!({
                                    "enabled": true,
                                    "checked": true,
                                    "http_status": status,
                                    "headers_found": found,
                                    "detected": !found.is_empty(),
                                }),
                            );
                        }
                        Err(error) => {
                            methods.insert(
                                "probe".to_string(),
                                json!({
                                    "enabled": true,
                                    "checked": false,
                                    "result": "probe_failed",
                                    "error": error.to_string(),
                                }),
                            );
                        }
                    }
                }
                Err(error) => {
                    methods.insert(
                        "probe".to_string(),
                        json!({
                            "enabled": true,
                            "checked": false,
                            "result": "probe_failed",
                            "error": format!("cannot build HTTP client: {error}"),
                        }),
                    );
                }
            }
        }

        // (c) Cloudflare API check through the hook bus.
        let mut ssl_mode: Option<String> = None;
        if !settings.cf_detect_api {
            methods.insert(
                "api".to_string(),
                json!({ "enabled": false, "result": "disabled" }),
            );
        } else {
            let context = state.bus().emit(
                cert_events::CLOUDFLARE_ZONE_LOOKUP,
                json!({ "domain": &domain, "found": false, "zone_id": null, "ssl_mode": null }),
            );
            let found = context
                .get("found")
                .and_then(|value| value.as_bool())
                .unwrap_or(false);
            let zone_id = context
                .get("zone_id")
                .and_then(|value| value.as_str())
                .map(str::to_string);
            ssl_mode = context
                .get("ssl_mode")
                .and_then(|value| value.as_str())
                .map(str::to_string);
            let api_note: Value = if context.results.is_empty() {
                json!("no cloudflare module handler answered the zone lookup; API result is unknown")
            } else {
                Value::Null
            };
            methods.insert(
                "api".to_string(),
                json!({
                    "enabled": true,
                    "checked": true,
                    "handler_answered": !context.results.is_empty(),
                    "found": found,
                    "zone_id": zone_id,
                    "ssl_mode": ssl_mode,
                    "detected": found,
                    "note": api_note,
                }),
            );
        }

        let dns_detected = methods
            .get("dns")
            .and_then(|value| value.get("detected"))
            .and_then(|value| value.as_bool())
            .unwrap_or(false);
        let probe_detected = methods
            .get("probe")
            .and_then(|value| value.get("detected"))
            .and_then(|value| value.as_bool())
            .unwrap_or(false);
        let api_detected = methods
            .get("api")
            .and_then(|value| value.get("detected"))
            .and_then(|value| value.as_bool())
            .unwrap_or(false);
        let detected = dns_detected || probe_detected || api_detected;
        let end_to_end_encrypted = match ssl_mode.as_deref() {
            Some("flexible") => Some(false),
            Some("full") | Some("full_strict") => Some(true),
            _ => None,
        };
        let flexible_warning = if ssl_mode.as_deref() == Some("flexible") {
            Some(
                "Cloudflare SSL mode is Flexible: traffic between Cloudflare and the origin is NOT encrypted end to end. Switch the zone to Full (Strict) with a valid origin certificate before claiming end-to-end encryption."
                    .to_string(),
            )
        } else if detected && ssl_mode.is_none() {
            Some(
                "Cloudflare was detected but the Cloudflare SSL mode could not be confirmed: end-to-end encryption is UNCONFIRMED. Confirm Full (Strict) mode in the Cloudflare dashboard; Flexible mode does NOT encrypt traffic between Cloudflare and the origin."
                    .to_string(),
            )
        } else {
            None
        };
        let detection = CloudflareDetection {
            domain: domain.clone(),
            detected,
            methods: Value::Object(methods),
            ssl_mode,
            end_to_end_encrypted,
            flexible_warning,
            checked_at: now(),
        };
        state.detections.insert(domain.clone(), detection.clone());
        state.emit(
            cert_events::SSL_CF_DETECTED,
            json!({
                "domain": &domain,
                "detected": detected,
                "ssl_mode": &detection.ssl_mode,
                "end_to_end_encrypted": &detection.end_to_end_encrypted,
                "checked_at": detection.checked_at.to_rfc3339(),
            }),
        );
        detection
    }
}

impl Default for CertificatesState {
    fn default() -> Self {
        Self::new()
    }
}

/// Spawn a future on the current runtime when there is one, otherwise
/// on a dedicated thread with its own runtime. Hook handlers are
/// synchronous, so renewal work (which needs async I/O) is always
/// spawned this way.
fn spawn_task<F>(future: F)
where
    F: std::future::Future<Output = ()> + Send + 'static,
{
    if tokio::runtime::Handle::try_current().is_ok() {
        tokio::spawn(future);
    } else {
        std::thread::spawn(move || {
            let Ok(runtime) = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            else {
                return;
            };
            runtime.block_on(future);
        });
    }
}

fn paginate<T: Serialize + Clone>(items: Vec<T>, params: &PaginationParams) -> PageResult<T> {
    let pagination = params.normalized();
    let total = items.len() as u64;
    let start = ((pagination.page - 1) * pagination.per_page) as usize;
    let page_items: Vec<T> = items
        .into_iter()
        .skip(start)
        .take(pagination.per_page as usize)
        .collect();
    PageResult::new(page_items, total, &pagination)
}

fn matches_search(haystack: &str, params: &PaginationParams) -> bool {
    match &params.search {
        Some(needle) if !needle.trim().is_empty() => haystack
            .to_lowercase()
            .contains(&needle.trim().to_lowercase()),
        _ => true,
    }
}

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

async fn list_certificates(
    state: web::Data<Arc<CertificatesState>>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let params = query.into_inner();
    // Refresh live data from the storage so renewals that arrived
    // through the server ACME listener show up here.
    if let Some(storage) = state.storage() {
        for info in storage.list_certificates() {
            if info.source != "fallback" {
                state.upsert_record_from_info(&info, "acme");
            }
        }
    }
    let mut items: Vec<CertificateRecord> = state
        .records
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|record| {
            matches_search(&format!("{} {} {}", record.domain, record.issuer, record.status), &params)
                && params
                    .filters
                    .get("status")
                    .is_none_or(|status| record.status == *status)
                && params
                    .filters
                    .get("source")
                    .is_none_or(|source| record.source == *source)
                && params
                    .filters
                    .get("provider")
                    .is_none_or(|provider| record.provider == *provider)
                && params.filters.get("revoked").is_none_or(|flag| {
                    record.revoked == (*flag == "true" || *flag == "1")
                })
        })
        .collect();
    let sort = params.allowed_sort(&["domain", "expires_at", "status"], "domain");
    match sort.as_str() {
        "expires_at" => items.sort_by(|a, b| a.expires_at.cmp(&b.expires_at)),
        "status" => items.sort_by(|a, b| a.status.cmp(&b.status)),
        _ => items.sort_by(|a, b| a.domain.cmp(&b.domain)),
    }
    if params.normalized().order == "desc" {
        items.reverse();
    }
    let page = paginate(items, &params);
    json::success_response(page, 200)
}

async fn get_config(state: web::Data<Arc<CertificatesState>>) -> HttpResponse {
    let settings = state.settings();
    let validation = match state.storage() {
        Some(_) => {
            // Re-validate the configured files live, so the dashboard
            // sees a changed file without a restart.
            let paths = state
                .cert_paths
                .read()
                .expect("paths lock poisoned")
                .clone();
            let mut reports: Vec<CertificateValidation> = Vec::new();
            let mut domains: Vec<String> = paths.keys().cloned().collect();
            domains.sort();
            for domain in domains {
                let (cert_path, key_path) = &paths[&domain];
                reports.push(validate_config_entry(&domain, cert_path, key_path));
            }
            reports
        }
        None => Vec::new(),
    };
    json::success_response(
        json!({
            "acme": settings.acme.redacted(),
            "acme_enabled": settings.enabled,
            "check_interval_secs": settings.check_interval_secs,
            "providers": provider_matrix(),
            "client_supported_challenges": ["http-01"],
            "validation": validation,
        }),
        200,
    )
}

async fn validate_all(state: web::Data<Arc<CertificatesState>>) -> HttpResponse {
    let paths = state
        .cert_paths
        .read()
        .expect("paths lock poisoned")
        .clone();
    let mut reports: Vec<CertificateValidation> = Vec::new();
    let mut domains: Vec<String> = paths.keys().cloned().collect();
    domains.sort();
    for domain in domains {
        let (cert_path, key_path) = &paths[&domain];
        reports.push(validate_config_entry(&domain, cert_path, key_path));
    }
    // Include live-only certificates (renewed or fallback) that have
    // no configured file pair, validated from their live data.
    if let Some(storage) = state.storage() {
        for info in storage.list_certificates() {
            if paths.contains_key(&info.domain) {
                continue;
            }
            let mut reasons: Vec<String> = Vec::new();
            if info.days_remaining < 0 {
                reasons.push(format!("certificate expired on {}", info.not_after));
            }
            if info.self_signed {
                reasons.push(
                    "self-signed certificate (not issued by a public authority)".to_string(),
                );
            }
            if !info.chain_complete {
                reasons.push(
                    "chain is incomplete: the leaf certificate has no intermediate certificate bundled"
                        .to_string(),
                );
            }
            reports.push(CertificateValidation {
                domain: info.domain.clone(),
                valid: reasons.is_empty(),
                reasons,
                info: Some(info),
            });
        }
    }
    reports.sort_by(|a, b| a.domain.cmp(&b.domain));
    json::success_response(json!({ "certificates": reports }), 200)
}

fn history_for(state: &CertificatesState, domain: &str, limit: usize) -> Vec<RenewalAttempt> {
    let mut items: Vec<RenewalAttempt> = state
        .history
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|attempt| attempt.domain == domain)
        .collect();
    items.sort_by(|a, b| b.started_at.cmp(&a.started_at));
    items.truncate(limit);
    items
}

async fn get_certificate(
    state: web::Data<Arc<CertificatesState>>,
    domain: web::Path<String>,
) -> HttpResponse {
    let domain = normalize_domain(&domain.into_inner());
    let result: UnifiedResult<Value> = (|| {
        let live = state
            .storage()
            .and_then(|storage| storage.certificate_info(&domain));
        if let Some(info) = &live {
            state.upsert_record_from_info(info, "acme");
        }
        let record = state
            .records
            .get(&domain)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| {
                UnifiedError::not_found(format!("no certificate known for {domain}"))
            })?;
        let previous = state
            .storage()
            .and_then(|storage| storage.previous_info(&domain));
        Ok(json!({
            "certificate": record,
            "live": live,
            "previous": previous,
            "history": history_for(&state, &domain, 10),
        }))
    })();
    json::respond(result, false)
}

async fn validate_certificate(
    state: web::Data<Arc<CertificatesState>>,
    domain: web::Path<String>,
) -> HttpResponse {
    let domain = normalize_domain(&domain.into_inner());
    let result: UnifiedResult<CertificateValidation> = (|| {
        // Prefer the configured files: only they carry the private
        // key, so only they prove key-match and chain completeness.
        if let Some((cert_path, key_path)) = state
            .cert_paths
            .read()
            .expect("paths lock poisoned")
            .get(&domain)
            .cloned()
        {
            return Ok(validate_config_entry(&domain, &cert_path, &key_path));
        }
        let info = state
            .storage()
            .and_then(|storage| storage.certificate_info(&domain))
            .ok_or_else(|| {
                UnifiedError::not_found(format!("no certificate known for {domain}"))
            })?;
        let mut reasons: Vec<String> = Vec::new();
        if info.days_remaining < 0 {
            reasons.push(format!("certificate expired on {}", info.not_after));
        }
        if info.self_signed {
            reasons.push(
                "self-signed certificate (not issued by a public authority)".to_string(),
            );
        }
        if !info.chain_complete {
            reasons.push(
                "chain is incomplete: the leaf certificate has no intermediate certificate bundled"
                    .to_string(),
            );
        }
        let valid = reasons.is_empty();
        Ok(CertificateValidation {
            domain,
            valid,
            reasons,
            info: Some(info),
        })
    })();
    json::respond(result, false)
}

async fn renew_certificate_handler(
    state: web::Data<Arc<CertificatesState>>,
    domain: web::Path<String>,
    body: Option<web::Json<RenewRequest>>,
) -> HttpResponse {
    let domain = normalize_domain(&domain.into_inner());
    let force = body.map(|body| body.force).unwrap_or(false);
    let state = state.into_inner();
    let result = CertificatesState::renew_domain(&state, &domain, force).await;
    json::respond(result, false)
}

async fn force_renew_certificate(
    state: web::Data<Arc<CertificatesState>>,
    domain: web::Path<String>,
) -> HttpResponse {
    let domain = normalize_domain(&domain.into_inner());
    let state = state.into_inner();
    let result = CertificatesState::renew_domain(&state, &domain, true).await;
    json::respond(result, false)
}

async fn renew_all_handler(
    state: web::Data<Arc<CertificatesState>>,
    body: Option<web::Json<RenewRequest>>,
) -> HttpResponse {
    let force = body.map(|body| body.force).unwrap_or(false);
    let state = state.into_inner();
    let attempts = CertificatesState::renew_all(&state, force).await;
    let failed = attempts
        .iter()
        .filter(|attempt| attempt.status == "failed")
        .count();
    json::success_response(
        json!({
            "attempts": attempts,
            "failed": failed,
        }),
        200,
    )
}

async fn revoke_certificate_handler(
    state: web::Data<Arc<CertificatesState>>,
    domain: web::Path<String>,
    body: Option<web::Json<RevokeRequest>>,
) -> HttpResponse {
    let domain = normalize_domain(&domain.into_inner());
    let reason = body
        .and_then(|body| body.reason.clone())
        .unwrap_or_else(|| "revoked by an administrator".to_string());
    let result: UnifiedResult<Value> = (|| {
        let storage = state.storage().ok_or_else(|| {
            UnifiedError::internal("certificate storage is not initialized")
        })?;
        let info = storage.revoke_live(&domain)?;
        if let Some(mut record) = state.records.get_mut(&domain) {
            record.revoked = true;
            record.status = "revoked".to_string();
            record.last_error = Some(reason.clone());
            record.updated_at = now();
        }
        state.emit(
            cert_events::SSL_REVOKED,
            json!({
                "domain": &domain,
                "reason": &reason,
                "fingerprint_sha256": &info.fingerprint_sha256,
            }),
        );
        state.notify(
            "ssl.revoked",
            format!("Certificate revoked for {domain}"),
            format!("The certificate for {domain} was revoked ({reason}); the domain falls back to the self-signed certificate."),
        );
        Ok(json!({
            "domain": domain,
            "revoked": true,
            "reason": reason,
            "fingerprint_sha256": info.fingerprint_sha256,
        }))
    })();
    json::respond(result, false)
}

async fn swap_certificate_handler(
    state: web::Data<Arc<CertificatesState>>,
    domain: web::Path<String>,
    body: web::Json<SwapRequest>,
) -> HttpResponse {
    let domain = normalize_domain(&domain.into_inner());
    let result: UnifiedResult<Value> = (|| {
        let storage = state.storage().ok_or_else(|| {
            UnifiedError::internal("certificate storage is not initialized")
        })?;
        // Validate before anything goes live: a bad replacement must
        // never replace a working certificate.
        let validation = validate_pem_pair(
            body.cert_pem.as_bytes(),
            body.key_pem.as_bytes(),
            Some(&domain),
        );
        if !validation.valid {
            return Err(UnifiedError::bad_request(format!(
                "new certificate failed validation: {}",
                validation.reasons.join("; ")
            ))
            .with_metadata("reasons", json!(validation.reasons)));
        }
        let new_fingerprint = validation
            .info
            .as_ref()
            .map(|info| info.fingerprint_sha256.clone())
            .unwrap_or_default();
        let previous_fingerprint = storage
            .certificate_info(&domain)
            .map(|info| info.fingerprint_sha256);
        storage.swap_certificate(&domain, body.cert_pem.as_bytes(), body.key_pem.as_bytes())?;
        // Post-swap verification: the live certificate must be the
        // one just installed. On any mismatch, roll back to the
        // previous certificate instead of serving the wrong one.
        let live = storage.certificate_info(&domain);
        let verified = live
            .as_ref()
            .is_some_and(|info| info.fingerprint_sha256 == new_fingerprint);
        if !verified {
            let _ = storage.rollback_certificate(&domain);
            return Err(UnifiedError::internal(
                "certificate swap verification failed: the live certificate is not the new one; rolled back to the previous certificate",
            ));
        }
        let info = live.expect("live certificate must exist after a verified swap");
        state.upsert_record_from_info(&info, "custom");
        state.emit(
            cert_events::SSL_SWAPPED,
            json!({
                "domain": &domain,
                "ssl_expires_at": &info.not_after,
                "fingerprint_sha256": &info.fingerprint_sha256,
                "previous_fingerprint_sha256": &previous_fingerprint,
                "rolled_back": false,
            }),
        );
        // Core completion event, so the Domains module updates its
        // SSL status.
        state.emit(
            events::SSL_SWAP,
            json!({
                "domain": &domain,
                "ssl_expires_at": &info.not_after,
            }),
        );
        Ok(json!({
            "domain": domain,
            "swapped": true,
            "certificate": info,
            "previous_fingerprint_sha256": previous_fingerprint,
        }))
    })();
    json::respond(result, false)
}

async fn rollback_certificate_handler(
    state: web::Data<Arc<CertificatesState>>,
    domain: web::Path<String>,
) -> HttpResponse {
    let domain = normalize_domain(&domain.into_inner());
    let result: UnifiedResult<Value> = (|| {
        let storage = state.storage().ok_or_else(|| {
            UnifiedError::internal("certificate storage is not initialized")
        })?;
        let previous_fingerprint = storage
            .previous_info(&domain)
            .map(|info| info.fingerprint_sha256);
        storage.rollback_certificate(&domain)?;
        let info = storage.certificate_info(&domain).ok_or_else(|| {
            UnifiedError::internal("no live certificate after rollback")
        })?;
        state.upsert_record_from_info(&info, "custom");
        state.emit(
            cert_events::SSL_SWAPPED,
            json!({
                "domain": &domain,
                "ssl_expires_at": &info.not_after,
                "fingerprint_sha256": &info.fingerprint_sha256,
                "previous_fingerprint_sha256": &previous_fingerprint,
                "rolled_back": true,
            }),
        );
        Ok(json!({
            "domain": domain,
            "rolled_back": true,
            "certificate": info,
        }))
    })();
    json::respond(result, false)
}

async fn install_certificate_handler(
    state: web::Data<Arc<CertificatesState>>,
    body: web::Json<InstallRequest>,
) -> HttpResponse {
    let domain = normalize_domain(&body.domain);
    let result: UnifiedResult<Value> = (|| {
        let storage = state.storage().ok_or_else(|| {
            UnifiedError::internal("certificate storage is not initialized")
        })?;
        // Validate before it goes live: key match, expiry, SAN
        // coverage, and chain completeness.
        let validation = validate_pem_pair(
            body.cert_pem.as_bytes(),
            body.key_pem.as_bytes(),
            Some(&domain),
        );
        if !validation.valid {
            return Err(UnifiedError::bad_request(format!(
                "certificate failed validation: {}",
                validation.reasons.join("; ")
            ))
            .with_metadata("reasons", json!(validation.reasons)));
        }
        storage.swap_certificate(&domain, body.cert_pem.as_bytes(), body.key_pem.as_bytes())?;
        let info = storage.certificate_info(&domain).ok_or_else(|| {
            UnifiedError::internal("certificate vanished after install")
        })?;
        state.upsert_record_from_info(&info, "custom");
        if let Some(mut record) = state.records.get_mut(&domain) {
            record.source = "custom".to_string();
            record.provider = "custom".to_string();
            record.revoked = false;
            record.updated_at = now();
        }
        state.emit(
            cert_events::SSL_INSTALLED,
            json!({
                "domain": &domain,
                "fingerprint_sha256": &info.fingerprint_sha256,
                "ssl_expires_at": &info.not_after,
                "source": "custom",
            }),
        );
        Ok(json!({
            "domain": domain,
            "installed": true,
            "certificate": info,
        }))
    })();
    json::respond(result, false)
}

async fn list_history(
    state: web::Data<Arc<CertificatesState>>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let params = query.into_inner();
    let mut items: Vec<RenewalAttempt> = state
        .history
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|attempt| {
            matches_search(&format!("{} {}", attempt.domain, attempt.status), &params)
                && params
                    .filters
                    .get("domain")
                    .is_none_or(|domain| attempt.domain == *domain)
                && params
                    .filters
                    .get("status")
                    .is_none_or(|status| attempt.status == *status)
                && params
                    .filters
                    .get("provider")
                    .is_none_or(|provider| attempt.provider == *provider)
        })
        .collect();
    items.sort_by(|a, b| b.started_at.cmp(&a.started_at));
    if params.normalized().order == "asc" {
        items.reverse();
    }
    let page = paginate(items, &params);
    json::success_response(page, 200)
}

async fn check_expiry_handler(state: web::Data<Arc<CertificatesState>>) -> HttpResponse {
    let summary = state.check_expiry();
    json::success_response(summary, 200)
}

async fn get_settings(state: web::Data<Arc<CertificatesState>>) -> HttpResponse {
    let settings = state.settings();
    json::success_response(
        json!({
            "enabled": settings.enabled,
            "acme": settings.acme.redacted(),
            "check_interval_secs": settings.check_interval_secs,
            "custom_min_validity_days": settings.custom_min_validity_days,
            "cloudflare_detection": {
                "dns": settings.cf_detect_dns,
                "probe": settings.cf_detect_probe,
                "api": settings.cf_detect_api,
            },
            "cloudflare_ranges_urls": {
                "v4": settings.cf_ranges_v4_url,
                "v6": settings.cf_ranges_v6_url,
            },
            "providers": provider_matrix(),
        }),
        200,
    )
}

async fn update_settings(
    state: web::Data<Arc<CertificatesState>>,
    body: web::Json<UpdateSettingsRequest>,
) -> HttpResponse {
    let result: UnifiedResult<Value> = (|| {
        let input = body.into_inner();
        let mut settings = state.settings();
        if let Some(enabled) = input.enabled {
            settings.enabled = enabled;
        }
        if let Some(provider) = input.provider {
            settings.acme.provider = provider;
        }
        if let Some(directory_url) = input.directory_url {
            settings.acme.directory_url = directory_url;
        }
        if let Some(staging) = input.staging {
            settings.acme.staging = staging;
        }
        if let Some(contacts) = input.contacts {
            settings.acme.contacts = contacts;
        }
        if let Some(eab_key_id) = input.eab_key_id {
            settings.acme.eab_key_id = eab_key_id;
        }
        if let Some(eab_hmac_key) = input.eab_hmac_key {
            settings.acme.eab_hmac_key = eab_hmac_key;
        }
        if let Some(days) = input.renew_days_before_expiry {
            settings.acme.renew_days_before_expiry = days;
        }
        if let Some(key_type) = input.key_type {
            settings.acme.key_type = key_type;
        }
        if let Some(key_size) = input.key_size {
            settings.acme.key_size = key_size;
        }
        if let Some(curve) = input.curve {
            settings.acme.curve = curve;
        }
        if let Some(challenge) = input.challenge {
            settings.acme.challenge = challenge;
        }
        if let Some(thresholds) = input.alert_thresholds_days {
            settings.acme.alert_thresholds_days = thresholds;
        }
        if let Some(interval) = input.check_interval_secs {
            settings.check_interval_secs = interval;
        }
        if let Some(days) = input.custom_min_validity_days {
            if !(0..=365).contains(&days) {
                return Err(UnifiedError::bad_request(
                    "custom_min_validity_days must be between 0 and 365",
                ));
            }
            settings.custom_min_validity_days = days;
        }
        if let Some(enabled) = input.cf_detect_dns {
            settings.cf_detect_dns = enabled;
        }
        if let Some(enabled) = input.cf_detect_probe {
            settings.cf_detect_probe = enabled;
        }
        if let Some(enabled) = input.cf_detect_api {
            settings.cf_detect_api = enabled;
        }
        if let Some(url) = input.cf_ranges_v4_url {
            validate_ranges_url(&url)?;
            settings.cf_ranges_v4_url = url;
        }
        if let Some(url) = input.cf_ranges_v6_url {
            validate_ranges_url(&url)?;
            settings.cf_ranges_v6_url = url;
        }
        let problems = settings.acme.validate();
        if !problems.is_empty() {
            return Err(UnifiedError::bad_request(format!(
                "invalid certificate settings: {}",
                problems.join("; ")
            ))
            .with_metadata("problems", json!(problems)));
        }
        *state
            .settings
            .write()
            .expect("settings lock poisoned") = settings.clone();
        Ok(json!({
            "enabled": settings.enabled,
            "acme": settings.acme.redacted(),
            "check_interval_secs": settings.check_interval_secs,
            "custom_min_validity_days": settings.custom_min_validity_days,
            "cloudflare_detection": {
                "dns": settings.cf_detect_dns,
                "probe": settings.cf_detect_probe,
                "api": settings.cf_detect_api,
            },
            "cloudflare_ranges_urls": {
                "v4": settings.cf_ranges_v4_url,
                "v6": settings.cf_ranges_v6_url,
            },
        }))
    })();
    json::respond(result, false)
}

/// A Cloudflare IP-range list URL must be an HTTPS URL: the ranges
/// are fetched over the network, so plain HTTP is refused.
fn validate_ranges_url(url: &str) -> UnifiedResult<()> {
    let parsed = url::Url::parse(url).map_err(|_| {
        UnifiedError::bad_request(format!("invalid Cloudflare ranges URL: \"{url}\""))
    })?;
    if parsed.scheme() != "https" || parsed.host_str().is_none_or(str::is_empty) {
        return Err(UnifiedError::bad_request(format!(
            "Cloudflare ranges URL must be an HTTPS URL with a host: \"{url}\""
        )));
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Certificate-mode and Cloudflare-detection handlers.
// ---------------------------------------------------------------------------

async fn list_cert_modes(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    state: web::Data<Arc<CertificatesState>>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let params = query.into_inner();
    let result: UnifiedResult<PageResult<DomainCertMode>> = (|| {
        let caller = caller(&req)?;
        let is_admin = check_capability(&ctx, &caller.role, PERM_CERTIFICATES_MANAGE);
        if !is_admin {
            require_permission(&ctx, &caller.role, PERM_CERTIFICATES_VIEW)?;
        }
        let mut items: Vec<DomainCertMode> = state
            .cert_modes
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|mode| {
                // Tenants only ever see their own verified domains.
                if is_admin {
                    true
                } else {
                    domain_owned(&ctx, &caller.actor, &mode.domain)
                }
            })
            .filter(|mode| {
                matches_search(&format!("{} {} {}", mode.domain, mode.mode, mode.status), &params)
                    && params
                        .filters
                        .get("mode")
                        .is_none_or(|mode_name| mode.mode == *mode_name)
                    && params
                        .filters
                        .get("status")
                        .is_none_or(|status| mode.status == *status)
                    && params
                        .filters
                        .get("provider")
                        .is_none_or(|provider| mode.provider == *provider)
            })
            .collect();
        let sort = params.allowed_sort(&["domain", "mode", "status"], "domain");
        match sort.as_str() {
            "mode" => items.sort_by(|a, b| a.mode.cmp(&b.mode)),
            "status" => items.sort_by(|a, b| a.status.cmp(&b.status)),
            _ => items.sort_by(|a, b| a.domain.cmp(&b.domain)),
        }
        if params.normalized().order == "desc" {
            items.reverse();
        }
        Ok(paginate(items, &params))
    })();
    json::respond(result, false)
}

async fn list_mode_definitions(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    state: web::Data<Arc<CertificatesState>>,
) -> HttpResponse {
    let result: UnifiedResult<Vec<CertModeDefinition>> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_CERTIFICATES_VIEW)?;
        Ok(state.mode_definitions())
    })();
    json::respond(result, false)
}

async fn update_mode_definitions(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    state: web::Data<Arc<CertificatesState>>,
    body: web::Json<UpdateModeDefinitionsRequest>,
) -> HttpResponse {
    let result: UnifiedResult<Vec<CertModeDefinition>> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_CERTIFICATES_MANAGE)?;
        let definitions = body.into_inner().modes;
        validate_mode_definitions(&definitions).map_err(UnifiedError::bad_request)?;
        *state
            .mode_definitions
            .write()
            .expect("mode definitions lock poisoned") = definitions.clone();
        Ok(definitions)
    })();
    json::respond(result, false)
}

async fn get_cert_mode(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    state: web::Data<Arc<CertificatesState>>,
    domain: web::Path<String>,
) -> HttpResponse {
    let domain = normalize_domain(&domain.into_inner());
    let result: UnifiedResult<Value> = (|| {
        authorize_domain(&ctx, &req, &domain, false)?;
        let mode = state
            .cert_modes
            .get(&domain)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| {
                UnifiedError::not_found(format!("no certificate mode set for {domain}"))
            })?;
        Ok(json!({
            "mode": mode,
            "history": state.mode_history_for(&domain),
        }))
    })();
    json::respond(result, false)
}

async fn set_cert_mode(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    state: web::Data<Arc<CertificatesState>>,
    domain: web::Path<String>,
    body: web::Json<SetCertModeRequest>,
) -> HttpResponse {
    let domain = normalize_domain(&domain.into_inner());
    let state = state.into_inner();
    let caller = match authorize_domain(&ctx, &req, &domain, true) {
        Ok(caller) => caller,
        Err(error) => return json::respond(Err::<Value, UnifiedError>(error), false),
    };
    let input = body.into_inner();
    let mode_id = input.mode.trim().to_ascii_lowercase();
    let definition = match state.mode_definition(&mode_id) {
        Some(definition) => definition,
        None => {
            return json::respond(
                Err::<Value, UnifiedError>(UnifiedError::bad_request(format!(
                    "unknown certificate mode \"{mode_id}\"; available modes: {}",
                    state
                        .mode_definitions()
                        .iter()
                        .filter(|definition| definition.enabled)
                        .map(|definition| definition.id.clone())
                        .collect::<Vec<_>>()
                        .join(", ")
                ))),
                false,
            );
        }
    };
    if !definition.enabled {
        return json::respond(
            Err::<Value, UnifiedError>(UnifiedError::bad_request(format!(
                "certificate mode \"{mode_id}\" is disabled"
            ))),
            false,
        );
    }
    let provider = match input.provider {
        Some(provider) if !provider.trim().is_empty() => provider.trim().to_string(),
        _ if definition.requires_acme => state.settings().acme.provider.clone(),
        _ if definition.requires_cloudflare => "cloudflare".to_string(),
        _ if definition.requires_upload => "custom".to_string(),
        _ => mode_id.clone(),
    };
    let previous_mode = state
        .cert_modes
        .get(&domain)
        .map(|entry| entry.value().mode.clone());
    let previous_fingerprint = state
        .cert_modes
        .get(&domain)
        .map(|entry| entry.value().fingerprint_sha256.clone())
        .filter(|fingerprint| !fingerprint.is_empty());
    let mut record = DomainCertMode {
        domain: domain.clone(),
        mode: mode_id.clone(),
        provider: provider.clone(),
        fingerprint_sha256: previous_fingerprint.clone().unwrap_or_default(),
        installed_at: None,
        previous_fingerprint: previous_fingerprint.clone(),
        status: "active".to_string(),
        last_error: None,
        updated_at: now(),
        updated_by: caller.actor.clone(),
    };
    if definition.requires_acme {
        // ACME issuance through the existing machinery: announce the
        // issuance on `ssl.renew` (the module's own handler skips
        // payloads marked `via: cert_mode`, so this does not queue a
        // second renewal) and run the renewal now.
        state.emit(
            events::SSL_RENEW,
            json!({ "domain": &domain, "force": false, "via": "cert_mode" }),
        );
        record.status = "pending".to_string();
        match CertificatesState::renew_domain(&state, &domain, false).await {
            Ok(_) => {
                if let Some(storage) = state.storage() {
                    if let Some(info) = storage.certificate_info(&domain) {
                        record.fingerprint_sha256 = info.fingerprint_sha256;
                        record.installed_at = Some(now());
                    }
                }
                record.status = "active".to_string();
            }
            Err(error) => {
                record.status = "error".to_string();
                record.last_error = Some(error.message.clone());
                state.record_cert_mode(record);
                return json::respond(Err::<Value, UnifiedError>(error), false);
            }
        }
    } else if definition.requires_upload {
        record.status = "awaiting_upload".to_string();
    }
    state.record_cert_mode(record.clone());
    state.emit(
        cert_events::SSL_CERT_MODE_CHANGED,
        json!({
            "domain": &domain,
            "mode": &mode_id,
            "previous_mode": &previous_mode,
            "provider": &provider,
            "status": &record.status,
            "actor": &caller.actor,
        }),
    );
    json::success_response(
        json!({
            "mode": record,
            "history": state.mode_history_for(&domain),
        }),
        200,
    )
}

async fn upload_custom_cert(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    state: web::Data<Arc<CertificatesState>>,
    domain: web::Path<String>,
    body: web::Json<CustomUploadRequest>,
) -> HttpResponse {
    let domain = normalize_domain(&domain.into_inner());
    let result: UnifiedResult<Value> = (|| {
        let caller = authorize_domain(&ctx, &req, &domain, true)?;
        let storage = state.storage().ok_or_else(|| {
            UnifiedError::internal("certificate storage is not initialized")
        })?;
        let settings = state.settings();
        // Validate BEFORE installing anything: chain, key match,
        // coverage, expiry. The minimum remaining validity is a
        // setting (`custom_min_validity_days`, default 7), not a
        // constant: uploads expiring sooner are rejected by name.
        let validation = validate_pem_pair(
            body.cert_pem.as_bytes(),
            body.key_pem.as_bytes(),
            Some(&domain),
        );
        if !validation.valid {
            return Err(UnifiedError::bad_request(format!(
                "certificate failed validation: {}",
                validation.reasons.join("; ")
            ))
            .with_metadata("reasons", json!(validation.reasons)));
        }
        let info = validation.info.as_ref().ok_or_else(|| {
            UnifiedError::internal("certificate validation produced no certificate info")
        })?;
        if info.days_remaining < settings.custom_min_validity_days {
            return Err(UnifiedError::bad_request(format!(
                "certificate expires in {} day(s); custom certificates must remain valid for at least {} day(s) (setting custom_min_validity_days)",
                info.days_remaining, settings.custom_min_validity_days
            ))
            .with_metadata(
                "reasons",
                json!([format!(
                    "certificate expires in {} day(s); minimum remaining validity is {} day(s)",
                    info.days_remaining, settings.custom_min_validity_days
                )]),
            ));
        }
        let new_fingerprint = info.fingerprint_sha256.clone();
        let previous_fingerprint = storage
            .certificate_info(&domain)
            .map(|live| live.fingerprint_sha256);
        storage.swap_certificate(&domain, body.cert_pem.as_bytes(), body.key_pem.as_bytes())?;
        // Post-swap verification with rollback on mismatch, mirroring
        // the existing swap handler.
        let live = storage.certificate_info(&domain);
        let verified = live
            .as_ref()
            .is_some_and(|live| live.fingerprint_sha256 == new_fingerprint);
        if !verified {
            let _ = storage.rollback_certificate(&domain);
            return Err(UnifiedError::internal(
                "certificate swap verification failed: the live certificate is not the new one; rolled back to the previous certificate",
            ));
        }
        let live = live.expect("live certificate must exist after a verified swap");
        let previous_mode = state
            .cert_modes
            .get(&domain)
            .map(|entry| entry.value().mode.clone());
        let record = DomainCertMode {
            domain: domain.clone(),
            mode: "custom".to_string(),
            provider: "custom".to_string(),
            fingerprint_sha256: new_fingerprint.clone(),
            installed_at: Some(now()),
            previous_fingerprint: previous_fingerprint.clone(),
            status: "active".to_string(),
            last_error: None,
            updated_at: now(),
            updated_by: caller.actor.clone(),
        };
        state.record_cert_mode(record.clone());
        state.upsert_record_from_info(&live, "custom");
        state.emit(
            cert_events::SSL_SWAPPED,
            json!({
                "domain": &domain,
                "ssl_expires_at": &live.not_after,
                "fingerprint_sha256": &live.fingerprint_sha256,
                "previous_fingerprint_sha256": &previous_fingerprint,
                "rolled_back": false,
            }),
        );
        state.emit(
            events::SSL_SWAP,
            json!({
                "domain": &domain,
                "ssl_expires_at": &live.not_after,
                "fingerprint_sha256": &live.fingerprint_sha256,
                "previous_fingerprint_sha256": &previous_fingerprint,
                "swapped_at": now().to_rfc3339(),
                "swapped": true,
            }),
        );
        state.emit(
            cert_events::SSL_CERT_MODE_CHANGED,
            json!({
                "domain": &domain,
                "mode": "custom",
                "previous_mode": &previous_mode,
                "provider": "custom",
                "status": "active",
                "actor": &caller.actor,
            }),
        );
        Ok(json!({
            "domain": domain,
            "installed": true,
            "mode": record,
            "certificate": live,
            "previous_fingerprint_sha256": previous_fingerprint,
        }))
    })();
    json::respond(result, false)
}

async fn detect_cloudflare_handler(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    state: web::Data<Arc<CertificatesState>>,
    domain: web::Path<String>,
) -> HttpResponse {
    let domain = normalize_domain(&domain.into_inner());
    let state = state.into_inner();
    match authorize_domain(&ctx, &req, &domain, true) {
        Err(error) => json::respond(Err::<CloudflareDetection, UnifiedError>(error), false),
        Ok(_) => {
            let detection = CertificatesState::detect_cloudflare(&state, &domain).await;
            json::success_response(detection, 200)
        }
    }
}

async fn get_cloudflare_detection(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    state: web::Data<Arc<CertificatesState>>,
    domain: web::Path<String>,
) -> HttpResponse {
    let domain = normalize_domain(&domain.into_inner());
    let result: UnifiedResult<CloudflareDetection> = (|| {
        authorize_domain(&ctx, &req, &domain, false)?;
        state
            .detections
            .get(&domain)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| {
                UnifiedError::not_found(format!(
                    "no Cloudflare detection stored for {domain}; run POST /detect-cloudflare first"
                ))
            })
    })();
    json::respond(result, false)
}

async fn recheck_cloudflare_handler(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    state: web::Data<Arc<CertificatesState>>,
    domain: web::Path<String>,
) -> HttpResponse {
    let domain = normalize_domain(&domain.into_inner());
    let state = state.into_inner();
    match authorize_domain(&ctx, &req, &domain, true) {
        Err(error) => json::respond(Err::<CloudflareDetection, UnifiedError>(error), false),
        Ok(_) => {
            let detection = CertificatesState::detect_cloudflare(&state, &domain).await;
            json::success_response(detection, 200)
        }
    }
}

async fn get_cf_ranges(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    state: web::Data<Arc<CertificatesState>>,
) -> HttpResponse {
    let result: UnifiedResult<CfIpRanges> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_CERTIFICATES_VIEW)?;
        Ok(state.cf_ranges_snapshot())
    })();
    json::respond(result, false)
}

async fn refresh_cf_ranges_handler(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    state: web::Data<Arc<CertificatesState>>,
) -> HttpResponse {
    let state = state.into_inner();
    match (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_CERTIFICATES_MANAGE)
    })() {
        Err(error) => json::respond(Err::<CfIpRanges, UnifiedError>(error), false),
        Ok(()) => {
            let result = CertificatesState::refresh_cf_ranges(&state).await;
            json::respond(result, false)
        }
    }
}

// ---------------------------------------------------------------------------
// Hook handlers
// ---------------------------------------------------------------------------

/// Handle the core `ssl.renew` event: a scheduler job (or the Domains
/// module) asks for a renewal. A payload with a `domain` renews that
/// domain (forced when `force` is true); a payload with `check: true`
/// or no domain runs the expiry check and renews every certificate
/// that is due. Completion payloads (carrying `ssl_expires_at` or
/// `completed`) are ignored here so a renewal is never re-run.
fn on_ssl_renew(state: Arc<CertificatesState>, ctx: &mut HookContext) {
    let data = ctx.data.clone();
    if data.get("ssl_expires_at").is_some() || data.get("completed").is_some() {
        return;
    }
    // The cert-mode setter announces its own issuance on `ssl.renew`
    // (so observers see it) and runs the renewal synchronously; do
    // not queue a second renewal for the same domain here.
    if data.get("via").and_then(|value| value.as_str()) == Some("cert_mode") {
        return;
    }
    let force = data
        .get("force")
        .and_then(|value| value.as_bool())
        .unwrap_or(false);
    match data.get("domain").and_then(|value| value.as_str()) {
        Some(domain) => {
            let domain = domain.to_string();
            let state = state.clone();
            let renewal_domain = domain.clone();
            spawn_task(async move {
                match CertificatesState::renew_domain(&state, &renewal_domain, force).await {
                    Ok(_) => {}
                    Err(error) => {
                        log::error!("certificate renewal failed for {renewal_domain}: {error}");
                    }
                }
            });
            ctx.add_result(json!({ "queued": true, "domain": domain }));
        }
        None => {
            let state = state.clone();
            spawn_task(async move {
                let _summary = state.check_expiry();
                let _attempts = CertificatesState::renew_all(&state, force).await;
            });
            ctx.add_result(json!({ "queued": true, "check": true }));
        }
    }
}

/// Handle the `ssl.cloudflare.recheck` event: a scheduler job (or an
/// admin) asks for a fresh Cloudflare detection. A payload with a
/// `domain` re-checks that domain; a payload with no domain re-checks
/// every domain with a stored certificate mode. Detection runs in a
/// spawned task because it needs async network I/O.
fn on_cloudflare_recheck(state: Arc<CertificatesState>, ctx: &mut HookContext) {
    let data = ctx.data.clone();
    match data.get("domain").and_then(|value| value.as_str()) {
        Some(domain) => {
            let domain = domain.to_string();
            let state = state.clone();
            spawn_task(async move {
                let _detection = CertificatesState::detect_cloudflare(&state, &domain).await;
            });
            ctx.add_result(json!({ "queued": true, "domain": domain }));
        }
        None => {
            let state = state.clone();
            spawn_task(async move {
                let domains: Vec<String> = state
                    .cert_modes
                    .iter()
                    .map(|entry| entry.key().clone())
                    .collect();
                for domain in domains {
                    let _detection =
                        CertificatesState::detect_cloudflare(&state, &domain).await;
                }
            });
            ctx.add_result(json!({ "queued": true, "check": true }));
        }
    }
}

/// Apply a completed renewal / swap (from the server ACME listener or
/// from this module itself) to the matching record.
fn on_ssl_completion(state: Arc<CertificatesState>, ctx: &mut HookContext) {
    let data = ctx.data.clone();
    let Some(domain) = data
        .get("domain")
        .and_then(|value| value.as_str())
        .map(str::to_string)
    else {
        return;
    };
    let expires_at = data
        .get("ssl_expires_at")
        .and_then(|value| value.as_str())
        .and_then(|value| value.parse::<DateTime<Utc>>().ok());
    if expires_at.is_none() && data.get("completed").is_none() {
        return;
    }
    let domain = normalize_domain(&domain);
    if let Some(storage) = state.storage() {
        if let Some(info) = storage.certificate_info(&domain) {
            state.upsert_record_from_info(&info, "acme");
        }
    }
    if let Some(mut record) = state.records.get_mut(&domain) {
        if let Some(expires_at) = expires_at {
            record.expires_at = Some(expires_at);
            record.days_remaining =
                Some(expires_at.signed_duration_since(now()).num_days());
            record.status =
                status_for(record.days_remaining.unwrap_or(0), record.self_signed).to_string();
        }
        record.last_renewed_at = Some(now());
        record.last_error = None;
        record.updated_at = now();
    }
    state.alerted_thresholds.remove(&domain);
}

// ---------------------------------------------------------------------------
// Module definition
// ---------------------------------------------------------------------------

pub struct CertificatesModule {
    state: Arc<CertificatesState>,
}

impl CertificatesModule {
    pub fn new() -> Self {
        Self {
            state: Arc::new(CertificatesState::new()),
        }
    }
}

impl Default for CertificatesModule {
    fn default() -> Self {
        Self::new()
    }
}

impl Module for CertificatesModule {
    fn id(&self) -> &'static str {
        "certificates"
    }

    fn name(&self) -> &'static str {
        "Certificates"
    }

    fn description(&self) -> &'static str {
        "Advanced SSL certificate management: config-first ACME certificates (multi-provider, EAB, HTTP-01), custom certificate install, swap with rollback, revocation, renewal history, and expiry alerts. Per-domain certificate modes from an admin-extensible registry (letsencrypt / custom / cloudflare), Cloudflare detection (DNS ranges, live probe, API check) with Flexible-mode warnings, and dynamic SSL swap without restart."
    }

    fn dependencies(&self) -> Vec<&'static str> {
        vec!["scheduler", "notifications", "domains"]
    }

    fn init(&self, ctx: &AppContext) -> UnifiedResult<()> {
        let config = ctx.config.read().expect("config lock poisoned").clone();
        // Prefer the shared storage the HTTP server attached to the
        // context; `ctx.tls()` is `None` at init time in the normal
        // startup order (module init runs before `run_server` builds
        // the storage), so seeding falls back to its own lenient
        // storage there — see `seed_from_config`.
        self.state.seed_from_config(&config, ctx.hooks.clone(), ctx.tls());
        Ok(())
    }

    fn register_hooks(&self, bus: &HookBus) {
        let state = self.state.clone();
        bus.register(
            events::SSL_RENEW,
            "certificates",
            Arc::new(move |ctx: &mut HookContext| on_ssl_renew(state.clone(), ctx)),
        );
        let state = self.state.clone();
        bus.register(
            cert_events::SSL_CF_RECHECK,
            "certificates",
            Arc::new(move |ctx: &mut HookContext| on_cloudflare_recheck(state.clone(), ctx)),
        );
        for event in [
            events::SSL_SWAP,
            cert_events::SSL_RENEWED,
            cert_events::SSL_SWAPPED,
        ] {
            let state = self.state.clone();
            bus.register(
                event,
                "certificates",
                Arc::new(move |ctx: &mut HookContext| on_ssl_completion(state.clone(), ctx)),
            );
        }
    }

    fn register_cli(&self, registry: &mut CliRegistry) {
        registry.register(CliCommandDef {
            name: "cert-list",
            about: "List certificates with expiry, issuer, SANs, and status",
            command: Command::new("cert-list")
                .about("List certificates with expiry, issuer, SANs, and status"),
            handler: Arc::new(|matches| cli_list(matches)),
        });
        registry.register(CliCommandDef {
            name: "cert-validate",
            about: "Validate one certificate (or every configured certificate)",
            command: Command::new("cert-validate")
                .about("Validate one certificate (or every configured certificate)")
                .arg(
                    Arg::new("domain")
                        .value_name("DOMAIN")
                        .required(false)
                        .help("Domain to validate (validates every configured certificate when omitted)"),
                ),
            handler: Arc::new(|matches| cli_validate(matches)),
        });
        registry.register(CliCommandDef {
            name: "cert-renew",
            about: "Renew one domain certificate now (ACME)",
            command: Command::new("cert-renew")
                .about("Renew one domain certificate now (ACME)")
                .arg(
                    Arg::new("domain")
                        .value_name("DOMAIN")
                        .required(true)
                        .help("Domain whose certificate to renew"),
                )
                .arg(
                    Arg::new("force")
                        .long("force")
                        .action(ArgAction::SetTrue)
                        .help("Renew regardless of expiry"),
                ),
            handler: Arc::new(|matches| cli_renew(matches)),
        });
        registry.register(CliCommandDef {
            name: "cert-renew-all",
            about: "Renew every certificate that is due (ACME)",
            command: Command::new("cert-renew-all")
                .about("Renew every certificate that is due (ACME)")
                .arg(
                    Arg::new("force")
                        .long("force")
                        .action(ArgAction::SetTrue)
                        .help("Renew every certificate regardless of expiry"),
                ),
            handler: Arc::new(|matches| cli_renew_all(matches)),
        });
        registry.register(CliCommandDef {
            name: "cert-revoke",
            about: "Revoke a certificate (stops serving it; falls back to self-signed)",
            command: Command::new("cert-revoke")
                .about("Revoke a certificate (stops serving it; falls back to self-signed)")
                .arg(
                    Arg::new("domain")
                        .value_name("DOMAIN")
                        .required(true)
                        .help("Domain whose certificate to revoke"),
                )
                .arg(
                    Arg::new("reason")
                        .long("reason")
                        .value_name("REASON")
                        .required(false)
                        .help("Reason for the revocation"),
                ),
            handler: Arc::new(|matches| cli_revoke(matches)),
        });
        registry.register(CliCommandDef {
            name: "cert-check-expiry",
            about: "Run the expiry check now (emits expiry alerts)",
            command: Command::new("cert-check-expiry")
                .about("Run the expiry check now (emits expiry alerts)"),
            handler: Arc::new(|matches| cli_check_expiry(matches)),
        });
    }

    fn configure_routes(&self, cfg: &mut web::ServiceConfig) {
        cfg.app_data(web::Data::new(self.state.clone()));
        cfg.service(
            web::scope("/api/v1/certificates")
                .route("", web::get().to(list_certificates))
                .route("/config", web::get().to(get_config))
                .route("/validate", web::get().to(validate_all))
                .route("/history", web::get().to(list_history))
                .route("/install", web::post().to(install_certificate_handler))
                .route("/renew-all", web::post().to(renew_all_handler))
                .route("/check-expiry", web::post().to(check_expiry_handler))
                .route("/settings", web::get().to(get_settings))
                .route("/settings", web::put().to(update_settings))
                .route("/cert-modes", web::get().to(list_cert_modes))
                .route("/cert-modes/definitions", web::get().to(list_mode_definitions))
                .route("/cert-modes/definitions", web::put().to(update_mode_definitions))
                .route("/cloudflare-ranges", web::get().to(get_cf_ranges))
                .route("/cloudflare-ranges/refresh", web::post().to(refresh_cf_ranges_handler))
                .route("/{domain}", web::get().to(get_certificate))
                .route("/{domain}/validate", web::post().to(validate_certificate))
                .route("/{domain}/renew", web::post().to(renew_certificate_handler))
                .route("/{domain}/force-renew", web::post().to(force_renew_certificate))
                .route("/{domain}/revoke", web::post().to(revoke_certificate_handler))
                .route("/{domain}/swap", web::post().to(swap_certificate_handler))
                .route("/{domain}/rollback", web::post().to(rollback_certificate_handler))
                .route("/{domain}/cert-mode", web::get().to(get_cert_mode))
                .route("/{domain}/cert-mode", web::put().to(set_cert_mode))
                .route("/{domain}/cert-mode/custom-upload", web::post().to(upload_custom_cert))
                .route("/{domain}/detect-cloudflare", web::post().to(detect_cloudflare_handler))
                .route("/{domain}/cloudflare-detection", web::get().to(get_cloudflare_detection))
                .route("/{domain}/cloudflare-detection/recheck", web::post().to(recheck_cloudflare_handler)),
        );
    }

    fn route_info(&self) -> Vec<RouteInfo> {
        vec![
            RouteInfo { method: "GET", path: "/api/v1/certificates", owner: "certificates", description: "List certificates with expiry, issuer, SANs, and status" },
            RouteInfo { method: "GET", path: "/api/v1/certificates/config", owner: "certificates", description: "Effective certificate config, provider matrix, and validation" },
            RouteInfo { method: "GET", path: "/api/v1/certificates/validate", owner: "certificates", description: "Validate every configured certificate" },
            RouteInfo { method: "GET", path: "/api/v1/certificates/history", owner: "certificates", description: "Renewal history" },
            RouteInfo { method: "POST", path: "/api/v1/certificates/install", owner: "certificates", description: "Install a custom certificate (validated first)" },
            RouteInfo { method: "POST", path: "/api/v1/certificates/renew-all", owner: "certificates", description: "Renew every certificate that is due" },
            RouteInfo { method: "POST", path: "/api/v1/certificates/check-expiry", owner: "certificates", description: "Run the expiry check now" },
            RouteInfo { method: "GET", path: "/api/v1/certificates/settings", owner: "certificates", description: "Runtime certificate settings (secrets redacted)" },
            RouteInfo { method: "PUT", path: "/api/v1/certificates/settings", owner: "certificates", description: "Update runtime certificate settings" },
            RouteInfo { method: "GET", path: "/api/v1/certificates/{domain}", owner: "certificates", description: "One certificate: detail plus recent history" },
            RouteInfo { method: "POST", path: "/api/v1/certificates/{domain}/validate", owner: "certificates", description: "Test and validate one certificate" },
            RouteInfo { method: "POST", path: "/api/v1/certificates/{domain}/renew", owner: "certificates", description: "Renew one certificate now" },
            RouteInfo { method: "POST", path: "/api/v1/certificates/{domain}/force-renew", owner: "certificates", description: "Force-renew one certificate" },
            RouteInfo { method: "POST", path: "/api/v1/certificates/{domain}/revoke", owner: "certificates", description: "Revoke one certificate" },
            RouteInfo { method: "POST", path: "/api/v1/certificates/{domain}/swap", owner: "certificates", description: "Swap the live certificate (validated first, rollback on failure)" },
            RouteInfo { method: "POST", path: "/api/v1/certificates/{domain}/rollback", owner: "certificates", description: "Restore the previous live certificate" },
            RouteInfo { method: "GET", path: "/api/v1/certificates/cert-modes", owner: "certificates", description: "List per-domain certificate modes (paginated, tenant-scoped)" },
            RouteInfo { method: "GET", path: "/api/v1/certificates/cert-modes/definitions", owner: "certificates", description: "Certificate-mode registry definitions" },
            RouteInfo { method: "PUT", path: "/api/v1/certificates/cert-modes/definitions", owner: "certificates", description: "Replace the certificate-mode registry (admin)" },
            RouteInfo { method: "GET", path: "/api/v1/certificates/cloudflare-ranges", owner: "certificates", description: "Stored Cloudflare IP-range snapshot with fetch date" },
            RouteInfo { method: "POST", path: "/api/v1/certificates/cloudflare-ranges/refresh", owner: "certificates", description: "Refresh the Cloudflare IP-range snapshot from the published lists" },
            RouteInfo { method: "GET", path: "/api/v1/certificates/{domain}/cert-mode", owner: "certificates", description: "One domain's certificate mode plus mode history" },
            RouteInfo { method: "PUT", path: "/api/v1/certificates/{domain}/cert-mode", owner: "certificates", description: "Set a domain's certificate mode (ACME issuance for letsencrypt modes)" },
            RouteInfo { method: "POST", path: "/api/v1/certificates/{domain}/cert-mode/custom-upload", owner: "certificates", description: "Upload a custom certificate for a domain (validated first, rollback on failure)" },
            RouteInfo { method: "POST", path: "/api/v1/certificates/{domain}/detect-cloudflare", owner: "certificates", description: "Detect Cloudflare usage for a domain (DNS ranges, live probe, API check)" },
            RouteInfo { method: "GET", path: "/api/v1/certificates/{domain}/cloudflare-detection", owner: "certificates", description: "Stored Cloudflare detection result for a domain" },
            RouteInfo { method: "POST", path: "/api/v1/certificates/{domain}/cloudflare-detection/recheck", owner: "certificates", description: "Re-run Cloudflare detection for a domain" },
        ]
    }
}

pub fn register(registry: &ModuleRegistry) {
    registry.register(Arc::new(CertificatesModule::new()));
}

// ---------------------------------------------------------------------------
// CLI handlers.
//
// The CLI builds its own state from the config file (the module state
// is not initialized in the CLI path: `build_cli_registry` never runs
// `init_all`), so every command loads the configuration fresh, builds
// the lenient storage, and does the work directly.
// ---------------------------------------------------------------------------

fn cli_state() -> UnifiedResult<(Arc<CertificatesState>, Arc<HookBus>)> {
    let config = AppConfig::load_default().unwrap_or_default();
    let hooks = Arc::new(HookBus::new());
    let state = Arc::new(CertificatesState::new());
    // The CLI has no server context and no shared storage handle, so
    // seeding builds its own storage with the lenient loader.
    state.seed_from_config(&config, hooks.clone(), None);
    Ok((state, hooks))
}

fn cli_config() -> AppConfig {
    AppConfig::load_default().unwrap_or_default()
}

fn cli_list(_matches: &ArgMatches) -> UnifiedResult<()> {
    let config = cli_config();
    let (storage, _reports) = SslStorage::from_config_lenient(&config.app.ssl);
    let infos = storage.list_certificates();
    if infos.is_empty() {
        println!("No certificates configured or stored.");
        return Ok(());
    }
    println!(
        "{:<32} {:<12} {:<12} {:<40} {}",
        "DOMAIN", "STATUS", "EXPIRES IN", "ISSUER", "SANS"
    );
    for info in infos {
        println!(
            "{:<32} {:<12} {:<12} {:<40} {}",
            info.domain,
            info.status,
            format!("{} day(s)", info.days_remaining),
            info.issuer,
            info.sans.join(", ")
        );
    }
    Ok(())
}

fn cli_validate(matches: &ArgMatches) -> UnifiedResult<()> {
    let config = cli_config();
    match matches.get_one::<String>("domain") {
        Some(domain) => {
            let domain = normalize_domain(domain);
            let paths = config.app.ssl.domains.get(&domain);
            let report = match paths {
                Some(entry) if !entry.cert.is_empty() && !entry.key.is_empty() => {
                    validate_config_entry(&domain, &entry.cert, &entry.key)
                }
                _ => {
                    let (storage, _) = SslStorage::from_config_lenient(&config.app.ssl);
                    match storage.certificate_info(&domain) {
                        Some(info) => CertificateValidation {
                            domain: domain.clone(),
                            valid: info.status == "valid" || info.status == "expiring",
                            reasons: if info.days_remaining < 0 {
                                vec![format!("certificate expired on {}", info.not_after)]
                            } else if info.self_signed {
                                vec!["self-signed certificate (not issued by a public authority)"
                                    .to_string()]
                            } else {
                                Vec::new()
                            },
                            info: Some(info),
                        },
                        None => {
                            return Err(UnifiedError::not_found(format!(
                                "no certificate known for {domain}"
                            )))
                        }
                    }
                }
            };
            print_validation(&report);
            if report.valid {
                Ok(())
            } else {
                Err(UnifiedError::bad_request(format!(
                    "certificate for {domain} failed validation"
                )))
            }
        }
        None => {
            let reports = validate_ssl_config(&config.app.ssl);
            if reports.is_empty() {
                println!("No certificates configured.");
                return Ok(());
            }
            let mut failed = 0;
            for report in &reports {
                print_validation(report);
                if !report.valid {
                    failed += 1;
                }
            }
            if failed == 0 {
                Ok(())
            } else {
                Err(UnifiedError::bad_request(format!(
                    "{failed} certificate(s) failed validation"
                )))
            }
        }
    }
}

fn print_validation(report: &CertificateValidation) {
    println!(
        "{}: {}",
        report.domain,
        if report.valid { "valid" } else { "INVALID" }
    );
    for reason in &report.reasons {
        println!("  - {reason}");
    }
    if let Some(info) = &report.info {
        println!("  issuer: {}", info.issuer);
        println!("  expires: {} (in {} day(s))", info.not_after, info.days_remaining);
        println!("  SANs: {}", info.sans.join(", "));
        println!("  fingerprint (SHA-256): {}", info.fingerprint_sha256);
        println!("  chain: {} certificate(s), complete: {}", info.chain_length, info.chain_complete);
        println!(
            "  key matches: {}",
            info.key_matches
                .map(|matches| if matches { "yes".to_string() } else { "no".to_string() })
                .unwrap_or_else(|| "unknown".to_string())
        );
    }
}

fn run_async<F>(future: F) -> UnifiedResult<()>
where
    F: std::future::Future<Output = UnifiedResult<()>> + Send + 'static,
{
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| UnifiedError::internal(format!("cannot start async runtime: {error}")))?;
    runtime.block_on(future)
}

fn cli_renew(matches: &ArgMatches) -> UnifiedResult<()> {
    let domain = matches
        .get_one::<String>("domain")
        .map(|value| value.to_string())
        .unwrap_or_default();
    let force = matches.get_flag("force");
    let (state, _hooks) = cli_state()?;
    run_async(async move {
        match CertificatesState::renew_domain(&state, &domain, force).await {
            Ok(attempt) => {
                println!(
                    "Renewal for {domain}: {} (new expiry: {})",
                    attempt.status,
                    attempt
                        .new_expires_at
                        .map(|expiry| expiry.to_rfc3339())
                        .unwrap_or_else(|| "-".to_string())
                );
                if attempt.status == "failed" {
                    return Err(UnifiedError::internal(
                        attempt.error.unwrap_or_else(|| "renewal failed".to_string()),
                    ));
                }
                Ok(())
            }
            Err(error) => Err(error),
        }
    })
}

fn cli_renew_all(matches: &ArgMatches) -> UnifiedResult<()> {
    let force = matches.get_flag("force");
    let (state, _hooks) = cli_state()?;
    run_async(async move {
        let attempts = CertificatesState::renew_all(&state, force).await;
        let failed = attempts
            .iter()
            .filter(|attempt| attempt.status == "failed")
            .count();
        for attempt in &attempts {
            println!(
                "{}: {} {}",
                attempt.domain,
                attempt.status,
                attempt.error.clone().unwrap_or_default()
            );
        }
        if failed == 0 {
            Ok(())
        } else {
            Err(UnifiedError::internal(format!(
                "{failed} renewal(s) failed"
            )))
        }
    })
}

fn cli_revoke(matches: &ArgMatches) -> UnifiedResult<()> {
    let domain = matches
        .get_one::<String>("domain")
        .map(|value| value.to_string())
        .unwrap_or_default();
    let reason = matches
        .get_one::<String>("reason")
        .cloned()
        .unwrap_or_else(|| "revoked by an administrator".to_string());
    let (state, _hooks) = cli_state()?;
    let storage = state.storage().ok_or_else(|| {
        UnifiedError::internal("certificate storage is not initialized")
    })?;
    let domain = normalize_domain(&domain);
    let info = storage.revoke_live(&domain)?;
    if let Some(mut record) = state.records.get_mut(&domain) {
        record.revoked = true;
        record.status = "revoked".to_string();
        record.last_error = Some(reason.clone());
        record.updated_at = now();
    }
    state.emit(
        cert_events::SSL_REVOKED,
        json!({
            "domain": &domain,
            "reason": &reason,
            "fingerprint_sha256": &info.fingerprint_sha256,
        }),
    );
    println!("Certificate for {domain} revoked ({reason}).");
    println!("The domain now falls back to the self-signed certificate.");
    Ok(())
}

fn cli_check_expiry(_matches: &ArgMatches) -> UnifiedResult<()> {
    let (state, _hooks) = cli_state()?;
    let summary = state.check_expiry();
    let checked = summary.get("checked").and_then(|value| value.as_u64()).unwrap_or(0);
    let expiring = summary
        .get("expiring")
        .and_then(|value| value.as_array())
        .map(|items| items.len())
        .unwrap_or(0);
    let expired = summary
        .get("expired")
        .and_then(|value| value.as_array())
        .map(|items| items.len())
        .unwrap_or(0);
    println!("Checked {checked} certificate(s): {expiring} expiring, {expired} expired.");
    for entry in summary
        .get("expiring")
        .and_then(|value| value.as_array())
        .cloned()
        .unwrap_or_default()
    {
        println!(
            "  expiring: {} ({} day(s) left)",
            entry.get("domain").and_then(|value| value.as_str()).unwrap_or("?"),
            entry.get("days_remaining").and_then(|value| value.as_i64()).unwrap_or(0)
        );
    }
    for entry in summary
        .get("expired")
        .and_then(|value| value.as_array())
        .cloned()
        .unwrap_or_default()
    {
        println!(
            "  expired: {}",
            entry.get("domain").and_then(|value| value.as_str()).unwrap_or("?")
        );
    }
    Ok(())
}
