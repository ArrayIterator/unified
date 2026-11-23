//! apikeys module - Module registration: struct, Module impl, register.

use unified_core::context::AppContext;
use unified_core::error::UnifiedResult;
use unified_core::modules::{Module, ModuleRegistry};
use actix_web::web;
use std::sync::Arc;

use super::store::HOOK_BUS;
use super::handlers::{create_key, get_key, key_usage, list_keys, list_scopes, revoke_key, rotate_key, update_key, verify_key};

// ---------------------------------------------------------------------------
// Module definition.
// ---------------------------------------------------------------------------

pub struct ApiKeysModule;

impl Module for ApiKeysModule {
    fn id(&self) -> &'static str {
        "apikeys"
    }

    fn name(&self) -> &'static str {
        "API Keys"
    }

    fn description(&self) -> &'static str {
        "Enterprise API key management: scoped keys with HMAC-peppered hash storage (plain SHA-256 digests from older keys still verify), expiry, rotation with a grace period, revocation, per-key rate-limit tiers, and usage tracking. Key management endpoints are admin-gated through the role.check hook (apikeys.view for reads, apikeys.manage for mutations); new keys require explicit scopes and never default to full access."
    }

    fn requires(&self) -> &[&'static str] {
        &["security", "api-manager"]
    }

    fn requires_libraries(&self) -> &[&'static str] {
        &["sha2", "rand"]
    }

    fn init(&self, ctx: &AppContext) -> UnifiedResult<()> {
        let _ = HOOK_BUS.set(Arc::clone(&ctx.hooks));
        // Stash the application secret as the fallback pepper for
        // new key hashes (domain-separated before use; a dedicated
        // UNIFIED_APIKEY_PEPPER environment variable takes
        // precedence). Without a pepper, new keys fall back to an
        // unpeppered HMAC, so set one in production.
        let (secret, sunset) = {
            let config = ctx.config.read().expect("config lock poisoned");
            (
                config.app.secret_key.clone(),
                config.app.apikey_legacy_hash_sunset.trim().to_string(),
            )
        };
        if !secret.is_empty() {
            super::service::set_key_pepper(secret);
        }
        // Stash the configured sunset for legacy unpeppered
        // hashes. An empty value leaves the built-in default
        // (`2027-03-01`) in place; config validation rejects a
        // malformed date before this runs, and an unparseable
        // value would fail closed (treated as reached) anyway.
        if !sunset.is_empty() {
            super::service::set_legacy_hash_sunset(sunset);
        }
        Ok(())
    }

    fn configure_routes(&self, cfg: &mut actix_web::web::ServiceConfig) {
        cfg.service(
            web::scope("/api/v1/apikeys")
                .route("", web::get().to(list_keys))
                .route("", web::post().to(create_key))
                .route("/scopes", web::get().to(list_scopes))
                .route("/verify", web::post().to(verify_key))
                .route("/{id}", web::get().to(get_key))
                .route("/{id}", web::put().to(update_key))
                .route("/{id}/rotate", web::post().to(rotate_key))
                .route("/{id}/revoke", web::post().to(revoke_key))
                .route("/{id}/usage", web::get().to(key_usage)),
        );
    }

    fn route_info(&self) -> Vec<unified_core::modules::RouteInfo> {
        use unified_core::modules::RouteInfo;
        vec![
            RouteInfo {
                method: "GET",
                path: "/api/v1/apikeys",
                owner: "apikeys",
                description: "List API keys (paginated, apikeys.view)",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/apikeys",
                owner: "apikeys",
                description: "Create an API key; full key returned once; explicit scopes required (apikeys.manage)",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/apikeys/scopes",
                owner: "apikeys",
                description: "Scope catalog for registered modules (apikeys.view)",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/apikeys/{id}",
                owner: "apikeys",
                description: "Get one API key (masked, apikeys.view)",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "PUT",
                path: "/api/v1/apikeys/{id}",
                owner: "apikeys",
                description: "Update an API key; rotating keys cannot be updated (apikeys.manage)",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/apikeys/{id}/rotate",
                owner: "apikeys",
                description: "Rotate an API key with a grace period (apikeys.manage)",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/apikeys/{id}/revoke",
                owner: "apikeys",
                description: "Revoke an API key (apikeys.manage)",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/apikeys/{id}/usage",
                owner: "apikeys",
                description: "Usage statistics for one API key (apikeys.view)",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/apikeys/verify",
                owner: "apikeys",
                description: "Verify a presented API key; returns only valid + rate tier and records usage (apikeys.view)",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
        ]
    }
}

pub fn register(registry: &ModuleRegistry) {
    registry.register(Arc::new(ApiKeysModule));
}
