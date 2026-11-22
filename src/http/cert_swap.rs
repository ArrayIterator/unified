// Dynamic SSL swap: replace the live certificate store (or one
// certificate inside it) without restarting the server.
//
// Hot-swap semantics, verified against `crate::http::tls` (read the
// code, do not take this on trust):
//
//   1. Per-certificate hot swap (the live path). `SslStorage` holds
//      its certificates in `DashMap`s and implements
//      `rustls::server::ResolvesServerCert::resolve`, which the TLS
//      stack calls on EVERY handshake. `add` / `renew` /
//      `swap_certificate` / `rollback_certificate` / `remove` mutate
//      those maps in place, so a certificate installed through them
//      is served by the NEXT handshake with no restart, no listener
//      rebuild, and no config reload. `CertStoreSwap::swap_certificate`
//      below is exactly this path: it installs into the CURRENT store.
//      In-flight connections keep the certificate they handshook with;
//      only new handshakes see the new certificate.
//
//   2. Whole-store swap (`ArcSwap`). `CertStoreSwap` holds the store
//      itself behind an `ArcSwap<SslStorage>`. `swap_store` atomically
//      replaces the holder's pointer, so any code that loads the store
//      AFTER the swap (for example a `ServerConfig` built after a
//      config reload) resolves through the NEW store. It does NOT
//      retroactively change a `ServerConfig` that was already built:
//      `server_config` / `server_config_from_swap` capture the `Arc`
//      they were built from, and a running listener keeps serving
//      through that captured `Arc`. A whole-store swap therefore only
//      takes effect for a listener (re)built from the swapped holder.
//      Do not claim otherwise. The per-certificate path (1) is the one
//      that changes live traffic.
//
// Integrator wiring (one line in the server startup, owned by the
// server builder — this file does not touch `server.rs`):
//
//   let swap = Arc::new(CertStoreSwap::new(storage.clone()));
//   let tls_config = crate::http::cert_swap::server_config_from_swap(&swap)?;
//   // ... and, to let hook callers request a swap:
//   crate::http::cert_swap::register_cert_swap_hooks(&ctx.hooks, swap.clone());
//
// Hook contract:
//
//   * Request: `ssl.swap` with `{domain, cert_pem, key_pem}`. The
//     handler registered by `register_cert_swap_hooks` validates the
//     pair, installs it into the current store, verifies the live
//     fingerprint, rolls back on mismatch, and emits the completion
//     event below. The private key is never logged or returned.
//   * Completion: `ssl.swap` with `{domain, fingerprint_sha256,
//     previous_fingerprint_sha256, ssl_expires_at, swapped_at,
//     swapped: true}`. Payloads WITHOUT `cert_pem`/`key_pem` are
//     completions and are ignored by the request handler (and by the
//     certificates module's completion handler, which only acts when
//     `ssl_expires_at` or `completed` is present). The hook bus skips
//     a handler re-entered on the same thread, so emitting the
//     completion from inside the request handler cannot loop.
//   * Whole-store swap: `ssl.store.swapped` with `{swapped_at}`.

use crate::core::error::{UnifiedError, UnifiedResult};
use crate::core::hooks::{events, HookBus, HookContext};
use crate::http::tls::{server_config, to_ascii_name, validate_pem_pair, SslStorage};
use arc_swap::ArcSwap;
use chrono::{DateTime, Utc};
use rustls::ServerConfig;
use serde::Serialize;
use serde_json::json;
use std::sync::{Arc, RwLock};

/// Emitted after the whole store behind a `CertStoreSwap` is
/// replaced. Payload: `{"swapped_at"}`. This is NOT a per-domain
/// certificate event: it only means later `load()` calls see the new
/// store (see the module header).
pub const SSL_STORE_SWAPPED: &str = "ssl.store.swapped";

/// Outcome of one successful per-certificate swap. Carries
/// fingerprints and metadata only; never any key material.
#[derive(Debug, Clone, Serialize)]
pub struct SwapOutcome {
    pub domain: String,
    pub swapped: bool,
    pub fingerprint_sha256: String,
    pub previous_fingerprint_sha256: Option<String>,
    pub ssl_expires_at: Option<String>,
    pub swapped_at: DateTime<Utc>,
}

/// An `ArcSwap` holder around the live `SslStorage`.
///
/// The holder itself is cheap to clone (`Arc` around it) and safe to
/// share between the HTTP server, the certificates module, and hook
/// handlers: `ArcSwap` gives lock-free atomic loads and swaps.
pub struct CertStoreSwap {
    store: ArcSwap<SslStorage>,
    hooks: RwLock<Option<Arc<HookBus>>>,
}

impl CertStoreSwap {
    /// Wrap an existing store. The holder takes the store as-is; it
    /// never copies certificates.
    pub fn new(storage: Arc<SslStorage>) -> Self {
        Self {
            store: ArcSwap::new(storage),
            hooks: RwLock::new(None),
        }
    }

    /// Attach the hook bus used to emit `ssl.swap` completions.
    /// Without a bus, swaps still install and verify, but no event
    /// is emitted (the certificates module then cannot observe them).
    pub fn attach_hooks(&self, hooks: Arc<HookBus>) {
        *self.hooks.write().expect("cert swap hooks lock poisoned") = Some(hooks);
    }

    /// Load the current store (the store new handshakes resolve
    /// through once a `ServerConfig` is built from it).
    pub fn load(&self) -> Arc<SslStorage> {
        self.store.load_full()
    }

    /// Atomically replace the whole store and return the previous
    /// one. See the module header: already-built `ServerConfig`s keep
    /// resolving through the previous store; only `ServerConfig`s
    /// built after this call (via `server_config_from_swap`) see the
    /// new store. Per-certificate changes to live traffic must use
    /// `swap_certificate` instead.
    pub fn swap_store(&self, new_store: Arc<SslStorage>) -> Arc<SslStorage> {
        let previous = self.store.swap(new_store);
        self.emit(
            SSL_STORE_SWAPPED,
            json!({ "swapped_at": Utc::now().to_rfc3339() }),
        );
        previous
    }

    fn bus(&self) -> Option<Arc<HookBus>> {
        self.hooks.read().expect("cert swap hooks lock poisoned").clone()
    }

    fn emit(&self, event: &str, data: serde_json::Value) {
        if let Some(bus) = self.bus() {
            bus.emit(event, data);
        }
    }

    /// Validate a certificate/key pair and install it into the
    /// CURRENT store (the DashMap hot-swap path: effective for new
    /// handshakes, no restart). The pair is validated BEFORE anything
    /// goes live; on validation failure nothing is installed. The
    /// previous fingerprint is recorded before the swap, and after
    /// the swap the live fingerprint is verified against the new one:
    /// on any mismatch the previous certificate is rolled back and
    /// an error is returned instead of serving the wrong certificate.
    /// Emits the `ssl.swap` completion event on success.
    pub fn swap_certificate(
        &self,
        domain: &str,
        cert_pem: &[u8],
        key_pem: &[u8],
    ) -> UnifiedResult<SwapOutcome> {
        let domain = to_ascii_name(domain);
        let validation = validate_pem_pair(cert_pem, key_pem, Some(&domain));
        if !validation.valid {
            return Err(UnifiedError::bad_request(format!(
                "certificate failed validation: {}",
                validation.reasons.join("; ")
            ))
            .with_metadata("reasons", json!(validation.reasons)));
        }
        let new_fingerprint = validation
            .info
            .as_ref()
            .map(|info| info.fingerprint_sha256.clone())
            .unwrap_or_default();
        let store = self.load();
        let previous_fingerprint = store
            .certificate_info(&domain)
            .map(|info| info.fingerprint_sha256);
        store.swap_certificate(&domain, cert_pem, key_pem)?;
        let live = store.certificate_info(&domain);
        let verified = live
            .as_ref()
            .is_some_and(|info| info.fingerprint_sha256 == new_fingerprint);
        if !verified {
            let _ = store.rollback_certificate(&domain);
            return Err(UnifiedError::internal(
                "certificate swap verification failed: the live certificate is not the new one; rolled back to the previous certificate",
            ));
        }
        let info = live.expect("live certificate must exist after a verified swap");
        let outcome = SwapOutcome {
            domain: domain.clone(),
            swapped: true,
            fingerprint_sha256: info.fingerprint_sha256.clone(),
            previous_fingerprint_sha256: previous_fingerprint.clone(),
            ssl_expires_at: Some(info.not_after.clone()),
            swapped_at: Utc::now(),
        };
        self.emit(
            events::SSL_SWAP,
            json!({
                "domain": &domain,
                "fingerprint_sha256": &outcome.fingerprint_sha256,
                "previous_fingerprint_sha256": &previous_fingerprint,
                "ssl_expires_at": &outcome.ssl_expires_at,
                "swapped_at": outcome.swapped_at.to_rfc3339(),
                "swapped": true,
            }),
        );
        Ok(outcome)
    }
}

/// Build a TLS server configuration that resolves certificates
/// through the store CURRENTLY held by `swap`. The returned config
/// captures that store's `Arc`: a later `swap_store` does not change
/// this config (see the module header). For live traffic changes
/// without rebuilding the listener, use `swap_certificate` on the
/// holder instead.
pub fn server_config_from_swap(swap: &CertStoreSwap) -> UnifiedResult<ServerConfig> {
    let storage = swap.load();
    server_config(&storage)
}

/// Handle an `ssl.swap` REQUEST on the hook bus: a payload carrying
/// `{domain, cert_pem, key_pem}`. Completion payloads (which carry
/// `ssl_expires_at` / `swapped` and no PEM material) are ignored, so
/// a swap is never re-run from its own completion event. The result
/// (`swapped: true` plus the fingerprint, or `swapped: false` plus
/// the error) is added to the hook context; the completion event is
/// emitted by `swap_certificate` itself.
fn on_ssl_swap_request(swap: Arc<CertStoreSwap>, ctx: &mut HookContext) {
    let data = ctx.data.clone();
    let Some(domain) = data.get("domain").and_then(|value| value.as_str()) else {
        return;
    };
    let Some(cert_pem) = data.get("cert_pem").and_then(|value| value.as_str()) else {
        return;
    };
    let Some(key_pem) = data.get("key_pem").and_then(|value| value.as_str()) else {
        return;
    };
    let domain = domain.to_string();
    let cert_pem = cert_pem.to_string();
    let key_pem = key_pem.to_string();
    match swap.swap_certificate(&domain, cert_pem.as_bytes(), key_pem.as_bytes()) {
        Ok(outcome) => {
            ctx.add_result(json!({
                "swapped": true,
                "domain": outcome.domain,
                "fingerprint_sha256": outcome.fingerprint_sha256,
                "swapped_at": outcome.swapped_at.to_rfc3339(),
            }));
        }
        Err(error) => {
            ctx.add_result(json!({
                "swapped": false,
                "domain": domain,
                "error": error.message,
            }));
        }
    }
}

/// Register the `ssl.swap` request handler on `bus`. The integrator
/// calls this once at startup with the shared `CertStoreSwap` (see
/// the module header for the one-line wiring).
pub fn register_cert_swap_hooks(bus: &HookBus, swap: Arc<CertStoreSwap>) {
    bus.register(
        events::SSL_SWAP,
        "cert_swap",
        Arc::new(move |ctx: &mut HookContext| on_ssl_swap_request(swap.clone(), ctx)),
    );
}
