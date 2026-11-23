// Third-party VCS integrations — Version draft, sync history, and asset download handlers.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::{events, HookBus, HookContext, HookHandler};
use unified_core::json;
use unified_core::modules::{Module, ModuleRegistry, RouteInfo};
use unified_core::pagination::{PageResult, PaginationParams};
use actix_web::{web, HttpRequest, HttpResponse};
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use futures_util::StreamExt;
use hmac::{Hmac, Mac};
use reqwest::header;
use semver::Version;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::net::IpAddr;
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};
use uuid::Uuid;

use super::constants::*;
use super::types::*;
use super::providers::*;
use super::store::*;
use super::helpers::*;
use super::validation::*;
use super::service::*;
use super::handlers_providers::*;
use super::handlers_connections::*;
use super::webhooks::*;
use super::module::*;

pub(crate) async fn list_sync_history(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<VcsState>>,
    req: HttpRequest,
    params: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return json::error_response(&error, debug),
    };
    let _company = match caller_company(&tenant, &req) {
        Ok(company) => company,
        Err(error) => return json::error_response(&error, debug),
    };
    if let Err(error) = authorize(&ctx, &req, PERM_VCS_VIEW) {
        return json::error_response(&error, debug);
    }
    let result: UnifiedResult<Value> =
        list_sync_history_filtered(&state, &params.into_inner(), None);
    json::respond(result, debug)
}

// ---------------------------------------------------------------------------
// Draft handlers and release-asset download.
// ---------------------------------------------------------------------------

pub(crate) async fn list_drafts(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<VcsState>>,
    req: HttpRequest,
    params: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return json::error_response(&error, debug),
    };
    let _company = match caller_company(&tenant, &req) {
        Ok(company) => company,
        Err(error) => return json::error_response(&error, debug),
    };
    if let Err(error) = authorize(&ctx, &req, PERM_VCS_VIEW) {
        return json::error_response(&error, debug);
    }
    let result: UnifiedResult<Value> = (|| {
        let params = params.into_inner();
        let sort = params.allowed_sort(&["provider", "version", "tag", "created_at"], "created_at");
        let mut items: Vec<VersionDraft> = state
            .drafts
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|draft| {
                matches_search(
                    &format!("{} {} {} {}", draft.version, draft.tag, draft.changelog, draft.software_id),
                    &params,
                ) && params
                    .filters
                    .get("provider")
                    .is_none_or(|provider| draft.provider.as_str() == provider)
                    && params
                        .filters
                        .get("connection_id")
                        .is_none_or(|id| draft.connection_id.to_string() == *id)
                    && params
                        .filters
                        .get("software_id")
                        .is_none_or(|software_id| draft.software_id == *software_id)
                    && params
                        .filters
                        .get("status")
                        .is_none_or(|status| draft.status == *status)
            })
            .collect();
        match sort.as_str() {
            "provider" => items.sort_by(|a, b| a.provider.as_str().cmp(b.provider.as_str())),
            "version" => items.sort_by(|a, b| {
                match (Version::parse(&a.version), Version::parse(&b.version)) {
                    (Ok(left), Ok(right)) => left.cmp(&right),
                    _ => a.version.cmp(&b.version),
                }
            }),
            "tag" => items.sort_by(|a, b| a.tag.cmp(&b.tag)),
            _ => items.sort_by(|a, b| a.created_at.cmp(&b.created_at)),
        }
        if params.normalized().order == "desc" {
            items.reverse();
        }
        Ok(paginate(items, &params).to_json())
    })();
    json::respond(result, debug)
}

pub(crate) async fn get_draft(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<VcsState>>,
    req: HttpRequest,
    id: web::Path<Uuid>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return json::error_response(&error, debug),
    };
    let _company = match caller_company(&tenant, &req) {
        Ok(company) => company,
        Err(error) => return json::error_response(&error, debug),
    };
    if let Err(error) = authorize(&ctx, &req, PERM_VCS_VIEW) {
        return json::error_response(&error, debug);
    }
    let result: UnifiedResult<VersionDraft> = state
        .drafts
        .get(&id.into_inner())
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found("Version draft not found"));
    json::respond(result, debug)
}

pub(crate) async fn delete_draft(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<VcsState>>,
    req: HttpRequest,
    id: web::Path<Uuid>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return json::error_response(&error, debug),
    };
    let _company = match require_caller_company(&tenant, &req) {
        Ok(company) => company,
        Err(error) => return json::error_response(&error, debug),
    };
    if let Err(error) = authorize(&ctx, &req, PERM_VCS_MANAGE) {
        return json::error_response(&error, debug);
    }
    let result: UnifiedResult<Value> = (|| {
        let id = id.into_inner();
        state
            .drafts
            .remove(&id)
            .map(|_| json!({ "deleted": true, "id": id }))
            .ok_or_else(|| UnifiedError::not_found("Version draft not found"))
    })();
    json::respond(result, debug)
}

/// Download one release asset through the allowlist, verify its
/// SHA-256, and store the reference (redacted URL, size, checksum) on
/// the draft. The bytes themselves are discarded after hashing; the
/// draft keeps only the reference. Redirect targets are re-validated
/// against the same allowlist.
pub(crate) async fn download_draft_asset(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<VcsState>>,
    req: HttpRequest,
    id: web::Path<Uuid>,
    body: web::Json<DownloadAssetRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return json::error_response(&error, debug),
    };
    let _company = match require_caller_company(&tenant, &req) {
        Ok(company) => company,
        Err(error) => return json::error_response(&error, debug),
    };
    if let Err(error) = authorize(&ctx, &req, PERM_VCS_MANAGE) {
        return json::error_response(&error, debug);
    }
    let result: UnifiedResult<DraftAsset> = (|| async {
        let draft_id = id.into_inner();
        let draft = state
            .drafts
            .get(&draft_id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Version draft not found"))?;
        let input = body.into_inner();
        let name = input.name.trim().to_string();
        if name.is_empty() || name.len() > 256 || name.contains('/') || name.contains('\\') {
            return Err(UnifiedError::bad_request(
                "Asset name is required, must be a plain file name, and must be at most 256 characters",
            ));
        }
        let gitlab_hosts = state.gitlab_hosts(draft.provider);
        let mut url = validate_asset_url(&input.url, &gitlab_hosts)?;
        let settings = state.provider_settings(draft.provider);
        let client = reqwest::Client::builder()
            .user_agent(USER_AGENT)
            .timeout(Duration::from_secs(HTTP_TIMEOUT_SECS))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|error| {
                UnifiedError::internal(format!("Cannot build HTTP client: {error}"))
            })?;
        let mut hasher = Sha256::new();
        let mut downloaded: u64 = 0;
        let mut redirects: u32 = 0;
        loop {
            let host = url
                .host_str()
                .unwrap_or("")
                .to_ascii_lowercase();
            let is_api_host = matches!(
                host.as_str(),
                "api.github.com" | "api.bitbucket.org" | "gitlab.com"
            ) || gitlab_hosts
                .iter()
                .any(|allowed| allowed.to_ascii_lowercase() == host);
            let mut request = client.get(url.clone());
            if is_api_host {
                if let Some(token) = settings.token.as_deref().filter(|token| !token.is_empty()) {
                    request = request.header(
                        draft.provider.auth_header_name(),
                        draft.provider.auth_header_value(token),
                    );
                }
            }
            let response = request
                .send()
                .await
                .map_err(|error| {
                    UnifiedError::internal(format!("Asset download failed: {error}"))
                })?;
            let status = response.status();
            if status.is_redirection() {
                if redirects >= MAX_ASSET_REDIRECTS {
                    return Err(UnifiedError::bad_request(
                        "Asset download followed too many redirects",
                    ));
                }
                let location = response
                    .headers()
                    .get(header::LOCATION)
                    .and_then(|value| value.to_str().ok())
                    .ok_or_else(|| {
                        UnifiedError::bad_request("Asset download redirect has no location")
                    })?
                    .to_string();
                let next = url.join(&location).map_err(|_| {
                    UnifiedError::bad_request("Asset download redirect URL is invalid")
                })?;
                // Re-validate every redirect target against the
                // allowlist before following it.
                url = validate_asset_url(next.as_str(), &gitlab_hosts)?;
                redirects += 1;
                continue;
            }
            if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
                let retry_after = response
                    .headers()
                    .get(header::RETRY_AFTER)
                    .and_then(|value| value.to_str().ok())
                    .and_then(|value| value.trim().parse::<u64>().ok());
                return Err(UnifiedError::new(
                    "Asset host rate limit reached; back off before retrying",
                    429,
                )
                .with_metadata(
                    "retry_after_seconds",
                    retry_after.map(Value::from).unwrap_or(Value::Null),
                ));
            }
            if !status.is_success() {
                return Err(UnifiedError::internal(format!(
                    "Asset download failed with HTTP {status}"
                )));
            }
            if let Some(total) = response.content_length() {
                if total > MAX_ASSET_BYTES {
                    return Err(UnifiedError::unprocessable(format!(
                        "Asset size {total} bytes exceeds the maximum of {MAX_ASSET_BYTES} bytes"
                    )));
                }
            }
            let mut stream = response.bytes_stream();
            while let Some(chunk) = stream.next().await {
                let chunk = chunk.map_err(|error| {
                    UnifiedError::internal(format!("Asset download failed: {error}"))
                })?;
                downloaded += chunk.len() as u64;
                if downloaded > MAX_ASSET_BYTES {
                    return Err(UnifiedError::unprocessable(format!(
                        "Asset exceeds the maximum of {MAX_ASSET_BYTES} bytes"
                    )));
                }
                hasher.update(&chunk);
            }
            break;
        }
        let sha256 = hex_encode(&hasher.finalize());
        let asset = DraftAsset {
            name,
            url: redact_url(url.as_str()).unwrap_or_else(|| {
                // The URL already passed validation, so redaction only
                // fails in theory; fall back to scheme + host + path so
                // a signed query string can never be stored.
                format!(
                    "{}://{}{}",
                    url.scheme(),
                    url.host_str().unwrap_or(""),
                    url.path()
                )
            }),
            size_bytes: Some(downloaded),
            sha256: Some(sha256),
            downloaded_at: Some(now()),
        };
        if let Some(mut entry) = state.drafts.get_mut(&draft_id) {
            entry.assets.push(asset.clone());
            entry.updated_at = now();
        }
        Ok(asset)
    })()
    .await;
    match result {
        Ok(asset) => json::success_response(asset, 201),
        Err(error) => json::error_response(&error, debug),
    }
}
