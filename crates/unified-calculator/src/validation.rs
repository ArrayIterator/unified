//! calculator - validation.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::json;
use unified_core::pagination::{PageResult, PaginationParams};
use actix_web::{HttpRequest, HttpResponse};
use chrono::{DateTime, Utc};
use serde::Serialize;
use serde_json::{json, Value};
use uuid::Uuid;

pub(crate) fn new_id() -> String {
    Uuid::new_v4().to_string()
}

pub(crate) fn new_share_token() -> String {
    Uuid::new_v4().simple().to_string()
}

pub(crate) fn now() -> DateTime<Utc> {
    Utc::now()
}

/// Round a money amount to four decimals (NUMERIC(19,4)).
pub(crate) fn round_money(value: f64) -> f64 {
    (value * 10_000.0).round() / 10_000.0
}

/// Round a ratio or percent to six decimals for stable output.
pub(crate) fn round_ratio(value: f64) -> f64 {
    (value * 1_000_000.0).round() / 1_000_000.0
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

/// The server-verified caller (see `unified_core::caller::Caller`):
/// the user id identifies the actor, the server-resolved role is the
/// role slug or id the capability check runs against.
pub(crate) struct Caller {
    pub(crate) actor: String,
    pub(crate) role: String,
}

pub(crate) fn caller(req: &HttpRequest) -> UnifiedResult<Caller> {
    let verified = unified_core::caller::require_caller(req)?;
    let actor = verified.user_id.clone();
    let role = verified.role().to_string();
    Ok(Caller { actor, role })
}

/// Ask the roles module whether `role` holds `permission` through
/// the `role.check` hook. Fails closed: no answer means not allowed.
pub(crate) fn check_capability(ctx: &AppContext, role: &str, permission: &str) -> bool {
    let context = ctx.hooks.emit(
        "role.check",
        json!({ "role": role, "permission": permission, "allowed": false }),
    );
    context
        .get("allowed")
        .and_then(|value| value.as_bool())
        .unwrap_or(false)
}

pub(crate) fn require_permission(ctx: &AppContext, role: &str, permission: &str) -> UnifiedResult<()> {
    if check_capability(ctx, role, permission) {
        Ok(())
    } else {
        Err(UnifiedError::forbidden(format!(
            "Role \"{role}\" does not hold permission \"{permission}\""
        )))
    }
}

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

/// A number that must be finite (rejects NaN and infinity, which
/// JSON cannot represent and which would poison every downstream
/// result).
pub(crate) fn require_finite(value: f64, field: &str) -> UnifiedResult<f64> {
    if !value.is_finite() {
        return Err(UnifiedError::bad_request(format!(
            "{field} must be a finite number"
        )));
    }
    Ok(value)
}

pub(crate) fn require_non_negative(value: f64, field: &str) -> UnifiedResult<f64> {
    let value = require_finite(value, field)?;
    if value < 0.0 {
        return Err(UnifiedError::bad_request(format!(
            "{field} must not be negative"
        )));
    }
    Ok(value)
}

pub(crate) fn require_positive(value: f64, field: &str) -> UnifiedResult<f64> {
    let value = require_finite(value, field)?;
    if value <= 0.0 {
        return Err(UnifiedError::bad_request(format!(
            "{field} must be greater than zero"
        )));
    }
    Ok(value)
}

/// A three-letter uppercase ISO 4217 currency code.
pub(crate) fn valid_currency_code(code: &str) -> bool {
    code.len() == 3
        && code
            .chars()
            .all(|ch| ch.is_ascii_uppercase() && ch.is_ascii_alphabetic())
}

/// Wrap a calculation result with the exact inputs that produced it.
pub(crate) fn calculation_response(calculator_type: &str, inputs: Value, results: Value) -> Value {
    json!({
        "calculator": calculator_type,
        "inputs": inputs,
        "results": results,
    })
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
    fn round_money_rounds_to_four_decimals() {
        assert_eq!(round_money(1.23456), 1.2346);
        assert_eq!(round_money(1.23454), 1.2345);
    }

    #[test]
    fn round_ratio_rounds_to_six_decimals() {
        assert_eq!(round_ratio(0.123456789), 0.123457);
    }

    #[test]
    fn require_finite_rejects_nan_and_infinite() {
        assert!(require_finite(f64::NAN, "value").is_err());
        assert!(require_finite(f64::INFINITY, "value").is_err());
        assert!(require_finite(1.5, "value").is_ok());
    }

    #[test]
    fn require_positive_rejects_zero_and_negative() {
        assert!(require_positive(0.0, "value").is_err());
        assert!(require_positive(-1.0, "value").is_err());
        assert!(require_positive(0.01, "value").is_ok());
    }

    #[test]
    fn require_non_negative_accepts_zero() {
        assert!(require_non_negative(0.0, "value").is_ok());
        assert!(require_non_negative(-0.1, "value").is_err());
    }

    #[test]
    fn require_text_trims_and_enforces_max_length() {
        assert_eq!(require_text("  hi  ", "label", 10).unwrap(), "hi");
        assert!(require_text("", "label", 10).is_err());
        assert!(require_text("   ", "label", 10).is_err());
        assert!(require_text("12345678901", "label", 10).is_err());
    }

    #[test]
    fn valid_currency_code_requires_three_uppercase_letters() {
        assert!(valid_currency_code("USD"));
        assert!(!valid_currency_code("usd"));
        assert!(!valid_currency_code("US"));
        assert!(!valid_currency_code("USDD"));
        assert!(!valid_currency_code("U1D"));
    }
}
