//! Small shared helpers.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::{HookBus, HookContext, HookHandler};
use unified_core::json;
use unified_core::modules::{Module, ModuleRegistry, RouteInfo};
use unified_core::pagination::{PageResult, PaginationParams};
use actix_web::{web, HttpRequest, HttpResponse};
use chrono::{DateTime, Datelike, NaiveDate, Utc};
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use uuid::Uuid;

use super::*;

// ---------------------------------------------------------------------------
// Helpers: caller, capabilities, validation, views
// ---------------------------------------------------------------------------

pub(crate) fn new_id() -> String {
    Uuid::new_v4().to_string()
}

pub(crate) fn now() -> DateTime<Utc> {
    Utc::now()
}

pub(crate) fn today() -> NaiveDate {
    Utc::now().date_naive()
}

/// Money rounded to four decimals. Only day counts and other
/// non-money quantities use this now; money is integer minor units.
pub(crate) fn round4(value: f64) -> f64 {
    (value * 10_000.0).round() / 10_000.0
}

/// True for zero-decimal currencies (no minor units), matching the
/// billing and store modules' list.
pub(crate) fn is_zero_decimal_currency(currency: &str) -> bool {
    unified_core::db::money::is_zero_decimal_currency(currency)
}

/// Convert integer minor units to major units, as JSON and hook
/// payloads carry them. This is the boundary conversion out of the
/// integer representation; business logic keeps minor units.
pub(crate) fn minor_to_major(amount_minor: i64, currency: &str) -> f64 {
    if is_zero_decimal_currency(currency) {
        amount_minor as f64
    } else {
        let major = amount_minor as f64 / 100.0;
        (major * 100.0).round() / 100.0
    }
}

/// Split the inclusive calendar days of `[start, end]` across the
/// calendar years they touch, as `(year, days)` in ascending year
/// order. A request that stays inside one year yields one entry.
pub(crate) fn leave_days_by_year(start: NaiveDate, end: NaiveDate) -> Vec<(i32, f64)> {
    if end < start {
        return Vec::new();
    }
    let mut out: Vec<(i32, f64)> = Vec::new();
    let mut cursor = start;
    while cursor <= end {
        let year_end = NaiveDate::from_ymd_opt(cursor.year(), 12, 31)
            .unwrap_or(end);
        let chunk_end = end.min(year_end);
        let days = (chunk_end - cursor).num_days() as f64 + 1.0;
        out.push((cursor.year(), days));
        cursor = chunk_end + chrono::Duration::days(1);
    }
    out
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

pub(crate) fn matches_search(haystack: &str, params: &PaginationParams) -> bool {
    match &params.search {
        Some(needle) if !needle.trim().is_empty() => haystack
            .to_lowercase()
            .contains(&needle.trim().to_lowercase()),
        _ => true,
    }
}

/// A contract with `salary_amount` removed for callers who may not
/// see money. HR-capable callers and the employee themself get the
/// full record.
pub(crate) fn contract_view(
    ctx: &AppContext,
    caller: &Caller,
    contract: &Contract,
    employee_user_id: &str,
) -> Value {
    if is_hr_capable(ctx, &caller.role) || employee_user_id == caller.actor {
        serde_json::to_value(contract).unwrap_or(Value::Null)
    } else {
        json!({
            "id": contract.id,
            "employee_id": contract.employee_id,
            "contract_type": contract.contract_type,
            "start_date": contract.start_date,
            "end_date": contract.end_date,
            "currency": contract.currency,
            "status": contract.status,
            "created_at": contract.created_at,
            "updated_at": contract.updated_at,
        })
    }
}

/// A position with the salary band removed for callers who may not
/// see money.
pub(crate) fn position_view(ctx: &AppContext, caller: &Caller, position: &Position) -> Value {
    if is_hr_capable(ctx, &caller.role) {
        serde_json::to_value(position).unwrap_or(Value::Null)
    } else {
        json!({
            "id": position.id,
            "title": position.title,
            "department_id": position.department_id,
            "level": position.level,
            "status": position.status,
            "created_at": position.created_at,
            "updated_at": position.updated_at,
        })
    }
}

/// A payroll item with money removed for callers who may not see it.
/// HR-capable callers and the employee themself get the full record.
pub(crate) fn payroll_item_view(
    ctx: &AppContext,
    caller: &Caller,
    item: &PayrollItem,
    employee_user_id: &str,
) -> Value {
    if is_hr_capable(ctx, &caller.role) || employee_user_id == caller.actor {
        serde_json::to_value(item).unwrap_or(Value::Null)
    } else {
        json!({
            "id": item.id,
            "payroll_run_id": item.payroll_run_id,
            "employee_id": item.employee_id,
            "notes": item.notes,
            "created_at": item.created_at,
            "updated_at": item.updated_at,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn minor_to_major_converts_cents_and_zero_decimal_currencies() {
        assert_eq!(minor_to_major(1025, "USD"), 10.25);
        assert_eq!(minor_to_major(1025, "JPY"), 1025.0);
        assert_eq!(minor_to_major(0, "USD"), 0.0);
    }

    #[test]
    fn round4_rounds_to_four_decimals() {
        assert_eq!(round4(1.23456), 1.2346);
        assert_eq!(round4(1.23454), 1.2345);
        assert_eq!(round4(2.5), 2.5);
        assert_eq!(round4(0.0), 0.0);
        assert_eq!(round4(-1.23456), -1.2346);
    }

    #[test]
    fn leave_days_by_year_single_year() {
        let start = NaiveDate::from_ymd_opt(2026, 3, 10).unwrap();
        let end = NaiveDate::from_ymd_opt(2026, 3, 12).unwrap();
        assert_eq!(leave_days_by_year(start, end), vec![(2026, 3.0)]);
    }

    #[test]
    fn leave_days_by_year_splits_across_new_year() {
        // 30 Dec 2026 - 3 Jan 2027: 2 days in 2026, 3 days in 2027.
        let start = NaiveDate::from_ymd_opt(2026, 12, 30).unwrap();
        let end = NaiveDate::from_ymd_opt(2027, 1, 3).unwrap();
        assert_eq!(
            leave_days_by_year(start, end),
            vec![(2026, 2.0), (2027, 3.0)]
        );
    }

    #[test]
    fn leave_days_by_year_rejects_reversed_range() {
        let start = NaiveDate::from_ymd_opt(2026, 3, 12).unwrap();
        let end = NaiveDate::from_ymd_opt(2026, 3, 10).unwrap();
        assert!(leave_days_by_year(start, end).is_empty());
    }
}
