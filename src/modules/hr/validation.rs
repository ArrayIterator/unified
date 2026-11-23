//! Input validation and URL safety checks.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::{HookBus, HookContext, HookHandler};
use unified_core::json;
use unified_core::modules::{Module, ModuleRegistry, RouteInfo};
use unified_core::pagination::{PageResult, PaginationParams};
use actix_web::{web, HttpRequest, HttpResponse};
use chrono::{DateTime, NaiveDate, Utc};
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use uuid::Uuid;

use super::*;
use super::types::{MAX_MONEY_MINOR};

pub(crate) fn require_text(value: &str, field: &str, max: usize) -> UnifiedResult<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err(UnifiedError::bad_request(format!("{field} must not be empty")));
    }
    if trimmed.len() > max {
        return Err(UnifiedError::bad_request(format!(
            "{field} must be at most {max} characters"
        )));
    }
    Ok(trimmed.to_string())
}

pub(crate) fn optional_text(value: &Option<String>, field: &str, max: usize) -> UnifiedResult<Option<String>> {
    match value {
        None => Ok(None),
        Some(text) => Ok(Some(require_text(text, field, max)?)),
    }
}

pub(crate) fn parse_date(raw: &str, field: &str) -> UnifiedResult<NaiveDate> {
    NaiveDate::parse_from_str(raw.trim(), "%Y-%m-%d")
        .map_err(|_| UnifiedError::bad_request(format!("{field} must be a date (YYYY-MM-DD)")))
}

/// Money: an amount already in minor units (request JSON converted
/// major units through `serde_major` before this runs), non-negative
/// and below `MAX_MONEY_MINOR`.
pub(crate) fn parse_money(value: i64, field: &str) -> UnifiedResult<i64> {
    if value < 0 || value >= MAX_MONEY_MINOR {
        return Err(UnifiedError::bad_request(format!(
            "{field} must be an amount from 0 up to 999999999999999.99"
        )));
    }
    Ok(value)
}

pub(crate) fn parse_days(value: f64, field: &str) -> UnifiedResult<f64> {
    if !value.is_finite() || value < 0.0 || value > 366.0 {
        return Err(UnifiedError::bad_request(format!(
            "{field} must be between 0 and 366 days"
        )));
    }
    Ok(round4(value))
}

pub(crate) fn valid_currency(raw: &str) -> UnifiedResult<String> {
    let currency = raw.trim().to_uppercase();
    if currency.len() != 3 || !currency.chars().all(|ch| ch.is_ascii_uppercase()) {
        return Err(UnifiedError::bad_request(
            "currency must be a 3-letter ISO code (for example USD)",
        ));
    }
    Ok(currency)
}

/// `YYYY-MM` period validation for payroll runs.
pub(crate) fn valid_period(raw: &str) -> UnifiedResult<String> {
    let period = raw.trim();
    let (year, month) = period
        .split_once('-')
        .ok_or_else(|| UnifiedError::bad_request("period must be YYYY-MM"))?;
    let year: i32 = year
        .parse()
        .map_err(|_| UnifiedError::bad_request("period must be YYYY-MM"))?;
    let month: u32 = month
        .parse()
        .map_err(|_| UnifiedError::bad_request("period must be YYYY-MM"))?;
    if !(2000..=2100).contains(&year) || !(1..=12).contains(&month) {
        return Err(UnifiedError::bad_request(
            "period must be YYYY-MM with a month from 01 to 12",
        ));
    }
    Ok(format!("{year:04}-{month:02}"))
}

/// `Annual Leave` -> `annual-leave`.
pub(crate) fn slugify(name: &str) -> String {
    let mut slug = String::new();
    let mut last_dash = true;
    for ch in name.to_lowercase().chars() {
        if ch.is_ascii_alphanumeric() {
            slug.push(ch);
            last_dash = false;
        } else if !last_dash {
            slug.push('-');
            last_dash = true;
        }
    }
    while slug.ends_with('-') {
        slug.pop();
    }
    slug
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn require_text_trims_and_accepts() {
        assert_eq!(require_text("  Jane Doe  ", "name", 100).unwrap(), "Jane Doe");
    }

    #[test]
    fn require_text_rejects_empty_and_too_long() {
        assert!(require_text("   ", "name", 100).is_err());
        assert!(require_text("", "name", 100).is_err());
        assert!(require_text("abc", "name", 2).is_err());
    }

    #[test]
    fn optional_text_none_some_and_blank() {
        assert_eq!(optional_text(&None, "note", 100).unwrap(), None);
        assert_eq!(
            optional_text(&Some("  hello  ".to_string()), "note", 100).unwrap(),
            Some("hello".to_string())
        );
        assert!(optional_text(&Some("   ".to_string()), "note", 100).is_err());
    }

    #[test]
    fn parse_date_accepts_iso_and_rejects_other() {
        assert_eq!(
            parse_date("2026-01-15", "start").unwrap(),
            NaiveDate::from_ymd_opt(2026, 1, 15).unwrap()
        );
        assert!(parse_date("15-01-2026", "start").is_err());
        assert!(parse_date("2026-02-30", "start").is_err());
    }

    #[test]
    fn parse_money_accepts_minor_units_and_rejects_bad_values() {
        assert_eq!(parse_money(1012, "salary").unwrap(), 1012);
        assert_eq!(parse_money(0, "salary").unwrap(), 0);
    }

    #[test]
    fn parse_money_rejects_negative_and_huge() {
        assert!(parse_money(-1, "salary").is_err());
        assert!(parse_money(MAX_MONEY_MINOR, "salary").is_err());
    }

    #[test]
    fn parse_days_accepts_range() {
        assert_eq!(parse_days(0.0, "days").unwrap(), 0.0);
        assert_eq!(parse_days(366.0, "days").unwrap(), 366.0);
        assert_eq!(parse_days(1.5, "days").unwrap(), 1.5);
        assert!(parse_days(366.0001, "days").is_err());
        assert!(parse_days(-0.5, "days").is_err());
    }

    #[test]
    fn valid_currency_uppercases_and_validates() {
        assert_eq!(valid_currency("usd").unwrap(), "USD");
        assert_eq!(valid_currency(" idr ").unwrap(), "IDR");
        assert!(valid_currency("US").is_err());
        assert!(valid_currency("USDD").is_err());
        assert!(valid_currency("U1D").is_err());
    }

    #[test]
    fn valid_period_accepts_and_normalizes() {
        assert_eq!(valid_period("2026-01").unwrap(), "2026-01");
        assert_eq!(valid_period("2026-1").unwrap(), "2026-01");
        assert_eq!(valid_period(" 2026-12 ").unwrap(), "2026-12");
    }

    #[test]
    fn valid_period_rejects_bad_values() {
        assert!(valid_period("2026-13").is_err());
        assert!(valid_period("2026").is_err());
        assert!(valid_period("1999-12").is_err());
        assert!(valid_period("abcd-01").is_err());
        assert!(valid_period("2026-00").is_err());
    }

    #[test]
    fn slugify_cases() {
        assert_eq!(slugify("Annual Leave"), "annual-leave");
        assert_eq!(slugify("HR & Payroll!"), "hr-payroll");
        assert_eq!(slugify("  spaced  out "), "spaced-out");
        assert_eq!(slugify(""), "");
    }
}
