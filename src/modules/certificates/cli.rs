//! Certificates - CLI handlers.
//!
//! Part of the `certificates` module split; see `src/modules/certificates.rs`.

use unified_core::config::AppConfig;
use unified_core::error::UnifiedError;
use unified_core::error::UnifiedResult;
use unified_core::hooks::HookBus;
use unified_core::json;
use unified_core::tls::CertificateValidation;
use unified_core::tls::SslStorage;
use unified_core::tls::validate_config_entry;
use unified_core::tls::validate_ssl_config;
use clap::ArgMatches;
use serde_json::json;
use std::sync::Arc;

use super::cert_events;
use super::state::CertificatesState;
use super::state::normalize_domain;
use super::state::now;

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

pub(crate) fn cli_list(_matches: &ArgMatches) -> UnifiedResult<()> {
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

/// Validate one live (stored, not file-backed) certificate for the
/// CLI: valid only when the status is `valid` or `expiring` AND the
/// certificate is neither self-signed nor chain-incomplete. Each
/// failing condition gets its own reason, so a self-signed
/// certificate is never reported `valid` alongside a "self-signed"
/// reason the way the old status-only check did.
pub(crate) fn validate_live_info(
    domain: &str,
    info: unified_core::tls::CertificateInfo,
) -> CertificateValidation {
    let mut reasons: Vec<String> = Vec::new();
    if info.days_remaining < 0 {
        reasons.push(format!("certificate expired on {}", info.not_after));
    }
    if info.status != "valid" && info.status != "expiring" {
        reasons.push(format!(
            "certificate status is \"{}\" (expected \"valid\" or \"expiring\")",
            info.status
        ));
    }
    if info.self_signed {
        reasons.push("self-signed certificate (not issued by a public authority)".to_string());
    }
    if !info.chain_complete {
        reasons.push(
            "chain is incomplete: the leaf certificate has no intermediate certificate bundled"
                .to_string(),
        );
    }
    let valid = reasons.is_empty();
    CertificateValidation {
        domain: domain.to_string(),
        valid,
        reasons,
        info: Some(info),
    }
}

pub(crate) fn cli_validate(matches: &ArgMatches) -> UnifiedResult<()> {
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
                        Some(info) => validate_live_info(&domain, info),
                        None => {
                            return Err(UnifiedError::not_found(format!(
                                "no certificate known for {domain}"
                            )));
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
        println!(
            "  expires: {} (in {} day(s))",
            info.not_after, info.days_remaining
        );
        println!("  SANs: {}", info.sans.join(", "));
        println!("  fingerprint (SHA-256): {}", info.fingerprint_sha256);
        println!(
            "  chain: {} certificate(s), complete: {}",
            info.chain_length, info.chain_complete
        );
        println!(
            "  key matches: {}",
            info.key_matches
                .map(|matches| if matches {
                    "yes".to_string()
                } else {
                    "no".to_string()
                })
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

pub(crate) fn cli_renew(matches: &ArgMatches) -> UnifiedResult<()> {
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
                        attempt
                            .error
                            .unwrap_or_else(|| "renewal failed".to_string()),
                    ));
                }
                Ok(())
            }
            Err(error) => Err(error),
        }
    })
}

pub(crate) fn cli_renew_all(matches: &ArgMatches) -> UnifiedResult<()> {
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

pub(crate) fn cli_revoke(matches: &ArgMatches) -> UnifiedResult<()> {
    let domain = matches
        .get_one::<String>("domain")
        .map(|value| value.to_string())
        .unwrap_or_default();
    let reason = matches
        .get_one::<String>("reason")
        .cloned()
        .unwrap_or_else(|| "revoked by an administrator".to_string());
    let (state, _hooks) = cli_state()?;
    let storage = state
        .storage()
        .ok_or_else(|| UnifiedError::internal("certificate storage is not initialized"))?;
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

pub(crate) fn cli_check_expiry(_matches: &ArgMatches) -> UnifiedResult<()> {
    let (state, _hooks) = cli_state()?;
    let summary = state.check_expiry();
    let checked = summary
        .get("checked")
        .and_then(|value| value.as_u64())
        .unwrap_or(0);
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
            entry
                .get("domain")
                .and_then(|value| value.as_str())
                .unwrap_or("?"),
            entry
                .get("days_remaining")
                .and_then(|value| value.as_i64())
                .unwrap_or(0)
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
            entry
                .get("domain")
                .and_then(|value| value.as_str())
                .unwrap_or("?")
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use unified_core::tls::CertificateInfo;

    fn make_info(
        status: &str,
        self_signed: bool,
        chain_complete: bool,
        days: i64,
    ) -> CertificateInfo {
        CertificateInfo {
            domain: "example.com".to_string(),
            subject: "CN=example.com".to_string(),
            issuer: "Test CA".to_string(),
            sans: vec!["example.com".to_string()],
            not_before: "2026-01-01T00:00:00Z".to_string(),
            not_after: "2027-01-01T00:00:00Z".to_string(),
            days_remaining: days,
            fingerprint_sha256: "aa".repeat(32),
            self_signed,
            chain_length: 2,
            chain_complete,
            key_matches: Some(true),
            status: status.to_string(),
            source: "custom".to_string(),
        }
    }

    #[test]
    fn live_validation_valid_and_expiring_are_valid() {
        let report = validate_live_info("example.com", make_info("valid", false, true, 30));
        assert!(report.valid);
        assert!(report.reasons.is_empty());
        let report = validate_live_info("example.com", make_info("expiring", false, true, 5));
        assert!(report.valid);
    }

    #[test]
    fn live_validation_self_signed_is_invalid_despite_valid_status() {
        let report = validate_live_info("example.com", make_info("valid", true, true, 30));
        assert!(!report.valid);
        assert!(report
            .reasons
            .iter()
            .any(|reason| reason.contains("self-signed")));
    }

    #[test]
    fn live_validation_incomplete_chain_is_invalid() {
        let report = validate_live_info("example.com", make_info("valid", false, false, 30));
        assert!(!report.valid);
        assert!(report
            .reasons
            .iter()
            .any(|reason| reason.contains("chain is incomplete")));
    }

    #[test]
    fn live_validation_expired_reports_every_failing_condition() {
        let report = validate_live_info("example.com", make_info("expired", true, false, -5));
        assert!(!report.valid);
        assert!(report
            .reasons
            .iter()
            .any(|reason| reason.contains("expired")));
        assert!(report
            .reasons
            .iter()
            .any(|reason| reason.contains("self-signed")));
        assert!(report
            .reasons
            .iter()
            .any(|reason| reason.contains("chain is incomplete")));
    }
}
