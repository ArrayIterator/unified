// bank_transfer — Validation helpers and small pure utilities.

use unified_core::error::{UnifiedError, UnifiedResult};
use chrono::{NaiveDate, Utc};

// ---------------------------------------------------------------------------
// Validation helpers.
// ---------------------------------------------------------------------------

/// Validate an ISO 4217 currency code (three uppercase letters).
pub(crate) fn validate_currency(currency: &str) -> UnifiedResult<String> {
    let code = currency.trim().to_uppercase();
    if code.len() != 3 || !code.chars().all(|c| c.is_ascii_alphabetic()) {
        return Err(UnifiedError::bad_request(format!(
            "Currency \"{currency}\" is not a three-letter ISO 4217 code"
        )));
    }
    Ok(code)
}

/// Normalize an account number for duplicate checks: strip
/// whitespace and uppercase.
pub(crate) fn normalize_account_number(value: &str) -> String {
    value
        .chars()
        .filter(|c| !c.is_whitespace())
        .flat_map(|c| c.to_uppercase())
        .collect()
}

/// Validate an account number / IBAN: 4-34 alphanumeric characters
/// after whitespace is stripped.
pub(crate) fn validate_account_number(value: &str) -> UnifiedResult<String> {
    let compact = normalize_account_number(value);
    if compact.len() < 4 || compact.len() > 34 {
        return Err(UnifiedError::bad_request(
            "Account number / IBAN must be 4 to 34 characters",
        ));
    }
    if !compact.chars().all(|c| c.is_ascii_alphanumeric()) {
        return Err(UnifiedError::bad_request(
            "Account number / IBAN must contain only letters and digits",
        ));
    }
    Ok(compact)
}

/// Validate a SWIFT / BIC code: 8 or 11 alphanumeric characters.
pub(crate) fn validate_swift_bic(value: &str) -> UnifiedResult<String> {
    let code = value.trim().to_uppercase();
    if !(code.len() == 8 || code.len() == 11)
        || !code.chars().all(|c| c.is_ascii_alphanumeric())
    {
        return Err(UnifiedError::bad_request(
            "SWIFT / BIC must be 8 or 11 alphanumeric characters",
        ));
    }
    Ok(code)
}

/// Validate a proof storage key: a relative key, never an absolute
/// path and never containing a parent-directory traversal. Only the
/// reference is stored; the file itself lives in the file store.
pub(crate) fn validate_storage_key(value: &str) -> UnifiedResult<String> {
    let key = value.trim().to_string();
    if key.is_empty() {
        return Err(UnifiedError::bad_request(
            "Proof storage_key is required (a reference to the uploaded proof file)",
        ));
    }
    if key.len() > 512 {
        return Err(UnifiedError::bad_request(
            "Proof storage_key must be at most 512 characters",
        ));
    }
    if key.starts_with('/') || key.contains('\\') || key.split('/').any(|part| part == "..") {
        return Err(UnifiedError::bad_request(
            "Proof storage_key must be a relative key without parent-directory traversal",
        ));
    }
    if key.chars().any(|c| c.is_control()) {
        return Err(UnifiedError::bad_request(
            "Proof storage_key must not contain control characters",
        ));
    }
    Ok(key)
}

/// Validate a transfer date (`YYYY-MM-DD`): well-formed and not in
/// the future.
pub(crate) fn validate_transfer_date(value: &str) -> UnifiedResult<String> {
    let date = NaiveDate::parse_from_str(value.trim(), "%Y-%m-%d").map_err(|_| {
        UnifiedError::bad_request(format!(
            "Transfer date \"{value}\" must be YYYY-MM-DD"
        ))
    })?;
    if date > Utc::now().date_naive() {
        return Err(UnifiedError::bad_request(
            "Transfer date must not be in the future",
        ));
    }
    Ok(date.format("%Y-%m-%d").to_string())
}

/// True for zero-decimal currencies (no minor units). The list is
/// the full ISO 4217 zero-decimal set: BIF, CLP, DJF, GNF, ISK, JPY,
/// KMF, KRW, PYG, RWF, UGX, VUV, XAF, XOF and XPF. Leaving any of
/// them out would convert their amounts x100 and flag every proof
/// for them as an amount mismatch that can never reconcile.
fn is_zero_decimal_currency(currency: &str) -> bool {
    matches!(
        currency,
        "BIF" | "CLP" | "DJF" | "GNF" | "ISK" | "JPY" | "KMF" | "KRW" | "PYG" | "RWF"
            | "UGX" | "VND" | "VUV" | "XAF" | "XOF" | "XPF"
    )
}

/// Convert a major-unit amount (as the store reports order totals)
/// to integer minor units. Returns `None` for a negative or
/// non-finite amount, and for an amount whose scaled value does not
/// fit in an `i64`: a plain `as i64` cast would silently saturate
/// to `i64::MAX` instead of signalling the overflow.
pub(crate) fn major_to_minor(amount_major: f64, currency: &str) -> Option<i64> {
    if !amount_major.is_finite() || amount_major < 0.0 {
        return None;
    }
    let scaled = if is_zero_decimal_currency(currency) {
        amount_major
    } else {
        amount_major * 100.0
    };
    if !scaled.is_finite() {
        return None;
    }
    let rounded = scaled.round();
    if rounded < 0.0 || rounded >= i64::MAX as f64 {
        return None;
    }
    Some(rounded as i64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validate_currency_accepts_three_letters() {
        assert_eq!(validate_currency("usd").unwrap(), "USD");
        assert_eq!(validate_currency(" EUR ").unwrap(), "EUR");
        assert!(validate_currency("US").is_err());
        assert!(validate_currency("USDD").is_err());
        assert!(validate_currency("U1D").is_err());
    }

    #[test]
    fn account_number_normalise_and_validate() {
        assert_eq!(normalize_account_number("DE89 3704"), "DE893704");
        assert_eq!(
            validate_account_number("DE89 3704 0044 0532 0130 00").unwrap(),
            "DE89370400440532013000"
        );
        assert!(validate_account_number("abc").is_err());
        assert!(validate_account_number(&"A".repeat(35)).is_err());
        assert!(validate_account_number("ab cd!").is_err());
    }

    #[test]
    fn validate_swift_bic_lengths() {
        assert_eq!(validate_swift_bic("deutdeff").unwrap(), "DEUTDEFF");
        assert_eq!(validate_swift_bic("DEUTDEFF500").unwrap(), "DEUTDEFF500");
        assert!(validate_swift_bic("DEUTDEF").is_err());
        assert!(validate_swift_bic("DEUTDEFF50").is_err());
        assert!(validate_swift_bic("DEUTDEFF5!").is_err());
    }

    #[test]
    fn validate_storage_key_rejects_traversal_and_absolute() {
        assert_eq!(
            validate_storage_key("proofs/abc.png").unwrap(),
            "proofs/abc.png"
        );
        assert!(validate_storage_key("").is_err());
        assert!(validate_storage_key("/abs/path").is_err());
        assert!(validate_storage_key("../escape").is_err());
        assert!(validate_storage_key("a/../b").is_err());
        assert!(validate_storage_key("a\\b").is_err());
        assert!(validate_storage_key(&"k".repeat(513)).is_err());
    }

    #[test]
    fn validate_transfer_date_rejects_future_and_bad_shape() {
        assert_eq!(
            validate_transfer_date("2020-01-01").unwrap(),
            "2020-01-01"
        );
        assert!(validate_transfer_date("2999-01-01").is_err());
        assert!(validate_transfer_date("01/01/2020").is_err());
        assert!(validate_transfer_date("2020-13-01").is_err());
    }

    #[test]
    fn zero_decimal_currencies() {
        // The full ISO 4217 zero-decimal set, including the seven
        // currencies that used to be missing (BIF, DJF, GNF, KMF,
        // PYG, RWF, VUV): each must convert 1:1, never x100.
        for code in [
            "BIF", "CLP", "DJF", "GNF", "ISK", "JPY", "KMF", "KRW", "PYG", "RWF", "UGX", "VND",
            "VUV", "XAF", "XOF", "XPF",
        ] {
            assert!(is_zero_decimal_currency(code), "{code} is zero-decimal");
            assert_eq!(major_to_minor(100.0, code), Some(100));
        }
        assert!(!is_zero_decimal_currency("USD"));
        assert!(!is_zero_decimal_currency("IDR"));
    }

    #[test]
    fn major_to_minor_conversion() {
        assert_eq!(major_to_minor(10.25, "USD"), Some(1025));
        assert_eq!(major_to_minor(0.1, "USD"), Some(10));
        assert_eq!(major_to_minor(10.5, "JPY"), Some(11));
        assert_eq!(major_to_minor(10.5, "BIF"), Some(11));
        assert_eq!(major_to_minor(10.5, "RWF"), Some(11));
        assert_eq!(major_to_minor(0.0, "USD"), Some(0));
        assert_eq!(major_to_minor(-1.0, "USD"), None);
        assert_eq!(major_to_minor(f64::NAN, "USD"), None);
        assert_eq!(major_to_minor(f64::INFINITY, "USD"), None);
    }

    #[test]
    fn major_to_minor_rejects_amounts_that_overflow_i64() {
        // Before the bounds check these silently saturated to
        // i64::MAX through `as i64` instead of returning None.
        assert_eq!(major_to_minor(1e18, "USD"), None);
        assert_eq!(major_to_minor(1e19, "JPY"), None);
        assert_eq!(major_to_minor(1e19, "BIF"), None);
        assert_eq!(major_to_minor(f64::MAX, "USD"), None);
        assert_eq!(major_to_minor(f64::MAX, "JPY"), None);
        // Large but representable amounts still convert.
        assert_eq!(major_to_minor(9.0e15, "JPY"), Some(9_000_000_000_000_000));
        assert_eq!(major_to_minor(9.0e13, "USD"), Some(9_000_000_000_000_000));
    }
}
