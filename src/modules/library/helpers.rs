// Library - HTTP response and pagination helpers.

use unified_core::error::UnifiedError;
use unified_core::json;
use unified_core::pagination::{PageResult, PaginationParams};
use actix_web::HttpResponse;
use serde::Serialize;
use serde_json::{json, Value};

pub(crate) fn ok<T: Serialize>(data: T) -> HttpResponse {
    json::success_response(data, 200)
}

pub(crate) fn created<T: Serialize>(data: T) -> HttpResponse {
    json::success_response(data, 201)
}

pub(crate) fn err(error: UnifiedError, debug: bool) -> HttpResponse {
    json::error_response(&error, debug)
}

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

pub(crate) fn opt_string(data: &Value, key: &str) -> Option<String> {
    data.get(key)
        .and_then(|value| value.as_str())
        .map(str::to_string)
}


#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn params(page: u32, per_page: u32) -> PaginationParams {
        PaginationParams {
            page: Some(page),
            per_page: Some(per_page),
            ..Default::default()
        }
    }

    #[test]
    fn paginate_first_page() {
        let page = paginate(vec![1, 2, 3, 4, 5], &params(1, 2));
        assert_eq!(page.items, vec![1, 2]);
        assert_eq!(page.pagination.total, 5);
        assert_eq!(page.pagination.total_pages, 3);
    }

    #[test]
    fn paginate_second_page() {
        let page = paginate(vec![1, 2, 3, 4, 5], &params(2, 2));
        assert_eq!(page.items, vec![3, 4]);
    }

    #[test]
    fn paginate_beyond_end_is_empty_but_keeps_total() {
        let page = paginate(vec![1, 2, 3, 4, 5], &params(4, 2));
        assert!(page.items.is_empty());
        assert_eq!(page.pagination.total, 5);
    }

    #[test]
    fn paginate_empty_has_zero_pages() {
        let page: PageResult<i32> = paginate(vec![], &params(1, 20));
        assert!(page.items.is_empty());
        assert_eq!(page.pagination.total, 0);
        assert_eq!(page.pagination.total_pages, 0);
    }

    #[test]
    fn opt_string_reads_string_field() {
        let data = json!({"a": "x"});
        assert_eq!(opt_string(&data, "a"), Some("x".to_string()));
    }

    #[test]
    fn opt_string_missing_is_none() {
        let data = json!({"a": "x"});
        assert_eq!(opt_string(&data, "b"), None);
    }

    #[test]
    fn opt_string_ignores_non_string() {
        let data = json!({"a": 1});
        assert_eq!(opt_string(&data, "a"), None);
    }
}
