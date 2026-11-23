//! Module registration: routes, hooks and metadata.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedResult};
use unified_core::hooks::{HookBus, HookContext};
use unified_core::modules::{Module, ModuleRegistry, RouteInfo};
use crate::modules::ai;
use actix_web::{web};
use std::sync::{Arc};
use super::state::SoftwareState;
use super::handlers_versions::{create_release, create_version, delete_release, delete_version, download_release, get_release, get_version, list_releases, list_versions, publish_release, release_files, unpublish_release, update_version};
use super::handlers_software::{attach_software_tag, create_software, delete_software, detach_software_tag, get_software, get_stats, latest_releases, list_software, list_software_tags, list_software_versions, publish_software, software_release_history, unpublish_software, update_software};
use super::handlers_files::{create_release_file, delete_release_file, get_release_file, update_release_file, verify_release_file};
use super::handlers_taxonomy::{create_tag, create_taxonomy_node, delete_tag, delete_taxonomy_node, get_tag, get_taxonomy_node, list_tags, list_taxonomy, list_taxonomy_children, move_taxonomy_node, tag_cloud, taxonomy_tree, update_tag, update_taxonomy_node};
use super::hooks::{on_vcs_release_imported, on_vcs_software_validate};
use super::ai_capabilities::AI_CAPABILITIES;

// ---------------------------------------------------------------------------
// Module
// ---------------------------------------------------------------------------

pub struct SoftwareModule {
    pub(crate) state: Arc<SoftwareState>,
}

impl SoftwareModule {
    pub fn new() -> Self {
        Self {
            state: Arc::new(SoftwareState::new()),
        }
    }
}

impl Default for SoftwareModule {
    fn default() -> Self {
        Self::new()
    }
}

impl Module for SoftwareModule {
    fn id(&self) -> &'static str {
        "software"
    }

    fn name(&self) -> &'static str {
        "Software"
    }

    fn description(&self) -> &'static str {
        "Enterprise software and assets manager: software records with a status lifecycle, versions with semver and platform requirements, releases with publish flow, per-release files with checksum integrity verification, hierarchical groups and categories, tags, full-text discovery, and license-gated delivery."
    }

    fn dependencies(&self) -> Vec<&'static str> {
        vec!["licensing", "files"]
    }

    fn init(&self, ctx: &AppContext) -> UnifiedResult<()> {

        ai::register_ai_capabilities("software", AI_CAPABILITIES);
        *self
            .state
            .hooks
            .write()
            .expect("hooks lock poisoned") = ctx.hooks.clone();
        Ok(())
    }

    fn register_hooks(&self, bus: &HookBus) {
        let state = self.state.clone();
        bus.register(
            "vcs.release.imported",
            "software",
            Arc::new(move |ctx: &mut HookContext| {
                on_vcs_release_imported(state.clone(), ctx)
            }),
        );
        let state = self.state.clone();
        bus.register(
            "vcs.software.validate",
            "software",
            Arc::new(move |ctx: &mut HookContext| {
                on_vcs_software_validate(state.clone(), ctx)
            }),
        );
    }

    fn route_info(&self) -> Vec<RouteInfo> {
        vec![
            RouteInfo { method: "GET", path: "/api/v1/software/versions", owner: "software", description: "List versions", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/software/versions", owner: "software", description: "Create version", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/software/versions/{id}", owner: "software", description: "Get version", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "PUT", path: "/api/v1/software/versions/{id}", owner: "software", description: "Update version", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "DELETE", path: "/api/v1/software/versions/{id}", owner: "software", description: "Delete version", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/software/releases", owner: "software", description: "List releases", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/software/releases", owner: "software", description: "Create release", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/software/releases/{id}", owner: "software", description: "Get release", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "DELETE", path: "/api/v1/software/releases/{id}", owner: "software", description: "Delete release", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/software/releases/{id}/publish", owner: "software", description: "Publish release", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/software/releases/{id}/unpublish", owner: "software", description: "Unpublish release", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/software/releases/{id}/files", owner: "software", description: "Release files", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/software/releases/{id}/files", owner: "software", description: "Create release file", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/software/releases/{id}/files/{file_id}", owner: "software", description: "Get release file", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "PUT", path: "/api/v1/software/releases/{id}/files/{file_id}", owner: "software", description: "Update release file", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "DELETE", path: "/api/v1/software/releases/{id}/files/{file_id}", owner: "software", description: "Delete release file", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/software/releases/{id}/files/{file_id}/verify", owner: "software", description: "Verify release file", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/software/releases/{id}/download", owner: "software", description: "PUBLIC: Download release (no auth; gated by the API Manager ApiAccessRule for this path; license-gated when the version requires a license).", auth: unified_core::auth::RouteAuth::Public},
            RouteInfo { method: "GET", path: "/api/v1/software/stats", owner: "software", description: "Get stats", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/software/latest", owner: "software", description: "PUBLIC: Latest published releases (no auth; gated by the API Manager ApiAccessRule for this path).", auth: unified_core::auth::RouteAuth::Public},
            RouteInfo { method: "GET", path: "/api/v1/software/taxonomy", owner: "software", description: "List taxonomy", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/software/taxonomy", owner: "software", description: "Create taxonomy node", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/software/taxonomy/tree", owner: "software", description: "Taxonomy tree", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/software/taxonomy/{id}", owner: "software", description: "Get taxonomy node", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "PUT", path: "/api/v1/software/taxonomy/{id}", owner: "software", description: "Update taxonomy node", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "DELETE", path: "/api/v1/software/taxonomy/{id}", owner: "software", description: "Delete taxonomy node", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/software/taxonomy/{id}/move", owner: "software", description: "Move taxonomy node", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/software/taxonomy/{id}/children", owner: "software", description: "List taxonomy children", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/software/tags", owner: "software", description: "List tags", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/software/tags", owner: "software", description: "Create tag", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/software/tags/cloud", owner: "software", description: "Tag cloud", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/software/tags/{id}", owner: "software", description: "Get tag", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "PUT", path: "/api/v1/software/tags/{id}", owner: "software", description: "Update tag", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "DELETE", path: "/api/v1/software/tags/{id}", owner: "software", description: "Delete tag", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/software", owner: "software", description: "List software", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/software", owner: "software", description: "Create software", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/software/{id}", owner: "software", description: "Get software", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "PUT", path: "/api/v1/software/{id}", owner: "software", description: "Update software", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "DELETE", path: "/api/v1/software/{id}", owner: "software", description: "Delete software", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/software/{id}/publish", owner: "software", description: "Publish software", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/software/{id}/unpublish", owner: "software", description: "Unpublish software", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/software/{id}/versions", owner: "software", description: "List software versions", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/software/{id}/releases", owner: "software", description: "Software release history", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/software/{id}/tags", owner: "software", description: "List software tags", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/software/{id}/tags", owner: "software", description: "Attach software tag", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "DELETE", path: "/api/v1/software/{id}/tags/{tag_id}", owner: "software", description: "Detach software tag", auth: unified_core::auth::RouteAuth::Authenticated},
        ]
    }

    fn configure_routes(&self, cfg: &mut web::ServiceConfig) {
        cfg.app_data(web::Data::new(self.state.clone()));
        // Specific single-segment paths are registered before the
        // generic `/{id}` routes so they are matched first.
        cfg.service(
            web::scope("/api/v1/software")
                .route("/versions", web::get().to(list_versions))
                .route("/versions", web::post().to(create_version))
                .route("/versions/{id}", web::get().to(get_version))
                .route("/versions/{id}", web::put().to(update_version))
                .route("/versions/{id}", web::delete().to(delete_version))
                .route("/releases", web::get().to(list_releases))
                .route("/releases", web::post().to(create_release))
                .route("/releases/{id}", web::get().to(get_release))
                .route("/releases/{id}", web::delete().to(delete_release))
                .route("/releases/{id}/publish", web::post().to(publish_release))
                .route("/releases/{id}/unpublish", web::post().to(unpublish_release))
                .route("/releases/{id}/files", web::get().to(release_files))
                .route("/releases/{id}/files", web::post().to(create_release_file))
                .route("/releases/{id}/files/{file_id}", web::get().to(get_release_file))
                .route("/releases/{id}/files/{file_id}", web::put().to(update_release_file))
                .route(
                    "/releases/{id}/files/{file_id}",
                    web::delete().to(delete_release_file),
                )
                .route(
                    "/releases/{id}/files/{file_id}/verify",
                    web::post().to(verify_release_file),
                )
                .route("/releases/{id}/download", web::get().to(download_release))
                .route("/stats", web::get().to(get_stats))
                .route("/latest", web::get().to(latest_releases))
                .route("/taxonomy", web::get().to(list_taxonomy))
                .route("/taxonomy", web::post().to(create_taxonomy_node))
                .route("/taxonomy/tree", web::get().to(taxonomy_tree))
                .route("/taxonomy/{id}", web::get().to(get_taxonomy_node))
                .route("/taxonomy/{id}", web::put().to(update_taxonomy_node))
                .route("/taxonomy/{id}", web::delete().to(delete_taxonomy_node))
                .route("/taxonomy/{id}/move", web::post().to(move_taxonomy_node))
                .route("/taxonomy/{id}/children", web::get().to(list_taxonomy_children))
                .route("/tags", web::get().to(list_tags))
                .route("/tags", web::post().to(create_tag))
                .route("/tags/cloud", web::get().to(tag_cloud))
                .route("/tags/{id}", web::get().to(get_tag))
                .route("/tags/{id}", web::put().to(update_tag))
                .route("/tags/{id}", web::delete().to(delete_tag))
                .route("", web::get().to(list_software))
                .route("", web::post().to(create_software))
                .route("/{id}", web::get().to(get_software))
                .route("/{id}", web::put().to(update_software))
                .route("/{id}", web::delete().to(delete_software))
                .route("/{id}/publish", web::post().to(publish_software))
                .route("/{id}/unpublish", web::post().to(unpublish_software))
                .route("/{id}/versions", web::get().to(list_software_versions))
                .route("/{id}/releases", web::get().to(software_release_history))
                .route("/{id}/tags", web::get().to(list_software_tags))
                .route("/{id}/tags", web::post().to(attach_software_tag))
                .route("/{id}/tags/{tag_id}", web::delete().to(detach_software_tag)),
        );
    }
}

pub fn register(registry: &ModuleRegistry) {
    registry.register(Arc::new(SoftwareModule::new()));
}
