// Third-party VCS integrations - Provider and provider-settings handlers.

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
use super::handlers_connections::*;
use super::handlers_drafts::*;
use super::webhooks::*;
use super::module::*;

/// Fetch one page from a provider API path. Returns the parsed JSON
/// body together with the rate-limit state the provider reported.
/// Provider failures are mapped to the standard error envelope; the
/// credential is sent only as a header and never appears in any
/// message.
pub(crate) async fn provider_get(
    state: &VcsState,
    provider: Provider,
    connection: &RepositoryConnection,
    path: &str,
    page: u32,
    per_page: u32,
) -> UnifiedResult<(Value, RateLimitInfo)> {
    let settings = state.provider_settings(provider);
    let base = provider.api_base(Some(&settings), connection.host.as_deref());
    let separator = if path.contains('?') { "&" } else { "?" };
    let (page_param, per_page_param) = match provider {
        // Bitbucket uses `pagelen` instead of `per_page`.
        Provider::Bitbucket => ("page", "pagelen"),
        _ => ("page", "per_page"),
    };
    let url = format!("{base}{path}{separator}{page_param}={page}&{per_page_param}={per_page}");
    let client = build_client()?;
    let mut request = client.get(url);
    if let Some(token) = settings.token.as_deref().filter(|token| !token.is_empty()) {
        request = request.header(provider.auth_header_name(), provider.auth_header_value(token));
    }
    match provider {
        Provider::GitHub => {
            request = request.header(header::ACCEPT, "application/vnd.github+json");
        }
        _ => {}
    }
    let response = request
        .send()
        .await
        .map_err(|error| UnifiedError::internal(format!("Provider request failed: {error}")))?;
    let limits = rate_limit_from_headers(response.headers());
    record_rate_limit(state, &rate_limit_key(provider, connection.host.as_deref()), &limits);
    let status = response.status();
    if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
        return Err(UnifiedError::new(
            "Provider rate limit reached; back off before retrying",
            429,
        )
        .with_metadata(
            "retry_after_seconds",
            limits
                .retry_after_seconds
                .map(Value::from)
                .unwrap_or(Value::Null),
        )
        .with_metadata(
            "rate_limit_remaining",
            limits.remaining.map(Value::from).unwrap_or(Value::Null),
        ));
    }
    if status == reqwest::StatusCode::NOT_FOUND {
        return Err(UnifiedError::not_found(format!(
            "Repository \"{}/{}\" was not found on {}",
            connection.owner,
            connection.repo,
            provider.display_name()
        )));
    }
    if status == reqwest::StatusCode::UNAUTHORIZED || status == reqwest::StatusCode::FORBIDDEN {
        return Err(UnifiedError::unauthorized(format!(
            "Provider authentication failed for {} (check the configured token)",
            provider.display_name()
        )));
    }
    if !status.is_success() {
        return Err(UnifiedError::internal(format!(
            "Provider request failed with HTTP {status}"
        )));
    }
    let body: Value = response
        .json()
        .await
        .map_err(|error| UnifiedError::internal(format!("Provider returned invalid JSON: {error}")))?;
    Ok((body, limits))
}

/// List releases for a connection (paginated). When the provider has
/// no releases, tags are listed instead so a tag-only repository can
/// still be imported. Bitbucket lists tags directly.
pub(crate) async fn fetch_releases(
    state: &VcsState,
    connection: &RepositoryConnection,
    page: u32,
    per_page: u32,
) -> UnifiedResult<ProviderReleases> {
    let provider = connection.provider;
    check_rate_limit(state, &rate_limit_key(provider, connection.host.as_deref()))?;
    if provider == Provider::Bitbucket {
        let path = provider.tags_path(&connection.owner, &connection.repo);
        let (body, limits) =
            provider_get(state, provider, connection, &path, page, per_page).await?;
        return Ok(ProviderReleases {
            items: parse_tag_list(provider, &body),
            rate_limit: limits,
        });
    }
    let releases_path = provider.releases_path(&connection.owner, &connection.repo);
    let (body, limits) =
        provider_get(state, provider, connection, &releases_path, page, per_page).await?;
    let items = parse_release_list(provider, &body);
    if !items.is_empty() {
        return Ok(ProviderReleases {
            items,
            rate_limit: limits,
        });
    }
    let tags_path = provider.tags_path(&connection.owner, &connection.repo);
    let (tag_body, tag_limits) =
        provider_get(state, provider, connection, &tags_path, page, per_page).await?;
    Ok(ProviderReleases {
        items: parse_tag_list(provider, &tag_body),
        rate_limit: tag_limits,
    })
}

pub(crate) async fn list_providers(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<VcsState>>,
    req: HttpRequest,
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
    let providers = [Provider::GitHub, Provider::GitLab, Provider::Bitbucket]
        .iter()
        .map(|provider| provider_view(&state, *provider))
        .collect::<Vec<ProviderView>>();
    json::respond(UnifiedResult::Ok(providers), debug)
}

pub(crate) async fn get_provider(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<VcsState>>,
    req: HttpRequest,
    path: web::Path<String>,
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
    let result: UnifiedResult<ProviderView> =
        Provider::parse(&path.into_inner()).map(|provider| provider_view(&state, provider));
    json::respond(result, debug)
}

pub(crate) async fn get_provider_settings(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<VcsState>>,
    req: HttpRequest,
    path: web::Path<String>,
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
    let result: UnifiedResult<ProviderSettingsView> = (|| {
        let provider = Provider::parse(&path.into_inner())?;
        let settings = state.provider_settings(provider);
        Ok(ProviderSettingsView {
            provider: provider.as_str().to_string(),
            configured: settings
                .token
                .as_deref()
                .is_some_and(|token| !token.is_empty()),
            webhook_configured: settings
                .webhook_secret
                .as_deref()
                .is_some_and(|secret| !secret.is_empty()),
            api_base: provider.api_base(Some(&settings), None),
            gitlab_hosts: settings.gitlab_hosts.clone(),
        })
    })();
    json::respond(result, debug)
}

pub(crate) async fn update_provider_settings(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<VcsState>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdateProviderSettings>,
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
    let result: UnifiedResult<ProviderSettingsView> = (|| {
        let provider = Provider::parse(&path.into_inner())?;
        let input = body.into_inner();
        if input.token.is_none()
            && input.webhook_secret.is_none()
            && input.api_base_override.is_none()
            && input.gitlab_hosts.is_none()
        {
            return Err(UnifiedError::bad_request(
                "No settings provided (expected token, webhook_secret, api_base_override, or gitlab_hosts)",
            ));
        }
        if let Some(base) = &input.api_base_override {
            if !base.trim().is_empty() {
                validate_api_base_override(base.trim())?;
            }
        }
        if let Some(hosts) = &input.gitlab_hosts {
            for host in hosts {
                if !valid_host(host) {
                    return Err(UnifiedError::bad_request(format!(
                        "GitLab host \"{host}\" is not a valid hostname"
                    )));
                }
            }
        }
        let mut settings = state.provider_settings(provider);
        if let Some(token) = input.token {
            settings.token = if token.trim().is_empty() {
                None
            } else {
                Some(token.trim().to_string())
            };
        }
        if let Some(secret) = input.webhook_secret {
            settings.webhook_secret = if secret.trim().is_empty() {
                None
            } else {
                Some(secret)
            };
        }
        if let Some(base) = input.api_base_override {
            settings.api_base_override = if base.trim().is_empty() {
                None
            } else {
                Some(base.trim().to_string())
            };
        }
        if let Some(hosts) = input.gitlab_hosts {
            settings.gitlab_hosts = hosts
                .into_iter()
                .map(|host| host.trim().to_ascii_lowercase())
                .filter(|host| !host.is_empty())
                .collect();
        }
        let configured = settings
            .token
            .as_deref()
            .is_some_and(|token| !token.is_empty());
        let webhook_configured = settings
            .webhook_secret
            .as_deref()
            .is_some_and(|secret| !secret.is_empty());
        state
            .provider_settings
            .insert(provider.as_str().to_string(), settings.clone());
        // Merge into the settings system under `vcs.<provider>` without
        // ever carrying the credential value: only flags are emitted.
        ctx.hooks.emit(
            "settings.merge",
            json!({
                "namespace": format!("vcs.{}", provider.as_str()),
                "settings": {
                    "token_configured": configured,
                    "webhook_configured": webhook_configured,
                    "api_base": provider.api_base(Some(&settings), None),
                    "gitlab_hosts": settings.gitlab_hosts,
                },
            }),
        );
        ctx.hooks.emit(
            "vcs.provider.settings.updated",
            json!({
                "provider": provider.as_str(),
                "configured": configured,
                "webhook_configured": webhook_configured,
            }),
        );
        Ok(ProviderSettingsView {
            provider: provider.as_str().to_string(),
            configured,
            webhook_configured,
            api_base: provider.api_base(Some(&settings), None),
            gitlab_hosts: settings.gitlab_hosts.clone(),
        })
    })();
    match result {
        Ok(view) => json::success_response(view, 200),
        Err(error) => json::error_response(&error, debug),
    }
}

pub(crate) async fn import_release(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<VcsState>>,
    req: HttpRequest,
    id: web::Path<Uuid>,
    body: web::Json<ImportRelease>,
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
    let result: UnifiedResult<VersionDraft> = (|| async {
        let connection = find_connection(&state, id.into_inner())?;
        if !connection.enabled {
            return Err(UnifiedError::conflict(
                "Repository connection is disabled",
            ));
        }
        let input = body.into_inner();
        let tag = input.tag.trim().to_string();
        if tag.is_empty() || tag.len() > MAX_SLUG_LEN {
            return Err(UnifiedError::bad_request("tag is required"));
        }
        if let Some(version) = &input.version {
            validate_version_string(version)?;
        }
        // Fetch the provider list so the import only succeeds for a
        // release or tag that really exists on the provider.
        let releases = fetch_releases(&state, &connection, 1, 100).await?;
        let release = releases
            .items
            .iter()
            .find(|release| release.tag == tag)
            .ok_or_else(|| {
                UnifiedError::not_found(format!(
                    "Release or tag \"{tag}\" was not found on {}",
                    connection.provider.display_name()
                ))
            })?
            .clone();
        create_draft(
            &ctx.hooks,
            &state,
            &connection,
            &release,
            "import",
            input.version,
            input.notes,
        )
    })()
    .await;
    match result {
        Ok(draft) => json::success_response(draft, 201),
        Err(error) => json::error_response(&error, debug),
    }
}
