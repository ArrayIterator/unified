// Saved Views - Visibility and lookup helpers.

use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::pagination::{PageResult, PaginationParams};
use serde::Serialize;

use super::{SavedView, SavedViewsState, VISIBILITY_SHARED};

// Helpers.

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

/// A view is visible to `requester_id` when it is theirs, or when it
/// is shared. Shared views are readable by any user but only the
/// owner may change or delete them.
pub(crate) fn is_visible_to(view: &SavedView, requester_id: Option<&str>) -> bool {
    match requester_id {
        Some(user_id) => view.user_id == user_id || view.visibility == VISIBILITY_SHARED,
        None => view.visibility == VISIBILITY_SHARED,
    }
}

pub(crate) fn get_view(state: &SavedViewsState, id: &str) -> UnifiedResult<SavedView> {
    state
        .views
        .get(id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found("Saved view not found"))
}


#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use serde_json::json;

    fn view(id: &str, owner: &str, visibility: &str) -> SavedView {
        let now = Utc::now();
        SavedView {
            id: id.to_string(),
            user_id: owner.to_string(),
            module_id: "tickets".to_string(),
            name: id.to_string(),
            filter: json!({"status": "open"}),
            sort: None,
            order: Some("asc".to_string()),
            visibility: visibility.to_string(),
            is_default: false,
            created_at: now,
            updated_at: now,
        }
    }

    fn search_params(needle: &str) -> PaginationParams {
        PaginationParams {
            search: Some(needle.to_string()),
            ..Default::default()
        }
    }

    #[test]
    fn is_visible_to_allows_owner() {
        let v = view("v1", "user-1", "private");
        assert!(is_visible_to(&v, Some("user-1")));
    }

    #[test]
    fn is_visible_to_hides_private_from_others() {
        let v = view("v1", "user-1", "private");
        assert!(!is_visible_to(&v, Some("user-2")));
    }

    #[test]
    fn is_visible_to_allows_shared_for_anyone() {
        let v = view("v1", "user-1", "shared");
        assert!(is_visible_to(&v, Some("user-2")));
    }

    #[test]
    fn is_visible_to_hides_private_from_anonymous() {
        let v = view("v1", "user-1", "private");
        assert!(!is_visible_to(&v, None));
    }

    #[test]
    fn is_visible_to_allows_shared_for_anonymous() {
        let v = view("v1", "user-1", "shared");
        assert!(is_visible_to(&v, None));
    }

    #[test]
    fn paginate_slices_items() {
        let params = PaginationParams {
            page: Some(2),
            per_page: Some(2),
            ..Default::default()
        };
        let page = paginate(vec![1, 2, 3, 4], &params);
        assert_eq!(page.items, vec![3, 4]);
        assert_eq!(page.pagination.total, 4);
    }

    #[test]
    fn matches_search_is_case_insensitive() {
        assert!(matches_search("Open Tickets", &search_params("tickets")));
        assert!(!matches_search("Open Tickets", &search_params("billing")));
        assert!(matches_search(
            "Open Tickets",
            &PaginationParams::default()
        ));
    }
}
