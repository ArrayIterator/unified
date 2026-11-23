// Global Search module.
//
// One search box across everything in the application. The index is
// in-memory and is fed in two ways:
//
//   1. The `search.index` hook: any module publishes a document by
//      emitting `search.index` with the document fields (`id`, `kind`,
//      `title`, `snippet`, `url`, `updated_at`) as the payload. The
//      hook bus accepts arbitrary event names, so no bus change is
//      needed for this.
//   2. The `POST /api/v1/search/index` endpoint below, for modules or
//      operators that prefer an HTTP call.
//
// `POST /api/v1/search/reindex` emits `search.reindex.requested` so
// every module can republish its documents; the search module itself
// answers the event with a listener that rebuilds the index from the
// documents it already holds (re-validating each one and dropping
// entries that no longer pass), so the event never goes into the void.
//
// Access control: the three index-mutating endpoints (`POST /index`,
// `POST /reindex`, `DELETE /{kind}/{id}`) require the caller identity
// headers (`X-User-Id` + `X-Role`) and the `search.manage` capability
// through the `role.check` hook, failing closed. The read endpoints
// (search, suggest, stats) are public.
//
// Ranking algorithm (documented honestly — this is substring and token
// matching, not a full-text engine): the query is lowercased and split
// on whitespace into tokens. A document is a hit only when every token
// appears as a substring of the title or the snippet (AND semantics).
// Score: +100 when the whole query appears in the title, +10 per token
// found in the title, +20 when the whole query appears in the snippet,
// +3 per token found in the snippet. Hits sort by score descending,
// then by `updated_at` descending (recency tiebreak), then by title
// ascending for determinism. `GET /search/suggest` returns distinct
// title words that start with the query prefix, most frequent first.


use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::{HookBus, HookContext, HookHandler};
use unified_core::json;
use unified_core::modules::{Module, ModuleRegistry, RouteInfo};
use unified_core::pagination::{PageResult, Pagination, PaginationParams};
use crate::modules::ai;
use actix_web::{web, HttpResponse};
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::Arc;

use super::constants::*;
use super::types::*;
use super::store::*;
use super::service::*;
use super::handlers::*;
use super::hooks::*;
use super::ai_capabilities::AI_CAPABILITIES;

// ---------------------------------------------------------------------------
// Module.
// ---------------------------------------------------------------------------

pub struct SearchModule {
    state: Arc<SearchState>,
}

impl SearchModule {
    pub fn new() -> Self {
        Self {
            state: Arc::new(SearchState::new()),
        }
    }
}

impl Default for SearchModule {
    fn default() -> Self {
        Self::new()
    }
}

impl Module for SearchModule {
    fn init(&self, _ctx: &AppContext) -> UnifiedResult<()> {
        ai::register_ai_capabilities("search", AI_CAPABILITIES);
        Ok(())
    }

    fn id(&self) -> &'static str {
        "search"
    }

    fn name(&self) -> &'static str {
        "Global Search"
    }

    fn description(&self) -> &'static str {
        "Global search across software, products, orders, invoices, licenses, users, companies, domains, tasks, articles, tickets, servers, clusters and backups."
    }

    fn register_hooks(&self, bus: &HookBus) {
        let state = self.state.clone();
        let handler: HookHandler =
            Arc::new(move |ctx: &mut HookContext| on_search_index(state.clone(), ctx));
        bus.register("search.index", "search", handler);
        // The reindex listener rebuilds the index from the documents
        // already held (re-validating each one); source modules
        // additionally republish through `search.index` when they hear
        // `search.reindex.requested`.
        let state = self.state.clone();
        let rebuild: HookHandler = Arc::new(move |ctx: &mut HookContext| {
            on_search_reindex_requested(state.clone(), ctx)
        });
        bus.register("search.reindex.requested", "search", rebuild);
    }

    fn configure_routes(&self, cfg: &mut web::ServiceConfig) {
        cfg.app_data(web::Data::new(self.state.clone()));
        cfg.service(
            web::scope("/api/v1/search")
                .route("", web::get().to(search))
                .route("/suggest", web::get().to(suggest))
                .route("/stats", web::get().to(stats))
                .route("/index", web::post().to(index_document))
                .route("/reindex", web::post().to(request_reindex))
                .route("/{kind}/{id}", web::delete().to(delete_document)),
        );
    }

    fn route_info(&self) -> Vec<RouteInfo> {
        vec![
            RouteInfo { method: "GET", path: "/api/v1/search", owner: "search", description: "PUBLIC: ranked global search, grouped by kind, paginated.", auth: unified_core::auth::RouteAuth::Public},
            RouteInfo { method: "GET", path: "/api/v1/search/suggest", owner: "search", description: "PUBLIC: prefix suggestions from indexed titles.", auth: unified_core::auth::RouteAuth::Public},
            RouteInfo { method: "GET", path: "/api/v1/search/stats", owner: "search", description: "PUBLIC: index statistics grouped by kind.", auth: unified_core::auth::RouteAuth::Public},
            RouteInfo { method: "POST", path: "/api/v1/search/index", owner: "search", description: "Index (or replace) one document (requires search.manage; document URLs must use http(s), a site-relative path, or a fragment).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/search/reindex", owner: "search", description: "Request a rebuild; emits search.reindex.requested (requires search.manage). The search module's own listener rebuilds the index from the documents it holds.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "DELETE", path: "/api/v1/search/{kind}/{id}", owner: "search", description: "Remove one document from the index (requires search.manage).", auth: unified_core::auth::RouteAuth::Authenticated},
        ]
    }
}

pub fn register(registry: &ModuleRegistry) {
    registry.register(Arc::new(SearchModule::new()));
}
