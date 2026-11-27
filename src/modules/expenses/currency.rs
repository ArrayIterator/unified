// Currency conversion (stored rates only).

use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::HookBus;
use chrono::Utc;
use serde_json::json;
use uuid::Uuid;

use super::*;

// Currency conversion (stored rates only)

/// Find a stored rate for a pair in this module's own rates: the
/// direct pair first, then the inverse pair derived as `1 / rate`.
/// Returns `None` when this module stores nothing for the pair.
/// When several rows exist for the same pair (rows stored before
/// the create handler became an upsert), the NEWEST row by
/// `created_at` wins - never the last row the map happens to
/// iterate, which is nondeterministic.
pub(crate) fn stored_rate(state: &ExpensesState, from: &str, to: &str) -> Option<f64> {
    let mut direct: Option<(f64, chrono::DateTime<chrono::Utc>)> = None;
    let mut inverse: Option<(f64, chrono::DateTime<chrono::Utc>)> = None;
    for entry in state.currency_rates.iter() {
        let row = entry.value();
        if row.base_currency == from && row.target_currency == to {
            if direct.map_or(true, |(_, at)| row.created_at >= at) {
                direct = Some((row.rate, row.created_at));
            }
        }
        if row.base_currency == to && row.target_currency == from && row.rate != 0.0 {
            if inverse.map_or(true, |(_, at)| row.created_at >= at) {
                inverse = Some((1.0 / row.rate, row.created_at));
            }
        }
    }
    direct.map(|(rate, _)| rate).or_else(|| inverse.map(|(rate, _)| rate))
}

/// Ask the calculator module for a stored rate through its
/// `calculator.rate_requested` event. The calculator answers with
/// a result row when it holds a stored rate; no answer means no
/// stored rate, and the caller must treat the rate as unavailable.
pub(crate) fn calculator_rate(bus: &HookBus, from: &str, to: &str) -> Option<f64> {
    let context = bus.emit(
        CALCULATOR_RATE_REQUESTED,
        json!({ "base_currency": from, "target_currency": to }),
    );
    context
        .results
        .iter()
        .find_map(|result| result.get("rate").and_then(|value| value.as_f64()))
        .filter(|rate| rate.is_finite() && *rate > 0.0)
}

/// Resolve a conversion rate from stored data only: this module's
/// own stored rates first, then the calculator module's stored
/// rates. Returns `None` when no stored rate exists anywhere -
/// the caller then fails the conversion instead of guessing.
pub(crate) fn resolve_rate(state: &ExpensesState, bus: &HookBus, from: &str, to: &str) -> Option<f64> {
    if from == to {
        return Some(1.0);
    }
    stored_rate(state, from, to).or_else(|| calculator_rate(bus, from, to))
}

/// Store a rate for a pair, upserting by (`base_currency`,
/// `target_currency`): a second store for the same pair updates
/// the existing row's rate and source instead of adding a
/// duplicate row. Returns the stored row and whether it was
/// newly created. An updated row keeps its original id and
/// `created_at`, so one pair is always one row.
pub(crate) fn upsert_currency_rate(
    state: &ExpensesState,
    base: &str,
    target: &str,
    rate: f64,
    source: &str,
) -> (CurrencyRate, bool) {
    let existing = state
        .currency_rates
        .iter()
        .map(|entry| entry.value().clone())
        .find(|row| row.base_currency == base && row.target_currency == target);
    match existing {
        Some(mut row) => {
            row.rate = rate;
            row.source = source.to_string();
            state.currency_rates.insert(row.id, row.clone());
            (row, false)
        }
        None => {
            let row = CurrencyRate {
                id: Uuid::now_v7(),
                base_currency: base.to_string(),
                target_currency: target.to_string(),
                rate,
                source: source.to_string(),
                created_at: Utc::now(),
            };
            state.currency_rates.insert(row.id, row.clone());
            (row, true)
        }
    }
}

/// Convert an amount in minor units using a stored rate only.
/// Fails with 422 when no stored rate exists for the pair.
pub(crate) fn convert_amount(
    state: &ExpensesState,
    bus: &HookBus,
    amount_minor: i64,
    from: &str,
    to: &str,
) -> UnifiedResult<i64> {
    if from == to {
        return Ok(amount_minor);
    }
    let rate = resolve_rate(state, bus, from, to).ok_or_else(|| {
        UnifiedError::unprocessable(format!(
            "No stored exchange rate for {from} to {to}; conversion refused rather than guessing a rate"
        ))
    })?;
    let converted = (amount_minor as f64 * rate).round();
    if !converted.is_finite() || converted < 0.0 || converted > MAX_AMOUNT_MINOR as f64 {
        return Err(UnifiedError::unprocessable(
            "Converted amount is out of range",
        ));
    }
    Ok(converted as i64)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn rate_row(base: &str, target: &str, rate: f64, at: chrono::DateTime<Utc>) -> CurrencyRate {
        CurrencyRate {
            id: Uuid::now_v7(),
            base_currency: base.to_string(),
            target_currency: target.to_string(),
            rate,
            source: "manual".to_string(),
            created_at: at,
        }
    }

    #[test]
    fn stored_rate_picks_the_newest_row_for_a_duplicate_pair() {
        let state = ExpensesState::default();
        let older = Utc.with_ymd_and_hms(2026, 11, 20, 10, 0, 0).unwrap();
        let newer = Utc.with_ymd_and_hms(2026, 11, 21, 10, 0, 0).unwrap();
        state.currency_rates.insert(
            Uuid::now_v7(),
            rate_row("USD", "EUR", 0.90, older),
        );
        state.currency_rates.insert(
            Uuid::now_v7(),
            rate_row("USD", "EUR", 0.85, newer),
        );
        assert_eq!(stored_rate(&state, "USD", "EUR"), Some(0.85));
    }

    #[test]
    fn stored_rate_picks_the_newest_inverse_row_too() {
        let state = ExpensesState::default();
        let older = Utc.with_ymd_and_hms(2026, 11, 20, 10, 0, 0).unwrap();
        let newer = Utc.with_ymd_and_hms(2026, 11, 21, 10, 0, 0).unwrap();
        state.currency_rates.insert(
            Uuid::now_v7(),
            rate_row("EUR", "USD", 1.10, older),
        );
        state.currency_rates.insert(
            Uuid::now_v7(),
            rate_row("EUR", "USD", 1.20, newer),
        );
        let got = stored_rate(&state, "USD", "EUR").unwrap();
        assert!((got - 1.0 / 1.20).abs() < 1e-12);
    }

    #[test]
    fn upsert_currency_rate_updates_the_same_pair_in_place() {
        let state = ExpensesState::default();
        let (first, created) = upsert_currency_rate(&state, "USD", "EUR", 0.90, "manual");
        assert!(created);
        assert_eq!(state.currency_rates.len(), 1);
        let (second, created) = upsert_currency_rate(&state, "USD", "EUR", 0.85, "feed");
        assert!(!created);
        assert_eq!(state.currency_rates.len(), 1);
        assert_eq!(second.id, first.id);
        assert_eq!(second.created_at, first.created_at);
        assert_eq!(second.rate, 0.85);
        assert_eq!(second.source, "feed");
    }

    #[test]
    fn upsert_currency_rate_keeps_other_pairs_separate() {
        let state = ExpensesState::default();
        upsert_currency_rate(&state, "USD", "EUR", 0.90, "manual");
        upsert_currency_rate(&state, "USD", "GBP", 0.79, "manual");
        assert_eq!(state.currency_rates.len(), 2);
        assert_eq!(stored_rate(&state, "USD", "EUR"), Some(0.90));
        assert_eq!(stored_rate(&state, "USD", "GBP"), Some(0.79));
    }
}
