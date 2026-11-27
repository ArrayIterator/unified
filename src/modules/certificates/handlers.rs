//! Certificates - certificate handlers.
//!
//! Part of the `certificates` module split; see `src/modules/certificates.rs`.

use unified_core::context::AppContext;
use unified_core::error::UnifiedError;
use unified_core::error::UnifiedResult;
use unified_core::hooks::events;
use unified_core::json;
use unified_core::pagination::PaginationParams;
use crate::http::acme::provider_matrix;
use unified_core::tls::CertificateValidation;
use unified_core::tls::validate_config_entry;
use unified_core::tls::validate_pem_pair;
use actix_web::HttpRequest;
use actix_web::HttpResponse;
use actix_web::web;
use serde_json::Value;
use serde_json::json;
use std::sync::Arc;

use super::modes::Caller;
use super::modes::PERM_CERTIFICATES_MANAGE;
use super::modes::PERM_CERTIFICATES_VIEW;
use super::modes::authorize_domain;
use super::modes::caller;
use super::modes::check_capability;
use super::modes::domain_owned;
use super::modes::require_permission;

use super::cert_events;
use super::state::CertificatesState;
use super::state::matches_search;
use super::state::normalize_domain;
use super::state::now;
use super::state::paginate;
use super::types::CertificateRecord;
use super::types::InstallRequest;
use super::types::RenewRequest;
use super::types::RenewalAttempt;
use super::types::RevokeRequest;
use super::types::SwapRequest;
use super::types::UpdateSettingsRequest;

// Authorization.
//
// There is NO global admin gate in front of `/api/v1/certificates`
// (verified 2026-11-22: the scope is registered bare in
// `configure_routes`, and the server middleware chain carries no
// per-module auth), so every handler below authorizes its own call,
// mirroring `mode_handlers.rs`:
//   - domain-scoped calls go through `authorize_domain` (admin, or
//     the tenant branch -- currently unavailable, see `modes.rs`);
//   - global calls go through `authorize_global` (VIEW for reads,
//     MANAGE for writes).
// `check_capability` fails closed through the `role.check` hook: no
// answer means not allowed.

/// Authorize a global (non-domain-scoped) call: the caller must hold
/// `permission`. Also reports whether the caller holds MANAGE
/// (admins see every domain; other callers only see their own, where
/// a list supports that filtering).
fn authorize_global(
    ctx: &AppContext,
    req: &HttpRequest,
    permission: &str,
) -> UnifiedResult<(Caller, bool)> {
    let caller = caller(req)?;
    let is_admin = check_capability(ctx, &caller.role, PERM_CERTIFICATES_MANAGE);
    if !is_admin {
        require_permission(ctx, &caller.role, permission)?;
    }
    Ok((caller, is_admin))
}

/// Turn an authorization result into the handler's early return: on
/// failure the denial response, so handlers can write
/// `auth_response(authorize_global(...))?;` at the top.
fn auth_response<T>(result: UnifiedResult<T>) -> Result<T, HttpResponse> {
    result.map_err(|error| json::respond(Err::<Value, UnifiedError>(error), false))
}

// Handlers

pub(crate) async fn list_certificates(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    state: web::Data<Arc<CertificatesState>>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let (caller, is_admin) =
        match auth_response(authorize_global(&ctx, &req, PERM_CERTIFICATES_VIEW)) {
            Ok(pair) => pair,
            Err(response) => return response,
        };
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
            // Tenants only ever see their own verified domains.
            (is_admin || domain_owned(&ctx, &caller.actor, &record.domain))
                && matches_search(
                    &format!("{} {} {}", record.domain, record.issuer, record.status),
                    &params,
                )
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
                && params
                    .filters
                    .get("revoked")
                    .is_none_or(|flag| record.revoked == (*flag == "true" || *flag == "1"))
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

pub(crate) async fn get_config(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    state: web::Data<Arc<CertificatesState>>,
) -> HttpResponse {
    let (caller, is_admin) =
        match auth_response(authorize_global(&ctx, &req, PERM_CERTIFICATES_VIEW)) {
            Ok(pair) => pair,
            Err(response) => return response,
        };
    let settings = state.settings();
    let mut validation = match state.storage() {
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
    if !is_admin {
        validation.retain(|report| domain_owned(&ctx, &caller.actor, &report.domain));
    }
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

pub(crate) async fn validate_all(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    state: web::Data<Arc<CertificatesState>>,
) -> HttpResponse {
    let (caller, is_admin) =
        match auth_response(authorize_global(&ctx, &req, PERM_CERTIFICATES_VIEW)) {
            Ok(pair) => pair,
            Err(response) => return response,
        };
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
                reasons
                    .push("self-signed certificate (not issued by a public authority)".to_string());
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
    if !is_admin {
        reports.retain(|report| domain_owned(&ctx, &caller.actor, &report.domain));
    }
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

pub(crate) async fn get_certificate(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    state: web::Data<Arc<CertificatesState>>,
    domain: web::Path<String>,
) -> HttpResponse {
    let domain = normalize_domain(&domain.into_inner());
    let result: UnifiedResult<Value> = (|| {
        authorize_domain(&ctx, &req, &domain, false)?;
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
            .ok_or_else(|| UnifiedError::not_found(format!("no certificate known for {domain}")))?;
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
    json::respond(result, ctx.is_debug())
}

pub(crate) async fn validate_certificate(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    state: web::Data<Arc<CertificatesState>>,
    domain: web::Path<String>,
) -> HttpResponse {
    let domain = normalize_domain(&domain.into_inner());
    let result: UnifiedResult<CertificateValidation> = (|| {
        authorize_domain(&ctx, &req, &domain, false)?;
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
            .ok_or_else(|| UnifiedError::not_found(format!("no certificate known for {domain}")))?;
        let mut reasons: Vec<String> = Vec::new();
        if info.days_remaining < 0 {
            reasons.push(format!("certificate expired on {}", info.not_after));
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
        Ok(CertificateValidation {
            domain,
            valid,
            reasons,
            info: Some(info),
        })
    })();
    json::respond(result, ctx.is_debug())
}

pub(crate) async fn renew_certificate_handler(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    state: web::Data<Arc<CertificatesState>>,
    domain: web::Path<String>,
    body: Option<web::Json<RenewRequest>>,
) -> HttpResponse {
    let domain = normalize_domain(&domain.into_inner());
    if let Err(response) = auth_response(authorize_domain(&ctx, &req, &domain, true)) {
        return response;
    }
    let force = body.map(|body| body.force).unwrap_or(false);
    let state = state.into_inner();
    let result = CertificatesState::renew_domain(&state, &domain, force).await;
    json::respond(result, ctx.is_debug())
}

pub(crate) async fn force_renew_certificate(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    state: web::Data<Arc<CertificatesState>>,
    domain: web::Path<String>,
) -> HttpResponse {
    let domain = normalize_domain(&domain.into_inner());
    if let Err(response) = auth_response(authorize_domain(&ctx, &req, &domain, true)) {
        return response;
    }
    let state = state.into_inner();
    let result = CertificatesState::renew_domain(&state, &domain, true).await;
    json::respond(result, ctx.is_debug())
}

pub(crate) async fn renew_all_handler(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    state: web::Data<Arc<CertificatesState>>,
    body: Option<web::Json<RenewRequest>>,
) -> HttpResponse {
    if let Err(response) =
        auth_response(authorize_global(&ctx, &req, PERM_CERTIFICATES_MANAGE))
    {
        return response;
    }
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

pub(crate) async fn revoke_certificate_handler(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    state: web::Data<Arc<CertificatesState>>,
    domain: web::Path<String>,
    body: Option<web::Json<RevokeRequest>>,
) -> HttpResponse {
    let domain = normalize_domain(&domain.into_inner());
    let reason = body
        .and_then(|body| body.reason.clone())
        .unwrap_or_else(|| "revoked by an administrator".to_string());
    let result: UnifiedResult<Value> = (|| {
        authorize_domain(&ctx, &req, &domain, true)?;
        let storage = state
            .storage()
            .ok_or_else(|| UnifiedError::internal("certificate storage is not initialized"))?;
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
    json::respond(result, ctx.is_debug())
}

pub(crate) async fn swap_certificate_handler(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    state: web::Data<Arc<CertificatesState>>,
    domain: web::Path<String>,
    body: web::Json<SwapRequest>,
) -> HttpResponse {
    let domain = normalize_domain(&domain.into_inner());
    let result: UnifiedResult<Value> = (|| {
        authorize_domain(&ctx, &req, &domain, true)?;
        let storage = state
            .storage()
            .ok_or_else(|| UnifiedError::internal("certificate storage is not initialized"))?;
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
    json::respond(result, ctx.is_debug())
}

pub(crate) async fn rollback_certificate_handler(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    state: web::Data<Arc<CertificatesState>>,
    domain: web::Path<String>,
) -> HttpResponse {
    let domain = normalize_domain(&domain.into_inner());
    let result: UnifiedResult<Value> = (|| {
        authorize_domain(&ctx, &req, &domain, true)?;
        let storage = state
            .storage()
            .ok_or_else(|| UnifiedError::internal("certificate storage is not initialized"))?;
        let previous_fingerprint = storage
            .previous_info(&domain)
            .map(|info| info.fingerprint_sha256);
        storage.rollback_certificate(&domain)?;
        let info = storage
            .certificate_info(&domain)
            .ok_or_else(|| UnifiedError::internal("no live certificate after rollback"))?;
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
    json::respond(result, ctx.is_debug())
}

pub(crate) async fn install_certificate_handler(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    state: web::Data<Arc<CertificatesState>>,
    body: web::Json<InstallRequest>,
) -> HttpResponse {
    let domain = normalize_domain(&body.domain);
    let result: UnifiedResult<Value> = (|| {
        authorize_domain(&ctx, &req, &domain, true)?;
        let storage = state
            .storage()
            .ok_or_else(|| UnifiedError::internal("certificate storage is not initialized"))?;
        let settings = state.settings();
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
        let info = validation.info.as_ref().ok_or_else(|| {
            UnifiedError::internal("certificate validation produced no certificate info")
        })?;
        // Minimum remaining validity, mirroring the custom-upload
        // handler: an almost-expired certificate must be rejected
        // here too, not only on the upload path.
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
        storage.swap_certificate(&domain, body.cert_pem.as_bytes(), body.key_pem.as_bytes())?;
        let info = storage
            .certificate_info(&domain)
            .ok_or_else(|| UnifiedError::internal("certificate vanished after install"))?;
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
        // Core completion event, so the Domains module updates its
        // SSL status after an install (previously missing: only the
        // swap/upload paths emitted it).
        state.emit(
            events::SSL_SWAP,
            json!({
                "domain": &domain,
                "ssl_expires_at": &info.not_after,
            }),
        );
        Ok(json!({
            "domain": domain,
            "installed": true,
            "certificate": info,
        }))
    })();
    json::respond(result, ctx.is_debug())
}

pub(crate) async fn list_history(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    state: web::Data<Arc<CertificatesState>>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let (caller, is_admin) =
        match auth_response(authorize_global(&ctx, &req, PERM_CERTIFICATES_VIEW)) {
            Ok(pair) => pair,
            Err(response) => return response,
        };
    let params = query.into_inner();
    let mut items: Vec<RenewalAttempt> = state
        .history
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|attempt| {
            // Tenants only ever see their own verified domains.
            (is_admin || domain_owned(&ctx, &caller.actor, &attempt.domain))
                && matches_search(&format!("{} {}", attempt.domain, attempt.status), &params)
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

pub(crate) async fn check_expiry_handler(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    state: web::Data<Arc<CertificatesState>>,
) -> HttpResponse {
    if let Err(response) =
        auth_response(authorize_global(&ctx, &req, PERM_CERTIFICATES_VIEW))
    {
        return response;
    }
    let summary = state.check_expiry();
    json::success_response(summary, 200)
}

pub(crate) async fn get_settings(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    state: web::Data<Arc<CertificatesState>>,
) -> HttpResponse {
    if let Err(response) =
        auth_response(authorize_global(&ctx, &req, PERM_CERTIFICATES_VIEW))
    {
        return response;
    }
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

pub(crate) async fn update_settings(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    state: web::Data<Arc<CertificatesState>>,
    body: web::Json<UpdateSettingsRequest>,
) -> HttpResponse {
    if let Err(response) =
        auth_response(authorize_global(&ctx, &req, PERM_CERTIFICATES_MANAGE))
    {
        return response;
    }
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
            validate_check_interval_secs(interval).map_err(UnifiedError::bad_request)?;
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
        *state.settings.write().expect("settings lock poisoned") = settings.clone();
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
    json::respond(result, ctx.is_debug())
}

/// Bounds for `check_interval_secs` (the expiry-check cadence).
/// 0 would mean "never run" (or a busy loop, depending on the
/// scheduler) and values above a week make expiry alerts useless;
/// `AcmeConfig::validate()` covers the other settings but not this
/// one, so it is checked here.
pub(crate) const MIN_CHECK_INTERVAL_SECS: u64 = 60;
pub(crate) const MAX_CHECK_INTERVAL_SECS: u64 = 604_800;

/// Reject an expiry-check interval outside the sane range.
pub(crate) fn validate_check_interval_secs(interval: u64) -> Result<(), String> {
    if (MIN_CHECK_INTERVAL_SECS..=MAX_CHECK_INTERVAL_SECS).contains(&interval) {
        Ok(())
    } else {
        Err(format!(
            "check_interval_secs must be between {MIN_CHECK_INTERVAL_SECS} \
             and {MAX_CHECK_INTERVAL_SECS} seconds"
        ))
    }
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

#[cfg(test)]
mod tests {

    /// Build a request carrying the server-verified caller the auth
    /// middleware would attach for this user and role. The old
    /// self-asserted identity headers grant nothing,
    /// so tests attach the verified caller to the request
    /// extensions instead.
    fn verified_request(user_id: &str, role: &str) -> actix_web::HttpRequest {
        use actix_web::HttpMessage;
        let req = actix_web::test::TestRequest::default().to_http_request();
        req.extensions_mut().insert(
            unified_core::auth::Caller::verified(test_user_uuid(user_id), Vec::new())
                .with_roles(vec![role.to_string()]),
        );
        req
    }

    /// Build a request carrying a verified caller with no roles
    /// resolved: the caller resolves, but holds the anonymous role.
    /// This is the "no role" case in the new model (roles are
    /// resolved server-side, never claimed in a header).
    fn verified_request_without_roles(user_id: &str) -> actix_web::HttpRequest {
        use actix_web::HttpMessage;
        let req = actix_web::test::TestRequest::default().to_http_request();
        req.extensions_mut()
            .insert(unified_core::auth::Caller::verified(
                test_user_uuid(user_id),
                Vec::new(),
            ));
        req
    }

    /// Deterministic test-user identity: distinct names map to
    /// distinct UUIDs (FNV-1a folded into the low bits), so
    /// ownership tests can still tell two users apart. A value
    /// that already parses as a UUID is used as-is.
    fn test_user_uuid(name: &str) -> uuid::Uuid {
        if let Ok(id) = uuid::Uuid::parse_str(name) {
            return id;
        }
        let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
        for byte in name.bytes() {
            hash ^= u64::from(byte);
            hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        }
        uuid::Uuid::from_u128(u128::from(hash) | 0x0000_0000_0000_4000_8000_0000_0000_0000)
    }

    use super::*;
    use unified_core::config::AppConfig;
    use unified_core::hooks::HookBus;
    use unified_core::hooks::HookContext;
    use unified_core::modules::ModuleRegistry;
    use actix_web::http::StatusCode;
    use actix_web::test::TestRequest;

    /// App context whose `role.check` hook allows exactly the given
    /// permissions (all other permissions are denied).
    fn test_ctx(allowed: &[&str]) -> web::Data<Arc<AppContext>> {
        let hooks = Arc::new(HookBus::new());
        let allowed: Vec<String> = allowed
            .iter()
            .map(|permission| permission.to_string())
            .collect();
        hooks.register(
            "role.check",
            "test",
            Arc::new(move |ctx: &mut HookContext| {
                let permission = ctx
                    .get("permission")
                    .and_then(|value| value.as_str())
                    .unwrap_or("");
                ctx.set(
                    "allowed",
                    serde_json::json!(allowed.iter().any(|held| held == permission)),
                );
            }),
        );
        let ctx = AppContext::new(
            AppConfig::default(),
            hooks,
            Arc::new(ModuleRegistry::new()),
        );
        web::Data::new(Arc::new(ctx))
    }

    fn test_request() -> HttpRequest {
        verified_request("tester", "admin")
    }

    fn test_state() -> web::Data<Arc<CertificatesState>> {
        web::Data::new(Arc::new(CertificatesState::new()))
    }

    #[test]
    fn ranges_url_validation() {
        assert!(validate_ranges_url("https://www.cloudflare.com/ips-v4").is_ok());
        assert!(validate_ranges_url("http://www.cloudflare.com/ips-v4").is_err());
        assert!(validate_ranges_url("not a url").is_err());
        assert!(validate_ranges_url("https://").is_err());
    }

    #[test]
    fn check_interval_validation_bounds() {
        assert!(validate_check_interval_secs(0).is_err());
        assert!(validate_check_interval_secs(59).is_err());
        assert!(validate_check_interval_secs(60).is_ok());
        assert!(validate_check_interval_secs(86_400).is_ok());
        assert!(validate_check_interval_secs(604_800).is_ok());
        assert!(validate_check_interval_secs(604_801).is_err());
    }

    #[tokio::test]
    async fn list_certificates_requires_authentication() {
        let ctx = test_ctx(&[PERM_CERTIFICATES_VIEW]);
        let anonymous = TestRequest::default().to_http_request();
        let denied = list_certificates(
            ctx.clone(),
            anonymous,
            test_state(),
            web::Query(PaginationParams::default()),
        )
        .await;
        assert_eq!(denied.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn list_certificates_allows_viewer_and_admin() {
        let viewer = list_certificates(
            test_ctx(&[PERM_CERTIFICATES_VIEW]),
            test_request(),
            test_state(),
            web::Query(PaginationParams::default()),
        )
        .await;
        assert_eq!(viewer.status(), StatusCode::OK);
        let admin = list_certificates(
            test_ctx(&[PERM_CERTIFICATES_MANAGE]),
            test_request(),
            test_state(),
            web::Query(PaginationParams::default()),
        )
        .await;
        assert_eq!(admin.status(), StatusCode::OK);
        let nobody = list_certificates(
            test_ctx(&[]),
            test_request(),
            test_state(),
            web::Query(PaginationParams::default()),
        )
        .await;
        assert_eq!(nobody.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn revoke_requires_authentication() {
        let denied = revoke_certificate_handler(
            test_ctx(&[PERM_CERTIFICATES_MANAGE]),
            TestRequest::default().to_http_request(),
            test_state(),
            web::Path::from("example.com".to_string()),
            None,
        )
        .await;
        assert_eq!(denied.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn update_settings_rejects_zero_check_interval() {
        let body: UpdateSettingsRequest =
            serde_json::from_value(json!({ "check_interval_secs": 0 })).unwrap();
        let denied = update_settings(
            test_ctx(&[PERM_CERTIFICATES_MANAGE]),
            test_request(),
            test_state(),
            web::Json(body),
        )
        .await;
        assert_eq!(denied.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn update_settings_accepts_sane_check_interval() {
        let body: UpdateSettingsRequest =
            serde_json::from_value(json!({ "check_interval_secs": 3600 })).unwrap();
        let ok = update_settings(
            test_ctx(&[PERM_CERTIFICATES_MANAGE]),
            test_request(),
            test_state(),
            web::Json(body),
        )
        .await;
        assert_eq!(ok.status(), StatusCode::OK);
    }
}
