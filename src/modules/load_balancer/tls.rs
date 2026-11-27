// load_balancer -- TLS termination lookup.

use unified_core::config::AppConfig;
use unified_core::tls::SslStorage;
use serde::Serialize;

use super::store::*;

// TLS termination lookup.

/// TLS certificate information for one domain.
#[derive(Debug, Clone, Serialize)]
pub struct TlsCertificateInfo {
    pub domain: String,
    /// A certificate is stored for the domain.
    pub present: bool,
    pub expires_at: Option<String>,
    /// DNS / IP names on the stored leaf certificate.
    pub names: Vec<String>,
}

/// Look a domain up in the live TLS storage. This is the lookup path
/// a balancer with `tls_mode: "terminate"` uses: `SslStorage::lookup`
/// resolves exact names first, then wildcard suffixes, with runtime
/// entries winning over configured ones; `cert_expiry` and
/// `certificate_names` read the stored leaf certificate. Callers pass
/// the shared handle from `AppContext::tls()` (attached by
/// `http::server::run_server`); when no handle is attached the
/// handlers fall back to `tls_status_from_config` below.
pub fn tls_certificate_status(storage: &SslStorage, domain: &str) -> TlsCertificateInfo {
    TlsCertificateInfo {
        domain: domain.to_string(),
        present: storage.lookup(domain).is_some(),
        expires_at: storage
            .cert_expiry(domain)
            .map(|expiry| expiry.to_rfc3339()),
        names: storage.certificate_names(domain).unwrap_or_default(),
    }
}

/// TLS status without a storage handle: configured certificates come
/// from `app.ssl.domains`, and expiry / presence details come from
/// the cache refreshed by `ssl.swap` / `ssl.renew` events.
pub(crate) fn tls_status_from_config(domain: &str, config: &AppConfig) -> TlsCertificateInfo {
    let configured = config.app.ssl.domains.contains_key(domain);
    let cached = tls_cache().get(domain).map(|entry| entry.clone());
    TlsCertificateInfo {
        domain: domain.to_string(),
        present: cached
            .as_ref()
            .map(|entry| entry.present)
            .unwrap_or(configured),
        expires_at: cached
            .as_ref()
            .and_then(|entry| entry.expires_at)
            .map(|expiry| expiry.to_rfc3339()),
        names: cached
            .as_ref()
            .map(|entry| vec![entry.domain.clone()])
            .unwrap_or_default(),
    }
}
