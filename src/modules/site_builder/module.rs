// Site Builder module.
//
// Drag-and-drop page building on top of Content and Theme: pages made of
// ordered blocks and sections, rendered to real HTML server-side, with a
// draft -> published flow.
//
// Routes (all under `/api/v1/site-builder`):
//   GET    /pages                                   - paginated pages
//   POST   /pages                                   - create a page {title, slug, theme_id}
//   GET    /pages/{id}                              - one page
//   PUT    /pages/{id}                              - update a page
//   DELETE /pages/{id}                              - delete a page
//   GET    /pages/{id}/blocks                       - list blocks (in order)
//   POST   /pages/{id}/blocks                       - add a block
//   PUT    /pages/{id}/blocks/{block_id}            - update a block
//   DELETE /pages/{id}/blocks/{block_id}            - delete a block
//   PUT    /pages/{id}/blocks/order                 - reorder blocks {ids: [...]}
//   GET    /pages/{id}/sections                     - list sections (in order)
//   POST   /pages/{id}/sections                     - add a section
//   PUT    /pages/{id}/sections/{section_id}        - update a section
//   DELETE /pages/{id}/sections/{section_id}        - delete a section
//   PUT    /pages/{id}/sections/order               - reorder sections {ids: [...]}
//   POST   /pages/{id}/publish                      - draft -> published
//   POST   /pages/{id}/unpublish                    - published -> draft
//   GET    /pages/{id}/render                       - PUBLIC: render a published page to HTML
//     (no auth; gated by the API Manager ApiAccessRule; drafts 404
//     unless the caller holds site-builder.manage for preview)
//
// Access control: every handler except the public render route
// requires the caller identity headers (`X-User-Id` + `X-Role`) and
// the `site-builder.view` (reads) or `site-builder.manage` (writes,
// publish) capability through the `role.check` hook, failing closed.
//
// Hook contracts:
//   Publish validates the theme through the `theme.exists` event owned by
//   the Theme module. When no handler answers, the page is published with
//   a warning in the response (the theme is accepted unchecked).
//   Rendering goes through the theme engine contract: every block is
//   rendered to a default HTML fragment and then offered to the Theme
//   module through the `theme.render_block` event, which may replace the
//   fragment. Emits `site.page_published` / `site.page_unpublished`.


use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::HookBus;
use unified_core::json as api_json;
use unified_core::modules::{Module, ModuleRegistry, RouteInfo};
use unified_core::pagination::{PageResult, PaginationParams};
use crate::modules::ai;
use actix_web::{web, HttpResponse};
use chrono::Utc;
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, RwLock};
use std::sync::OnceLock;
use uuid::Uuid;

use super::constants::*;
use super::types::*;
use super::store::*;
use super::helpers::*;
use super::validation::*;
use super::rendering::*;
use super::handlers::*;
use super::ai_capabilities::AI_CAPABILITIES;

// Module

pub struct SiteBuilderModule {
    state: Arc<SiteBuilderState>,
}

impl SiteBuilderModule {
    pub fn new() -> Self {
        Self {
            state: Arc::new(SiteBuilderState::new()),
        }
    }
}

pub fn register(registry: &ModuleRegistry) {
    registry.register(Arc::new(SiteBuilderModule::new()));
}

impl Module for SiteBuilderModule {
    fn id(&self) -> &'static str {
        "site-builder"
    }

    fn name(&self) -> &'static str {
        "Site Builder"
    }

    fn description(&self) -> &'static str {
        "Drag-and-drop page building on top of Content and Theme: pages of ordered blocks and sections, server-side HTML rendering, and a draft to published flow."
    }

    /// The builder is a SELLABLE module, not a builtin: a tenant gets
    /// it only through a package (see the `site-builder-saas` seed
    /// package) or a license/subscription, via the licensing gate.
    /// Non-core so the stored-row leg of `check_module_entitlement`
    /// decides for tenants; the control plane itself is never gated
    /// (see `helpers::require_site_builder_entitlement`).
    fn is_core(&self) -> bool {
        false
    }

    fn available_as_product(&self) -> bool {
        true
    }

    fn product_category(&self) -> Option<&'static str> {
        Some("productivity")
    }

    fn dependencies(&self) -> Vec<&'static str> {
        vec!["content", "theme"]
    }

    fn init(&self, ctx: &AppContext) -> UnifiedResult<()> {

        ai::register_ai_capabilities("site-builder", AI_CAPABILITIES);
        self.state.debug.store(ctx.is_debug(), Ordering::SeqCst);
        self.state.set_hooks(ctx.hooks.clone());
        // The default home page is seeded as a site-builder page
        // (hero + text blocks), never as a static template; see
        // `SiteBuilderState::seed_default_home_page`.
        self.state.seed_default_home_page();
        Ok(())
    }

    fn register_hooks(&self, bus: &HookBus) {
        let state = self.state.clone();
        bus.register(
            EVENT_RENDER_PAGE,
            "site-builder",
            Arc::new(move |ctx: &mut unified_core::hooks::HookContext| {
                on_render_page_request(&state, ctx);
            }),
        );
    }

    fn route_info(&self) -> Vec<RouteInfo> {
        vec![
            RouteInfo { method: "GET", path: "/api/v1/site-builder/pages", owner: "site-builder", description: "List pages (requires site-builder.view).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/site-builder/pages", owner: "site-builder", description: "Create page (requires site-builder.manage).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/site-builder/pages/{id}", owner: "site-builder", description: "Get page (requires site-builder.view).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "PUT", path: "/api/v1/site-builder/pages/{id}", owner: "site-builder", description: "Update page (requires site-builder.manage).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "DELETE", path: "/api/v1/site-builder/pages/{id}", owner: "site-builder", description: "Delete page (requires site-builder.manage).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/site-builder/pages/{id}/blocks", owner: "site-builder", description: "List blocks (requires site-builder.view).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/site-builder/pages/{id}/blocks", owner: "site-builder", description: "Create block (requires site-builder.manage).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "PUT", path: "/api/v1/site-builder/pages/{id}/blocks/order", owner: "site-builder", description: "Reorder blocks (requires site-builder.manage).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "PUT", path: "/api/v1/site-builder/pages/{id}/blocks/{block_id}", owner: "site-builder", description: "Update block (requires site-builder.manage).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "DELETE", path: "/api/v1/site-builder/pages/{id}/blocks/{block_id}", owner: "site-builder", description: "Delete block (requires site-builder.manage).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/site-builder/pages/{id}/sections", owner: "site-builder", description: "List sections (requires site-builder.view).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/site-builder/pages/{id}/sections", owner: "site-builder", description: "Create section (requires site-builder.manage).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "PUT", path: "/api/v1/site-builder/pages/{id}/sections/order", owner: "site-builder", description: "Reorder sections (requires site-builder.manage).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "PUT", path: "/api/v1/site-builder/pages/{id}/sections/{section_id}", owner: "site-builder", description: "Update section (requires site-builder.manage).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "DELETE", path: "/api/v1/site-builder/pages/{id}/sections/{section_id}", owner: "site-builder", description: "Delete section (requires site-builder.manage).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/site-builder/pages/{id}/publish", owner: "site-builder", description: "Publish page (requires site-builder.manage).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/site-builder/pages/{id}/unpublish", owner: "site-builder", description: "Unpublish page (requires site-builder.manage).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/site-builder/pages/{id}/render", owner: "site-builder", description: "PUBLIC: render a published page to HTML (no auth; gated by the API Manager ApiAccessRule for this path; drafts return 404 unless the caller holds site-builder.manage for preview).", auth: unified_core::auth::RouteAuth::Public},
        ]
    }

    fn configure_routes(&self, cfg: &mut actix_web::web::ServiceConfig) {
        let state = self.state.clone();
        cfg.service(
            web::scope("/api/v1/site-builder")
                .app_data(web::Data::new(state))
                .route("/pages", web::get().to(list_pages))
                .route("/pages", web::post().to(create_page))
                .route("/pages/{id}", web::get().to(get_page))
                .route("/pages/{id}", web::put().to(update_page))
                .route("/pages/{id}", web::delete().to(delete_page))
                .route("/pages/{id}/blocks", web::get().to(list_blocks))
                .route("/pages/{id}/blocks", web::post().to(create_block))
                .route("/pages/{id}/blocks/order", web::put().to(reorder_blocks))
                .route("/pages/{id}/blocks/{block_id}", web::put().to(update_block))
                .route("/pages/{id}/blocks/{block_id}", web::delete().to(delete_block))
                .route("/pages/{id}/sections", web::get().to(list_sections))
                .route("/pages/{id}/sections", web::post().to(create_section))
                .route("/pages/{id}/sections/order", web::put().to(reorder_sections))
                .route("/pages/{id}/sections/{section_id}", web::put().to(update_section))
                .route("/pages/{id}/sections/{section_id}", web::delete().to(delete_section))
                .route("/pages/{id}/publish", web::post().to(publish_page))
                .route("/pages/{id}/unpublish", web::post().to(unpublish_page))
                .route("/pages/{id}/render", web::get().to(render_page)),
        );
    }
}
