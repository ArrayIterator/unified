//! Cloudflare connector module. — Module definition.
//!
//! Split from `src/modules/cloudflare.rs`; behaviour unchanged.

use super::accounts_zones::*;
use super::cache_handlers::*;
use super::client::*;
use super::constants::*;
use super::dns_handlers::*;
use super::dns_validation::*;
use super::helpers::*;
use super::requests::*;
use super::state::*;
use super::status_handlers::*;

use unified_core::modules::{Module, ModuleRegistry, RouteInfo};
use actix_web::web;
use std::sync::Arc;

pub struct CloudflareModule {
    pub(super) state: CloudflareState,
}

impl CloudflareModule {
    pub fn new() -> Self {
        Self {
            state: CloudflareState::default(),
        }
    }
}

impl Default for CloudflareModule {
    fn default() -> Self {
        Self::new()
    }
}

impl Module for CloudflareModule {
    fn id(&self) -> &'static str {
        "cloudflare"
    }

    fn name(&self) -> &'static str {
        "Cloudflare Connector"
    }

    fn description(&self) -> &'static str {
        "Cloudflare API connector: account and zone listing, DNS record CRUD with validation, cache purge, development mode, and cache level, with rate-limit backoff and hook events for DNS changes and purges."
    }

    fn route_info(&self) -> Vec<RouteInfo> {
        vec![
            RouteInfo { method: "GET", path: "/api/v1/cloudflare/status", owner: "cloudflare", description: "Get status", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "PUT", path: "/api/v1/cloudflare/settings", owner: "cloudflare", description: "Update settings", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/cloudflare/test", owner: "cloudflare", description: "Test connection", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/cloudflare/accounts", owner: "cloudflare", description: "List accounts", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/cloudflare/zones", owner: "cloudflare", description: "List zones", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/cloudflare/zones/{zone_id}", owner: "cloudflare", description: "Get zone", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/cloudflare/zones/{zone_id}/dns", owner: "cloudflare", description: "List dns", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/cloudflare/zones/{zone_id}/dns", owner: "cloudflare", description: "Create dns", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/cloudflare/zones/{zone_id}/dns/{record_id}", owner: "cloudflare", description: "Get dns", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "PUT", path: "/api/v1/cloudflare/zones/{zone_id}/dns/{record_id}", owner: "cloudflare", description: "Update dns", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "DELETE", path: "/api/v1/cloudflare/zones/{zone_id}/dns/{record_id}", owner: "cloudflare", description: "Delete dns", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/cloudflare/zones/{zone_id}/cache/purge", owner: "cloudflare", description: "Purge cache", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/cloudflare/zones/{zone_id}/cache/purges", owner: "cloudflare", description: "List purges for zone", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/cloudflare/cache/purges", owner: "cloudflare", description: "List purges", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/cloudflare/zones/{zone_id}/cache/development-mode", owner: "cloudflare", description: "Get development mode", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/cloudflare/zones/{zone_id}/cache/development-mode", owner: "cloudflare", description: "Set development mode", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/cloudflare/zones/{zone_id}/cache/level", owner: "cloudflare", description: "Get cache level", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "PUT", path: "/api/v1/cloudflare/zones/{zone_id}/cache/level", owner: "cloudflare", description: "Set cache level", auth: unified_core::auth::RouteAuth::Authenticated},
        ]
    }

    fn configure_routes(&self, cfg: &mut web::ServiceConfig) {
        let state = web::Data::new(self.state.clone());
        cfg.service(
            web::scope("/api/v1/cloudflare")
                .app_data(state)
                .route("/status", web::get().to(get_status))
                .route("/settings", web::put().to(update_settings))
                .route("/test", web::post().to(test_connection))
                .route("/accounts", web::get().to(list_accounts))
                .route("/zones", web::get().to(list_zones))
                .route("/zones/{zone_id}", web::get().to(get_zone))
                .route("/zones/{zone_id}/dns", web::get().to(list_dns))
                .route("/zones/{zone_id}/dns", web::post().to(create_dns))
                .route("/zones/{zone_id}/dns/{record_id}", web::get().to(get_dns))
                .route("/zones/{zone_id}/dns/{record_id}", web::put().to(update_dns))
                .route(
                    "/zones/{zone_id}/dns/{record_id}",
                    web::delete().to(delete_dns),
                )
                .route("/zones/{zone_id}/cache/purge", web::post().to(purge_cache))
                .route(
                    "/zones/{zone_id}/cache/purges",
                    web::get().to(list_purges_for_zone),
                )
                .route("/cache/purges", web::get().to(list_purges))
                .route(
                    "/zones/{zone_id}/cache/development-mode",
                    web::get().to(get_development_mode),
                )
                .route(
                    "/zones/{zone_id}/cache/development-mode",
                    web::post().to(set_development_mode),
                )
                .route(
                    "/zones/{zone_id}/cache/level",
                    web::get().to(get_cache_level),
                )
                .route(
                    "/zones/{zone_id}/cache/level",
                    web::put().to(set_cache_level),
                ),
        );
    }
}

pub fn register(registry: &ModuleRegistry) {
    registry.register(Arc::new(CloudflareModule::new()));
}
