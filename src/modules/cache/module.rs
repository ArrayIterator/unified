//! Module registration: routes and metadata.

use unified_core::modules::{Module, ModuleRegistry, RouteInfo};
use actix_web::{web};
use std::sync::Arc;
use super::handlers::{clear_cache, delete_entry, delete_tenant_entry, get_entry, get_status, get_tenant_entry, invalidate_prefix, invalidate_tenant, prune_cache, set_entry, set_tenant_entry};

// ---------------------------------------------------------------------------
// Module definition.
// ---------------------------------------------------------------------------

pub struct CacheModule;

impl Module for CacheModule {
    fn id(&self) -> &'static str {
        "cache"
    }

    fn name(&self) -> &'static str {
        "Cache"
    }

    fn description(&self) -> &'static str {
        "Shared application cache: filesystem adapter by default, Redis when redis.enabled is true, with filesystem failover, tenant-scoped entries, stampede-protected fills, and prefix invalidation."
    }

    fn route_info(&self) -> Vec<RouteInfo> {
        vec![
            RouteInfo { method: "GET", path: "/api/v1/cache", owner: "cache", description: "Get status (adapter, default TTL, Redis enabled, failover state).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/cache", owner: "cache", description: "Set entry (capability cache.write).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/cache/clear", owner: "cache", description: "Clear cache (capability cache.manage).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/cache/prune", owner: "cache", description: "Evict entries past the filesystem bound (capability cache.manage).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/cache/invalidate", owner: "cache", description: "Delete every entry under a key prefix (capability cache.manage).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/cache/{key}", owner: "cache", description: "Get entry (capability cache.read).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "DELETE", path: "/api/v1/cache/{key}", owner: "cache", description: "Delete entry (capability cache.write).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/cache/tenant/{tenant}/{key}", owner: "cache", description: "Get a tenant-scoped entry (capability cache.read).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/cache/tenant/{tenant}", owner: "cache", description: "Set a tenant-scoped entry (capability cache.write).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "DELETE", path: "/api/v1/cache/tenant/{tenant}/{key}", owner: "cache", description: "Delete a tenant-scoped entry (capability cache.write).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/cache/tenant/{tenant}/invalidate", owner: "cache", description: "Delete every entry of one tenant (capability cache.manage).", auth: unified_core::auth::RouteAuth::Authenticated},
        ]
    }

    fn configure_routes(&self, cfg: &mut actix_web::web::ServiceConfig) {
        cfg.service(
            web::scope("/api/v1/cache")
                .route("", web::get().to(get_status))
                .route("", web::post().to(set_entry))
                .route("/clear", web::post().to(clear_cache))
                .route("/prune", web::post().to(prune_cache))
                .route("/invalidate", web::post().to(invalidate_prefix))
                .route("/tenant/{tenant}", web::post().to(set_tenant_entry))
                .route("/tenant/{tenant}/invalidate", web::post().to(invalidate_tenant))
                .route("/tenant/{tenant}/{key}", web::get().to(get_tenant_entry))
                .route("/tenant/{tenant}/{key}", web::delete().to(delete_tenant_entry))
                .route("/{key}", web::get().to(get_entry))
                .route("/{key}", web::delete().to(delete_entry)),
        );
    }
}

pub fn register(registry: &ModuleRegistry) {
    registry.register(Arc::new(CacheModule));
}
