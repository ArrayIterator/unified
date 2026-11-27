// Global Search - HTTP handlers.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::{HookBus, HookContext, HookHandler};
use unified_core::json;
use unified_core::modules::{Module, ModuleRegistry, RouteInfo};
use unified_core::pagination::{PageResult, Pagination, PaginationParams};
use actix_web::{web, HttpRequest, HttpResponse};
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
use super::hooks::*;
use super::module::*;

// Handlers.

pub(crate) async fn search(
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

pub(crate) async fn suggest(
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

pub(crate) async fn index_document(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<SearchState>>,
    req: HttpRequest,
    body: web::Json<IndexDocumentRequest>,
) -> HttpResponse {
    if let Err(error) = authorize(&ctx, &req, PERM_SEARCH_MANAGE) {
        return json::error_response(&error, ctx.is_debug());
    }
    let result = state.index(body.into_inner());
    match result {
        Ok(document) => json::success_response(document, 201),
        Err(error) => json::error_response(&error, ctx.is_debug()),
    }
}

pub(crate) async fn request_reindex(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<SearchState>>,
    req: HttpRequest,
) -> HttpResponse {
    let debug = ctx.is_debug();
    if let Err(error) = authorize(&ctx, &req, PERM_SEARCH_MANAGE) {
        return json::error_response(&error, debug);
    }
    // The search module's own listener rebuilds the index from the
    // documents it holds (see `on_search_reindex_requested`); source
    // modules additionally republish through `search.index` when they
    // hear this event.
    let outcome = ctx.hooks.emit(
        "search.reindex.requested",
        json!({
            "requested_at": Utc::now().to_rfc3339(),
            "indexed_documents": state.documents.len(),
        }),
    );
    let rebuilt = outcome.results.first();
    let kept = rebuilt
        .and_then(|result| result.get("kept_documents"))
        .and_then(|value| value.as_u64())
        .unwrap_or(state.documents.len() as u64);
    let dropped = rebuilt
        .and_then(|result| result.get("dropped_documents"))
        .and_then(|value| value.as_u64())
        .unwrap_or(0);
    json::success_response(
        json!({
            "reindex_requested": true,
            "indexed_documents": kept,
            "rebuilt_documents": kept,
            "dropped_documents": dropped,
        }),
        200,
    )
}

pub(crate) async fn delete_document(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<SearchState>>,
    req: HttpRequest,
    path: web::Path<(String, String)>,
) -> HttpResponse {
    if let Err(error) = authorize(&ctx, &req, PERM_SEARCH_MANAGE) {
        return json::error_response(&error, ctx.is_debug());
    }
    let (kind, id) = path.into_inner();
    let key = SearchState::key(&kind.to_lowercase(), &id);
    let result: UnifiedResult<Value> = state
        .documents
        .remove(&key)
        .map(|_| json!({ "deleted": true, "kind": kind, "id": id }))
        .ok_or_else(|| UnifiedError::not_found("Search document not found"));
    json::respond(result, ctx.is_debug())
}

pub(crate) async fn stats(state: web::Data<Arc<SearchState>>) -> HttpResponse {
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
