// Pagination and filtering for core APIs and list pages.
//
// Every list endpoint accepts the same query parameters:
//
//   - `page` — 1-based page number, default 1.
//   - `per_page` — items per page, default 20, clamped to 1..=100.
//   - `sort` — field name to sort by; validated against an allow-list by
//     the caller.
//   - `order` — `asc` or `desc`, default `asc`.
//   - `search` — free-text search applied by the caller.
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

impl PaginationParams {
    pub fn normalized(&self) -> Pagination {
        let page = self.page.unwrap_or(DEFAULT_PAGE).max(1);
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
    pub fn offset(&self) -> i64 {
        let pagination = self.normalized();
        ((pagination.page - 1) * pagination.per_page) as i64
    }

    /// Limit for SQL `LIMIT` / `OFFSET` style queries.
    pub fn limit(&self) -> i64 {
        self.normalized().per_page as i64
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
