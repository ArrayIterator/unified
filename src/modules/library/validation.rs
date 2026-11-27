// Library - Input validation and normalisation helpers.

use unified_core::error::{UnifiedError, UnifiedResult};
use chrono::{DateTime, Utc};
use uuid::Uuid;

use super::{MAX_SLUG_LEN};

pub(crate) fn parse_uuid(raw: &str, what: &str) -> UnifiedResult<Uuid> {
    Uuid::parse_str(raw).map_err(|_| UnifiedError::bad_request(format!("Invalid {what} id: {raw}")))
}

pub(crate) fn parse_optional_uuid(raw: &Option<String>, what: &str) -> UnifiedResult<Option<Uuid>> {
    match raw {
        Some(value) => Ok(Some(parse_uuid(value, what)?)),
        None => Ok(None),
    }
}

pub(crate) fn parse_datetime(raw: &str, field: &str) -> UnifiedResult<DateTime<Utc>> {
    raw.parse::<DateTime<Utc>>()
        .map_err(|_| UnifiedError::bad_request(format!("{field} must be an RFC 3339 timestamp")))
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

pub(crate) fn optional_text(value: &Option<String>, field: &str, max: usize) -> UnifiedResult<Option<String>> {
    match value {
        Some(raw) => Ok(Some(require_text(raw, field, max)?)),
        None => Ok(None),
    }
}

/// Validate a value against a settings list. The list is the live
/// admin-configured one - never a constant in this file.
pub(crate) fn require_in_list(value: &str, field: &str, allowed: &[String]) -> UnifiedResult<String> {
    let checked = require_text(value, field, MAX_SLUG_LEN)?;
    if !allowed.iter().any(|item| item == &checked) {
        return Err(UnifiedError::bad_request(format!(
            "{field} \"{checked}\" is not allowed; allowed values: {}",
            allowed.join(", ")
        )));
    }
    Ok(checked)
}

pub(crate) fn require_currency(value: &str) -> UnifiedResult<String> {
    let checked = require_text(value, "currency", 3)?;
    if checked.len() != 3 || !checked.chars().all(|c| c.is_ascii_alphabetic()) {
        return Err(UnifiedError::bad_request(
            "currency must be a 3-letter ISO code",
        ));
    }
    Ok(checked.to_ascii_uppercase())
}

pub(crate) fn require_non_negative(value: i64, field: &str) -> UnifiedResult<i64> {
    if value < 0 {
        return Err(UnifiedError::bad_request(format!(
            "{field} must not be negative"
        )));
    }
    Ok(value)
}

pub(crate) fn require_positive(value: i64, field: &str) -> UnifiedResult<i64> {
    if value < 1 {
        return Err(UnifiedError::bad_request(format!(
            "{field} must be at least 1"
        )));
    }
    Ok(value)
}

pub(crate) fn slugify(raw: &str) -> String {
    let mut slug = String::new();
    let mut last_dash = true;
    for ch in raw.trim().to_lowercase().chars() {
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

/// Normalize an ISBN by stripping hyphens and spaces, then
/// validate the ISBN-10 or ISBN-13 checksum. Returns the
/// normalized digits, or an error naming the problem.
pub(crate) fn normalize_isbn(raw: &str) -> UnifiedResult<String> {
    let digits: String = raw
        .chars()
        .filter(|ch| !ch.is_whitespace() && *ch != '-')
        .collect();
    let upper = digits.to_ascii_uppercase();
    match upper.len() {
        10 => {
            let mut sum: i64 = 0;
            for (index, ch) in upper.chars().enumerate() {
                let value: i64 = if index == 9 && ch == 'X' {
                    10
                } else if ch.is_ascii_digit() {
                    (ch as i64) - ('0' as i64)
                } else {
                    return Err(UnifiedError::bad_request(
                        "isbn must be a valid ISBN-10 (digits, with X allowed as the last character)",
                    ));
                };
                sum += value * (10 - index as i64);
            }
            if sum % 11 != 0 {
                return Err(UnifiedError::bad_request(
                    "isbn checksum is invalid for ISBN-10",
                ));
            }
            Ok(upper)
        }
        13 => {
            if !upper.chars().all(|ch| ch.is_ascii_digit()) {
                return Err(UnifiedError::bad_request(
                    "isbn must be a valid ISBN-13 (13 digits)",
                ));
            }
            let mut sum: i64 = 0;
            for (index, ch) in upper.chars().enumerate() {
                let value: i64 = (ch as i64) - ('0' as i64);
                sum += if index % 2 == 0 { value } else { value * 3 };
            }
            if sum % 10 != 0 {
                return Err(UnifiedError::bad_request(
                    "isbn checksum is invalid for ISBN-13",
                ));
            }
            Ok(upper)
        }
        _ => Err(UnifiedError::bad_request(
            "isbn must be 10 or 13 characters after removing hyphens and spaces",
        )),
    }
}


#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slugify_lowercases_and_joins_with_dashes() {
        assert_eq!(slugify("Hello World!"), "hello-world");
    }

    #[test]
    fn slugify_collapses_repeated_separators() {
        assert_eq!(slugify("  A--B__C  "), "a-b-c");
    }

    #[test]
    fn slugify_empty_stays_empty() {
        assert_eq!(slugify("!!!"), "");
    }

    #[test]
    fn normalize_isbn_accepts_isbn10_with_hyphens() {
        assert_eq!(normalize_isbn("0-306-40615-2").unwrap(), "0306406152");
    }

    #[test]
    fn normalize_isbn_accepts_isbn13() {
        assert_eq!(normalize_isbn("978-0-306-40615-7").unwrap(), "9780306406157");
    }

    #[test]
    fn normalize_isbn_accepts_isbn10_with_x_check_digit() {
        assert_eq!(normalize_isbn("0-8044-2957-X").unwrap(), "080442957X");
    }

    #[test]
    fn normalize_isbn_rejects_bad_checksum() {
        assert!(normalize_isbn("0306406153").is_err());
    }

    #[test]
    fn normalize_isbn_rejects_wrong_length() {
        assert!(normalize_isbn("12345").is_err());
    }

    #[test]
    fn require_text_trims() {
        assert_eq!(require_text("  hello  ", "title", 10).unwrap(), "hello");
    }

    #[test]
    fn require_text_rejects_empty() {
        assert!(require_text("   ", "title", 10).is_err());
    }

    #[test]
    fn require_text_rejects_too_long() {
        assert!(require_text("abcdef", "title", 5).is_err());
    }

    #[test]
    fn require_currency_uppercases() {
        assert_eq!(require_currency("usd").unwrap(), "USD");
    }

    #[test]
    fn require_currency_rejects_short_code() {
        assert!(require_currency("US").is_err());
    }

    #[test]
    fn require_currency_rejects_non_alpha() {
        assert!(require_currency("U1D").is_err());
    }

    #[test]
    fn require_non_negative_accepts_zero() {
        assert_eq!(require_non_negative(0, "count").unwrap(), 0);
    }

    #[test]
    fn require_non_negative_rejects_negative() {
        assert!(require_non_negative(-1, "count").is_err());
    }

    #[test]
    fn require_positive_accepts_one() {
        assert_eq!(require_positive(1, "count").unwrap(), 1);
    }

    #[test]
    fn require_positive_rejects_zero() {
        assert!(require_positive(0, "count").is_err());
    }

    #[test]
    fn parse_uuid_round_trip() {
        let id = Uuid::new_v4();
        assert_eq!(parse_uuid(&id.to_string(), "item").unwrap(), id);
    }

    #[test]
    fn parse_uuid_rejects_garbage() {
        assert!(parse_uuid("not-a-uuid", "item").is_err());
    }

    #[test]
    fn parse_datetime_accepts_rfc3339() {
        assert!(parse_datetime("2026-01-01T00:00:00Z", "due").is_ok());
    }

    #[test]
    fn parse_datetime_rejects_garbage() {
        assert!(parse_datetime("tomorrow", "due").is_err());
    }

    #[test]
    fn require_in_list_accepts_allowed_value() {
        let allowed = vec!["active".to_string(), "withdrawn".to_string()];
        assert_eq!(
            require_in_list(" active ", "status", &allowed).unwrap(),
            "active"
        );
    }

    #[test]
    fn require_in_list_rejects_other_value() {
        let allowed = vec!["active".to_string()];
        assert!(require_in_list("lost", "status", &allowed).is_err());
    }

    #[test]
    fn optional_text_none_stays_none() {
        assert_eq!(optional_text(&None, "note", 10).unwrap(), None);
    }

    #[test]
    fn optional_text_some_trims() {
        assert_eq!(
            optional_text(&Some("  x ".to_string()), "note", 10).unwrap(),
            Some("x".to_string())
        );
    }
}
