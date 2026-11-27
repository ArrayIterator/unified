// Whitelabel module - validation helpers.
//
// Split from `src/modules/whitelabel.rs`; the module root re-exports the public API of this module.

use unified_core::error::{UnifiedError, UnifiedResult};
use lettre::Address;

use super::constants::{MAX_DOMAIN_LEN, MAX_STORAGE_REF_LEN};

// Validation helpers

pub(crate) fn valid_hex_color(color: &str) -> bool {
    let digits = color.strip_prefix('#').unwrap_or("");
    (digits.len() == 3 || digits.len() == 6)
        && digits.chars().all(|c| c.is_ascii_hexdigit())
}

pub(crate) fn require_hex_color(value: &str, field: &str) -> UnifiedResult<String> {
    let trimmed = value.trim();
    if !valid_hex_color(trimmed) {
        return Err(UnifiedError::bad_request(format!(
            "{field} must be a hex colour such as \"#2563EB\" or \"#FFF\""
        )));
    }
    Ok(trimmed.to_uppercase())
}

pub(crate) fn require_text(value: &str, field: &str, max: usize) -> UnifiedResult<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err(UnifiedError::bad_request(format!("{field} must not be empty")));
    }
    if trimmed.chars().count() > max {
        return Err(UnifiedError::bad_request(format!(
            "{field} must be at most {max} characters"
        )));
    }
    Ok(trimmed.to_string())
}

pub(crate) fn require_optional_text(value: &str, field: &str, max: usize) -> UnifiedResult<String> {
    require_text(value, field, max)
}

/// A custom domain is a hostname: dot-separated labels of letters,
/// digits, and hyphens, never leading or trailing with a hyphen or
/// a dot, at least one dot, at most 253 chars. No scheme, port, or
/// path is allowed. Same rule as the portals module.
pub(crate) fn valid_domain(domain: &str) -> bool {
    if domain.is_empty() || domain.len() > MAX_DOMAIN_LEN {
        return false;
    }
    if domain.starts_with('.')
        || domain.ends_with('.')
        || domain.starts_with('-')
        || domain.ends_with('-')
        || !domain.contains('.')
    {
        return false;
    }
    domain.split('.').all(|label| {
        !label.is_empty()
            && label.len() <= 63
            && !label.starts_with('-')
            && !label.ends_with('-')
            && label
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-')
    })
}

pub(crate) fn require_domain(value: &str) -> UnifiedResult<String> {
    let trimmed = value.trim().to_lowercase();
    if !valid_domain(&trimmed) {
        return Err(UnifiedError::bad_request(format!(
            "custom_domain \"{value}\" is not a valid hostname"
        )));
    }
    Ok(trimmed)
}

pub(crate) fn require_email(value: &str, field: &str) -> UnifiedResult<String> {
    let trimmed = value.trim().to_string();
    trimmed.parse::<Address>().map_err(|_| {
        UnifiedError::bad_request(format!("{field} is not a valid email address"))
    })?;
    Ok(trimmed)
}

/// A storage reference (target + key) must be given together, like
/// the portals module logo: the bytes live in the storage module
/// and this module only stores the reference.
pub(crate) fn require_storage_pair(
    target: &Option<Option<String>>,
    key: &Option<Option<String>>,
    field: &str,
) -> UnifiedResult<(Option<Option<String>>, Option<Option<String>>)> {
    let clean = |value: &Option<Option<String>>| -> UnifiedResult<Option<Option<String>>> {
        match value {
            None => Ok(None),
            Some(None) => Ok(Some(None)),
            Some(Some(raw)) => Ok(Some(Some(require_text(
                raw,
                field,
                MAX_STORAGE_REF_LEN,
            )?))),
        }
    };
    let target = clean(target)?;
    let key = clean(key)?;
    // A storage pair is set or cleared as a unit: touching one side
    // without the other (including clearing only one side, which
    // would leave a dangling reference) is rejected. Callers that
    // change one side must resend the other side in the same
    // request, even when it is already stored.
    let target_touched = target.is_some();
    let key_touched = key.is_some();
    if target_touched != key_touched {
        return Err(UnifiedError::bad_request(format!(
            "{field} storage target and key must be given together"
        )));
    }
    Ok((target, key))
}

pub(crate) fn apply_storage_pair(
    current_target: &mut Option<String>,
    current_key: &mut Option<String>,
    target: Option<Option<String>>,
    key: Option<Option<String>>,
) {
    if let Some(target) = target {
        *current_target = target;
    }
    if let Some(key) = key {
        *current_key = key;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn valid_hex_color_shapes() {
        assert!(valid_hex_color("#FFF"));
        assert!(valid_hex_color("#2563EB"));
        assert!(valid_hex_color("2563EB"));
        assert!(!valid_hex_color("#GGG"));
        assert!(!valid_hex_color("#FF"));
        assert!(!valid_hex_color(""));
        assert!(!valid_hex_color("#1234567"));
    }

    #[test]
    fn require_hex_color_normalizes() {
        assert_eq!(require_hex_color("#ff0000", "primary").unwrap(), "#FF0000");
        assert!(require_hex_color("red", "primary").is_err());
        assert!(require_hex_color("#12", "primary").is_err());
    }

    #[test]
    fn require_text_trims_and_bounds() {
        assert_eq!(require_text("  hello  ", "name", 10).unwrap(), "hello");
        assert!(require_text("", "name", 10).is_err());
        assert!(require_text("   ", "name", 10).is_err());
        assert!(require_text("abc", "name", 2).is_err());
        assert!(require_optional_text("  hello  ", "name", 10).is_ok());
    }

    #[test]
    fn valid_domain_rules() {
        assert!(valid_domain("example.com"));
        assert!(valid_domain("a.b"));
        assert!(!valid_domain("bad"));
        assert!(!valid_domain("a..b.com"));
        assert!(!valid_domain("-bad.com"));
        assert!(!valid_domain("bad.com-"));
        assert!(!valid_domain("https://x.com"));
        assert!(!valid_domain("a.b_c.com"));
    }

    #[test]
    fn require_domain_lowercases() {
        assert_eq!(require_domain("EXAMPLE.COM").unwrap(), "example.com");
        assert!(require_domain("bad").is_err());
    }

    #[test]
    fn require_email_validates() {
        assert!(require_email("a@b.com", "sender").is_ok());
        assert!(require_email("not-an-email", "sender").is_err());
    }

    #[test]
    fn require_storage_pair_must_match_sides() {
        assert_eq!(
            require_storage_pair(&None, &None, "logo").unwrap(),
            (None, None)
        );
        assert_eq!(
            require_storage_pair(
                &Some(Some("t".to_string())),
                &Some(Some("k".to_string())),
                "logo"
            )
            .unwrap(),
            (Some(Some("t".to_string())), Some(Some("k".to_string())))
        );
        assert!(require_storage_pair(&Some(Some("t".to_string())), &None, "logo").is_err());
        assert!(
            require_storage_pair(&Some(None), &Some(None), "logo").is_ok()
        );
        assert!(
            require_storage_pair(
                &Some(Some("  ".to_string())),
                &Some(Some("k".to_string())),
                "logo"
            )
            .is_err()
        );
    }

    #[test]
    fn apply_storage_pair_updates_set_sides() {
        let mut target = Some("old-t".to_string());
        let mut key = Some("old-k".to_string());
        apply_storage_pair(
            &mut target,
            &mut key,
            Some(Some("new-t".to_string())),
            Some(Some("new-k".to_string())),
        );
        assert_eq!(target.as_deref(), Some("new-t"));
        assert_eq!(key.as_deref(), Some("new-k"));
        apply_storage_pair(&mut target, &mut key, None, None);
        assert_eq!(target.as_deref(), Some("new-t"));
        apply_storage_pair(&mut target, &mut key, Some(None), Some(None));
        assert_eq!(target, None);
        assert_eq!(key, None);
    }
}
