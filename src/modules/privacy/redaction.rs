// PII redaction helpers (public, for other modules to reuse).

use hmac::Hmac;
use regex::{Captures, Regex};
use serde_json::{Map, Value};
use sha2::Sha256;
use std::sync::OnceLock;

pub(crate) type HmacSha256 = Hmac<Sha256>;

pub const PERM_PRIVACY_READ: &str = "privacy:read";
pub const PERM_PRIVACY_WRITE: &str = "privacy:write";
pub const PERM_PRIVACY_ADMIN: &str = "privacy:admin";

/// Event emitted when a user is anonymized. Payload:
/// `{"user_id", "token", "scope_modules", "actor_id", "reason",
/// "already_anonymized"}`. The token is the derived token, never
/// an original value.
pub const EVENT_USER_ANONYMIZED: &str = "user.anonymized";
/// Privacy action events, emitted for the audit log to consume
/// (the integrator adds them to the audit module's subscription
/// list): `privacy.anonymized`, `privacy.consent.changed`,
/// `privacy.erasure.requested`, `privacy.erasure.fulfilled`,
/// `privacy.visibility.changed`, `privacy.fields.changed`,
/// `privacy.notice.published`.
pub const EVENT_PRIVACY_ANONYMIZED: &str = "privacy.anonymized";
pub const EVENT_PRIVACY_CONSENT_CHANGED: &str = "privacy.consent.changed";
pub const EVENT_PRIVACY_ERASURE_REQUESTED: &str = "privacy.erasure.requested";
pub const EVENT_PRIVACY_ERASURE_FULFILLED: &str = "privacy.erasure.fulfilled";
pub const EVENT_PRIVACY_VISIBILITY_CHANGED: &str = "privacy.visibility.changed";
pub const EVENT_PRIVACY_FIELDS_CHANGED: &str = "privacy.fields.changed";
pub const EVENT_PRIVACY_NOTICE_PUBLISHED: &str = "privacy.notice.published";
/// Compliance events this module emits for the Compliance module
/// to consume (this module never edits compliance code).
pub const EVENT_COMPLIANCE_CONSENT_GRANTED: &str = "compliance.consent.granted";
pub const EVENT_COMPLIANCE_CONSENT_WITHDRAWN: &str = "compliance.consent.withdrawn";
pub const EVENT_COMPLIANCE_ERASURE_REQUESTED: &str = "compliance.erasure.requested";
pub const EVENT_COMPLIANCE_ERASURE_FULFILLED: &str = "compliance.erasure.fulfilled";

/// Generic fallback notice returned for a module that has no
/// published notice of its own. Always marked `generic: true`.
pub const GENERIC_NOTICE_TEXT: &str = "We collect only the personal data needed to provide this feature, keep it private by default, and never share it without your consent. You may request access, correction, or erasure of your personal data at any time.";

/// Built-in consent types. Admins may define more through
/// `POST /api/v1/privacy/consent-types`.
pub const BUILTIN_CONSENT_TYPES: &[&str] =
    &["marketing", "analytics", "data-processing", "ai-training"];

/// Keys that look like personal data. A payload key matches when
/// its lowercased form contains one of these substrings.
pub(crate) const PII_KEY_MARKERS: &[&str] = &[
    "email", "mail", "phone", "tel", "mobile", "cell", "address", "street", "name",
    "birth", "dob", "national", "passport", "ssn", "tax_id", "taxid", "bank", "iban",
    "card_number", "salary", "gender", "photo", "latitude", "longitude", "ip_address",
    "device_id", "biometric", "fingerprint", "face", "account_number", "emergency_contact",
];

pub(crate) fn email_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"[A-Za-z0-9._%+\-]+@[A-Za-z0-9.\-]+\.[A-Za-z]{2,}").expect("email regex")
    })
}

pub(crate) fn phone_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"\+?\d[\d\s().\-]{7,17}\d").expect("phone regex")
    })
}

pub(crate) fn long_digits_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"\b\d{10,}\b").expect("long digits regex"))
}

// ---------------------------------------------------------------------------
// PII redaction helpers (public, for other modules to reuse)
// ---------------------------------------------------------------------------

/// Redact an email address, keeping the first character of the
/// local part and the full domain: `jane@example.com` becomes
/// `j***@example.com`. Values without `@` become `***`.
pub fn redact_email(value: &str) -> String {
    let trimmed = value.trim();
    match trimmed.split_once('@') {
        Some((local, domain)) if !domain.is_empty() => {
            let first = local.chars().next().unwrap_or('*');
            format!("{first}***@{domain}")
        }
        _ => "***".to_string(),
    }
}

/// Redact a phone number, keeping only the last two digits and a
/// leading `+` when present: `+62 812-3456-7890` becomes `+***90`.
pub fn redact_phone(value: &str) -> String {
    let digits: String = value.chars().filter(|c| c.is_ascii_digit()).collect();
    if digits.len() < 4 {
        return "***".to_string();
    }
    let tail = &digits[digits.len() - 2..];
    if value.trim_start().starts_with('+') {
        format!("+***{tail}")
    } else {
        format!("***{tail}")
    }
}

/// Redact an ID number, keeping the first two and last two
/// characters: `1234567890123456` becomes `12************56`.
pub fn redact_id_number(value: &str) -> String {
    let trimmed = value.trim();
    let chars: Vec<char> = trimmed.chars().collect();
    if chars.len() <= 4 {
        return "***".to_string();
    }
    let first: String = chars.iter().take(2).collect();
    let last: String = chars.iter().skip(chars.len() - 2).collect();
    format!("{first}{}{last}", "*".repeat(chars.len() - 4))
}

/// Redact a personal name, keeping only the first character:
/// `Jane Doe` becomes `J***`.
pub fn redact_name(value: &str) -> String {
    match value.trim().chars().next() {
        Some(first) => format!("{first}***"),
        None => "***".to_string(),
    }
}

/// True when the match looks like a date or time rather than a
/// phone number, so `redact_text` leaves timestamps alone.
pub(crate) fn looks_like_date_or_time(matched: &str) -> bool {
    if matched.contains(':') {
        return true;
    }
    let bytes = matched.as_bytes();
    // `YYYY-MM-DD` or `YYYY/MM/DD` at the start of the match.
    bytes.len() >= 10
        && bytes[0..4].iter().all(|b| b.is_ascii_digit())
        && (bytes[4] == b'-' || bytes[4] == b'/')
        && bytes[5..7].iter().all(|b| b.is_ascii_digit())
        && (bytes[7] == b'-' || bytes[7] == b'/')
}

/// Redact emails, phone-like numbers, and long ID-like digit runs
/// inside free text, keeping enough structure to stay useful.
/// Timestamps and dates are left untouched.
pub fn redact_text(input: &str) -> String {
    let step1 = email_re().replace_all(input, |caps: &Captures| {
        redact_email(caps.get(0).map(|m| m.as_str()).unwrap_or(""))
    });
    let step2 = long_digits_re().replace_all(&step1, |caps: &Captures| {
        redact_id_number(caps.get(0).map(|m| m.as_str()).unwrap_or(""))
    });
    let step3 = phone_re().replace_all(&step2, |caps: &Captures| {
        let matched = caps.get(0).map(|m| m.as_str()).unwrap_or("");
        if looks_like_date_or_time(matched) {
            return matched.to_string();
        }
        let digits = matched.chars().filter(|c| c.is_ascii_digit()).count();
        if (7..=15).contains(&digits) {
            redact_phone(matched)
        } else {
            matched.to_string()
        }
    });
    step3.into_owned()
}

/// True when a JSON object key looks like personal data.
pub fn is_pii_key(key: &str) -> bool {
    let lower = key.to_lowercase();
    PII_KEY_MARKERS.iter().any(|marker| lower.contains(marker))
}

/// Redact a JSON value in place, walking objects and arrays.
/// String values under keys that look like personal data are
/// fully redacted (email keys with `redact_email`, phone keys
/// with `redact_phone`, name keys with `redact_name` only when
/// `redact_names` is true, other PII keys by value shape);
/// every other string value is passed through `redact_text`,
/// which only masks detected email / phone / ID patterns.
pub fn redact_value(value: &Value, redact_names: bool) -> Value {
    match value {
        Value::Object(map) => {
            let mut out = Map::new();
            for (key, item) in map {
                out.insert(key.clone(), redact_value_for_key(key, item, redact_names));
            }
            Value::Object(out)
        }
        Value::Array(items) => Value::Array(
            items
                .iter()
                .map(|item| redact_value(item, redact_names))
                .collect(),
        ),
        Value::String(text) => Value::String(redact_text(text)),
        other => other.clone(),
    }
}

pub(crate) fn redact_value_for_key(key: &str, value: &Value, redact_names: bool) -> Value {
    if !is_pii_key(key) {
        return redact_value(value, redact_names);
    }
    let lower = key.to_lowercase();
    match value {
        Value::String(text) => {
            let redacted = if lower.contains("email") || lower.contains("mail") {
                redact_email(text)
            } else if lower.contains("phone")
                || lower.contains("tel")
                || lower.contains("mobile")
                || lower.contains("cell")
            {
                redact_phone(text)
            } else if lower.contains("name") {
                if redact_names {
                    redact_name(text)
                } else {
                    redact_text(text)
                }
            } else if text.contains('@') {
                redact_email(text)
            } else if text.chars().filter(|c| c.is_ascii_digit()).count() >= 10
                && text
                    .chars()
                    .all(|c| c.is_ascii_digit() || c.is_whitespace() || c == '-' || c == '+')
            {
                redact_id_number(text)
            } else {
                redact_text(text)
            };
            Value::String(redacted)
        }
        Value::Object(_) | Value::Array(_) => redact_value(value, redact_names),
        other => other.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redact_name_keeps_first_char_only() {
        assert_eq!(redact_name("Jane Doe"), "J***");
        assert_eq!(redact_name("a"), "a***");
        assert_eq!(redact_name("  Budi  "), "B***");
    }

    #[test]
    fn redact_name_empty_or_blank_is_fully_masked() {
        assert_eq!(redact_name(""), "***");
        assert_eq!(redact_name("   "), "***");
    }

    #[test]
    fn redact_name_keeps_unicode_first_char() {
        assert_eq!(redact_name("Élodie"), "É***");
    }
}
