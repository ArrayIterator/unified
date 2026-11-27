//! Certificates - certificate-mode and Cloudflare-detection handlers.
//!
//! Part of the `certificates` module split; see `src/modules/certificates.rs`.

use unified_core::context::AppContext;
use unified_core::error::UnifiedError;
use unified_core::error::UnifiedResult;
use unified_core::hooks::events;
use unified_core::json;
use unified_core::pagination::PageResult;
use unified_core::pagination::PaginationParams;
use unified_core::tls::validate_pem_pair;
use actix_web::HttpRequest;
use actix_web::HttpResponse;
use actix_web::web;
use serde_json::Value;
use serde_json::json;
use std::sync::Arc;

use super::cert_events;
use super::modes::CertModeDefinition;
use super::modes::CfIpRanges;
use super::modes::CloudflareDetection;
use super::modes::CustomUploadRequest;
use super::modes::DomainCertMode;
use super::modes::PERM_CERTIFICATES_MANAGE;
use super::modes::PERM_CERTIFICATES_VIEW;
use super::modes::SetCertModeRequest;
use super::modes::UpdateModeDefinitionsRequest;
use super::modes::authorize_domain;
use super::modes::caller;
use super::modes::check_capability;
use super::modes::domain_owned;
use super::modes::require_permission;
use super::modes::validate_mode_definitions;
use super::state::CertificatesState;
use super::state::matches_search;
use super::state::normalize_domain;
use super::state::now;
use super::state::paginate;

// Certificate-mode and Cloudflare-detection handlers.

pub(crate) async fn list_cert_modes(
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
                matches_search(
                    &format!("{} {} {}", mode.domain, mode.mode, mode.status),
                    &params,
                ) && params
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
    json::respond(result, ctx.is_debug())
}

pub(crate) async fn list_mode_definitions(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    state: web::Data<Arc<CertificatesState>>,
) -> HttpResponse {
    let result: UnifiedResult<Vec<CertModeDefinition>> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_CERTIFICATES_VIEW)?;
        Ok(state.mode_definitions())
    })();
    json::respond(result, ctx.is_debug())
}

pub(crate) async fn update_mode_definitions(
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
    json::respond(result, ctx.is_debug())
}

pub(crate) async fn get_cert_mode(
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
    json::respond(result, ctx.is_debug())
}

pub(crate) async fn set_cert_mode(
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
        Err(error) => return json::respond(Err::<Value, UnifiedError>(error), ctx.is_debug()),
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
                return json::respond(Err::<Value, UnifiedError>(error), ctx.is_debug());
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

pub(crate) async fn upload_custom_cert(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    state: web::Data<Arc<CertificatesState>>,
    domain: web::Path<String>,
    body: web::Json<CustomUploadRequest>,
) -> HttpResponse {
    let domain = normalize_domain(&domain.into_inner());
    let result: UnifiedResult<Value> = (|| {
        let caller = authorize_domain(&ctx, &req, &domain, true)?;
        let storage = state
            .storage()
            .ok_or_else(|| UnifiedError::internal("certificate storage is not initialized"))?;
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
    json::respond(result, ctx.is_debug())
}

pub(crate) async fn detect_cloudflare_handler(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    state: web::Data<Arc<CertificatesState>>,
    domain: web::Path<String>,
) -> HttpResponse {
    let domain = normalize_domain(&domain.into_inner());
    let state = state.into_inner();
    match authorize_domain(&ctx, &req, &domain, true) {
        Err(error) => json::respond(
            Err::<CloudflareDetection, UnifiedError>(error),
            ctx.is_debug(),
        ),
        Ok(_) => {
            let detection = CertificatesState::detect_cloudflare(&state, &domain).await;
            json::success_response(detection, 200)
        }
    }
}

pub(crate) async fn get_cloudflare_detection(
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
    json::respond(result, ctx.is_debug())
}

pub(crate) async fn recheck_cloudflare_handler(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    state: web::Data<Arc<CertificatesState>>,
    domain: web::Path<String>,
) -> HttpResponse {
    let domain = normalize_domain(&domain.into_inner());
    let state = state.into_inner();
    match authorize_domain(&ctx, &req, &domain, true) {
        Err(error) => json::respond(
            Err::<CloudflareDetection, UnifiedError>(error),
            ctx.is_debug(),
        ),
        Ok(_) => {
            let detection = CertificatesState::detect_cloudflare(&state, &domain).await;
            json::success_response(detection, 200)
        }
    }
}

pub(crate) async fn get_cf_ranges(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    state: web::Data<Arc<CertificatesState>>,
) -> HttpResponse {
    let result: UnifiedResult<CfIpRanges> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_CERTIFICATES_VIEW)?;
        Ok(state.cf_ranges_snapshot())
    })();
    json::respond(result, ctx.is_debug())
}

pub(crate) async fn refresh_cf_ranges_handler(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    state: web::Data<Arc<CertificatesState>>,
) -> HttpResponse {
    let state = state.into_inner();
    match (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_CERTIFICATES_MANAGE)
    })() {
        Err(error) => json::respond(Err::<CfIpRanges, UnifiedError>(error), ctx.is_debug()),
        Ok(()) => {
            let result = CertificatesState::refresh_cf_ranges(&state).await;
            json::respond(result, ctx.is_debug())
        }
    }
}
