//! Money amounts: decimal-safe integer minor units.

use unified_core::error::{UnifiedError, UnifiedResult};
use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Money (integer minor units; never f64)
// ---------------------------------------------------------------------------

/// A decimal-safe amount: integer minor units plus an ISO 4217
/// currency code. Two-decimal currencies store cents (1999 = 19.99
/// USD); zero-decimal currencies store whole units (1999 = 1999 JPY).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Money {
    pub amount_minor: i64,
    pub currency: String,
}

/// A money amount as supplied by API callers: an exact decimal
/// string such as `"19.99"`, parsed with integer arithmetic only.
#[derive(Debug, Clone, Deserialize)]
pub struct MoneyInput {
    pub amount: String,
    #[serde(default)]
    pub currency: Option<String>,
}

/// True for zero-decimal currencies (no minor units), matching the
/// store and payments modules' lists.
pub(crate) fn is_zero_decimal_currency(currency: &str) -> bool {
    matches!(
        currency,
        "JPY" | "KRW" | "VND" | "CLP" | "ISK" | "UGX" | "XAF" | "XOF" | "XPF"
    )
}

/// Number of fractional digits a currency uses: 0 or 2.
pub(crate) fn currency_exponent(currency: &str) -> u32 {
    if is_zero_decimal_currency(currency) {
        0
    } else {
        2
    }
}

pub(crate) fn validate_currency(currency: &str) -> UnifiedResult<()> {
    if currency.len() == 3 && currency.chars().all(|c| c.is_ascii_alphabetic()) {
        Ok(())
    } else {
        Err(UnifiedError::bad_request(
            "Currency must be a 3-letter code such as USD",
        ))
    }
}

/// Parse an exact decimal string into integer minor units. Rejects
/// negative amounts, non-numeric input, and more fractional digits
/// than the currency allows. No floating point is involved.
pub(crate) fn parse_money_amount(amount: &str, currency: &str) -> UnifiedResult<i64> {
    let exponent = currency_exponent(currency);
    let text = amount.trim();
    if text.is_empty() {
        return Err(UnifiedError::bad_request("Amount must not be empty"));
    }
    let (whole, frac) = match text.split_once('.') {
        Some((whole, frac)) => (whole, Some(frac)),
        None => (text, None),
    };
    if whole.is_empty() || !whole.chars().all(|c| c.is_ascii_digit()) {
        return Err(UnifiedError::bad_request(format!(
            "Amount \"{amount}\" is not a valid non-negative decimal"
        )));
    }
    let frac_digits = frac.unwrap_or("");
    if !frac_digits.chars().all(|c| c.is_ascii_digit()) {
        return Err(UnifiedError::bad_request(format!(
            "Amount \"{amount}\" is not a valid non-negative decimal"
        )));
    }
    if frac_digits.len() as u32 > exponent {
        return Err(UnifiedError::bad_request(format!(
            "Amount \"{amount}\" has more than {exponent} decimal places for {currency}"
        )));
    }
    let mut frac_padded = frac_digits.to_string();
    while frac_padded.len() < exponent as usize {
        frac_padded.push('0');
    }
    let scale: i128 = 10i128.pow(exponent);
    let whole_val: i128 = whole.parse().map_err(|_| {
        UnifiedError::bad_request(format!("Amount \"{amount}\" is not a valid decimal"))
    })?;
    let frac_val: i128 = if frac_padded.is_empty() {
        0
    } else {
        frac_padded.parse().map_err(|_| {
            UnifiedError::bad_request(format!("Amount \"{amount}\" is not a valid decimal"))
        })?
    };
    let minor = whole_val
        .checked_mul(scale)
        .and_then(|value| value.checked_add(frac_val))
        .ok_or_else(|| UnifiedError::bad_request("Amount is too large"))?;
    i64::try_from(minor)
        .map_err(|_| UnifiedError::bad_request("Amount is too large"))
}

/// Format integer minor units as an exact decimal string (`"19.99"`,
/// `"1999"` for zero-decimal currencies). Negative amounts format
/// from the absolute value with a leading `"-"` (`-199` is `"-1.99"`):
/// formatting the remainder directly would print `"-1.-99"`, because
/// `%` keeps the dividend's sign in Rust.
pub(crate) fn format_minor(amount_minor: i64, currency: &str) -> String {
    if currency_exponent(currency) == 0 {
        format!("{amount_minor}")
    } else {
        let sign = if amount_minor < 0 { "-" } else { "" };
        let absolute = (amount_minor as i128).abs();
        format!("{sign}{}.{:02}", absolute / 100, absolute % 100)
    }
}

/// Resolve a caller-supplied money input against a fallback
/// currency (the product currency, else USD).
pub(crate) fn resolve_money(input: &MoneyInput, fallback_currency: Option<&str>) -> UnifiedResult<Money> {
    let currency = input
        .currency
        .clone()
        .or_else(|| fallback_currency.map(str::to_string))
        .unwrap_or_else(|| "USD".to_string())
        .to_uppercase();
    validate_currency(&currency)?;
    let amount_minor = parse_money_amount(&input.amount, &currency)?;
    Ok(Money {
        amount_minor,
        currency,
    })
}

/// Round half-up of `value * numerator / denominator` using a wide
/// intermediate, so percentage math never overflows i64 and never
/// touches floating point.
pub(crate) fn mul_div_round_half_up(value: i64, numerator: i64, denominator: i64) -> i64 {
    let wide = value as i128 * numerator as i128;
    let rounded = (wide + denominator as i128 / 2) / denominator as i128;
    rounded.clamp(i64::MIN as i128, i64::MAX as i128) as i64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zero_decimal_currencies_are_known() {
        assert!(is_zero_decimal_currency("JPY"));
        assert!(is_zero_decimal_currency("KRW"));
        assert!(is_zero_decimal_currency("VND"));
        assert!(!is_zero_decimal_currency("USD"));
        assert!(!is_zero_decimal_currency("EUR"));
    }

    #[test]
    fn currency_exponent_is_two_or_zero() {
        assert_eq!(currency_exponent("USD"), 2);
        assert_eq!(currency_exponent("EUR"), 2);
        assert_eq!(currency_exponent("JPY"), 0);
    }

    #[test]
    fn validate_currency_accepts_three_letter_codes() {
        assert!(validate_currency("USD").is_ok());
        assert!(validate_currency("eur").is_ok());
        assert!(validate_currency("US").is_err());
        assert!(validate_currency("US1").is_err());
        assert!(validate_currency("USDX").is_err());
    }

    #[test]
    fn parse_money_amount_parses_usd_cents() {
        assert_eq!(parse_money_amount("19.99", "USD").unwrap(), 1999);
        assert_eq!(parse_money_amount("10", "USD").unwrap(), 1000);
        assert_eq!(parse_money_amount("0.01", "USD").unwrap(), 1);
        assert_eq!(parse_money_amount("  2.50  ", "USD").unwrap(), 250);
    }

    #[test]
    fn parse_money_amount_rejects_bad_input() {
        assert!(parse_money_amount("", "USD").is_err());
        assert!(parse_money_amount("-5", "USD").is_err());
        assert!(parse_money_amount("abc", "USD").is_err());
        assert!(parse_money_amount("10.005", "USD").is_err());
        assert!(parse_money_amount("10.0", "JPY").is_err());
        assert_eq!(parse_money_amount("10", "JPY").unwrap(), 10);
    }

    #[test]
    fn format_minor_round_trips() {
        assert_eq!(format_minor(1999, "USD"), "19.99");
        assert_eq!(format_minor(100, "USD"), "1.00");
        assert_eq!(format_minor(0, "USD"), "0.00");
        assert_eq!(format_minor(1999, "JPY"), "1999");
    }

    #[test]
    fn format_minor_formats_negative_amounts() {
        assert_eq!(format_minor(-199, "USD"), "-1.99");
        assert_eq!(format_minor(-100, "USD"), "-1.00");
        assert_eq!(format_minor(-105, "USD"), "-1.05");
        assert_eq!(format_minor(-1, "USD"), "-0.01");
        assert_eq!(format_minor(-1999, "JPY"), "-1999");
    }

    #[test]
    fn resolve_money_uses_fallback_currency() {
        let input = MoneyInput {
            amount: "5".to_string(),
            currency: None,
        };
        let money = resolve_money(&input, Some("EUR")).unwrap();
        assert_eq!(money.amount_minor, 500);
        assert_eq!(money.currency, "EUR");
        let lower = MoneyInput {
            amount: "1.00".to_string(),
            currency: Some("usd".to_string()),
        };
        assert_eq!(resolve_money(&lower, None).unwrap().currency, "USD");
        let bad = MoneyInput {
            amount: "1.00".to_string(),
            currency: Some("US".to_string()),
        };
        assert!(resolve_money(&bad, None).is_err());
    }

    #[test]
    fn mul_div_round_half_up_rounds() {
        assert_eq!(mul_div_round_half_up(100, 15, 100), 15);
        assert_eq!(mul_div_round_half_up(101, 50, 100), 51);
        assert_eq!(mul_div_round_half_up(1, 1, 3), 0);
        assert_eq!(mul_div_round_half_up(2, 1, 3), 1);
    }
}
