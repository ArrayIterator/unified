// Third-party VCS integrations - Inbound webhook verification and handling.

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
use super::handlers_drafts::*;
use super::module::*;

// Webhook receivers.

/// Verify a webhook signature for the provider. Returns 401 when no
/// secret is configured or the payload cannot be verified. The secret
/// itself never appears in any message.
pub(crate) fn verify_webhook_signature(
    provider: Provider,
    req: &HttpRequest,
    body: &[u8],
    secret: &str,
) -> UnifiedResult<()> {
    let header_name = provider.webhook_signature_header();
    let header_value = req
        .headers()
        .get(header_name)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("");
    if header_value.is_empty() {
        return Err(UnifiedError::unauthorized(format!(
            "Webhook signature header \"{header_name}\" is missing"
        )));
    }
    match provider {
        Provider::GitHub => {
            let provided = header_value
                .strip_prefix("sha256=")
                .ok_or_else(|| {
                    UnifiedError::unauthorized(
                        "Webhook signature must use the sha256 scheme",
                    )
                })?
                .to_ascii_lowercase();
            let mut mac = HmacSha256::new_from_slice(secret.as_bytes())
                .map_err(|_| UnifiedError::internal("Cannot initialise webhook verification"))?;
            mac.update(body);
            let expected = hex_encode(&mac.finalize().into_bytes());
            if !secrets_equal(&expected, &provided) {
                return Err(UnifiedError::unauthorized(
                    "Webhook signature verification failed",
                ));
            }
            Ok(())
        }
        // GitLab and Bitbucket compare a shared secret header in
        // constant time.
        Provider::GitLab | Provider::Bitbucket => {
            if !secrets_equal(secret, header_value) {
                return Err(UnifiedError::unauthorized(
                    "Webhook signature verification failed",
                ));
            }
            Ok(())
        }
    }
}

/// Split a `owner/repo` full name. For GitLab the owner part may
/// contain subgroup slashes, so the split is at the last `/`.
pub(crate) fn split_full_name(full_name: &str) -> Option<(String, String)> {
    let (owner, repo) = full_name.rsplit_once('/')?;
    if owner.is_empty() || repo.is_empty() {
        return None;
    }
    Some((owner.to_string(), repo.to_string()))
}

pub(crate) fn webhook_event_name(provider: Provider, req: &HttpRequest) -> String {
    let header_name = match provider {
        Provider::GitHub => "x-github-event",
        Provider::GitLab => "x-gitlab-event",
        Provider::Bitbucket => "x-event-key",
    };
    req.headers()
        .get(header_name)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("")
        .to_string()
}

/// Extract a release from a verified webhook payload. Returns `None`
/// for events that carry no release or tag (branch pushes, pings,
/// edits), so those are acknowledged without creating a draft.
pub(crate) fn release_from_webhook(provider: Provider, event: &str, payload: &Value) -> Option<ProviderRelease> {
    match provider {
        Provider::GitHub => match event {
            "push" => {
                // Deleting a tag sends a push with `deleted: true`; it
                // creates no release, so there is nothing to import.
                if payload.get("deleted").and_then(|value| value.as_bool()) == Some(true) {
                    return None;
                }
                let reference = payload.get("ref").and_then(|value| value.as_str())?;
                let tag = reference.strip_prefix("refs/tags/")?;
                let notes = payload
                    .get("head_commit")
                    .and_then(|commit| commit.get("message"))
                    .and_then(|message| message.as_str())
                    .unwrap_or("")
                    .chars()
                    .take(MAX_NOTES_LEN)
                    .collect::<String>();
                Some(ProviderRelease {
                    version: version_from_tag(tag).ok().map(|version| version.to_string()),
                    tag: tag.to_string(),
                    name: None,
                    notes,
                    published_at: None,
                    prerelease: false,
                    assets: Vec::new(),
                })
            }
            "release" => {
                let action = payload.get("action").and_then(|value| value.as_str())?;
                if !matches!(action, "published" | "released") {
                    return None;
                }
                let release = payload.get("release")?;
                let tag = string_field(release, "tag_name")?;
                let assets = release
                    .get("assets")
                    .and_then(|assets| assets.as_array())
                    .map(|assets| {
                        assets
                            .iter()
                            .filter_map(|asset| {
                                Some(ProviderAsset {
                                    name: string_field(asset, "name")?,
                                    url: string_field(asset, "browser_download_url")
                                        .or_else(|| string_field(asset, "url"))?,
                                    size: asset.get("size").and_then(|size| size.as_u64()),
                                })
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                Some(ProviderRelease {
                    version: version_from_tag(&tag).ok().map(|version| version.to_string()),
                    tag,
                    name: string_field(release, "name"),
                    notes: string_field(release, "body").unwrap_or_default(),
                    published_at: parse_datetime(
                        release.get("published_at").and_then(|field| field.as_str()),
                    ),
                    prerelease: release
                        .get("prerelease")
                        .and_then(|field| field.as_bool())
                        .unwrap_or(false),
                    assets,
                })
            }
            _ => None,
        },
        Provider::GitLab => match event {
            "Push Hook" | "Tag Push Hook" => {
                let reference = payload.get("ref").and_then(|value| value.as_str())?;
                let tag = reference.strip_prefix("refs/tags/")?;
                Some(ProviderRelease {
                    version: version_from_tag(tag).ok().map(|version| version.to_string()),
                    tag: tag.to_string(),
                    name: None,
                    notes: String::new(),
                    published_at: None,
                    prerelease: false,
                    assets: Vec::new(),
                })
            }
            "Release Hook" => {
                if payload
                    .get("object_kind")
                    .and_then(|value| value.as_str())
                    != Some("release")
                {
                    return None;
                }
                let tag = string_field(payload, "tag")?;
                let assets = payload
                    .get("assets")
                    .and_then(|assets| assets.get("links"))
                    .and_then(|links| links.as_array())
                    .map(|links| {
                        links
                            .iter()
                            .filter_map(|link| {
                                Some(ProviderAsset {
                                    name: string_field(link, "name")?,
                                    url: string_field(link, "url")?,
                                    size: None,
                                })
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                Some(ProviderRelease {
                    version: version_from_tag(&tag).ok().map(|version| version.to_string()),
                    tag,
                    name: string_field(payload, "name"),
                    notes: string_field(payload, "description").unwrap_or_default(),
                    published_at: parse_datetime(
                        payload.get("released_at").and_then(|field| field.as_str()),
                    ),
                    prerelease: false,
                    assets,
                })
            }
            _ => None,
        },
        Provider::Bitbucket => {
            if event != "repo:push" {
                return None;
            }
            let changes = payload
                .get("push")
                .and_then(|push| push.get("changes"))
                .and_then(|changes| changes.as_array())?;
            let tag_change = changes.iter().find(|change| {
                change
                    .get("new")
                    .and_then(|new| new.get("type"))
                    .and_then(|kind| kind.as_str())
                    == Some("tag")
            })?;
            let tag = tag_change
                .get("new")
                .and_then(|new| new.get("name"))
                .and_then(|name| name.as_str())?;
            Some(ProviderRelease {
                version: version_from_tag(tag).ok().map(|version| version.to_string()),
                tag: tag.to_string(),
                name: None,
                notes: String::new(),
                published_at: None,
                prerelease: false,
                assets: Vec::new(),
            })
        }
    }
}

pub(crate) async fn handle_webhook(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<VcsState>>,
    provider: Provider,
    req: HttpRequest,
    body: web::Bytes,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let settings = state.provider_settings(provider);
        let secret = settings
            .webhook_secret
            .clone()
            .filter(|secret| !secret.is_empty())
            .ok_or_else(|| {
                UnifiedError::unauthorized(format!(
                    "No webhook secret is configured for {}",
                    provider.display_name()
                ))
            })?;
        verify_webhook_signature(provider, &req, &body, &secret)?;
        let payload: Value = serde_json::from_slice(&body).map_err(|_| {
            UnifiedError::bad_request("Webhook payload is not valid JSON")
        })?;
        // Resolve the repository the event belongs to.
        let full_name = match provider {
            Provider::GitHub | Provider::Bitbucket => payload
                .get("repository")
                .and_then(|repository| repository.get("full_name"))
                .and_then(|name| name.as_str())
                .map(str::to_string),
            Provider::GitLab => payload
                .get("project")
                .and_then(|project| project.get("path_with_namespace"))
                .and_then(|name| name.as_str())
                .map(str::to_string),
        };
        let Some(full_name) = full_name else {
            return Err(UnifiedError::bad_request(
                "Webhook payload does not identify a repository",
            ));
        };
        let Some((owner, repo)) = split_full_name(&full_name) else {
            return Err(UnifiedError::bad_request(
                "Webhook payload repository name is invalid",
            ));
        };
        let Some(connection) = find_connection_by_repo(&state, provider, &owner, &repo) else {
            return Ok(json!({
                "accepted": true,
                "matched": false,
                "draft_created": false,
            }));
        };
        let event = webhook_event_name(provider, &req);
        let Some(release) = release_from_webhook(provider, &event, &payload) else {
            return Ok(json!({
                "accepted": true,
                "matched": true,
                "draft_created": false,
                "reason": "event does not carry a release or tag",
            }));
        };
        match create_draft(&ctx.hooks, &state, &connection, &release, "webhook", None, None) {
            Ok(draft) => {
                ctx.hooks.emit(
                    "notification.requested",
                    json!({
                        "kind": "vcs.release.imported",
                        "title": format!(
                            "New {} release {} for {}/{}",
                            provider.display_name(),
                            draft.tag,
                            connection.owner,
                            connection.repo
                        ),
                        "body": format!(
                            "Version {} was imported as a draft for review. It was not published.",
                            draft.version
                        ),
                        "user_id": "",
                    }),
                );
                Ok(json!({
                    "accepted": true,
                    "matched": true,
                    "draft_created": true,
                    "draft_id": draft.id,
                }))
            }
            // A redelivered webhook for a tag that already has a draft
            // is acknowledged, not an error.
            Err(error) if error.code == 409 => Ok(json!({
                "accepted": true,
                "matched": true,
                "draft_created": false,
                "reason": "a draft already exists for this tag",
            })),
            Err(error) => Err(error),
        }
    })();
    json::respond(result, debug)
}

pub(crate) async fn github_webhook(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<VcsState>>,
    req: HttpRequest,
    body: web::Bytes,
) -> HttpResponse {
    handle_webhook(ctx, state, Provider::GitHub, req, body).await
}

pub(crate) async fn gitlab_webhook(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<VcsState>>,
    req: HttpRequest,
    body: web::Bytes,
) -> HttpResponse {
    handle_webhook(ctx, state, Provider::GitLab, req, body).await
}

pub(crate) async fn bitbucket_webhook(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<VcsState>>,
    req: HttpRequest,
    body: web::Bytes,
) -> HttpResponse {
    handle_webhook(ctx, state, Provider::Bitbucket, req, body).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn github_tag_deletion_creates_no_release() {
        let payload = json!({
            "ref": "refs/tags/v1.2.3",
            "deleted": true,
            "head_commit": { "message": "remove the tag" },
        });
        assert!(release_from_webhook(Provider::GitHub, "push", &payload).is_none());
    }

    #[test]
    fn github_tag_push_still_creates_a_release() {
        let payload = json!({
            "ref": "refs/tags/v1.2.3",
            "deleted": false,
            "head_commit": { "message": "release notes" },
        });
        let release = release_from_webhook(Provider::GitHub, "push", &payload).unwrap();
        assert_eq!(release.tag, "v1.2.3");
    }

    #[test]
    fn github_tag_push_without_deleted_flag_creates_a_release() {
        let payload = json!({
            "ref": "refs/tags/v1.2.3",
            "head_commit": { "message": "release notes" },
        });
        assert!(release_from_webhook(Provider::GitHub, "push", &payload).is_some());
    }
}
