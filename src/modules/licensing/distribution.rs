//! Licensing - Distribution channels and the git webhook surface.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::{events, HookBus, HookContext};
use unified_core::json;
use unified_core::modules::{
    Module, ModuleRegistry, MODULE_ENTITLEMENT_DENIED_EVENT, MODULE_ENTITLEMENT_GRANTED_EVENT,
    RouteInfo,
};
use unified_core::pagination::{PageResult, PaginationParams};
use unified_core::db::models::{License, LicenseActivation, NewLicense, NewLicenseActivation};
use crate::modules::apikeys::{
    authenticate_key, key_has_scope, note_used, AuthenticatedKey,
};
use actix_multipart::Multipart;
use actix_web::{http::header, web, HttpRequest, HttpResponse};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use diesel::prelude::*;
use futures_util::TryStreamExt;
use hmac::{Hmac, Mac};
use rand::Rng;
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};
use uuid::Uuid;
use super::{keys::*, expiry::*, store::*, hooks::*, types::*, validation::*, handlers::*, products::*, helpers::*, product_handlers::*, verify::*, offline::*, releases::*, module::*};

// SaaS handlers: distribution channels (git).

pub(crate) fn validate_provider(provider: &str) -> UnifiedResult<String> {
    match provider.trim().to_lowercase().as_str() {
        "github" | "gitlab" | "bitbucket" | "generic" => {
            Ok(provider.trim().to_lowercase())
        }
        other => Err(UnifiedError::bad_request(format!(
            "Unknown provider \"{other}\" (expected github, gitlab, bitbucket, or generic)"
        ))),
    }
}

pub(crate) fn validate_repo(repo: &str) -> UnifiedResult<String> {
    let repo = repo.trim().to_string();
    let parts: Vec<&str> = repo.split('/').collect();
    let valid = parts.len() >= 2
        && parts.iter().all(|part| {
            !part.is_empty()
                && part
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.')
        });
    if !valid {
        return Err(UnifiedError::bad_request(
            "repo must look like \"owner/repo\" (letters, digits, '-', '_' or '.')",
        ));
    }
    Ok(repo)
}

pub(crate) fn distribution_view(record: &LicenseDistribution) -> LicenseDistributionView {
    LicenseDistributionView::from(record)
}

pub(crate) async fn list_distributions(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    params: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        require_control_plane(&req)?;
        require_caller_capability(&ctx, &req, "licensing:view")?;
        let mut items: Vec<LicenseDistributionView> = license_distributions_store()
            .iter()
            .map(|entry| distribution_view(entry.value()))
            .collect();
        if let Some(product) = params.filters.get("product") {
            items.retain(|row| row.product == *product);
        }
        if let Some(search) = &params.search {
            let needle = search.to_lowercase();
            items.retain(|row| {
                row.product.to_lowercase().contains(&needle)
                    || row.repo.to_lowercase().contains(&needle)
            });
        }
        items.sort_by(|a, b| a.created_at.cmp(&b.created_at));
        if params.normalized().order == "desc" {
            items.reverse();
        }
        Ok(paginate(items, &params).to_json())
    })();
    json::respond(result, debug)
}

pub(crate) async fn create_distribution(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    body: web::Json<CreateDistributionRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<LicenseDistributionView> = (|| {
        require_control_plane(&req)?;
        require_caller_capability(&ctx, &req, "licensing:manage")?;
        let product = validate_product_slug(&body.product)?;
        let Some(settings) = product_settings_for(&product) else {
            return Err(UnifiedError::bad_request(format!(
                "Unknown license product \"{product}\": create the product first"
            )));
        };
        if !settings
            .allowed_distribution_channels
            .iter()
            .any(|channel| channel == DISTRIBUTION_CHANNEL_GIT)
        {
            return Err(UnifiedError::bad_request(format!(
                "License product \"{product}\" does not allow the git distribution channel"
            )));
        }
        let provider = validate_provider(&body.provider)?;
        let repo = validate_repo(&body.repo)?;
        let git_ref = body
            .git_ref
            .clone()
            .unwrap_or_else(|| "main".to_string())
            .trim()
            .to_string();
        if git_ref.is_empty() {
            return Err(UnifiedError::bad_request("git_ref must not be empty"));
        }
        if let Some(secret) = &body.webhook_secret {
            if secret.len() < 16 {
                return Err(UnifiedError::bad_request(
                    "webhook_secret must be at least 16 characters",
                ));
            }
        }
        let now = Utc::now();
        let record = LicenseDistribution {
            id: Uuid::now_v7(),
            product,
            provider,
            repo,
            git_ref,
            webhook_secret_hash: body
                .webhook_secret
                .as_ref()
                .map(|secret| sha256_hex(secret.as_bytes())),
            webhook_secret: body.webhook_secret.clone(),
            created_at: now,
            updated_at: now,
        };
        license_distributions_store().insert(record.id, record.clone());
        Ok(distribution_view(&record))
    })();
    match result {
        Ok(view) => json::success_response(view, 201),
        Err(error) => json::error_response(&error, debug),
    }
}

pub(crate) async fn get_distribution(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    id: web::Path<Uuid>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<LicenseDistributionView> = (|| {
        require_control_plane(&req)?;
        require_caller_capability(&ctx, &req, "licensing:view")?;
        license_distributions_store()
            .get(&id.into_inner())
            .map(|entry| distribution_view(entry.value()))
            .ok_or_else(|| UnifiedError::not_found("Distribution not found"))
    })();
    json::respond(result, debug)
}

pub(crate) async fn update_distribution(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    id: web::Path<Uuid>,
    body: web::Json<UpdateDistributionRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<LicenseDistributionView> = (|| {
        require_control_plane(&req)?;
        require_caller_capability(&ctx, &req, "licensing:manage")?;
        let id = id.into_inner();
        let mut record = license_distributions_store()
            .get(&id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Distribution not found"))?;
        if let Some(provider) = &body.provider {
            record.provider = validate_provider(provider)?;
        }
        if let Some(repo) = &body.repo {
            record.repo = validate_repo(repo)?;
        }
        if let Some(git_ref) = &body.git_ref {
            if git_ref.trim().is_empty() {
                return Err(UnifiedError::bad_request("git_ref must not be empty"));
            }
            record.git_ref = git_ref.trim().to_string();
        }
        if let Some(secret) = &body.webhook_secret {
            match secret {
                Some(secret) => {
                    if secret.len() < 16 {
                        return Err(UnifiedError::bad_request(
                            "webhook_secret must be at least 16 characters",
                        ));
                    }
                    record.webhook_secret_hash = Some(sha256_hex(secret.as_bytes()));
                    record.webhook_secret = Some(secret.clone());
                }
                None => {
                    record.webhook_secret_hash = None;
                    record.webhook_secret = None;
                }
            }
        }
        record.updated_at = Utc::now();
        license_distributions_store().insert(id, record.clone());
        Ok(distribution_view(&record))
    })();
    json::respond(result, debug)
}

pub(crate) async fn delete_distribution(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    id: web::Path<Uuid>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        require_control_plane(&req)?;
        require_caller_capability(&ctx, &req, "licensing:manage")?;
        let id = id.into_inner();
        if license_distributions_store().remove(&id).is_none() {
            return Err(UnifiedError::not_found("Distribution not found"));
        }
        Ok(serde_json::json!({ "deleted": true, "id": id }))
    })();
    json::respond(result, debug)
}

/// Verify a git provider webhook signature against the stored
/// secret, following the VCS module's verification rules
/// (`src/modules/vcs.rs`, `verify_webhook_signature`): GitHub uses
/// HMAC-SHA256 in `X-Hub-Signature-256` (`sha256=<hex>` over the raw
/// body); GitLab compares the shared secret in `X-Gitlab-Token`;
/// Bitbucket -- which provides no signature scheme -- compares the
/// shared secret in the custom `X-Webhook-Secret` header; `generic`
/// uses the same shared-secret header. The secret itself never
/// appears in any message.
pub(crate) fn verify_git_webhook_signature(
    provider: &str,
    req: &HttpRequest,
    body: &[u8],
    secret: &str,
) -> UnifiedResult<()> {
    let header_value = |name: &str| {
        req.headers()
            .get(name)
            .and_then(|value| value.to_str().ok())
            .unwrap_or("")
            .to_string()
    };
    match provider {
        "github" => {
            let provided = header_value("x-hub-signature-256");
            let provided = provided.strip_prefix("sha256=").ok_or_else(|| {
                UnifiedError::unauthorized("Webhook signature must use the sha256 scheme")
            })?;
            let mut mac = HmacSha256::new_from_slice(secret.as_bytes())
                .map_err(|_| UnifiedError::internal("Cannot initialise webhook verification"))?;
            mac.update(body);
            let expected: String = {
                const HEX: &[u8; 16] = b"0123456789abcdef";
                let tag = mac.finalize().into_bytes();
                let mut out = String::with_capacity(tag.len() * 2);
                for &byte in tag.iter() {
                    out.push(HEX[(byte >> 4) as usize] as char);
                    out.push(HEX[(byte & 0x0f) as usize] as char);
                }
                out
            };
            if !constant_time_equal(&expected, &provided.to_ascii_lowercase()) {
                return Err(UnifiedError::unauthorized(
                    "Webhook signature verification failed",
                ));
            }
            Ok(())
        }
        "gitlab" => {
            if !constant_time_equal(secret, &header_value("x-gitlab-token")) {
                return Err(UnifiedError::unauthorized(
                    "Webhook signature verification failed",
                ));
            }
            Ok(())
        }
        _ => {
            if !constant_time_equal(secret, &header_value("x-webhook-secret")) {
                return Err(UnifiedError::unauthorized(
                    "Webhook signature verification failed",
                ));
            }
            Ok(())
        }
    }
}

/// Detect the git provider from the webhook event headers (the same
/// headers the VCS module reads).
pub(crate) fn git_provider_from_headers(req: &HttpRequest) -> &'static str {
    if req.headers().contains_key("x-github-event") {
        "github"
    } else if req.headers().contains_key("x-gitlab-event") {
        "gitlab"
    } else if req.headers().contains_key("x-event-key") {
        "bitbucket"
    } else {
        "generic"
    }
}

pub(crate) fn git_event_from_headers(req: &HttpRequest, provider: &str) -> String {
    let header_name = match provider {
        "github" => "x-github-event",
        "gitlab" => "x-gitlab-event",
        "bitbucket" => "x-event-key",
        _ => "x-webhook-event",
    };
    req.headers()
        .get(header_name)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("")
        .to_string()
}

/// Confirm a download is licensed for a git-distributed product:
/// `GET /api/v1/licensing/distribute/git/{product}/access?license_key=...&instance_id=...`.
///
/// API-key authenticated (`licensing:read`). The license is checked
/// WITHOUT consuming a seat (the release download endpoint is the
/// one that activates). On success the response carries a signed,
/// expiring download link for the product's latest release, when a
/// release exists.
pub(crate) async fn git_access_handler(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    product: web::Path<String>,
    query: web::Query<GitAccessQuery>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let authenticated = authenticate_public_key(&req, "read")?;
        let product = product.into_inner().trim().to_lowercase();
        if let Some(settings) = product_settings_for(&product) {
            if let Some(owner_id) = settings.owner_id {
                if authenticated.owner_id != Some(owner_id) {
                    return Err(UnifiedError::forbidden(
                        "API key does not belong to this product owner",
                    ));
                }
            }
            if !settings
                .allowed_distribution_channels
                .iter()
                .any(|channel| channel == DISTRIBUTION_CHANNEL_GIT)
            {
                return Ok(serde_json::json!({
                    "allowed": false,
                    "reason": "channel_not_allowed",
                    "product": product,
                    "download_url": Value::Null,
                }));
            }
        }
        let outcome = validate_and_maybe_activate(
            &ctx,
            &query.license_key,
            Some(query.instance_id.trim()),
            query.hostname.as_deref(),
            false,
        )?;
        let reason = match (&outcome.valid, &outcome.reason) {
            (true, _) => "valid".to_string(),
            (false, Some(reason)) => match reason.as_str() {
                "not_found" => "not_found".to_string(),
                "invalid_checksum" => "invalid".to_string(),
                "seats_exceeded" => "seats-exceeded".to_string(),
                other => other.to_string(),
            },
            (false, None) => "invalid".to_string(),
        };
        let Some(license) = &outcome.license else {
            return Ok(serde_json::json!({
                "allowed": false,
                "reason": reason,
                "product": product,
                "download_url": Value::Null,
            }));
        };
        if license.product != product || !outcome.valid {
            let reason = if license.product != product {
                "product_mismatch".to_string()
            } else {
                reason
            };
            return Ok(serde_json::json!({
                "allowed": false,
                "reason": reason,
                "license_id": license.id,
                "product": license.product,
                "download_url": Value::Null,
            }));
        }
        let mut releases: Vec<LicenseRelease> = license_releases_store()
            .iter()
            .filter(|entry| entry.value().product == product)
            .map(|entry| entry.value().clone())
            .collect();
        releases.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        let (download_url, download_expires_at, release_id, release_version) =
            match releases.first() {
                Some(release) => {
                    let (url, expires_at) =
                        build_signed_download_url(&ctx, release.id, license.id);
                    (
                        serde_json::json!(url),
                        serde_json::json!(expires_at),
                        serde_json::json!(release.id),
                        serde_json::json!(release.version),
                    )
                }
                None => (
                    Value::Null,
                    Value::Null,
                    Value::Null,
                    Value::Null,
                ),
            };
        Ok(serde_json::json!({
            "allowed": true,
            "reason": "valid",
            "license_id": license.id,
            "product": license.product,
            "release_id": release_id,
            "release_version": release_version,
            "download_url": download_url,
            "download_expires_at": download_expires_at,
            "grace": outcome.grace,
            "days_in_grace": outcome.days_in_grace,
        }))
    })();
    json::respond(result, debug)
}

/// Git provider webhook receiver:
/// `POST /api/v1/licensing/distribute/git/{product}/webhook`.
/// Authenticated by the provider signature against the stored
/// distribution secret (no API key: the webhook comes from the
/// provider). Records the release event and emits
/// `license.distribution.release`; it never serves downloads.
pub(crate) async fn git_webhook_handler(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    product: web::Path<String>,
    body: web::Bytes,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let product = product.into_inner().trim().to_lowercase();
        let provider = git_provider_from_headers(&req);
        let distribution = license_distributions_store()
            .iter()
            .find(|entry| {
                entry.value().product == product && entry.value().provider == provider
            })
            .map(|entry| entry.value().clone())
            .ok_or_else(|| {
                UnifiedError::not_found(format!(
                    "No {provider} distribution configured for product \"{product}\""
                ))
            })?;
        let secret = distribution.webhook_secret.clone().ok_or_else(|| {
            UnifiedError::unauthorized(
                "No webhook secret configured for this distribution",
            )
        })?;
        verify_git_webhook_signature(provider, &req, &body, &secret)?;
        let payload: Value = serde_json::from_slice(&body).map_err(|_| {
            UnifiedError::bad_request("Webhook body is not valid JSON")
        })?;
        let event = git_event_from_headers(&req, provider);
        let tag = match provider {
            "github" => payload
                .get("release")
                .and_then(|release| release.get("tag_name"))
                .and_then(|value| value.as_str())
                .map(str::to_string)
                .or_else(|| {
                    payload
                        .get("ref")
                        .and_then(|value| value.as_str())
                        .map(|value| value.trim_start_matches("refs/tags/").to_string())
                }),
            "gitlab" => payload
                .get("ref")
                .and_then(|value| value.as_str())
                .map(|value| value.trim_start_matches("refs/tags/").to_string()),
            _ => payload
                .get("tag")
                .and_then(|value| value.as_str())
                .map(str::to_string)
                .or_else(|| {
                    payload
                        .get("version")
                        .and_then(|value| value.as_str())
                        .map(str::to_string)
                }),
        };
        ctx.hooks.emit(
            LICENSE_DISTRIBUTION_RELEASE,
            serde_json::json!({
                "product": product,
                "provider": provider,
                "repo": distribution.repo,
                "git_ref": distribution.git_ref,
                "event": event,
                "tag": tag,
            }),
        );
        Ok(serde_json::json!({
            "received": true,
            "product": product,
            "provider": provider,
            "event": event,
            "tag": tag,
        }))
    })();
    json::respond(result, debug)
}

#[cfg(test)]
mod tests {
    use super::*;
    use actix_web::test::TestRequest;

    #[test]
    fn verify_git_webhook_signature_accepts_rfc4231_vector() {
        // RFC 4231 test case 2: key "Jefe", data
        // "what do ya want for nothing?".
        let body = b"what do ya want for nothing?";
        let request = TestRequest::default()
            .insert_header((
                "x-hub-signature-256",
                "sha256=5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843",
            ))
            .to_http_request();
        assert!(verify_git_webhook_signature("github", &request, body, "Jefe").is_ok());
        let tampered = TestRequest::default()
            .insert_header((
                "x-hub-signature-256",
                "sha256=5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3842",
            ))
            .to_http_request();
        assert!(verify_git_webhook_signature("github", &tampered, body, "Jefe").is_err());
    }
}
