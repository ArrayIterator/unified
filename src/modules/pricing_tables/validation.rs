//! pricing tables - validation.

use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::json;
use unified_core::pagination::{PageResult, PaginationParams};
use actix_web::HttpResponse;
use chrono::{DateTime, Utc};
use serde::Serialize;
use uuid::Uuid;
use super::types::*;

pub(crate) fn new_id() -> String {
    Uuid::new_v4().to_string()
}

pub(crate) fn now() -> DateTime<Utc> {
    Utc::now()
}

/// True for zero-decimal currencies (no minor units), matching the
/// billing and store modules' list.
pub(crate) fn is_zero_decimal_currency(currency: &str) -> bool {
    unified_core::db::money::is_zero_decimal_currency(currency)
}

/// Convert a major-unit amount from a hook payload to integer minor
/// units. Rounds half away from zero, matching `serde_major`.
pub(crate) fn major_to_minor(amount_major: f64, currency: &str) -> i64 {
    if is_zero_decimal_currency(currency) {
        amount_major.round() as i64
    } else {
        (amount_major * 100.0).round() as i64
    }
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

/// Apply a ratio (a fraction such as `1.0 - discount`) to a
/// minor-unit amount, rounding half away from zero to minor units.
/// Rates stay `f64` by design; the result is rounded exactly once.
pub(crate) fn apply_ratio_to_minor(amount_minor: i64, ratio: f64) -> i64 {
    (amount_minor as f64 * ratio).round() as i64
}

/// Apply a percentage (0..=100) to a minor-unit amount, rounding
/// half away from zero to minor units.
pub(crate) fn apply_percent_to_minor(amount_minor: i64, percent: f64) -> i64 {
    (amount_minor as f64 * percent / 100.0).round() as i64
}

/// Integer division rounded half away from zero, with `i128`
/// intermediates so splits never overflow. A zero divisor yields
/// zero (callers only divide by positive sums).
pub(crate) fn div_rounded(dividend: i128, divisor: i128) -> i128 {
    if divisor == 0 {
        return 0;
    }
    let negative = (dividend < 0) != (divisor < 0);
    let numerator = dividend.unsigned_abs();
    let denominator = divisor.unsigned_abs();
    let quotient = (numerator + denominator / 2) / denominator;
    if negative {
        -(quotient as i128)
    } else {
        quotient as i128
    }
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

/// `My Pricing` -> `my-pricing`.
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

pub(crate) fn valid_slug(slug: &str) -> bool {
    !slug.is_empty()
        && slug
            .chars()
            .all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '-')
}

pub(crate) fn valid_email(email: &str) -> bool {
    let mut parts = email.split('@');
    matches!((parts.next(), parts.next(), parts.next()), (Some(local), Some(domain), None)
        if !local.is_empty() && !domain.is_empty() && domain.contains('.'))
}

/// A CTA URL is a relative path (`/pricing`) or an `https://` URL.
/// Rejects `javascript:`, `data:`, `vbscript:`, protocol-relative
/// (`//host`), plain `http://`, and anything with whitespace or
/// control characters -- no open redirects, no script URLs.
pub(crate) fn valid_cta_url(url: &str) -> bool {
    if url.is_empty() || url.len() > MAX_URL_LEN {
        return false;
    }
    if url.chars().any(|ch| ch.is_whitespace() || ch.is_control()) {
        return false;
    }
    let lower = url.to_lowercase();
    if lower.starts_with("javascript:")
        || lower.starts_with("data:")
        || lower.starts_with("vbscript:")
    {
        return false;
    }
    if let Some(rest) = url.strip_prefix('/') {
        // A single leading slash is a relative path; `//host` is a
        // protocol-relative URL to another host and is rejected.
        return !rest.starts_with('/');
    }
    if let Some(rest) = lower.strip_prefix("https://") {
        // A host must follow the scheme; no userinfo tricks, no
        // trailing-only slash games beyond a normal path.
        let host = rest.split(['/', '?', '#']).next().unwrap_or("");
        return !host.is_empty() && !host.contains('@') && !host.contains('\\');
    }
    false
}

/// `#0B6B3A` style hex colour, exactly 7 characters.
pub(crate) fn valid_hex_color(value: &str) -> bool {
    value.len() == 7
        && value.starts_with('#')
        && value[1..].chars().all(|ch| ch.is_ascii_hexdigit())
}

/// ISO 4217 currency code: exactly three uppercase letters.
pub(crate) fn valid_currency(value: &str) -> bool {
    value.len() == 3 && value.chars().all(|ch| ch.is_ascii_uppercase())
}

pub(crate) fn valid_layout(layout: &str) -> bool {
    matches!(layout, "grid" | "columns" | "comparison")
}

pub(crate) fn require_text(value: &str, field: &str, max: usize) -> UnifiedResult<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err(UnifiedError::bad_request(format!(
            "{field} must not be empty"
        )));
    }
    if trimmed.len() > max {
        return Err(UnifiedError::bad_request(format!(
            "{field} must be at most {max} characters"
        )));
    }
    Ok(trimmed.to_string())
}

pub(crate) fn optional_text(value: Option<String>, max: usize) -> UnifiedResult<String> {
    match value {
        Some(text) => require_text(&text, "value", max).map_err(|_| {
            UnifiedError::bad_request(format!("value must be at most {max} characters"))
        }),
        None => Ok(String::new()),
    }
}

pub(crate) fn valid_price(price: i64, field: &str) -> UnifiedResult<()> {
    if price < 0 {
        return Err(UnifiedError::bad_request(format!("{field} must be >= 0")));
    }
    Ok(())
}

pub(crate) fn valid_percent(value: f64, field: &str) -> UnifiedResult<()> {
    if !value.is_finite() || !(0.0..=100.0).contains(&value) {
        return Err(UnifiedError::bad_request(format!(
            "{field} must be between 0 and 100"
        )));
    }
    Ok(())
}

pub(crate) fn respond_created<T: Serialize>(result: UnifiedResult<T>, debug: bool) -> HttpResponse {
    match result {
        Ok(data) => json::success_response(data, 201),
        Err(error) => json::error_response(&error, debug),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slugify_lowercases_and_collapses_separators() {
        assert_eq!(slugify("My Pricing!"), "my-pricing");
        assert_eq!(slugify("  A  B "), "a-b");
    }

    #[test]
    fn slugify_collapses_separator_only_input_to_empty() {
        assert_eq!(slugify("---"), "");
        assert_eq!(slugify(""), "");
    }

    #[test]
    fn valid_slug_accepts_lowercase_dash_slugs() {
        assert!(valid_slug("my-pricing"));
        assert!(valid_slug("a-b-1"));
        assert!(!valid_slug("My_Pricing"));
        assert!(!valid_slug(""));
    }

    #[test]
    fn valid_email_accepts_plain_addresses() {
        assert!(valid_email("a@b.co"));
        assert!(!valid_email("a@b"));
        assert!(!valid_email("a@@b.co"));
        assert!(!valid_email("@b.co"));
    }

    #[test]
    fn valid_cta_url_accepts_relative_and_https_urls() {
        assert!(valid_cta_url("/pricing"));
        assert!(valid_cta_url("https://example.com/x"));
    }

    #[test]
    fn valid_cta_url_rejects_scripts_and_open_redirects() {
        assert!(!valid_cta_url("javascript:alert(1)"));
        assert!(!valid_cta_url("http://example.com"));
        assert!(!valid_cta_url("//evil.com"));
        assert!(!valid_cta_url("https://"));
        assert!(!valid_cta_url("/a b"));
        assert!(!valid_cta_url("data:text/html"));
    }

    #[test]
    fn valid_hex_color_accepts_six_digit_codes() {
        assert!(valid_hex_color("#0B6B3A"));
        assert!(valid_hex_color("#0b6b3a"));
        assert!(!valid_hex_color("#0B6B3"));
        assert!(!valid_hex_color("0B6B3A"));
    }

    #[test]
    fn valid_currency_requires_three_uppercase_letters() {
        assert!(valid_currency("USD"));
        assert!(!valid_currency("usd"));
        assert!(!valid_currency("US"));
    }

    #[test]
    fn valid_layout_accepts_known_layouts_only() {
        assert!(valid_layout("grid"));
        assert!(!valid_layout("list"));
    }

    #[test]
    fn money_conversions_round_trip() {
        assert_eq!(major_to_minor(10.25, "USD"), 1025);
        assert_eq!(major_to_minor(10.005, "USD"), 1001);
        assert_eq!(major_to_minor(10.0, "JPY"), 10);
        assert_eq!(minor_to_major(1025, "USD"), 10.25);
        assert_eq!(minor_to_major(1025, "JPY"), 1025.0);
        assert_eq!(apply_percent_to_minor(1025, 10.0), 103);
        assert_eq!(apply_ratio_to_minor(1200, 0.9), 1080);
        assert_eq!(div_rounded(100, 12), 8);
        assert_eq!(div_rounded(1025, 12), 85);
    }

    #[test]
    fn valid_price_accepts_minor_units_and_rejects_negatives() {
        assert!(valid_price(0, "monthly_price").is_ok());
        assert!(valid_price(1000, "monthly_price").is_ok());
        assert!(valid_price(-1, "monthly_price").is_err());
    }

    #[test]
    fn require_text_trims_and_enforces_max_length() {
        assert_eq!(require_text("  hi  ", "label", 10).unwrap(), "hi");
        assert!(require_text("", "label", 10).is_err());
        assert!(require_text("12345678901", "label", 10).is_err());
    }

    #[test]
    fn valid_price_accepts_zero_and_rejects_negative() {
        assert!(valid_price(0, "amount").is_ok());
        assert!(valid_price(-1, "amount").is_err());
    }

    #[test]
    fn valid_percent_accepts_zero_to_one_hundred() {
        assert!(valid_percent(100.0).is_ok());
        assert!(valid_percent(0.0).is_ok());
        assert!(valid_percent(100.01).is_err());
        assert!(valid_percent(-0.1).is_err());
    }
}
