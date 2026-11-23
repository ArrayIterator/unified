// Global Search — Hook-bus listeners.

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
use super::store::*;
use super::service::*;
use super::handlers::*;
use super::module::*;

// ---------------------------------------------------------------------------
// Hook handlers.
// ---------------------------------------------------------------------------

pub(crate) fn on_search_index(state: Arc<SearchState>, ctx: &mut HookContext) {
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

/// Rebuild the index when `search.reindex.requested` fires:
/// re-validate every stored document and drop entries that no longer
/// pass the current rules, then report the counts so the requester
/// can see the rebuild actually ran instead of the event going into
/// the void.
pub(crate) fn on_search_reindex_requested(state: Arc<SearchState>, ctx: &mut HookContext) {
    let (kept, dropped) = state.rebuild_index();
    ctx.add_result(json!({
        "rebuilt": true,
        "kept_documents": kept,
        "dropped_documents": dropped,
    }));
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{DateTime, Utc};

    #[test]
    fn reindex_listener_rebuilds_the_index_and_reports_counts() {
        let state = Arc::new(SearchState::new());
        state
            .index(IndexDocumentRequest {
                id: "good-1".to_string(),
                kind: "task".to_string(),
                title: "Good".to_string(),
                snippet: None,
                url: Some("https://example.com/tasks/1".to_string()),
                updated_at: None,
            })
            .unwrap();
        // A document stored before URL validation existed.
        state.documents.insert(
            "task:stale-1".to_string(),
            SearchDocument {
                id: "stale-1".to_string(),
                kind: "task".to_string(),
                title: "Stale".to_string(),
                snippet: String::new(),
                url: Some("javascript:alert(1)".to_string()),
                updated_at: DateTime::from_timestamp(1_700_000_000, 0).unwrap(),
            },
        );
        let mut ctx = HookContext::new(
            "search.reindex.requested",
            json!({ "requested_at": Utc::now().to_rfc3339() }),
        );
        on_search_reindex_requested(state.clone(), &mut ctx);
        assert_eq!(ctx.results.len(), 1);
        assert_eq!(ctx.results[0].get("rebuilt"), Some(&json!(true)));
        assert_eq!(ctx.results[0].get("kept_documents"), Some(&json!(1)));
        assert_eq!(ctx.results[0].get("dropped_documents"), Some(&json!(1)));
        assert_eq!(state.all_documents().len(), 1);
        assert_eq!(state.all_documents()[0].id, "good-1");
    }
}
