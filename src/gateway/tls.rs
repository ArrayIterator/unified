// TLS termination for the Pingora gateway.
//
// The gateway does not parse certificates, does not store them, and
// does not implement its own resolver. Everything certificate-shaped
// goes through the existing `SslStorage` (`unified_core::tls`):
// SNI lookup, expiry, certificate names, the self-signed fallback,
// and the rustls `ServerConfig` used by the listener. This file only
// adapts that storage to the gateway's questions:
//
//   - "Is there a certificate for this SNI name?" (`resolve_certificate`)
//   - "What should the management API report for this name?"
//     (`certificate_status`)
//   - "Give me the listener TLS configuration." (`build_server_config`)
//
// When no `SslStorage` handle is attached yet (the HTTP server has
// not wired it in - see the integrator snippet for
// `src/http/server.rs`), status falls back to the configured
// certificates in `app.ssl.domains`, exactly like the load balancer's
// config-only TLS status. Presence is then "configured", never
// "verified live".

use unified_core::config::AppConfig;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::tls::SslStorage;
use rustls::ServerConfig;
use serde::Serialize;
use std::sync::Arc;

/// TLS status for one server name, as reported by the management API.
#[derive(Debug, Clone, Serialize)]
pub struct TlsStatusView {
    pub server_name: String,
    /// A certificate is stored for the name (live lookup), or is
    /// configured for it (config fallback).
    pub present: bool,
    pub expires_at: Option<String>,
    /// DNS / IP names on the stored leaf certificate.
    pub names: Vec<String>,
    /// True when no certificate is stored but the storage would
    /// create a self-signed fallback on first handshake
    /// (`app.ssl.auto_create`).
    pub auto_create_fallback: bool,
    /// Where the answer came from: `"storage"` (live `SslStorage`
    /// lookup) or `"config"` (configured certificates only).
    pub source: &'static str,
}

/// TLS status for one server name. Prefers the live storage handle;
/// falls back to the application configuration when no handle is
/// attached yet. Reuses the load-balancer module's
/// `tls_certificate_status`, which already performs the live lookup
/// (`SslStorage::lookup`, `cert_expiry`, `certificate_names`).
pub fn certificate_status(
    storage: Option<&SslStorage>,
    server_name: &str,
    config: &AppConfig,
) -> TlsStatusView {
    let name = server_name.trim().trim_end_matches('.').to_ascii_lowercase();
    match storage {
        Some(storage) => {
            let info = crate::modules::load_balancer::tls_certificate_status(storage, &name);
            TlsStatusView {
                server_name: name.clone(),
                present: info.present,
                expires_at: info.expires_at,
                names: info.names,
                auto_create_fallback: !info.present && storage.is_auto_create(),
                source: "storage",
            }
        }
        None => {
            let configured = config.app.ssl.domains.contains_key(&name);
            TlsStatusView {
                server_name: name.clone(),
                present: configured,
                expires_at: None,
                names: if configured { vec![name] } else { Vec::new() },
                auto_create_fallback: !configured && config.app.ssl.auto_create,
                source: "config",
            }
        }
    }
}

/// Resolve a certificate for the requested SNI name against the live
/// storage. Returns `Ok` when a certificate is stored, or when none
/// is stored but auto-creation is enabled (the storage creates a
/// self-signed fallback on first handshake, inside
/// `SslStorage::resolve`). Returns a clear `404` error when no
/// certificate matches and no fallback will be created, so the caller
/// can report the mismatch instead of failing the handshake with an
/// opaque TLS alert.
pub fn resolve_certificate(storage: &SslStorage, server_name: &str) -> UnifiedResult<()> {
    let name = server_name.trim().trim_end_matches('.').to_ascii_lowercase();
    if storage.lookup(&name).is_some() {
        return Ok(());
    }
    if storage.is_auto_create() {
        return Ok(());
    }
    Err(UnifiedError::not_found(format!(
        "No certificate matches SNI name \"{name}\"; add it to app.ssl.domains or enable app.ssl.auto_create"
    ))
    .with_metadata("server_name", serde_json::json!(name)))
}

/// Build the rustls listener configuration from the shared storage.
/// This is the same `server_config` the Actix HTTPS listener uses, so
/// the gateway terminates TLS with exactly the certificates the main
/// server already serves. No second TLS stack is built.
pub fn build_server_config(storage: &Arc<SslStorage>) -> UnifiedResult<ServerConfig> {
    unified_core::tls::server_config(storage)
}

/// Check every configured TLS listen name at startup and report the
/// names that have no certificate and no auto-create fallback. The
/// gateway still starts (plain listeners are unaffected); the report
/// lets the operator fix the gap before TLS traffic arrives.
pub fn check_tls_listen_names(
    storage: Option<&SslStorage>,
    server_names: &[String],
    config: &AppConfig,
) -> Vec<String> {
    let mut problems = Vec::new();
    for name in server_names {
        let status = certificate_status(storage, name, config);
        if !status.present && !status.auto_create_fallback {
            problems.push(format!(
                "No certificate for TLS name \"{name}\" (add it to app.ssl.domains or enable app.ssl.auto_create)"
            ));
        }
    }
    problems
}
