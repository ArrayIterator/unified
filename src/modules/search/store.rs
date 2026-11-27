// Global Search - In-memory state and its constructors.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::{HookBus, HookContext, HookHandler};
use unified_core::json;
use unified_core::modules::{Module, ModuleRegistry, RouteInfo};
use unified_core::pagination::{PageResult, Pagination, PaginationParams};
use actix_web::{web, HttpResponse};
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::Arc;

use super::constants::*;
use super::types::*;
use super::service::*;
use super::handlers::*;
use super::hooks::*;
use super::module::*;

// State.

pub struct SearchState {
    /// Keyed by `{kind}:{id}` so re-indexing the same document replaces
    /// it instead of duplicating it.
    pub(crate) documents: DashMap<String, SearchDocument>,
}

impl SearchState {
    pub(crate) fn new() -> Self {
        Self {
            documents: DashMap::new(),
        }
    }

    pub(crate) fn key(kind: &str, id: &str) -> String {
        format!("{kind}:{id}")
    }

    /// Validate and insert (or replace) one document. Returns the
    /// stored document.
    pub(crate) fn index(&self, input: IndexDocumentRequest) -> UnifiedResult<SearchDocument> {
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
        // A stored document link must use the allowlisted URL shapes
        // (see `is_safe_url`): a `javascript:` URL stored here would
        // execute on click wherever the hit is rendered.
        if let Some(url) = input.url.as_deref().filter(|url| !url.trim().is_empty()) {
            if !is_safe_url(url) {
                return Err(UnifiedError::bad_request(
                    "url must use http(s), a site-relative path, or a fragment",
                ));
            }
        }
        let document = SearchDocument {
            id: input.id.trim().to_string(),
            kind: kind.clone(),
            title: input.title.trim().to_string(),
            snippet: input.snippet.unwrap_or_default().trim().to_string(),
            url: input
                .url
                .filter(|url| !url.trim().is_empty())
                .map(|url| url.trim().to_string()),
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
    pub(crate) fn index_from_hook(&self, data: &Value) -> UnifiedResult<SearchDocument> {
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
    pub(crate) fn evict_oldest_if_over_cap(&self) {
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

    pub(crate) fn all_documents(&self) -> Vec<SearchDocument> {
        self.documents
            .iter()
            .map(|entry| entry.value().clone())
            .collect()
    }

    /// Rebuild the in-memory index from the documents it already
    /// holds: every stored document is re-validated against the
    /// current rules (known kind, non-empty id/title, allowlisted URL
    /// scheme) and only valid documents are kept. Returns `(kept,
    /// dropped)`. This is the search module's own
    /// `search.reindex.requested` listener (see `hooks`); source
    /// modules additionally republish their documents through
    /// `search.index` when they hear the event.
    pub(crate) fn rebuild_index(&self) -> (usize, usize) {
        let documents = self.all_documents();
        self.documents.clear();
        let mut kept = 0usize;
        let mut dropped = 0usize;
        for document in documents {
            let valid = SEARCH_KINDS.contains(&document.kind.as_str())
                && !document.id.trim().is_empty()
                && !document.title.trim().is_empty()
                && document
                    .url
                    .as_deref()
                    .map_or(true, is_safe_url);
            if valid {
                self.documents
                    .insert(Self::key(&document.kind, &document.id), document);
                kept += 1;
            } else {
                dropped += 1;
            }
        }
        (kept, dropped)
    }

    /// Score one document against the query tokens. Returns `None`
    /// when the document is not a hit (any token missing).
    pub(crate) fn score(query: &str, tokens: &[String], document: &SearchDocument) -> Option<i64> {
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

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{DateTime, Utc};
    use serde_json::json;

    fn request(id: &str, kind: &str, title: &str, snippet: &str) -> IndexDocumentRequest {
        IndexDocumentRequest {
            id: id.to_string(),
            kind: kind.to_string(),
            title: title.to_string(),
            snippet: Some(snippet.to_string()),
            url: None,
            updated_at: None,
        }
    }

    fn document(title: &str, snippet: &str) -> SearchDocument {
        SearchDocument {
            id: "1".to_string(),
            kind: "task".to_string(),
            title: title.to_string(),
            snippet: snippet.to_string(),
            url: None,
            updated_at: DateTime::from_timestamp(1_700_000_000, 0).unwrap(),
        }
    }

    #[test]
    fn index_normalises_kind_and_trims_fields() {
        let state = SearchState::new();
        let stored = state.index(request("doc-1", "Task", "  Quarterly Report  ", "sales")).unwrap();
        assert_eq!(stored.kind, "task");
        assert_eq!(stored.title, "Quarterly Report");
        assert_eq!(stored.id, "doc-1");
        assert_eq!(state.all_documents().len(), 1);
    }

    #[test]
    fn index_rejects_unknown_kinds_and_empty_fields() {
        let state = SearchState::new();
        assert!(state.index(request("1", "spaceship", "Title", "")).is_err());
        assert!(state.index(request("", "task", "Title", "")).is_err());
        assert!(state.index(request("1", "task", "   ", "")).is_err());
    }

    #[test]
    fn index_replaces_a_document_with_the_same_key() {
        let state = SearchState::new();
        state.index(request("doc-1", "task", "First title", "")).unwrap();
        state.index(request("doc-1", "task", "Second title", "")).unwrap();
        let docs = state.all_documents();
        assert_eq!(docs.len(), 1);
        assert_eq!(docs[0].title, "Second title");
    }

    #[test]
    fn score_rewards_title_matches_and_requires_every_token() {
        let doc = document("Quarterly Report", "sales numbers");
        let tokens = tokenize("quarterly report");
        assert_eq!(SearchState::score("quarterly report", &tokens, &doc), Some(120));
        let tokens = tokenize("quarterly missing");
        assert_eq!(SearchState::score("quarterly missing", &tokens, &doc), None);
        let tokens = tokenize("sales");
        assert_eq!(SearchState::score("sales", &tokens, &doc), Some(23));
        let tokens = tokenize("nothing");
        assert_eq!(SearchState::score("nothing", &tokens, &doc), None);
    }

    #[test]
    fn index_from_hook_accepts_valid_payloads_and_rejects_bad_ones() {
        let state = SearchState::new();
        let stored = state
            .index_from_hook(&json!({"id": "h-1", "kind": "invoice", "title": "Invoice 42", "snippet": "paid"}))
            .unwrap();
        assert_eq!(stored.kind, "invoice");
        assert!(state
            .index_from_hook(&json!({"id": "h-2", "kind": "spaceship", "title": "Nope"}))
            .is_err());
        assert!(state
            .index_from_hook(&json!({"id": "h-3", "kind": "task"}))
            .is_err());
    }

    fn request_with_url(id: &str, url: &str) -> IndexDocumentRequest {
        IndexDocumentRequest {
            id: id.to_string(),
            kind: "task".to_string(),
            title: "Title".to_string(),
            snippet: None,
            url: Some(url.to_string()),
            updated_at: None,
        }
    }

    #[test]
    fn index_rejects_dangerous_urls_and_accepts_allowlisted_ones() {
        let state = SearchState::new();
        assert!(state.index(request_with_url("1", "javascript:alert(1)")).is_err());
        assert!(state.index(request_with_url("2", "data:text/html,hi")).is_err());
        assert!(state.index(request_with_url("3", "//evil.example.com/x")).is_err());
        assert!(state.index(request_with_url("4", "JaVaScRiPt:alert(1)")).is_err());
        assert!(state
            .index_from_hook(&json!({"id": "h-9", "kind": "task", "title": "Hook", "url": "javascript:alert(1)"}))
            .is_err());
        for (id, url) in [
            ("ok-1", "https://example.com/tasks/1"),
            ("ok-2", "http://example.com/tasks/2"),
            ("ok-3", "/tasks/3"),
            ("ok-4", "#task-4"),
        ] {
            let stored = state.index(request_with_url(id, url)).unwrap();
            assert_eq!(stored.url.as_deref(), Some(url));
        }
        assert_eq!(state.all_documents().len(), 4);
    }

    #[test]
    fn rebuild_index_drops_documents_that_no_longer_validate() {
        let state = SearchState::new();
        state.index(request("good-1", "task", "Good", "")).unwrap();
        // A document stored before URL validation existed (inserted
        // directly, bypassing `index`): the rebuild must drop it.
        state.documents.insert(
            "task:stale-1".to_string(),
            document("Stale", ""),
        );
        state
            .documents
            .get_mut("task:stale-1")
            .expect("inserted document is present")
            .url = Some("javascript:alert(1)".to_string());
        state.documents.insert(
            "spaceship:stale-2".to_string(),
            SearchDocument {
                id: "stale-2".to_string(),
                kind: "spaceship".to_string(),
                title: "Unknown kind".to_string(),
                snippet: String::new(),
                url: None,
                updated_at: DateTime::from_timestamp(1_700_000_000, 0).unwrap(),
            },
        );
        let (kept, dropped) = state.rebuild_index();
        assert_eq!((kept, dropped), (1, 2));
        let docs = state.all_documents();
        assert_eq!(docs.len(), 1);
        assert_eq!(docs[0].id, "good-1");
    }
}
