//! seo - module.

// SEO Manager module.
//
// Site-wide SEO settings, a real on-page analyzer (title length, keyword
// presence and density, heading structure, meta description length, and a
// Flesch reading-ease estimate) that produces a 0-100 score, and a meta
// tag generator. The analyzer also runs automatically whenever the
// `content.published` hook fires.

use unified_core::hooks::HookBus;
use unified_core::modules::{Module, ModuleRegistry, RouteInfo};
use unified_core::context::AppContext;
use unified_core::error::UnifiedResult;
use crate::modules::ai;
use actix_web::{web};
use std::sync::{Arc};
use uuid::Uuid;
use super::{types::*, state::*, analysis::*, handlers::*};
use super::ai_capabilities::AI_CAPABILITIES;



// Module
pub struct SeoModule {
    pub(crate) state: SeoState,
}


impl SeoModule {
    pub fn new() -> Self {
        Self {
            state: SeoState::default(),
        }
    }
}


impl Default for SeoModule {
    fn default() -> Self {
        Self::new()
    }
}


impl Module for SeoModule {
    fn init(&self, _ctx: &AppContext) -> UnifiedResult<()> {
        ai::register_ai_capabilities("seo", AI_CAPABILITIES);
        Ok(())
    }

    fn id(&self) -> &'static str {
        "seo"
    }

    fn name(&self) -> &'static str {
        "SEO Manager"
    }

    fn description(&self) -> &'static str {
        "SEO settings, on-page analysis with scoring, and meta tag generation."
    }

    fn dependencies(&self) -> Vec<&'static str> {
        vec!["content"]
    }

    fn register_hooks(&self, bus: &HookBus) {
        // Auto-analyze every published post or page so the SEO overview
        // stays in sync with the content module.
        let state = self.state.clone();
        bus.register("content.published", "seo", Arc::new(move |ctx| {
            let title = ctx
                .get("title")
                .and_then(|value| value.as_str())
                .unwrap_or("")
                .to_string();
            let content = ctx
                .get("content")
                .and_then(|value| value.as_str())
                .unwrap_or("")
                .to_string();
            if title.is_empty() || content.is_empty() {
                return;
            }
            let post_id = ctx
                .get("id")
                .and_then(|value| value.as_str())
                .and_then(|raw| raw.parse::<Uuid>().ok());
            let analysis = analyze_content(&title, &content, None, None, None, post_id);
            state.analyses.insert(analysis.id, analysis);
        }));
    }

    fn route_info(&self) -> Vec<RouteInfo> {
        vec![
            RouteInfo { method: "GET", path: "/api/v1/seo/settings", owner: "seo", description: "Get settings", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "PUT", path: "/api/v1/seo/settings", owner: "seo", description: "Update settings", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/seo/analyze", owner: "seo", description: "Analyze", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/seo/meta", owner: "seo", description: "Generate meta tags", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/seo/analyses", owner: "seo", description: "List analyses", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/seo/analyses/{id}", owner: "seo", description: "Get analysis", auth: unified_core::auth::RouteAuth::Authenticated},
        ]
    }

    fn configure_routes(&self, cfg: &mut web::ServiceConfig) {
        let state = web::Data::new(self.state.clone());
        cfg.service(
            web::scope("/api/v1/seo")
                .app_data(state)
                .route("/settings", web::get().to(get_settings))
                .route("/settings", web::put().to(update_settings))
                .route("/analyze", web::post().to(analyze))
                .route("/meta", web::post().to(generate_meta_tags))
                .route("/analyses", web::get().to(list_analyses))
                .route("/analyses/{id}", web::get().to(get_analysis)),
        );
    }
}


pub fn register(registry: &ModuleRegistry) {
    registry.register(Arc::new(SeoModule::new()));
}
