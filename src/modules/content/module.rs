//! Content Management module. — Module.
//!
//! Split from `src/modules/content.rs`; behaviour unchanged.

use super::ai_capabilities::*;
use super::categories::*;
use super::custom_types::*;
use super::helpers::*;
use super::pages::*;
use super::posts::*;
use super::state::*;
use super::tags::*;
use super::types::*;
use unified_core::error::UnifiedResult;

use unified_core::context::AppContext;
use unified_core::hooks::HookBus;
use unified_core::modules::{Module, ModuleRegistry, RouteInfo};
use crate::modules::ai;
use actix_web::web;
use serde_json::{json, Value};
use std::sync::Arc;

pub struct ContentModule {
    pub(super) state: ContentState,
}

impl ContentModule {
    pub fn new() -> Self {
        Self {
            state: ContentState::default(),
        }
    }
}

impl Default for ContentModule {
    fn default() -> Self {
        Self::new()
    }
}

impl Module for ContentModule {
    fn id(&self) -> &'static str {
        "content"
    }

    fn name(&self) -> &'static str {
        "Content Management"
    }

    fn description(&self) -> &'static str {
        "Posts, categories, tags, and pages with slug handling, publishing, and excerpts, plus admin-definable custom post types with dynamic fields."
    }

    fn init(&self, _ctx: &AppContext) -> UnifiedResult<()> {
        ai::register_ai_capabilities("content", AI_CAPABILITIES);
        Ok(())
    }

    fn register_hooks(&self, bus: &HookBus) {
        // Ground AI capability calls with related published posts
        // and pages (style context only; the caller supplies the
        // text to draft or summarize). Tenant scoping: this store
        // is in-memory with no tenant partition yet, so there is
        // nothing to filter on; when batch 17 persistence lands,
        // this handler must return only the calling tenant's
        // content, keyed by the caller company id in the payload.
        for event in [
            "content.ai.draft_requested",
            "content.ai.summarize_requested",
        ] {
            let state = self.state.clone();
            bus.register(
                event,
                "content",
                Arc::new(move |ctx| {
                    let input = ctx.get("input").cloned().unwrap_or(Value::Null);
                    let query = ["title", "brief", "text"]
                        .iter()
                        .filter_map(|field| {
                            input.get(*field).and_then(|value| value.as_str())
                        })
                        .collect::<Vec<_>>()
                        .join(" ");
                    let related = related_content(&state, &query);
                    ctx.add_result(json!({ "related": related }));
                }),
            );
        }
    }

    fn route_info(&self) -> Vec<RouteInfo> {
        vec![
            RouteInfo { method: "GET", path: "/api/v1/content/posts", owner: "content", description: "List posts", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/content/posts", owner: "content", description: "Create post", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/content/posts/{id}", owner: "content", description: "Get post", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "PUT", path: "/api/v1/content/posts/{id}", owner: "content", description: "Update post", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "DELETE", path: "/api/v1/content/posts/{id}", owner: "content", description: "Delete post", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/content/posts/{id}/publish", owner: "content", description: "Publish post", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/content/posts/{id}/unpublish", owner: "content", description: "Unpublish post", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/content/categories", owner: "content", description: "List categories", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/content/categories", owner: "content", description: "Create category", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/content/categories/{id}", owner: "content", description: "Get category", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "PUT", path: "/api/v1/content/categories/{id}", owner: "content", description: "Update category", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "DELETE", path: "/api/v1/content/categories/{id}", owner: "content", description: "Delete category", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/content/pages", owner: "content", description: "List pages", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/content/pages", owner: "content", description: "Create page", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/content/pages/{id}", owner: "content", description: "Get page", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "PUT", path: "/api/v1/content/pages/{id}", owner: "content", description: "Update page", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "DELETE", path: "/api/v1/content/pages/{id}", owner: "content", description: "Delete page", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/content/pages/{id}/publish", owner: "content", description: "Publish page", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/content/pages/{id}/unpublish", owner: "content", description: "Unpublish page", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/content/ai/{capability}", owner: "content", description: "Run one registered content AI capability (draft, summarize)", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/content/tags", owner: "content", description: "List tags", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/content/tags", owner: "content", description: "Create tag", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/content/tags/{id}", owner: "content", description: "Get tag", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "PUT", path: "/api/v1/content/tags/{id}", owner: "content", description: "Update tag", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "DELETE", path: "/api/v1/content/tags/{id}", owner: "content", description: "Delete tag", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/content/posts/{id}/tags", owner: "content", description: "List a post's tags", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/content/posts/{id}/tags", owner: "content", description: "Assign tags to a post", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "DELETE", path: "/api/v1/content/posts/{id}/tags/{tag_id}", owner: "content", description: "Unassign a tag from a post", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/content/post-types", owner: "content", description: "List custom post type definitions", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/content/post-types", owner: "content", description: "Create custom post type definition", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/content/post-types/{id}", owner: "content", description: "Get custom post type definition", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "PUT", path: "/api/v1/content/post-types/{id}", owner: "content", description: "Update custom post type definition", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "DELETE", path: "/api/v1/content/post-types/{id}", owner: "content", description: "Soft-disable a custom post type (entries preserved)", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/content/post-types/{id}/enable", owner: "content", description: "Enable a custom post type", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/content/post-types/{id}/disable", owner: "content", description: "Disable a custom post type (entries preserved)", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/content/custom/{type_slug}", owner: "content", description: "List entries of a custom post type", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/content/custom/{type_slug}", owner: "content", description: "Create an entry of a custom post type", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/content/custom/{type_slug}/{id}", owner: "content", description: "Get a custom entry", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "PUT", path: "/api/v1/content/custom/{type_slug}/{id}", owner: "content", description: "Update a custom entry", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "DELETE", path: "/api/v1/content/custom/{type_slug}/{id}", owner: "content", description: "Delete a custom entry", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/content/custom/{type_slug}/slug/{slug}", owner: "content", description: "Public render of a custom entry by slug", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/content/custom/{type_slug}/entries/{id}/tags", owner: "content", description: "List a custom entry's tags", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/content/custom/{type_slug}/entries/{id}/tags", owner: "content", description: "Assign tags to a custom entry", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "DELETE", path: "/api/v1/content/custom/{type_slug}/entries/{id}/tags/{tag_id}", owner: "content", description: "Unassign a tag from a custom entry", auth: unified_core::auth::RouteAuth::Authenticated},
        ]
    }

    fn configure_routes(&self, cfg: &mut web::ServiceConfig) {
        let state = web::Data::new(self.state.clone());
        cfg.service(
            web::scope("/api/v1/content")
                .app_data(state)
                .route("/posts", web::get().to(list_posts))
                .route("/posts", web::post().to(create_post))
                .route("/posts/{id}", web::get().to(get_post))
                .route("/posts/{id}", web::put().to(update_post))
                .route("/posts/{id}", web::delete().to(delete_post))
                .route("/posts/{id}/publish", web::post().to(publish_post))
                .route("/posts/{id}/unpublish", web::post().to(unpublish_post))
                .route("/categories", web::get().to(list_categories))
                .route("/categories", web::post().to(create_category))
                .route("/categories/{id}", web::get().to(get_category))
                .route("/categories/{id}", web::put().to(update_category))
                .route("/categories/{id}", web::delete().to(delete_category))
                .route("/pages", web::get().to(list_pages))
                .route("/pages", web::post().to(create_page))
                .route("/pages/{id}", web::get().to(get_page))
                .route("/pages/{id}", web::put().to(update_page))
                .route("/pages/{id}", web::delete().to(delete_page))
                .route("/pages/{id}/publish", web::post().to(publish_page))
                .route("/pages/{id}/unpublish", web::post().to(unpublish_page))
                .route("/ai/{capability}", web::post().to(ai_capability))
                .route("/tags", web::get().to(list_tags))
                .route("/tags", web::post().to(create_tag))
                .route("/tags/{id}", web::get().to(get_tag))
                .route("/tags/{id}", web::put().to(update_tag))
                .route("/tags/{id}", web::delete().to(delete_tag))
                .route("/posts/{id}/tags", web::get().to(list_post_tags))
                .route("/posts/{id}/tags", web::post().to(assign_post_tags))
                .route("/posts/{id}/tags/{tag_id}", web::delete().to(unassign_post_tag))
                .route("/post-types", web::get().to(list_post_types))
                .route("/post-types", web::post().to(create_post_type))
                .route("/post-types/{id}", web::get().to(get_post_type))
                .route("/post-types/{id}", web::put().to(update_post_type))
                .route("/post-types/{id}", web::delete().to(delete_post_type))
                .route("/post-types/{id}/enable", web::post().to(enable_post_type))
                .route("/post-types/{id}/disable", web::post().to(disable_post_type))
                .route("/custom/{type_slug}", web::get().to(list_custom_entries))
                .route("/custom/{type_slug}", web::post().to(create_custom_entry))
                .route("/custom/{type_slug}/{id}", web::get().to(get_custom_entry))
                .route("/custom/{type_slug}/{id}", web::put().to(update_custom_entry))
                .route("/custom/{type_slug}/{id}", web::delete().to(delete_custom_entry))
                .route("/custom/{type_slug}/slug/{slug}", web::get().to(get_custom_entry_by_slug))
                .route("/custom/{type_slug}/entries/{id}/tags", web::get().to(list_custom_entry_tags))
                .route("/custom/{type_slug}/entries/{id}/tags", web::post().to(assign_custom_entry_tags))
                .route("/custom/{type_slug}/entries/{id}/tags/{tag_id}", web::delete().to(unassign_custom_entry_tag)),
        );
    }
}

pub fn register(registry: &ModuleRegistry) {
    registry.register(Arc::new(ContentModule::new()));
}
