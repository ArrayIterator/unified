//! Certificates - certificate event names.
//!
//! Part of the `certificates` module split; see `src/modules/certificates.rs`.

// Events.
//
// The core bus (`unified_core::hooks::events`) defines only `ssl.renew`
// and `ssl.swap`. The certificate lifecycle events below are defined
// here, in the module that owns them, so no shared file has to change
// for this module to work. For the core definitions to live in
// `unified_core::hooks::events` instead, add one constant per name
// there (for example `pub const SSL_RENEWED: &str = "ssl.renewed";`)
// and switch the emits below to those constants; the audit module
// would then also need the new names in its `AUDITED_EVENTS` list.

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
/// NOTE: no module in the repo answers this hook yet (verified
/// 2026-11-22: no listener in the Cloudflare module), so the API
/// detection method always reports "unknown" and `cf_detect_api`
/// defaults to `false` until a listener lands. The cross-module gap
/// (a zone-lookup listener in the Cloudflare module) is tracked as
/// follow-up work outside this module.
pub const CLOUDFLARE_ZONE_LOOKUP: &str = "cloudflare.zone.lookup";
/// Hook contract: ask the SaaS/domains side whether a tenant user
/// owns a domain. Request payload: `{"domain", "user_id",
/// "owned": false}`; the answering handler sets `owned: true`
/// when the user owns the (verified) domain. No answer means "not
/// owned": tenant-scoped access fails closed.
pub const DOMAIN_OWNERSHIP_CHECK: &str = "domain.ownership.check";
