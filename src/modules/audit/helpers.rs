// Audit module - shared helpers.
//
// Split from `src/modules/audit.rs`; the module root re-exports the public API of this module.

use unified_core::pagination::{PageResult, PaginationParams};
use serde::Serialize;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

pub(crate) fn paginate<T: Serialize + Clone>(items: Vec<T>, params: &PaginationParams) -> PageResult<T> {
    let pagination = params.normalized();
    let total = items.len() as u64;
    let start = ((pagination.page - 1) * pagination.per_page) as usize;
    let page_items: Vec<T> = items
        .into_iter()
        .skip(start)
        .take(pagination.per_page as usize)
        .collect();
    PageResult::new(page_items, total, &pagination)
}

pub(crate) fn matches_search(haystack: &str, params: &PaginationParams) -> bool {
    match &params.search {
        Some(needle) if !needle.trim().is_empty() => haystack
            .to_lowercase()
            .contains(&needle.trim().to_lowercase()),
        _ => true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paginate_returns_the_requested_page() {
        let params = PaginationParams {
            page: Some(2),
            per_page: Some(2),
            ..Default::default()
        };
        let page = paginate(vec![1, 2, 3, 4, 5], &params);
        assert_eq!(page.items, vec![3, 4]);
        assert_eq!(page.pagination.total, 5);
        assert_eq!(page.pagination.total_pages, 3);
    }

    #[test]
    fn matches_search_is_case_insensitive() {
        let params = PaginationParams {
            search: Some("world".to_string()),
            ..Default::default()
        };
        assert!(matches_search("Hello World", &params));
        let params = PaginationParams {
            search: Some("missing".to_string()),
            ..Default::default()
        };
        assert!(!matches_search("Hello World", &params));
        assert!(matches_search("Hello World", &PaginationParams::default()));
    }
}
