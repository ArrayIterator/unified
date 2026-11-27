// Site Builder - In-memory state and its constructors.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::HookBus;
use unified_core::json as api_json;
use unified_core::modules::{Module, ModuleRegistry, RouteInfo};
use unified_core::pagination::{PageResult, PaginationParams};
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
use super::helpers::*;
use super::validation::*;
use super::rendering::*;
use super::handlers::*;
use super::module::*;

pub struct SiteBuilderState {
    pub(crate) pages: RwLock<HashMap<String, Page>>,
    pub(crate) hooks: RwLock<Option<Arc<HookBus>>>,
    pub(crate) debug: AtomicBool,
}

impl SiteBuilderState {
    pub(crate) fn new() -> Self {
        Self {
            pages: RwLock::new(HashMap::new()),
            hooks: RwLock::new(None),
            debug: AtomicBool::new(false),
        }
    }

    pub(crate) fn set_hooks(&self, hooks: Arc<HookBus>) {
        *self.hooks.write().expect("site builder hooks lock poisoned") = Some(hooks);
    }

    pub(crate) fn hooks(&self) -> Option<Arc<HookBus>> {
        self.hooks.read().expect("site builder hooks lock poisoned").clone()
    }

    pub(crate) fn is_debug(&self) -> bool {
        self.debug.load(Ordering::SeqCst)
    }

    pub(crate) fn get_page(&self, id: &str) -> Option<Page> {
        self.pages
            .read()
            .expect("site builder lock poisoned")
            .get(id)
            .cloned()
    }

    pub(crate) fn get_page_by_slug(&self, slug: &str) -> Option<Page> {
        self.pages
            .read()
            .expect("site builder lock poisoned")
            .values()
            .find(|page| page.slug == slug)
            .cloned()
    }

    /// Seed the default home page as a site-builder page: 
    /// a hero block and a text block - never a static template. The
    /// hero carries the tagline and description as editable block
    /// content (the old hard-coded strings live here as the
    /// documented fallbacks). Runs once at startup; a page with slug
    /// `home` already present is left alone.
    pub(crate) fn seed_default_home_page(&self) {
        let mut pages = self.pages.write().expect("site builder lock poisoned");
        if pages.values().any(|page| page.slug == "home") {
            return;
        }
        let now = Utc::now().to_rfc3339();
        let page = Page {
            id: "home".to_string(),
            title: "Unified".to_string(),
            slug: "home".to_string(),
            theme_id: "default".to_string(),
            status: "published".to_string(),
            blocks: vec![
                Block {
                    id: "home-hero".to_string(),
                    block_type: "hero".to_string(),
                    content: json!({
                        "heading": "Unified",
                        "subheading": "One control plane for everything you run.",
                        "cta_text": "Open dashboard",
                        "cta_url": "/dashboard"
                    }),
                    order: 0,
                },
                Block {
                    id: "home-intro".to_string(),
                    block_type: "text".to_string(),
                    content: json!({
                        "text": "Licensing, downloads, storefront, software and asset management, and day-to-day operations in one place."
                    }),
                    order: 1,
                },
            ],
            sections: Vec::new(),
            created_at: now.clone(),
            updated_at: now,
        };
        pages.insert(page.id.clone(), page);
    }

    pub(crate) fn slug_taken(&self, slug: &str, except_id: Option<&str>) -> bool {
        self.pages
            .read()
            .expect("site builder lock poisoned")
            .values()
            .any(|page| page.slug == slug && Some(page.id.as_str()) != except_id)
    }

    /// Ask the Theme module whether a theme exists. Returns `Ok(true)` /
    /// `Ok(false)` when the Theme module answers, and `Ok(None)` when no
    /// handler is registered for `theme.exists`.
    pub(crate) fn theme_exists(&self, theme_id: &str) -> Option<bool> {
        let hooks = self.hooks()?;
        let answer = hooks.emit(EVENT_THEME_EXISTS, json!({ "theme_id": theme_id }));
        answer.results.first().and_then(|result| {
            result
                .get("exists")
                .and_then(|v| v.as_bool())
                .or_else(|| result.get("found").and_then(|v| v.as_bool()))
        })
    }
}
