// Certificates module — advanced SSL certificate management.
//
// Config-first certificate management built on `unified_core::tls`
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

pub mod ai_capabilities;
pub mod cert_events;
mod cli;
mod handlers;
mod hooks;
mod mode_handlers;
pub mod modes;
pub mod module;
pub mod state;
pub mod types;

pub use self::ai_capabilities::AI_CAPABILITIES;
pub use self::modes::{
    CertModeDefinition, CfIpRanges, CloudflareDetection, DomainCertMode,
    default_cert_mode_definitions,
};
pub use self::module::*;
pub use self::state::CertificatesState;
pub use self::types::{
    CertModuleSettings, CertificateRecord, DEFAULT_CF_RANGES_V4_URL, DEFAULT_CF_RANGES_V6_URL,
    DEFAULT_CUSTOM_MIN_VALIDITY_DAYS, RenewalAttempt,
};
