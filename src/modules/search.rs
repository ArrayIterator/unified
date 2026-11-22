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
// does not reach into other modules' stores.
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

use crate::core::context::AppContext;
use crate::core::error::{UnifiedError, UnifiedResult};
use crate::core::hooks::{HookBus, HookContext, HookHandler};
use crate::core::json;
use crate::core::modules::{Module, ModuleRegistry, RouteInfo};
use crate::core::pagination::{PageResult, Pagination, PaginationParams};
use actix_web::{web, HttpResponse};
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::Arc;

// ---------------------------------------------------------------------------
// Types.
// ---------------------------------------------------------------------------

/// Document kinds the global index accepts. Keep this list in sync with
/// the kinds named in the module documentation above.
pub const SEARCH_KINDS: &[&str] = &[
    "software", "product", "order", "invoice", "license", "user", "company", "domain",
    "task", "article", "ticket", "server", "cluster", "backup",
];

/// Maximum number of documents kept in the in-memory index. Oldest
/// documents (by `updated_at`) are evicted first when the cap is hit.
pub const MAX_DOCUMENTS: usize = 20_000;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchDocument {
    pub id: String,
    pub kind: String,
    pub title: String,
    pub snippet: String,
    #[serde(default)]
    pub url: Option<String>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct IndexDocumentRequest {
    pub id: String,
    pub kind: String,
    pub title: String,
    #[serde(default)]
    pub snippet: Option<String>,
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub updated_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SearchHit {
    pub document: SearchDocument,
    pub score: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct KindGroup {
    pub kind: String,
    pub count: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct SearchResponse {
    pub items: Vec<SearchHit>,
    pub groups: Vec<KindGroup>,
    pub pagination: crate::core::pagination::PageInfo,
}

#[derive(Debug, Clone, Serialize)]
pub struct SuggestResponse {
    pub query: String,
    pub suggestions: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SearchStats {
    pub total_documents: u64,
    pub by_kind: Vec<KindGroup>,
    pub kinds: Vec<String>,
    pub max_documents: usize,
}

// ---------------------------------------------------------------------------
// State.
// ---------------------------------------------------------------------------

pub struct SearchState {
    /// Keyed by `{kind}:{id}` so re-indexing the same document replaces
    /// it instead of duplicating it.
    documents: DashMap<String, SearchDocument>,
}

impl SearchState {
    fn new() -> Self {
        Self {
            documents: DashMap::new(),
        }
    }

    fn key(kind: &str, id: &str) -> String {
        format!("{kind}:{id}")
    }

    /// Validate and insert (or replace) one document. Returns the
    /// stored document.
    fn index(&self, input: IndexDocumentRequest) -> UnifiedResult<SearchDocument> {
        let kind = input.kind.trim().to_lowercase();
        if !SEARCH_KINDS.contains(&kind.as_str()) {
            return Err(UnifiedError::bad_request(format!(
                "kind must be one of: {}",
                SEARCH_KINDS.join(", ")
            ))
            .with_metadata("allowed_kinds", json!(SEARCH_KINDS)));
        }
        if input.id.trim().is_empty() {
            return Err(UnifiedError::bad_request("id must not be empty"));
        }
        if input.title.trim().is_empty() {
            return Err(UnifiedError::bad_request("title must not be empty"));
        }
        let document = SearchDocument {
            id: input.id.trim().to_string(),
            kind: kind.clone(),
            title: input.title.trim().to_string(),
            snippet: input.snippet.unwrap_or_default().trim().to_string(),
            url: input.url.filter(|url| !url.trim().is_empty()),
            updated_at: input.updated_at.unwrap_or_else(Utc::now),
        };
        self.documents
            .insert(Self::key(&kind, &document.id), document.clone());
        self.evict_oldest_if_over_cap();
        Ok(document)
    }

    /// Index a document published through the `search.index` hook.
    /// Malformed payloads are ignored (a hook must never panic the
    /// emitter); the handler records the outcome in the hook results.
    fn index_from_hook(&self, data: &Value) -> UnifiedResult<SearchDocument> {
        let input = IndexDocumentRequest {
            id: data
                .get("id")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
            kind: data
                .get("kind")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
            title: data
                .get("title")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
            snippet: data
                .get("snippet")
                .and_then(|v| v.as_str())
                .map(str::to_string),
            url: data
                .get("url")
                .and_then(|v| v.as_str())
                .map(str::to_string),
            updated_at: data
                .get("updated_at")
                .and_then(|v| v.as_str())
                .and_then(|raw| raw.parse::<DateTime<Utc>>().ok()),
        };
        self.index(input)
    }

    /// Drop the oldest documents while the index is over the cap.
    fn evict_oldest_if_over_cap(&self) {
        if self.documents.len() <= MAX_DOCUMENTS {
            return;
        }
        let mut entries: Vec<(String, DateTime<Utc>)> = self
            .documents
            .iter()
            .map(|entry| (entry.key().clone(), entry.value().updated_at))
            .collect();
        entries.sort_by(|a, b| a.1.cmp(&b.1));
        let excess = self.documents.len() - MAX_DOCUMENTS;
        for (key, _) in entries.into_iter().take(excess) {
            self.documents.remove(&key);
        }
    }

    fn all_documents(&self) -> Vec<SearchDocument> {
        self.documents
            .iter()
            .map(|entry| entry.value().clone())
            .collect()
    }

    /// Score one document against the query tokens. Returns `None`
    /// when the document is not a hit (any token missing).
    fn score(query: &str, tokens: &[String], document: &SearchDocument) -> Option<i64> {
        let title = document.title.to_lowercase();
        let snippet = document.snippet.to_lowercase();
        let mut score: i64 = 0;
        if title.contains(query) {
            score += 100;
        }
        if snippet.contains(query) {
            score += 20;
        }
        for token in tokens {
            let in_title = title.contains(token.as_str());
            let in_snippet = snippet.contains(token.as_str());
            if !in_title && !in_snippet {
                return None;
            }
            if in_title {
                score += 10;
            }
            if in_snippet {
                score += 3;
            }
        }
        Some(score)
    }
}

// ---------------------------------------------------------------------------
// Helpers.
// ---------------------------------------------------------------------------

fn tokenize(query: &str) -> Vec<String> {
    query
        .split_whitespace()
        .map(|token| token.to_lowercase())
        .filter(|token| !token.is_empty())
        .collect()
}

fn paginate_hits(
    mut hits: Vec<SearchHit>,
    params: &PaginationParams,
) -> PageResult<SearchHit> {
    let pagination: Pagination = params.normalized();
    hits.sort_by(|a, b| {
        b.score
            .cmp(&a.score)
            .then(b.document.updated_at.cmp(&a.document.updated_at))
            .then(a.document.title.cmp(&b.document.title))
    });
    let total = hits.len() as u64;
    let start = ((pagination.page - 1) * pagination.per_page) as usize;
    let page_items: Vec<SearchHit> = hits
        .into_iter()
        .skip(start)
        .take(pagination.per_page as usize)
        .collect();
    PageResult::new(page_items, total, &pagination)
}

fn group_counts(documents: &[SearchDocument]) -> Vec<KindGroup> {
    let mut counts: HashMap<String, u64> = HashMap::new();
    for document in documents {
        *counts.entry(document.kind.clone()).or_insert(0) += 1;
    }
    let mut groups: Vec<KindGroup> = counts
        .into_iter()
        .map(|(kind, count)| KindGroup { kind, count })
        .collect();
    groups.sort_by(|a, b| b.count.cmp(&a.count).then(a.kind.cmp(&b.kind)));
    groups
}

// ---------------------------------------------------------------------------
// Handlers.
// ---------------------------------------------------------------------------

async fn search(
    state: web::Data<Arc<SearchState>>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = false;
    let params = query.into_inner();
    let raw_query = params
        .search
        .clone()
        .or_else(|| params.filters.get("q").cloned())
        .unwrap_or_default();
    let query_text = raw_query.trim().to_lowercase();
    if query_text.is_empty() {
        return json::error_response(
            &UnifiedError::bad_request("Query parameter q is required"),
            debug,
        );
    }
    let kind_filter = params.filters.get("kind").cloned();
    if let Some(kind) = &kind_filter {
        if !SEARCH_KINDS.contains(&kind.to_lowercase().as_str()) {
            return json::error_response(
                &UnifiedError::bad_request(format!(
                    "kind must be one of: {}",
                    SEARCH_KINDS.join(", ")
                )),
                debug,
            );
        }
    }
    let tokens = tokenize(&query_text);
    let documents = state.all_documents();
    let mut hits: Vec<SearchHit> = Vec::new();
    let mut matched: Vec<SearchDocument> = Vec::new();
    for document in &documents {
        if let Some(kind) = &kind_filter {
            if !document.kind.eq_ignore_ascii_case(kind) {
                continue;
            }
        }
        if let Some(score) = SearchState::score(&query_text, &tokens, document) {
            hits.push(SearchHit {
                document: document.clone(),
                score,
            });
            matched.push(document.clone());
        }
    }
    let groups = group_counts(&matched);
    let page = paginate_hits(hits, &params);
    let response = SearchResponse {
        items: page.items.clone(),
        groups,
        pagination: page.pagination,
    };
    json::success_response(response, 200)
}

async fn suggest(
    state: web::Data<Arc<SearchState>>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let params = query.into_inner();
    let raw_query = params
        .search
        .clone()
        .or_else(|| params.filters.get("q").cloned())
        .unwrap_or_default();
    let prefix = raw_query.trim().to_lowercase();
    if prefix.is_empty() {
        return json::error_response(
            &UnifiedError::bad_request("Query parameter q is required"),
            false,
        );
    }
    if prefix.len() < 2 {
        return json::error_response(
            &UnifiedError::bad_request("Query parameter q must be at least 2 characters"),
            false,
        );
    }
    let limit = params.normalized().per_page as usize;
    let mut frequencies: HashMap<String, u64> = HashMap::new();
    for document in state.all_documents() {
        for word in document.title.split_whitespace() {
            let word = word
                .trim_matches(|c: char| !c.is_alphanumeric())
                .to_lowercase();
            if word.starts_with(&prefix) && word.len() > prefix.len() {
                *frequencies.entry(word).or_insert(0) += 1;
            }
        }
    }
    let mut suggestions: Vec<(String, u64)> = frequencies.into_iter().collect();
    suggestions.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    let suggestions: Vec<String> = suggestions
        .into_iter()
        .take(limit)
        .map(|(word, _)| word)
        .collect();
    json::success_response(
        SuggestResponse {
            query: prefix,
            suggestions,
        },
        200,
    )
}

async fn index_document(
    state: web::Data<Arc<SearchState>>,
    body: web::Json<IndexDocumentRequest>,
) -> HttpResponse {
    let result = state.index(body.into_inner());
    match result {
        Ok(document) => json::success_response(document, 201),
        Err(error) => json::error_response(&error, false),
    }
}

async fn request_reindex(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<SearchState>>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    ctx.hooks.emit(
        "search.reindex.requested",
        json!({
            "requested_at": Utc::now().to_rfc3339(),
            "indexed_documents": state.documents.len(),
        }),
    );
    json::success_response(
        json!({
            "reindex_requested": true,
            "indexed_documents": state.documents.len(),
        }),
        200,
    )
}

async fn delete_document(
    state: web::Data<Arc<SearchState>>,
    path: web::Path<(String, String)>,
) -> HttpResponse {
    let (kind, id) = path.into_inner();
    let key = SearchState::key(&kind.to_lowercase(), &id);
    let result: UnifiedResult<Value> = state
        .documents
        .remove(&key)
        .map(|_| json!({ "deleted": true, "kind": kind, "id": id }))
        .ok_or_else(|| UnifiedError::not_found("Search document not found"));
    json::respond(result, false)
}

async fn stats(state: web::Data<Arc<SearchState>>) -> HttpResponse {
    let documents = state.all_documents();
    let by_kind = group_counts(&documents);
    json::success_response(
        SearchStats {
            total_documents: documents.len() as u64,
            by_kind,
            kinds: SEARCH_KINDS.iter().map(|kind| kind.to_string()).collect(),
            max_documents: MAX_DOCUMENTS,
        },
        200,
    )
}

// ---------------------------------------------------------------------------
// Hook handlers.
// ---------------------------------------------------------------------------

fn on_search_index(state: Arc<SearchState>, ctx: &mut HookContext) {
    let data = ctx.data.clone();
    match state.index_from_hook(&data) {
        Ok(document) => {
            ctx.add_result(json!({
                "indexed": true,
                "kind": document.kind,
                "id": document.id,
            }));
        }
        Err(error) => {
            ctx.add_result(json!({ "indexed": false, "error": error.message }));
        }
    }
}

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
            RouteInfo { method: "GET", path: "/api/v1/search", owner: "search", description: "Ranked global search, grouped by kind, paginated." },
            RouteInfo { method: "GET", path: "/api/v1/search/suggest", owner: "search", description: "Prefix suggestions from indexed titles." },
            RouteInfo { method: "GET", path: "/api/v1/search/stats", owner: "search", description: "Index statistics grouped by kind." },
            RouteInfo { method: "POST", path: "/api/v1/search/index", owner: "search", description: "Index (or replace) one document." },
            RouteInfo { method: "POST", path: "/api/v1/search/reindex", owner: "search", description: "Request a rebuild; emits search.reindex.requested." },
            RouteInfo { method: "DELETE", path: "/api/v1/search/{kind}/{id}", owner: "search", description: "Remove one document from the index." },
        ]
    }
}

pub fn register(registry: &ModuleRegistry) {
    registry.register(Arc::new(SearchModule::new()));
}
