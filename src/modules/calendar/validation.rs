//! calendar module - Module, time, clock, country and weekend validation.

use unified_core::error::{UnifiedError, UnifiedResult};
use chrono::{DateTime, FixedOffset, Utc};
use url::Url;

use super::constants::MAX_MODULE_FILTER;
use super::constants::MAX_MODULE_ID_LEN;

/// A module id is a lowercase identifier: letters, digits, dash,
/// underscore only. Anything else is rejected so a filter can
/// never smuggle a path or query fragment into a hook payload.
pub(crate) fn validate_module_id(value: &str) -> UnifiedResult<String> {
    let cleaned = value.trim().to_lowercase();
    if cleaned.is_empty() || cleaned.len() > MAX_MODULE_ID_LEN {
        return Err(UnifiedError::bad_request(
            "module id must be 1-64 characters",
        ));
    }
    if !cleaned
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || ch == '-' || ch == '_')
    {
        return Err(UnifiedError::bad_request(format!(
            "Invalid module id \"{cleaned}\": expected letters, digits, dash, or underscore"
        )));
    }
    Ok(cleaned)
}

/// Parse a comma-separated `modules` query value into a validated,
/// de-duplicated list.
pub(crate) fn parse_module_filter(raw: Option<&str>) -> UnifiedResult<Option<Vec<String>>> {
    let Some(raw) = raw else {
        return Ok(None);
    };
    let mut out: Vec<String> = Vec::new();
    for part in raw.split(',') {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        let cleaned = validate_module_id(part)?;
        if !out.contains(&cleaned) {
            out.push(cleaned);
        }
    }
    if out.len() > MAX_MODULE_FILTER {
        return Err(UnifiedError::bad_request(format!(
            "At most {MAX_MODULE_FILTER} modules may be requested at once"
        )));
    }
    Ok(Some(out))
}

/// Parse an RFC 3339 timestamp (UTC or with an offset; converted
/// to UTC for comparison).
pub(crate) fn parse_time(value: &str, field: &str) -> UnifiedResult<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(value.trim())
        .map(|value| value.with_timezone(&Utc))
        .map_err(|_| {
            UnifiedError::bad_request(format!(
                "{field} must be an RFC 3339 timestamp, for example 2026-11-23T02:00:00Z"
            ))
        })
}

/// Parse the `timezone` parameter. Accepts "UTC"/"Z" or a fixed
/// offset ("+07:00", "-05:30", "+0700"). IANA names are rejected
/// with a plain explanation: `chrono-tz` is not in Cargo.toml, so
/// named zones cannot be resolved in this build.
pub(crate) fn parse_timezone(raw: Option<&str>) -> UnifiedResult<FixedOffset> {
    let Some(raw) = raw else {
        return Ok(FixedOffset::east_opt(0).expect("zero offset is always valid"));
    };
    let cleaned = raw.trim();
    if cleaned.eq_ignore_ascii_case("utc")
        || cleaned.eq_ignore_ascii_case("z")
        || cleaned.eq_ignore_ascii_case("etc/utc")
    {
        return Ok(FixedOffset::east_opt(0).expect("zero offset is always valid"));
    }
    let digits = cleaned.replace(':', "");
    if (digits.len() == 5 || digits.len() == 6)
        && (digits.starts_with('+') || digits.starts_with('-'))
        && digits[1..].chars().all(|ch| ch.is_ascii_digit())
    {
        let sign: i32 = if digits.starts_with('-') { -1 } else { 1 };
        let hours: i32 = digits[1..3]
            .parse()
            .map_err(|_| UnifiedError::bad_request("Invalid timezone offset"))?;
        let minutes: i32 = digits[3..5]
            .parse()
            .map_err(|_| UnifiedError::bad_request("Invalid timezone offset"))?;
        if hours > 14 || minutes > 59 {
            return Err(UnifiedError::bad_request(
                "Timezone offset hours must be at most 14 and minutes at most 59",
            ));
        }
        return FixedOffset::east_opt(sign * (hours * 3600 + minutes * 60))
            .ok_or_else(|| UnifiedError::bad_request("Timezone offset is out of range"));
    }
    Err(UnifiedError::unprocessable(format!(
        "Timezone \"{cleaned}\" is not supported: pass \"UTC\" or a fixed offset such as \"+07:00\". IANA zone names need the chrono-tz crate, which is not in Cargo.toml."
    )))
}

/// Validate a "HH:MM" 24-hour clock value.
pub(crate) fn validate_clock(value: &str, field: &str) -> UnifiedResult<String> {
    let cleaned = value.trim();
    let parts: Vec<&str> = cleaned.split(':').collect();
    if parts.len() != 2 {
        return Err(UnifiedError::bad_request(format!(
            "{field} must be a 24-hour clock value such as \"09:00\""
        )));
    }
    let hours: u32 = parts[0]
        .parse()
        .map_err(|_| UnifiedError::bad_request(format!("{field} must be a 24-hour clock value")))?;
    let minutes: u32 = parts[1]
        .parse()
        .map_err(|_| UnifiedError::bad_request(format!("{field} must be a 24-hour clock value")))?;
    if hours > 23 || minutes > 59 {
        return Err(UnifiedError::bad_request(format!(
            "{field} must be a 24-hour clock value between \"00:00\" and \"23:59\""
        )));
    }
    Ok(format!("{hours:02}:{minutes:02}"))
}

/// Validate a working-hours window: both ends must be 24-hour
/// clock values and the start must be strictly before the end.
/// Clock strings are normalized to "HH:MM" first, so the
/// comparison is a plain string comparison on zero-padded values.
pub(crate) fn validate_working_hours(start: &str, end: &str) -> UnifiedResult<(String, String)> {
    let start = validate_clock(start, "working_hours_start")?;
    let end = validate_clock(end, "working_hours_end")?;
    if start >= end {
        return Err(UnifiedError::bad_request(
            "working_hours_start must be before working_hours_end",
        ));
    }
    Ok((start, end))
}

/// Host part of an allowed-host entry, with any `:port` stripped
/// (bracketed IPv6 hosts keep their brackets). The parsed URL's
/// host never carries a port, so both sides compare bare hosts.
fn bare_host(host: &str) -> &str {
    let host = host.trim();
    if let Some(rest) = host.strip_prefix('[') {
        return match rest.find(']') {
            // `rest` starts one byte into `host`, so the bracket
            // sits at `end + 1` in `host`.
            Some(end) => &host[..=end + 1],
            None => host,
        };
    }
    match host.split_once(':') {
        Some((bare, _)) => bare,
        None => host,
    }
}

/// Validate a create-intent URL before it is returned to a client.
/// Only two shapes are allowed, both same-site:
/// - a relative path ("/todos/new?..."), never protocol-relative
///   ("//evil.example/...") and never containing a backslash;
/// - an absolute http(s) URL whose host matches one of
///   `allowed_hosts` (the site the request arrived on), with no
///   embedded credentials.
/// Everything else - `javascript:`, `data:`, `ftp:`, external
/// hosts, credentialed URLs - is rejected, so a hook answer or an
/// admin-set template can never serve a client an off-site or
/// script URL.
pub(crate) fn validate_create_url(raw: &str, allowed_hosts: &[&str]) -> UnifiedResult<String> {
    let url = raw.trim();
    if url.is_empty() {
        return Err(UnifiedError::bad_request(
            "create URL must not be empty",
        ));
    }
    if url.chars().any(|ch| ch.is_control()) || url.contains('\\') {
        return Err(UnifiedError::bad_request(
            "create URL must be a same-site path or an http(s) URL on this site",
        ));
    }
    if let Some(path) = url.strip_prefix('/') {
        if path.starts_with('/') {
            return Err(UnifiedError::bad_request(
                "create URL must be a same-site path or an http(s) URL on this site",
            ));
        }
        return Ok(url.to_string());
    }
    let parsed = Url::parse(url).map_err(|_| {
        UnifiedError::bad_request(
            "create URL must be a same-site path or an http(s) URL on this site",
        )
    })?;
    if parsed.scheme() != "http" && parsed.scheme() != "https" {
        return Err(UnifiedError::bad_request(
            "create URL must use http or https",
        ));
    }
    if !parsed.username().is_empty() || parsed.password().is_some() {
        return Err(UnifiedError::bad_request(
            "create URL must not contain embedded credentials",
        ));
    }
    let Some(host) = parsed.host_str() else {
        return Err(UnifiedError::bad_request(
            "create URL must be a same-site path or an http(s) URL on this site",
        ));
    };
    // Brackets are stripped on both sides: an IPv6 host may be
    // written "[::1]" (request host) or "::1" (parsed URL host).
    let host = host
        .trim_start_matches('[')
        .trim_end_matches(']');
    if !allowed_hosts.iter().any(|allowed| {
        bare_host(allowed)
            .trim_start_matches('[')
            .trim_end_matches(']')
            .eq_ignore_ascii_case(host)
    }) {
        return Err(UnifiedError::bad_request(
            "create URL must point at this site",
        ));
    }
    Ok(url.to_string())
}

/// Validate a create-link template at save time. Templates are
/// stored with `{start}`/`{end}`/`{title}` placeholders, so they
/// cannot be parsed as URLs yet - this only rejects shapes that
/// could never render into a safe URL: a dangerous scheme
/// (`javascript:`, `data:`, `vbscript:`, `file:`), a
/// protocol-relative URL ("//evil.example/..."), or an absolute
/// URL with a non-http(s) scheme. The rendered URL is checked
/// again with `validate_create_url` before it is returned.
pub(crate) fn validate_create_template(raw: &str) -> UnifiedResult<()> {
    let template = raw.trim();
    let lower = template.to_ascii_lowercase();
    if lower.starts_with("//") {
        return Err(UnifiedError::bad_request(
            "create-link template must be a same-site path or an http(s) URL on this site",
        ));
    }
    for scheme in ["javascript:", "data:", "vbscript:", "file:"] {
        if lower.starts_with(scheme) {
            return Err(UnifiedError::bad_request(format!(
                "create-link template must not use the \"{scheme}\" scheme"
            )));
        }
    }
    if let Some(end) = lower.find("://") {
        let scheme = &lower[..end];
        if !scheme.is_empty()
            && scheme
                .chars()
                .all(|ch| ch.is_ascii_alphanumeric() || ch == '+' || ch == '-' || ch == '.')
            && scheme != "http"
            && scheme != "https"
        {
            return Err(UnifiedError::bad_request(
                "create-link template must use http or https",
            ));
        }
    }
    Ok(())
}

/// Validate an ISO 3166-1 alpha-2 country code (two ASCII
/// letters), or an empty string meaning "no country configured".
pub(crate) fn validate_country(value: &str) -> UnifiedResult<String> {
    let cleaned = value.trim().to_uppercase();
    if cleaned.is_empty() {
        return Ok(String::new());
    }
    if cleaned.len() == 2 && cleaned.chars().all(|ch| ch.is_ascii_alphabetic()) {
        return Ok(cleaned);
    }
    Err(UnifiedError::bad_request(
        "holiday_country must be an ISO 3166-1 alpha-2 code such as \"ID\", or empty",
    ))
}

/// Validate weekend days: ISO weekday numbers 1=Monday..7=Sunday,
/// at least one day, at most six, de-duplicated and sorted.
pub(crate) fn validate_weekend_days(days: &[u8]) -> UnifiedResult<Vec<u8>> {
    if days.is_empty() || days.len() > 6 {
        return Err(UnifiedError::bad_request(
            "weekend_days must contain 1 to 6 ISO weekday numbers (1=Monday..7=Sunday)",
        ));
    }
    let mut cleaned: Vec<u8> = Vec::new();
    for day in days {
        if !(1..=7).contains(day) {
            return Err(UnifiedError::bad_request(format!(
                "weekend_days entry {day} is out of range: expected 1 (Monday) to 7 (Sunday)"
            )));
        }
        if !cleaned.contains(day) {
            cleaned.push(*day);
        }
    }
    cleaned.sort_unstable();
    Ok(cleaned)
}

/// URL-encode a short value for interpolation into a deep-link
/// template (percent-encoding; only unreserved characters pass).
pub(crate) fn url_encode(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char)
            }
            other => out.push_str(&format!("%{other:02X}")),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::parse_module_filter;
    use super::parse_time;
    use super::url_encode;
    use super::validate_clock;
    use super::validate_country;
    use super::validate_create_template;
    use super::validate_create_url;
    use super::validate_weekend_days;
    use super::validate_working_hours;

    #[test]
    fn clock_accepts_and_normalizes() {
        assert_eq!(validate_clock("9:05", "start").unwrap(), "09:05");
        assert_eq!(validate_clock("23:59", "end").unwrap(), "23:59");
        assert_eq!(validate_clock(" 08:00 ", "start").unwrap(), "08:00");
    }

    #[test]
    fn clock_rejects_out_of_range() {
        assert!(validate_clock("24:00", "start").is_err());
        assert!(validate_clock("09:60", "start").is_err());
    }

    #[test]
    fn clock_rejects_bad_shape() {
        assert!(validate_clock("9", "start").is_err());
        assert!(validate_clock("ab:cd", "start").is_err());
        assert!(validate_clock("", "start").is_err());
    }

    #[test]
    fn country_uppercase_and_empty() {
        assert_eq!(validate_country("id").unwrap(), "ID");
        assert_eq!(validate_country("").unwrap(), "");
        assert_eq!(validate_country("  us ").unwrap(), "US");
    }

    #[test]
    fn country_rejects_bad_codes() {
        assert!(validate_country("USA").is_err());
        assert!(validate_country("1d").is_err());
    }

    #[test]
    fn weekend_days_sorted_and_deduped() {
        assert_eq!(validate_weekend_days(&[7, 6, 6]).unwrap(), vec![6, 7]);
        assert_eq!(validate_weekend_days(&[7]).unwrap(), vec![7]);
    }

    #[test]
    fn weekend_days_reject_invalid() {
        assert!(validate_weekend_days(&[]).is_err());
        assert!(validate_weekend_days(&[0]).is_err());
        assert!(validate_weekend_days(&[8]).is_err());
        assert!(validate_weekend_days(&[1, 2, 3, 4, 5, 6, 7]).is_err());
    }

    #[test]
    fn module_filter_parses_and_dedupes() {
        let out = parse_module_filter(Some("appointments, events,appointments")).unwrap();
        assert_eq!(
            out,
            Some(vec!["appointments".to_string(), "events".to_string()])
        );
    }

    #[test]
    fn module_filter_rejects_invalid_id() {
        assert!(parse_module_filter(Some("Bad Module!")).is_err());
    }

    #[test]
    fn module_filter_none_is_none() {
        assert_eq!(parse_module_filter(None).unwrap(), None);
    }

    #[test]
    fn time_parses_rfc3339() {
        let parsed = parse_time("2026-01-02T03:04:05Z", "start").unwrap();
        assert_eq!(parsed.format("%Y%m%d").to_string(), "20260102");
    }

    #[test]
    fn time_rejects_garbage() {
        assert!(parse_time("not-a-date", "start").is_err());
    }

    #[test]
    fn url_encode_keeps_unreserved() {
        assert_eq!(url_encode("abc-_.~123"), "abc-_.~123");
    }

    #[test]
    fn url_encode_escapes_reserved() {
        assert_eq!(url_encode("hello world"), "hello%20world");
        assert_eq!(url_encode("a/b?c=d&e"), "a%2Fb%3Fc%3Dd%26e");
    }

    #[test]
    fn working_hours_accepts_ordered_window() {
        assert_eq!(
            validate_working_hours("09:00", "17:00").unwrap(),
            ("09:00".to_string(), "17:00".to_string())
        );
        assert_eq!(
            validate_working_hours("9:00", "17:00").unwrap(),
            ("09:00".to_string(), "17:00".to_string())
        );
    }

    #[test]
    fn working_hours_rejects_reversed_window() {
        assert!(validate_working_hours("17:00", "09:00").is_err());
    }

    #[test]
    fn working_hours_rejects_equal_bounds() {
        assert!(validate_working_hours("09:00", "09:00").is_err());
    }

    #[test]
    fn working_hours_rejects_bad_clock() {
        assert!(validate_working_hours("25:00", "17:00").is_err());
        assert!(validate_working_hours("09:00", "ab:cd").is_err());
    }

    #[test]
    fn create_url_accepts_same_site_relative_path() {
        assert_eq!(
            validate_create_url("/todos/new?due_at=2026-01-02T10%3A00%3A00Z", &[]).unwrap(),
            "/todos/new?due_at=2026-01-02T10%3A00%3A00Z"
        );
        assert_eq!(validate_create_url("/", &[]).unwrap(), "/");
    }

    #[test]
    fn create_url_accepts_same_site_absolute_url() {
        let allowed = ["app.example.com"];
        assert_eq!(
            validate_create_url("https://app.example.com/todos/new", &allowed).unwrap(),
            "https://app.example.com/todos/new"
        );
        // A port on the request host does not matter: hosts compare
        // without ports, and the scheme is case-insensitive.
        let allowed_with_port = ["app.example.com:8443"];
        assert!(validate_create_url("HTTPS://app.example.com/x", &allowed_with_port).is_ok());
        assert!(validate_create_url("http://app.example.com/x", &allowed).is_ok());
    }

    #[test]
    fn create_url_rejects_off_site_absolute_url() {
        let allowed = ["app.example.com"];
        assert!(validate_create_url("https://evil.example.com/x", &allowed).is_err());
        // Subdomains do not inherit the allow-list.
        assert!(validate_create_url("https://sub.app.example.com/x", &allowed).is_err());
        // No allowed host at all: only relative paths pass.
        assert!(validate_create_url("https://app.example.com/x", &[]).is_err());
    }

    #[test]
    fn create_url_rejects_script_and_other_schemes() {
        let allowed = ["app.example.com"];
        assert!(validate_create_url("javascript:alert(1)", &allowed).is_err());
        assert!(validate_create_url("JaVaScRiPt:alert(1)", &allowed).is_err());
        assert!(validate_create_url("data:text/html,<h1>x</h1>", &allowed).is_err());
        assert!(validate_create_url("ftp://app.example.com/x", &allowed).is_err());
    }

    #[test]
    fn create_url_rejects_protocol_relative_and_credentialed() {
        let allowed = ["app.example.com"];
        assert!(validate_create_url("//evil.example.com/x", &allowed).is_err());
        assert!(validate_create_url("/\\evil.example.com/x", &allowed).is_err());
        assert!(validate_create_url("https://user:pass@app.example.com/x", &allowed).is_err());
        assert!(validate_create_url("", &allowed).is_err());
    }

    #[test]
    fn create_template_rejects_dangerous_schemes() {
        assert!(validate_create_template("javascript:alert(1)").is_err());
        assert!(validate_create_template("data:text/html,x").is_err());
        assert!(validate_create_template("//evil.example.com/x").is_err());
        assert!(validate_create_template("ftp://example.com/x").is_err());
    }

    #[test]
    fn create_template_accepts_relative_and_same_site_shapes() {
        assert!(validate_create_template("/todos/new?due_at={start}&title={title}").is_ok());
        assert!(validate_create_template(
            "https://app.example.com/events/new?starts_at={start}"
        )
        .is_ok());
    }
}
