// Events Manager — Input validation and pure value checks.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::{HookBus, HookContext};
use unified_core::json;
use unified_core::modules::{Module, ModuleRegistry, RouteInfo, RouteVersionSupport};
use unified_core::pagination::{PageResult, PaginationParams};
use actix_web::{web, HttpRequest, HttpResponse};
use chrono::{DateTime, Datelike, Duration, NaiveDate, TimeZone, Utc};
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::sync::{Arc, RwLock};
use uuid::Uuid;

use super::constants::*;
use super::types::*;
use super::store::*;
use super::helpers::*;
use super::service::*;
use super::handlers_events::*;
use super::handlers_tickets::*;
use super::handlers_registrations::*;
use super::handlers_reminders::*;
use super::handlers_public::*;
use super::hooks::*;
use super::module::*;

pub(crate) fn parse_uuid(raw: &str, what: &str) -> UnifiedResult<Uuid> {
    Uuid::parse_str(raw).map_err(|_| UnifiedError::bad_request(format!("Invalid {what} id: {raw}")))
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

/// A slug is lowercase letters, digits, and single hyphens only,
/// never leading or trailing with a hyphen, at most 128 chars.
pub(crate) fn valid_slug(slug: &str) -> bool {
    if slug.is_empty() || slug.len() > MAX_SLUG_LEN {
        return false;
    }
    if slug.starts_with('-') || slug.ends_with('-') || slug.contains("--") {
        return false;
    }
    slug.chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

/// A timezone is an IANA name (`"Asia/Jakarta"`, `"UTC"`): letters,
/// digits, `_`, `-`, `+`, and `/` only, no empty segments. Shape
/// only; no conversion is done (all stored times are UTC).
pub(crate) fn valid_timezone(timezone: &str) -> bool {
    if timezone.is_empty() || timezone.len() > MAX_TIMEZONE_LEN {
        return false;
    }
    if timezone == "UTC" {
        return true;
    }
    timezone.contains('/')
        && timezone.split('/').all(|segment| {
            !segment.is_empty()
                && segment
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-' || c == '+')
        })
}

pub(crate) fn valid_email(email: &str) -> bool {
    if email.is_empty() || email.len() > MAX_EMAIL_LEN || email.contains(' ') {
        return false;
    }
    let mut parts = email.split('@');
    let (Some(local), Some(domain)) = (parts.next(), parts.next()) else {
        return false;
    };
    if parts.next().is_some() || local.is_empty() || domain.is_empty() {
        return false;
    }
    domain.contains('.') && !domain.starts_with('.') && !domain.ends_with('.')
}

/// Money: a price in minor units, non-negative. The request JSON
/// already converted major units to minor units through
/// `serde_major` (half away from zero to minor units), so this only
/// rejects negative amounts.
pub(crate) fn valid_price(price: i64) -> UnifiedResult<i64> {
    if price < 0 {
        return Err(UnifiedError::bad_request("price must be >= 0"));
    }
    Ok(price)
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

/// Currency: 3-letter ISO code, uppercased.
pub(crate) fn valid_currency(currency: &str) -> UnifiedResult<String> {
    let upper = currency.trim().to_uppercase();
    if upper.len() != 3 || !upper.chars().all(|c| c.is_ascii_uppercase()) {
        return Err(UnifiedError::bad_request(
            "currency must be a 3-letter ISO code such as \"USD\"",
        ));
    }
    Ok(upper)
}

pub(crate) fn validate_venue(venue: &Venue) -> UnifiedResult<()> {
    if venue.name.trim().is_empty() || venue.name.len() > MAX_VENUE_NAME_LEN {
        return Err(UnifiedError::bad_request(
            "venue.name must not be empty and must be at most 200 characters",
        ));
    }
    if let Some(address) = &venue.address {
        if address.len() > MAX_ADDRESS_LEN {
            return Err(UnifiedError::bad_request(
                "venue.address must be at most 500 characters",
            ));
        }
    }
    if let Some(latitude) = venue.latitude {
        if !latitude.is_finite() || !(-90.0..=90.0).contains(&latitude) {
            return Err(UnifiedError::bad_request("venue.latitude must be between -90 and 90"));
        }
    }
    if let Some(longitude) = venue.longitude {
        if !longitude.is_finite() || !(-180.0..=180.0).contains(&longitude) {
            return Err(UnifiedError::bad_request(
                "venue.longitude must be between -180 and 180",
            ));
        }
    }
    if let Some(capacity) = venue.capacity {
        if capacity < 0 {
            return Err(UnifiedError::bad_request("venue.capacity must be >= 0"));
        }
    }
    Ok(())
}

pub(crate) fn validate_recurrence(rule: &RecurrenceRule, starts_at: DateTime<Utc>) -> UnifiedResult<()> {
    if rule.interval == 0 {
        return Err(UnifiedError::bad_request("recurrence.interval must be >= 1"));
    }
    if let Some(count) = rule.count {
        if count == 0 || count as usize > MAX_OCCURRENCES {
            return Err(UnifiedError::bad_request(format!(
                "recurrence.count must be between 1 and {MAX_OCCURRENCES}"
            )));
        }
    }
    if let Some(until) = rule.until {
        if until <= starts_at {
            return Err(UnifiedError::bad_request("recurrence.until must be after starts_at"));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{TimeZone, Utc};

    #[test]
    fn valid_slug_accepts_lowercase_hyphenated_slugs() {
        assert!(valid_slug("summer-fest-2026"));
        assert!(valid_slug("a"));
        assert!(!valid_slug(""));
        assert!(!valid_slug("Summer"));
        assert!(!valid_slug("-leading"));
        assert!(!valid_slug("trailing-"));
        assert!(!valid_slug("double--hyphen"));
        assert!(!valid_slug("has space"));
        assert!(!valid_slug(&"a".repeat(129)));
    }

    #[test]
    fn valid_timezone_accepts_iana_names() {
        assert!(valid_timezone("UTC"));
        assert!(valid_timezone("Asia/Jakarta"));
        assert!(valid_timezone("America/New_York"));
        assert!(!valid_timezone(""));
        assert!(!valid_timezone("Jakarta"));
        assert!(!valid_timezone("Asia//Jakarta"));
        assert!(!valid_timezone("Asia/Jakarta/"));
    }

    #[test]
    fn valid_email_rejects_bad_shapes() {
        assert!(valid_email("a@b.co"));
        assert!(!valid_email("a@@b.co"));
        assert!(!valid_email("a@b"));
        assert!(!valid_email("a@.b.co"));
        assert!(!valid_email("a@b.co."));
        assert!(!valid_email("a b@c.co"));
        assert!(!valid_email(""));
    }

    #[test]
    fn valid_price_accepts_minor_units_and_rejects_negatives() {
        assert_eq!(valid_price(1000).unwrap(), 1000);
        assert_eq!(valid_price(0).unwrap(), 0);
        assert!(valid_price(-1).is_err());
    }

    #[test]
    fn minor_to_major_converts_cents_and_zero_decimal_currencies() {
        assert_eq!(minor_to_major(1025, "USD"), 10.25);
        assert_eq!(minor_to_major(1025, "JPY"), 1025.0);
        assert_eq!(minor_to_major(0, "USD"), 0.0);
        assert_eq!(minor_to_major(-250, "EUR"), -2.5);
    }

    #[test]
    fn valid_currency_uppercases_iso_codes() {
        assert_eq!(valid_currency("usd").unwrap(), "USD");
        assert_eq!(valid_currency("eur").unwrap(), "EUR");
        assert!(valid_currency("US").is_err());
        assert!(valid_currency("USD1").is_err());
        assert!(valid_currency("").is_err());
    }

    #[test]
    fn require_text_trims_and_enforces_bounds() {
        assert_eq!(require_text("  hello  ", "title", 10).unwrap(), "hello");
        assert!(require_text("", "title", 10).is_err());
        assert!(require_text("toolong", "title", 3).is_err());
    }

    fn recurrence_rule(interval: u32, count: Option<u32>, until: Option<DateTime<Utc>>) -> RecurrenceRule {
        RecurrenceRule {
            frequency: RecurrenceFrequency::Daily,
            interval,
            count,
            until,
            exceptions: Vec::new(),
        }
    }

    #[test]
    fn validate_recurrence_enforces_interval_count_and_until() {
        let start = Utc.with_ymd_and_hms(2026, 1, 1, 9, 0, 0).unwrap();
        assert!(validate_recurrence(&recurrence_rule(1, Some(3), None), start).is_ok());
        assert!(validate_recurrence(&recurrence_rule(0, None, None), start).is_err());
        assert!(validate_recurrence(&recurrence_rule(1, Some(0), None), start).is_err());
        assert!(validate_recurrence(&recurrence_rule(1, Some(367), None), start).is_err());
        assert!(validate_recurrence(
            &recurrence_rule(1, None, Some(Utc.with_ymd_and_hms(2025, 12, 31, 9, 0, 0).unwrap())),
            start
        )
        .is_err());
        assert!(validate_recurrence(
            &recurrence_rule(1, None, Some(Utc.with_ymd_and_hms(2026, 2, 1, 9, 0, 0).unwrap())),
            start
        )
        .is_ok());
    }

    #[test]
    fn validate_venue_checks_name_and_coordinates() {
        let venue = Venue {
            name: "Hall A".to_string(),
            address: None,
            latitude: Some(-6.2),
            longitude: Some(106.8),
            capacity: Some(100),
        };
        assert!(validate_venue(&venue).is_ok());
        let bad_lat = Venue { latitude: Some(91.0), ..venue.clone() };
        assert!(validate_venue(&bad_lat).is_err());
        let bad_lon = Venue { longitude: Some(-181.0), ..venue.clone() };
        assert!(validate_venue(&bad_lon).is_err());
        let empty_name = Venue { name: "  ".to_string(), ..venue.clone() };
        assert!(validate_venue(&empty_name).is_err());
        let negative_capacity = Venue { capacity: Some(-1), ..venue };
        assert!(validate_venue(&negative_capacity).is_err());
    }
}
