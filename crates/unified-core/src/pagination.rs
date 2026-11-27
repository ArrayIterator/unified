// Pagination and filtering for core APIs and list pages.
//
// Every list endpoint accepts the same query parameters:
//
//   - `page` - 1-based page number, default 1.
//   - `per_page` - items per page, default 20, clamped to 1..=100.
//   - `sort` - field name to sort by; validated against an allow-list by
//     the caller.
//   - `order` - `asc` or `desc`, default `asc`.
//   - `search` - free-text search applied by the caller.
//   - any other query parameter is treated as an exact-match filter
//     (`field=value`).
//
// The response data is an object with `items` plus a `pagination` object
// holding `page`, `per_page`, `total`, and `total_pages`, so the top-level
// envelope stays exactly `{"data": ...}`.

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;

pub const DEFAULT_PAGE: u32 = 1;
pub const DEFAULT_PER_PAGE: u32 = 20;
pub const MAX_PER_PAGE: u32 = 100;

/// Query parameters shared by every list endpoint.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct PaginationParams {
    pub page: Option<u32>,
    pub per_page: Option<u32>,
    pub sort: Option<String>,
    pub order: Option<String>,
    pub search: Option<String>,
    /// Any extra query parameter is an exact-match filter.
    #[serde(flatten)]
    pub filters: HashMap<String, String>,
}

/// A normalized, validated view of `PaginationParams`.
#[derive(Debug, Clone, Serialize)]
pub struct Pagination {
    pub page: u32,
    pub per_page: u32,
    pub sort: Option<String>,
    pub order: String,
    pub search: Option<String>,
    pub filters: HashMap<String, String>,
}

/// Largest page number a client may request. The offset for page
/// `p` is `(p - 1) * per_page`, so an unbounded `page: u32` overflows
/// that multiplication (`?page=4294967295` panics in debug builds
/// and wraps in release). Clamping the page keeps every offset
/// inside `i64` - `MAX_PAGE * MAX_PER_PAGE` is far below
/// `i64::MAX` - and a page past the end of the data simply returns
/// an empty page, which is the correct answer for it.
pub const MAX_PAGE: u32 = 1_000_000;

impl PaginationParams {
    pub fn normalized(&self) -> Pagination {
        let page = self.page().min(MAX_PAGE);
        let per_page = self
            .per_page
            .unwrap_or(DEFAULT_PER_PAGE)
            .clamp(1, MAX_PER_PAGE);
        let order = match self.order.as_deref() {
            Some("desc") => "desc".to_string(),
            _ => "asc".to_string(),
        };
        Pagination {
            page,
            per_page,
            sort: self.sort.clone(),
            order,
            search: self.search.clone(),
            filters: self.filters.clone(),
        }
    }

    /// Offset for SQL `LIMIT` / `OFFSET` style queries.
    ///
    /// Computed straight from `page` / `per_page` instead of going
    /// through `normalized()`, which clones `sort`, `search` and the
    /// whole `filters` map that an offset never needs. The page is
    /// clamped to `MAX_PAGE` and the multiplication saturates, so
    /// no client-supplied page can overflow the offset.
    pub fn offset(&self) -> i64 {
        (self.page().min(MAX_PAGE).saturating_sub(1) as i64).saturating_mul(self.per_page() as i64)
    }

    /// Limit for SQL `LIMIT` / `OFFSET` style queries. See `offset`
    /// for why this does not go through `normalized()`.
    pub fn limit(&self) -> i64 {
        self.per_page() as i64
    }

    /// Validated 1-based page number, clamped the same way
    /// `normalized()` clamps it.
    fn page(&self) -> u32 {
        self.page.unwrap_or(DEFAULT_PAGE).max(1)
    }

    /// Validated items-per-page, clamped to `1..=MAX_PER_PAGE` the
    /// same way `normalized()` clamps it.
    fn per_page(&self) -> u32 {
        self.per_page.unwrap_or(DEFAULT_PER_PAGE).clamp(1, MAX_PER_PAGE)
    }

    /// Validate the requested sort field against an allow-list.
    /// Returns the field to sort by, or `None` when it is not allowed.
    pub fn allowed_sort(&self, allowed: &[&str], default: &str) -> String {
        match &self.sort {
            Some(field) if allowed.contains(&field.as_str()) => field.clone(),
            _ => default.to_string(),
        }
    }
}

/// One page of results, ready to be serialized into `data`.
#[derive(Debug, Clone, Serialize)]
pub struct PageResult<T: Serialize> {
    pub items: Vec<T>,
    pub pagination: PageInfo,
}

#[derive(Debug, Clone, Serialize)]
pub struct PageInfo {
    pub page: u32,
    pub per_page: u32,
    pub total: u64,
    pub total_pages: u64,
}

impl<T: Serialize> PageResult<T> {
    pub fn new(items: Vec<T>, total: u64, pagination: &Pagination) -> Self {
        let total_pages = if total == 0 {
            0
        } else {
            total.div_ceil(pagination.per_page as u64)
        };
        Self {
            items,
            pagination: PageInfo {
                page: pagination.page,
                per_page: pagination.per_page,
                total,
                total_pages,
            },
        }
    }

    /// Serialize the page as the `data` value of the standard envelope.
    pub fn to_json(&self) -> Value {
        json!({
            "items": self.items,
            "pagination": self.pagination,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn params(page: Option<u32>, per_page: Option<u32>) -> PaginationParams {
        PaginationParams {
            page,
            per_page,
            sort: None,
            order: None,
            search: None,
            filters: HashMap::new(),
        }
    }

    #[test]
    fn offset_and_limit_use_defaults() {
        let params = params(None, None);
        assert_eq!(params.offset(), 0);
        assert_eq!(params.limit(), DEFAULT_PER_PAGE as i64);
    }

    #[test]
    fn offset_and_limit_match_normalized() {
        // offset()/limit() must agree exactly with the values
        // normalized() reports, including clamping, even though they
        // no longer go through normalized() (which clones the filters).
        for (page, per_page) in [
            (Some(1), Some(10)),
            (Some(3), Some(25)),
            (Some(0), Some(10)),
            (Some(2), Some(0)),
            (Some(2), Some(500)),
            (None, Some(7)),
        ] {
            let params = params(page, per_page);
            let normalized = params.normalized();
            assert_eq!(params.offset(), ((normalized.page - 1) * normalized.per_page) as i64);
            assert_eq!(params.limit(), normalized.per_page as i64);
        }
    }

    #[test]
    fn normalized_clamps_page_and_per_page() {
        let normalized = params(Some(0), Some(500)).normalized();
        assert_eq!(normalized.page, 1);
        assert_eq!(normalized.per_page, MAX_PER_PAGE);
        let normalized = params(Some(5), Some(0)).normalized();
        assert_eq!(normalized.page, 5);
        assert_eq!(normalized.per_page, 1);
    }

    #[test]
    fn huge_page_is_clamped_and_offset_never_overflows() {
        // ?page=4294967295 used to overflow (page - 1) * per_page
        // in u32 (panic in debug, wrap in release).
        let params = params(Some(u32::MAX), Some(100));
        let normalized = params.normalized();
        assert_eq!(normalized.page, MAX_PAGE);
        assert_eq!(
            params.offset(),
            (i64::from(MAX_PAGE) - 1) * i64::from(MAX_PER_PAGE)
        );
    }

    #[test]
    fn page_result_total_pages_rounds_up() {
        let pagination = params(Some(1), Some(20)).normalized();
        assert_eq!(PageResult::new(Vec::<u32>::new(), 0, &pagination).pagination.total_pages, 0);
        assert_eq!(PageResult::new(Vec::<u32>::new(), 20, &pagination).pagination.total_pages, 1);
        assert_eq!(PageResult::new(Vec::<u32>::new(), 21, &pagination).pagination.total_pages, 2);
    }
}
