// security -- Pagination and search helpers.

use unified_core::pagination::{PageResult, PaginationParams};
use serde::Serialize;
use serde_json::Value;

// ---------------------------------------------------------------------------
// Hardening HTTP surface
//
// Every handler below reads the shared `HardeningState` (the same
// instance the middleware enforces), so a settings change or an
// unlock takes effect immediately. All list endpoints use the shared
// pagination parameters (`page`, `per_page`, `sort`, `order`,
// `search`, plus exact-match filters) and return
// `PageResult::to_json()` inside the standard success envelope.
// ---------------------------------------------------------------------------

/// Paginate an already-collected, already-sorted item list.
pub(crate) fn paginate_items<T: Serialize + Clone>(
    items: Vec<T>,
    params: &PaginationParams,
) -> serde_json::Value {
    let pagination = params.normalized();
    let total = items.len() as u64;
    let start = ((pagination.page - 1) * pagination.per_page) as usize;
    let end = (start + pagination.per_page as usize).min(items.len());
    let page_items = if start >= items.len() {
        Vec::new()
    } else {
        items[start..end].to_vec()
    };
    PageResult::new(page_items, total, &pagination).to_json()
}

pub(crate) fn matches_search(haystack: &str, search: &Option<String>) -> bool {
    match search {
        Some(needle) => haystack.to_lowercase().contains(&needle.to_lowercase()),
        None => true,
    }
}
