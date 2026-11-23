// Tax module — filing periods and stored-computation totals.
// Split from the former single-file module; items keep their original names and behaviour. Private items are `pub(crate)` so sibling files in this module can use them.

use super::*;
use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::{HookBus, HookContext, HookHandler};
use unified_core::json;
use unified_core::modules::{Module, ModuleRegistry, RouteInfo, RouteVersionSupport};
use unified_core::pagination::{PageResult, PaginationParams};
use actix_web::{HttpRequest, HttpResponse, web};
use base64::Engine as _;
use chrono::{DateTime, Datelike, NaiveDate, Utc};
use dashmap::DashMap;
use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::HashSet;
use std::net::{IpAddr, ToSocketAddrs};
use std::sync::{Arc, RwLock};
use std::time::Duration;
use url::Url;
use uuid::Uuid;

// ---------------------------------------------------------------------------
// Filing periods
// ---------------------------------------------------------------------------

pub(crate) fn last_day_of_month(year: i32, month: u32) -> NaiveDate {
    let (next_year, next_month) = if month == 12 {
        (year + 1, 1)
    } else {
        (year, month + 1)
    };
    NaiveDate::from_ymd_opt(next_year, next_month, 1)
        .map(|first| first.pred_opt().unwrap_or(first))
        .unwrap_or_else(|| NaiveDate::from_ymd_opt(year, month, 28).unwrap_or(today()))
}

/// The period containing `on` for a filing frequency, using the
/// company's fiscal year start for yearly filings.
pub(crate) fn period_for(
    on: NaiveDate,
    frequency: &str,
    settings: &TaxSettings,
) -> (NaiveDate, NaiveDate) {
    match frequency {
        "monthly" => {
            let start = NaiveDate::from_ymd_opt(on.year(), on.month(), 1).unwrap_or(on);
            (start, last_day_of_month(on.year(), on.month()))
        }
        "yearly" => {
            let start_month = settings.fiscal_year_start_month;
            let start_day = settings.fiscal_year_start_day.min(28);
            let (start_year, end_year) = if (on.month(), on.day()) < (start_month, start_day) {
                (on.year() - 1, on.year())
            } else {
                (on.year(), on.year() + 1)
            };
            let start = NaiveDate::from_ymd_opt(start_year, start_month, start_day).unwrap_or(on);
            let end = NaiveDate::from_ymd_opt(end_year, start_month, start_day)
                .and_then(|date| date.pred_opt())
                .unwrap_or(on);
            (start, end)
        }
        _ => {
            // Quarterly: calendar quarters.
            let quarter_start_month = ((on.month() - 1) / 3) * 3 + 1;
            let start = NaiveDate::from_ymd_opt(on.year(), quarter_start_month, 1).unwrap_or(on);
            let end_month = quarter_start_month + 2;
            (start, last_day_of_month(on.year(), end_month))
        }
    }
}

/// Shift a period one step back for its frequency.
pub(crate) fn previous_period(
    start: NaiveDate,
    _end: NaiveDate,
    frequency: &str,
    settings: &TaxSettings,
) -> (NaiveDate, NaiveDate) {
    let before = start.pred_opt().unwrap_or(start);
    period_for(before, frequency, settings)
}

/// Tax total of stored computations for one company, jurisdiction,
/// and period (inclusive of both ends), in integer minor units.
/// The sum is exact: minor units add without rounding.
pub(crate) fn period_tax_total(
    state: &TaxState,
    company_id: Uuid,
    country_code: &str,
    region_code: Option<&str>,
    start: NaiveDate,
    end: NaiveDate,
) -> i64 {
    let mut total: i64 = 0;
    for entry in state.computations.iter() {
        let computation = entry.value();
        if computation.company_id != company_id {
            continue;
        }
        if computation.country_code.as_deref() != Some(country_code) {
            continue;
        }
        if region_code.is_some() && computation.region_code.as_deref() != region_code {
            continue;
        }
        let day = computation.computed_at.date_naive();
        if day >= start && day <= end {
            total += computation.tax_total;
        }
    }
    total
}

// ---------------------------------------------------------------------------
// Shared compute + store
// ---------------------------------------------------------------------------

/// Compute tax for a source document and store the computation.
/// Re-computing the same (`source_type`, `source_id`) replaces the
/// stored row instead of duplicating it, so a billing event that
/// arrives twice (real name and alias spelling) stores exactly one
/// computation. The source document itself is never mutated.
/// `amount` is integer minor units; the emitted `tax.computed`
/// payload carries major units, as before.
pub(crate) fn compute_and_store(
    state: &TaxState,
    company_id: Uuid,
    source_type: &str,
    source_id: &str,
    amount: i64,
    currency: &str,
    country_code: Option<&str>,
    region_code: Option<&str>,
    on: NaiveDate,
) -> TaxComputation {
    let rules = state.applicable_rules(company_id, country_code, region_code, amount, on);
    let mut breakdown = compute_tax_breakdown(amount, &rules, on);
    breakdown.currency = currency.to_string();
    let existing_id = state
        .computations
        .iter()
        .find(|entry| {
            entry.value().source_type == source_type && entry.value().source_id == source_id
        })
        .map(|entry| entry.key().clone());
    let id = existing_id.unwrap_or_else(new_id);
    let computation = TaxComputation {
        id: id.clone(),
        company_id,
        source_type: source_type.to_string(),
        source_id: source_id.to_string(),
        country_code: country_code.map(str::to_string),
        region_code: region_code.map(str::to_string),
        currency: currency.to_string(),
        base_amount: breakdown.base,
        tax_total: breakdown.tax_total,
        grand_total: breakdown.grand_total,
        lines: breakdown.lines.clone(),
        computed_at: now(),
        created_at: now(),
        updated_at: now(),
    };
    state.computations.insert(id, computation.clone());
    state.emit(
        "tax.computed",
        json!({
            "computation_id": computation.id,
            "company_id": company_id.to_string(),
            "source_type": source_type,
            "source_id": source_id,
            "country_code": country_code,
            "region_code": region_code,
            "currency": currency,
            "base_amount": minor_to_major(computation.base_amount, currency),
            "tax_total": minor_to_major(computation.tax_total, currency),
            "grand_total": minor_to_major(computation.grand_total, currency),
            "lines": computation.lines,
        }),
    );
    computation
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::modules::tax::TaxSettings;

    fn settings(fiscal_month: u32, fiscal_day: u32) -> TaxSettings {
        TaxSettings {
            fiscal_year_start_month: fiscal_month,
            fiscal_year_start_day: fiscal_day,
            ..Default::default()
        }
    }

    fn date(year: i32, month: u32, day: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(year, month, day).unwrap()
    }

    #[test]
    fn last_day_of_month_handles_leap_years() {
        assert_eq!(last_day_of_month(2025, 2), date(2025, 2, 28));
        assert_eq!(last_day_of_month(2024, 2), date(2024, 2, 29));
        assert_eq!(last_day_of_month(2025, 12), date(2025, 12, 31));
        assert_eq!(last_day_of_month(2025, 4), date(2025, 4, 30));
    }

    #[test]
    fn period_for_monthly_covers_the_calendar_month() {
        assert_eq!(
            period_for(date(2025, 6, 15), "monthly", &settings(1, 1)),
            (date(2025, 6, 1), date(2025, 6, 30))
        );
    }

    #[test]
    fn period_for_quarterly_covers_calendar_quarters() {
        assert_eq!(
            period_for(date(2025, 5, 10), "quarterly", &settings(1, 1)),
            (date(2025, 4, 1), date(2025, 6, 30))
        );
        assert_eq!(
            period_for(date(2025, 11, 2), "quarterly", &settings(1, 1)),
            (date(2025, 10, 1), date(2025, 12, 31))
        );
    }

    #[test]
    fn period_for_yearly_follows_the_fiscal_year() {
        assert_eq!(
            period_for(date(2025, 6, 15), "yearly", &settings(1, 1)),
            (date(2025, 1, 1), date(2025, 12, 31))
        );
        assert_eq!(
            period_for(date(2025, 3, 15), "yearly", &settings(4, 1)),
            (date(2024, 4, 1), date(2025, 3, 31))
        );
        assert_eq!(
            period_for(date(2025, 4, 1), "yearly", &settings(4, 1)),
            (date(2025, 4, 1), date(2026, 3, 31))
        );
    }

    #[test]
    fn previous_period_steps_back_one_period() {
        let (start, end) = previous_period(
            date(2025, 6, 1),
            date(2025, 6, 30),
            "monthly",
            &settings(1, 1),
        );
        assert_eq!((start, end), (date(2025, 5, 1), date(2025, 5, 31)));
    }
}
