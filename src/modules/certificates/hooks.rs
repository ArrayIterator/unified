//! Certificates - hook handlers.
//!
//! Part of the `certificates` module split; see `src/modules/certificates.rs`.

use unified_core::hooks::HookContext;
use unified_core::json;
use unified_core::tls::status_for;
use chrono::DateTime;
use chrono::Utc;
use serde_json::json;
use std::sync::Arc;

use super::cert_events;
use super::state::CertificatesState;
use super::state::normalize_domain;
use super::state::now;
use super::state::spawn_task;

// Hook handlers

/// Handle the core `ssl.renew` event: a scheduler job (or the Domains
/// module) asks for a renewal. A payload with a `domain` renews that
/// domain (forced when `force` is true); a payload with `check: true`
/// or no domain runs the expiry check and renews every certificate
/// that is due. Completion payloads (carrying `ssl_expires_at` or
/// `completed`) are ignored here so a renewal is never re-run.
pub(crate) fn on_ssl_renew(state: Arc<CertificatesState>, ctx: &mut HookContext) {
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
pub(crate) fn on_cloudflare_recheck(state: Arc<CertificatesState>, ctx: &mut HookContext) {
    let data = ctx.data.clone();
    match data.get("domain").and_then(|value| value.as_str()) {
        Some(domain) => {
            let domain = domain.to_string();
            let domain_for_task = domain.clone();
            let state = state.clone();
            spawn_task(async move {
                let _detection =
                    CertificatesState::detect_cloudflare(&state, &domain_for_task).await;
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
                    let _detection = CertificatesState::detect_cloudflare(&state, &domain).await;
                }
            });
            ctx.add_result(json!({ "queued": true, "check": true }));
        }
    }
}

/// Apply a completed renewal / swap (from the server ACME listener or
/// from this module itself) to the matching record.
pub(crate) fn on_ssl_completion(state: Arc<CertificatesState>, ctx: &mut HookContext) {
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
            record.days_remaining = Some(expires_at.signed_duration_since(now()).num_days());
            record.status =
                status_for(record.days_remaining.unwrap_or(0), record.self_signed).to_string();
        }
        // Only a real renewal stamps `last_renewed_at`: the payload
        // carries `renewed: true` (the ACME renewal path emits it on
        // `ssl.swap`), or the event itself is `ssl.renewed`. A plain
        // swap / install (`ssl.swap` / `ssl.swapped` without
        // `renewed`) must NOT be recorded as a renewal.
        let renewed = data
            .get("renewed")
            .and_then(|value| value.as_bool())
            .unwrap_or(false)
            || ctx.event == cert_events::SSL_RENEWED;
        if renewed {
            record.last_renewed_at = Some(now());
        }
        record.last_error = None;
        record.updated_at = now();
    }
    state.alerted_thresholds.remove(&domain);
}

#[cfg(test)]
mod tests {
    use super::*;
    use unified_core::hooks::HookContext;
    use unified_core::hooks::events;
    use super::super::CertificateRecord;

    fn seed_record(state: &CertificatesState) {
        let record = CertificateRecord {
            id: "record-1".to_string(),
            domain: "example.com".to_string(),
            provider: "custom".to_string(),
            status: "valid".to_string(),
            issuer: "Test CA".to_string(),
            sans: vec!["example.com".to_string()],
            fingerprint_sha256: "aa".repeat(32),
            not_before: None,
            expires_at: None,
            days_remaining: Some(30),
            self_signed: false,
            wildcard: false,
            source: "custom".to_string(),
            revoked: false,
            last_renewed_at: None,
            last_error: None,
            created_at: now(),
            updated_at: now(),
        };
        state.records.insert("example.com".to_string(), record);
    }

    fn completion_ctx(event: &str, data: serde_json::Value) -> HookContext {
        HookContext::new(event, data)
    }

    fn expiry() -> String {
        (now() + chrono::Duration::days(30)).to_rfc3339()
    }

    #[test]
    fn swap_completion_does_not_stamp_last_renewed_at() {
        let state = Arc::new(CertificatesState::new());
        seed_record(&state);
        let mut ctx = completion_ctx(
            events::SSL_SWAP,
            json!({ "domain": "example.com", "ssl_expires_at": expiry() }),
        );
        on_ssl_completion(state.clone(), &mut ctx);
        let record = state.records.get("example.com").unwrap();
        assert!(record.last_renewed_at.is_none());
        assert!(record.expires_at.is_some());
    }

    #[test]
    fn renewed_swap_completion_stamps_last_renewed_at() {
        let state = Arc::new(CertificatesState::new());
        seed_record(&state);
        let mut ctx = completion_ctx(
            events::SSL_SWAP,
            json!({ "domain": "example.com", "ssl_expires_at": expiry(), "renewed": true }),
        );
        on_ssl_completion(state.clone(), &mut ctx);
        assert!(state
            .records
            .get("example.com")
            .unwrap()
            .last_renewed_at
            .is_some());
    }

    #[test]
    fn renewed_event_stamps_last_renewed_at_without_flag() {
        let state = Arc::new(CertificatesState::new());
        seed_record(&state);
        let mut ctx = completion_ctx(
            cert_events::SSL_RENEWED,
            json!({ "domain": "example.com", "ssl_expires_at": expiry(), "completed": true }),
        );
        on_ssl_completion(state.clone(), &mut ctx);
        assert!(state
            .records
            .get("example.com")
            .unwrap()
            .last_renewed_at
            .is_some());
    }

    #[test]
    fn swapped_event_does_not_stamp_last_renewed_at() {
        let state = Arc::new(CertificatesState::new());
        seed_record(&state);
        let mut ctx = completion_ctx(
            cert_events::SSL_SWAPPED,
            json!({ "domain": "example.com", "ssl_expires_at": expiry(), "rolled_back": false }),
        );
        on_ssl_completion(state.clone(), &mut ctx);
        assert!(state
            .records
            .get("example.com")
            .unwrap()
            .last_renewed_at
            .is_none());
    }
}
