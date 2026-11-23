//! Storage - module struct, route table and registration.
//!
//! Part of the `storage` module split; see `src/modules/storage.rs`.

use unified_core::modules::Module;
use unified_core::modules::ModuleRegistry;
use unified_core::modules::RouteInfo;
use actix_web::web;
use std::sync::Arc;

use super::handlers::create_target;
use super::handlers::delete_object;
use super::handlers::delete_target;
use super::handlers::download_object;
use super::handlers::get_status;
use super::handlers::get_target;
use super::handlers::list_targets;
use super::handlers::signed_url;
use super::handlers::test_target;
use super::handlers::upload_object;

// ---------------------------------------------------------------------------
// Module definition.
// ---------------------------------------------------------------------------

pub struct StorageModule;

impl Module for StorageModule {
    fn id(&self) -> &'static str {
        "storage"
    }

    fn name(&self) -> &'static str {
        "Storage"
    }

    fn description(&self) -> &'static str {
        "Storage provider abstraction: local, S3, Cloudflare R2, and Google Drive targets with a single object API for backup, assets serving, and the downloader."
    }

    fn configure_routes(&self, cfg: &mut actix_web::web::ServiceConfig) {
        cfg.service(
            web::scope("/api/v1/storage")
                .route("", web::get().to(get_status))
                .route("/targets", web::get().to(list_targets))
                .route("/targets", web::post().to(create_target))
                .route("/targets/{name}", web::get().to(get_target))
                .route("/targets/{name}", web::delete().to(delete_target))
                .route("/targets/{name}/test", web::post().to(test_target))
                .route("/targets/{name}/objects", web::post().to(upload_object))
                .route("/targets/{name}/objects", web::get().to(download_object))
                .route("/targets/{name}/objects", web::delete().to(delete_object))
                .route("/targets/{name}/signed-url", web::post().to(signed_url)),
        );
    }

    fn route_info(&self) -> Vec<RouteInfo> {
        vec![
            RouteInfo {
                method: "GET",
                path: "/api/v1/storage",
                owner: "storage",
                description: "Storage status: supported kinds and registered targets (capability storage.read).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/storage/targets",
                owner: "storage",
                description: "List storage targets (paginated, secrets masked, capability storage.read).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/storage/targets",
                owner: "storage",
                description: "Add a storage target (secrets write-only, optional quota_bytes / max_objects, capability storage.manage_targets).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/storage/targets/{name}",
                owner: "storage",
                description: "Get one storage target (secrets masked, capability storage.read).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "DELETE",
                path: "/api/v1/storage/targets/{name}",
                owner: "storage",
                description: "Delete a storage target (capability storage.manage_targets).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/storage/targets/{name}/test",
                owner: "storage",
                description: "Test a storage target connection (capability storage.manage_targets).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/storage/targets/{name}/objects",
                owner: "storage",
                description: "Upload object bytes through a target (?key=, SHA-256 recorded, target quota enforced, capability storage.write).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/storage/targets/{name}/objects",
                owner: "storage",
                description: "Download object bytes through a target (?key=, ?expected_sha256= verifies, capability storage.read).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "DELETE",
                path: "/api/v1/storage/targets/{name}/objects",
                owner: "storage",
                description: "Delete an object through a target (?key=, capability storage.write).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/storage/targets/{name}/signed-url",
                owner: "storage",
                description: "Generate a signed URL for an object (capability storage.write).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
        ]
    }
}

pub fn register(registry: &ModuleRegistry) {
    registry.register(Arc::new(StorageModule));
}
