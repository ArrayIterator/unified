//! files module - Module registration: struct, Default, Module impl, register.
//!
//! Events emitted by this module: `file.uploaded` and `file.deleted`.
//! Neither has a consumer anywhere in the codebase (checked
//! 2026-11-22): they are emitted for future listeners only. Do not
//! invent a listener for them; if one is ever added, document it here.

use unified_core::context::AppContext;
use unified_core::error::UnifiedResult;
use unified_core::hooks::{HookBus, HookContext, HookHandler};
use unified_core::modules::{Module, ModuleRegistry, RouteInfo};
use actix_web::web;
use std::sync::Arc;

use super::handlers::create_folder;
use super::handlers::delete_file;
use super::handlers::download_file;
use super::handlers::get_file;
use super::handlers::list_files;
use super::handlers::move_file;
use super::handlers::upload_file;
use super::hooks::on_file_shared;
use super::types::FilesState;

// Module

pub struct FilesModule {
    pub(crate) state: Arc<FilesState>,
}

impl FilesModule {
    pub fn new() -> Self {
        Self {
            state: Arc::new(FilesState::new()),
        }
    }
}

impl Default for FilesModule {
    fn default() -> Self {
        Self::new()
    }
}

impl Module for FilesModule {
    fn id(&self) -> &'static str {
        "files"
    }

    fn name(&self) -> &'static str {
        "File Manager"
    }

    fn description(&self) -> &'static str {
        "File manager: folders, uploads, streaming downloads, move and delete."
    }

    fn init(&self, ctx: &AppContext) -> UnifiedResult<()> {
        *self.state.hooks.write().expect("hooks lock poisoned") = ctx.hooks.clone();
        Ok(())
    }

    fn register_hooks(&self, bus: &HookBus) {
        let state = self.state.clone();
        let handler: HookHandler =
            Arc::new(move |ctx: &mut HookContext| on_file_shared(state.clone(), ctx));
        bus.register("file.shared", "files", handler);
    }

    fn route_info(&self) -> Vec<RouteInfo> {
        vec![
            RouteInfo {
                method: "GET",
                path: "/api/v1/files",
                owner: "files",
                description: "List files",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/files",
                owner: "files",
                description: "Create folder",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/files/upload",
                owner: "files",
                description: "Upload file",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/files/{id}",
                owner: "files",
                description: "Get file",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "DELETE",
                path: "/api/v1/files/{id}",
                owner: "files",
                description: "Delete file",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/files/{id}/download",
                owner: "files",
                description: "Download file",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/files/{id}/move",
                owner: "files",
                description: "Move file",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
        ]
    }

    fn configure_routes(&self, cfg: &mut web::ServiceConfig) {
        cfg.app_data(web::Data::new(self.state.clone()));
        cfg.service(
            web::scope("/api/v1/files")
                .route("", web::get().to(list_files))
                .route("", web::post().to(create_folder))
                .route("/upload", web::post().to(upload_file))
                .route("/{id}", web::get().to(get_file))
                .route("/{id}", web::delete().to(delete_file))
                .route("/{id}/download", web::get().to(download_file))
                .route("/{id}/move", web::post().to(move_file)),
        );
    }
}

pub fn register(registry: &ModuleRegistry) {
    registry.register(Arc::new(FilesModule::new()));
}
