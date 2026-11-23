// Third-party VCS integrations module.
//
// Provider abstraction for GitHub, GitLab, and Bitbucket: repository
// connections linked to software records, release/tag import as review
// drafts (never auto-published), signed webhook receivers, allowlisted
// release-asset downloads, manual sync with rate-limit handling, and
// sync history. Everything is emitted through the hook bus; nothing in
// this module calls another module directly.
//
// Settings keys read by this module (namespace `vcs`, managed through
// the settings system; the module also emits `settings.merge` so the
// settings module can persist them under `vcs.<provider>`):
//
//   - `vcs.github.token` / `vcs.gitlab.token` / `vcs.bitbucket.token`
//     (write-only personal access / OAuth token, never returned, never
//     logged)
//   - `vcs.github.webhook_secret` / `vcs.gitlab.webhook_secret` /
//     `vcs.bitbucket.webhook_secret` (shared secret used to verify
//     inbound webhooks)
//   - `vcs.github.api_base` / `vcs.gitlab.api_base` /
//     `vcs.bitbucket.api_base` (optional API base URL override)
//   - `vcs.gitlab.hosts` (JSON array of extra allowed self-hosted
//     GitLab hostnames, used both as API hosts and as allowed asset
//     download hosts)
//
// Persistence: this module keeps its records in memory (the same
// pattern as the software module). The wiring builder adds the
// Diesel tables listed at the bottom of this file's report note:
// `vcs_connections`, `vcs_version_drafts`, `vcs_sync_history`, and
// `vcs_provider_settings`.
//
// Security notes:
//   - Tokens and webhook secrets are write-only: they are accepted on
//     input, kept in a struct that does not implement `Serialize`, and
//     are never written to a log line, an error message, or a response
//     payload. Hook payloads carry only `configured: true/false`
//     flags.
//   - Release-asset downloads accept only allowlisted hosts
//     (`github.com`, `api.github.com`, `objects.githubusercontent.com`,
//     `gitlab.com`, `bitbucket.org`, `api.bitbucket.org`, plus the
//     configured GitLab hosts). Every redirect target is re-validated
//     against the same allowlist, the scheme must be `https` (plain
//     `http` is allowed only for configured self-hosted GitLab hosts),
//     URLs with userinfo or IP-literal hosts are rejected, and stored
//     asset URLs are redacted (query string and fragment removed).
//   - Webhooks are verified before any payload is acted on: GitHub
//     uses HMAC-SHA256 (`X-Hub-Signature-256`), GitLab uses its shared
//     token header (`X-Gitlab-Token`), and Bitbucket — which provides
//     no signature scheme — requires the configured webhook secret in
//     the `X-Webhook-Secret` header. Unsigned or unverifiable payloads
//     are rejected with the standard error envelope.
//   - Imported versions are always drafts. Nothing in this module
//     publishes a release; publishing stays with the software module.


use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::{events, HookBus, HookContext, HookHandler};
use unified_core::json;
use unified_core::modules::{Module, ModuleRegistry, RouteInfo};
use unified_core::pagination::{PageResult, PaginationParams};
use crate::modules::ai;
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
use super::webhooks::*;
use super::ai_capabilities::AI_CAPABILITIES;

// ---------------------------------------------------------------------------
// Module definition.
// ---------------------------------------------------------------------------

pub struct VcsModule {
    state: Arc<VcsState>,
}

impl VcsModule {
    pub fn new() -> Self {
        Self {
            state: Arc::new(VcsState::new()),
        }
    }
}

impl Default for VcsModule {
    fn default() -> Self {
        Self::new()
    }
}

impl Module for VcsModule {
    fn id(&self) -> &'static str {
        "vcs"
    }

    fn name(&self) -> &'static str {
        "Third-Party VCS Integrations"
    }

    fn description(&self) -> &'static str {
        "GitHub, GitLab, and Bitbucket integrations: repository connections, release import as review drafts, signed webhooks, allowlisted release-asset downloads, and sync with rate-limit handling."
    }

    fn dependencies(&self) -> Vec<&'static str> {
        vec!["software", "settings", "notifications"]
    }

    fn init(&self, ctx: &AppContext) -> UnifiedResult<()> {

        ai::register_ai_capabilities("vcs", AI_CAPABILITIES);
        *self
            .state
            .hooks
            .write()
            .expect("hooks lock poisoned") = ctx.hooks.clone();
        Ok(())
    }

    fn register_hooks(&self, bus: &HookBus) {
        // Validate `vcs.*` settings values through the config.validate
        // hook: tokens and secrets must be strings, GitLab hosts must
        // be valid hostnames, and API base overrides must be https URLs.
        let handler: HookHandler = Arc::new(|ctx: &mut HookContext| {
            let namespace = ctx
                .get("namespace")
                .and_then(|value| value.as_str())
                .unwrap_or("");
            if namespace != "vcs" {
                return;
            }
            let key = ctx
                .get("key")
                .and_then(|value| value.as_str())
                .unwrap_or("")
                .to_string();
            let value = ctx.get("value").cloned().unwrap_or(Value::Null);
            let problem: Option<String> = if key.ends_with(".token")
                || key.ends_with(".webhook_secret")
            {
                if value.is_string() {
                    None
                } else {
                    Some(format!("Setting \"{key}\" must be a string"))
                }
            } else if key == "gitlab.hosts" {
                match value.as_array() {
                    Some(hosts)
                        if hosts.iter().all(|host| {
                            host.as_str().is_some_and(|host| valid_host(host))
                        }) =>
                    {
                        None
                    }
                    _ => Some("Setting \"gitlab.hosts\" must be an array of valid hostnames".to_string()),
                }
            } else if key.ends_with(".api_base") {
                match value.as_str() {
                    Some(base) if validate_api_base_override(base).is_ok() => None,
                    _ => Some(format!(
                        "Setting \"{key}\" must be a valid https URL that does not point at an IP-literal or local host"
                    )),
                }
            } else {
                None
            };
            if let Some(message) = problem {
                ctx.set("valid", json!(false));
                ctx.set("message", json!(message));
            }
        });
        bus.register(events::CONFIG_VALIDATE, "vcs", handler);
    }

    fn configure_routes(&self, cfg: &mut web::ServiceConfig) {
        cfg.app_data(web::Data::new(self.state.clone()));
        cfg.service(
            web::scope("/api/v1/vcs")
                .route("/providers", web::get().to(list_providers))
                .route("/providers/{provider}", web::get().to(get_provider))
                .route(
                    "/providers/{provider}/settings",
                    web::get().to(get_provider_settings),
                )
                .route(
                    "/providers/{provider}/settings",
                    web::put().to(update_provider_settings),
                )
                .route("/connections", web::get().to(list_connections))
                .route("/connections", web::post().to(create_connection))
                .route("/connections/{id}", web::get().to(get_connection))
                .route("/connections/{id}", web::put().to(update_connection))
                .route("/connections/{id}", web::delete().to(delete_connection))
                .route(
                    "/connections/{id}/enable",
                    web::post().to(enable_connection),
                )
                .route(
                    "/connections/{id}/disable",
                    web::post().to(disable_connection),
                )
                .route(
                    "/connections/{id}/releases",
                    web::get().to(list_connection_releases),
                )
                .route(
                    "/connections/{id}/releases/import",
                    web::post().to(import_release),
                )
                .route("/connections/{id}/sync", web::post().to(sync_connection))
                .route(
                    "/connections/{id}/sync-history",
                    web::get().to(list_connection_sync_history),
                )
                .route("/drafts", web::get().to(list_drafts))
                .route("/drafts/{id}", web::get().to(get_draft))
                .route("/drafts/{id}", web::delete().to(delete_draft))
                .route(
                    "/drafts/{id}/assets",
                    web::post().to(download_draft_asset),
                )
                .route("/sync-history", web::get().to(list_sync_history))
                .route("/webhooks/github", web::post().to(github_webhook))
                .route("/webhooks/gitlab", web::post().to(gitlab_webhook))
                .route("/webhooks/bitbucket", web::post().to(bitbucket_webhook)),
        );
    }

    fn route_info(&self) -> Vec<RouteInfo> {
        vec![
            RouteInfo { method: "GET", path: "/api/v1/vcs/providers", owner: "vcs", description: "List VCS providers with configuration flags.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/vcs/providers/{provider}", owner: "vcs", description: "Get one VCS provider.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/vcs/providers/{provider}/settings", owner: "vcs", description: "Get provider settings flags (tokens are never returned).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "PUT", path: "/api/v1/vcs/providers/{provider}/settings", owner: "vcs", description: "Update provider settings (token and webhook secret are write-only).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/vcs/connections", owner: "vcs", description: "List repository connections (paginated, filter by provider, software_id, enabled, sync_status).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/vcs/connections", owner: "vcs", description: "Connect a repository to a software record.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/vcs/connections/{id}", owner: "vcs", description: "Get one repository connection.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "PUT", path: "/api/v1/vcs/connections/{id}", owner: "vcs", description: "Update a repository connection.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "DELETE", path: "/api/v1/vcs/connections/{id}", owner: "vcs", description: "Delete a repository connection.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/vcs/connections/{id}/enable", owner: "vcs", description: "Enable a repository connection.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/vcs/connections/{id}/disable", owner: "vcs", description: "Disable a repository connection.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/vcs/connections/{id}/releases", owner: "vcs", description: "List provider releases/tags for a connection (paginated, rate-limit state in metadata).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/vcs/connections/{id}/releases/import", owner: "vcs", description: "Import a provider release/tag as a review draft (never published).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/vcs/connections/{id}/sync", owner: "vcs", description: "Sync a connection now: fetch releases, create drafts for new tags.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/vcs/connections/{id}/sync-history", owner: "vcs", description: "List sync history for a connection (paginated).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/vcs/drafts", owner: "vcs", description: "List imported version drafts (paginated, filter by provider, connection_id, software_id, status).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/vcs/drafts/{id}", owner: "vcs", description: "Get one version draft.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "DELETE", path: "/api/v1/vcs/drafts/{id}", owner: "vcs", description: "Discard a version draft.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/vcs/drafts/{id}/assets", owner: "vcs", description: "Download a release asset through the allowlist and store its SHA-256 reference on the draft.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/vcs/sync-history", owner: "vcs", description: "List all sync history (paginated, filter by provider, connection_id, status).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/vcs/webhooks/github", owner: "vcs", description: "GitHub webhook receiver (HMAC-SHA256 verified).", auth: unified_core::auth::RouteAuth::Public},
            RouteInfo { method: "POST", path: "/api/v1/vcs/webhooks/gitlab", owner: "vcs", description: "GitLab webhook receiver (shared token verified).", auth: unified_core::auth::RouteAuth::Public},
            RouteInfo { method: "POST", path: "/api/v1/vcs/webhooks/bitbucket", owner: "vcs", description: "Bitbucket webhook receiver (configured shared secret verified).", auth: unified_core::auth::RouteAuth::Public},
        ]
    }
}

pub fn register(registry: &ModuleRegistry) {
    registry.register(Arc::new(VcsModule::new()));
}
