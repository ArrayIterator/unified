//! Input validation and URL safety checks.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::{HookBus, HookContext};
use unified_core::json;
use unified_core::modules::{Module, ModuleRegistry, ProductCategory, RouteInfo, RouteVersionSupport};
use unified_core::tenancy::{schema_name_for_str, TenantContext as TenancyContext};
use unified_core::pagination::{PageResult, PaginationParams};
use actix_web::{web, HttpRequest, HttpResponse};
use base64::Engine as _;
use chrono::{DateTime, Datelike, NaiveDate, Utc};
use dashmap::DashMap;
use hmac::{Hmac, Mac};
use reqwest::header::RETRY_AFTER;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet, VecDeque};
use std::net::{IpAddr, ToSocketAddrs};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};
use uuid::Uuid;

use super::*;

// SSRF guard - follows `src/modules/downloader.rs` exactly, except
// provider fetches are https ONLY (the downloader also allows http;
// no holiday provider serves plain http, so the stricter rule
// applies here).

fn is_loopback(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(addr) => addr.is_loopback(),
        IpAddr::V6(addr) => addr.is_loopback(),
    }
}

/// IPv6 documentation range 2001:db8::/32 (RFC 3849).
fn is_ipv6_documentation(addr: &std::net::Ipv6Addr) -> bool {
    addr.segments()[0] == 0x2001 && addr.segments()[1] == 0x0db8
}

/// IPv6 benchmarking range 2001:2::/48 (RFC 5180).
fn is_ipv6_benchmarking(addr: &std::net::Ipv6Addr) -> bool {
    addr.segments()[0] == 0x2001 && addr.segments()[1] == 0x0002 && addr.segments()[2] == 0
}

/// Deprecated IPv6 site-local range fec0::/10 (RFC 3879).
fn is_ipv6_site_local(addr: &std::net::Ipv6Addr) -> bool {
    (addr.segments()[0] & 0xffc0) == 0xfec0
}

/// True when the address is private, link-local, or otherwise
/// reserved: never a valid provider target. Loopback is handled
/// separately (never allowed for provider fetches).
fn is_forbidden_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(addr) => {
            addr.is_private()
                || addr.is_link_local()
                || addr.is_unspecified()
                || addr.is_broadcast()
                || addr.is_multicast()
                || addr.is_documentation()
                || unified_core::net::ipv4_is_benchmarking(&addr)
                || unified_core::net::ipv4_is_reserved(&addr)
        }
        IpAddr::V6(addr) => {
            addr.is_unspecified()
                || addr.is_multicast()
                || (addr.segments()[0] & 0xfe00) == 0xfc00 // unique-local fc00::/7
                || (addr.segments()[0] & 0xffc0) == 0xfe80 // link-local fe80::/10
                || addr.is_unique_local()
                || is_ipv6_documentation(&addr)
                || is_ipv6_benchmarking(&addr)
                || is_ipv6_site_local(&addr)
                || !unified_core::net::ipv6_is_global(&addr) // any other reserved / non-globally-routable range
        }
    }
}

/// Resolve a hostname and require EVERY resolved address to be safe.
/// A mixed answer (one public, one private address) is refused: the
/// HTTP client picks among the resolved addresses, so any private
/// address in the answer could be the one fetched.
fn resolved_addresses_are_safe(url: &url::Url) -> bool {
    let Some(host) = url.host_str() else {
        return false;
    };
    if let Ok(ip) = host.parse::<IpAddr>() {
        return !is_loopback(ip) && !is_forbidden_ip(ip);
    }
    let port = url.port_or_known_default().unwrap_or(443);
    let Ok(addresses) = (host, port).to_socket_addrs() else {
        return false;
    };
    let mut any = false;
    for socket in addresses {
        any = true;
        let ip = socket.ip();
        if is_loopback(ip) || is_forbidden_ip(ip) {
            return false;
        }
    }
    any
}

/// Validate a provider fetch URL: https only, no embedded
/// credentials, host on the provider's allowlist, and no hostname
/// that resolves to a forbidden address. A DNS change between
/// validation and fetch (TOCTOU rebinding) is still possible and is
/// recorded as an open limitation, not a closed gap.
pub(crate) fn validate_provider_url(raw: &str, allowed_hosts: &[String]) -> UnifiedResult<()> {
    let url = url::Url::parse(raw.trim())
        .map_err(|_| UnifiedError::bad_request("Invalid provider URL"))?;
    if url.scheme() != "https" {
        return Err(UnifiedError::bad_request(
            "Provider URL must use https",
        ));
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(UnifiedError::bad_request(
            "Provider URL must not contain embedded credentials",
        ));
    }
    let Some(host) = url.host_str() else {
        return Err(UnifiedError::bad_request(
            "Provider URL must have a host",
        ));
    };
    if host.eq_ignore_ascii_case("localhost") {
        return Err(UnifiedError::bad_request(
            "Provider URL host \"localhost\" is not allowed",
        ));
    }
    if url.port().is_some_and(|port| port != 443) {
        return Err(UnifiedError::bad_request(
            "Provider URL must use the default https port (443)",
        ));
    }
    let host_lower = host.to_ascii_lowercase();
    if !allowed_hosts
        .iter()
        .any(|allowed| allowed.to_ascii_lowercase() == host_lower)
    {
        return Err(UnifiedError::bad_request(
            "Provider URL host is not on the provider allowlist",
        ));
    }
    if !resolved_addresses_are_safe(&url) {
        return Err(UnifiedError::bad_request(
            "Provider URL host resolves to a private, link-local, or reserved address; fetch refused",
        ));
    }
    Ok(())
}

/// Build an HTTP client whose redirect policy re-validates every
/// redirect target with the same SSRF rules as the original URL
/// (same as the downloader's `build_client`).
pub(crate) fn build_fetch_client(allowed_hosts: Arc<Vec<String>>) -> UnifiedResult<reqwest::Client> {
    let redirect = reqwest::redirect::Policy::custom(move |attempt| {
        if attempt.previous().len() as u32 >= MAX_REDIRECTS {
            return attempt.stop();
        }
        match validate_provider_url(attempt.url().as_str(), &allowed_hosts) {
            Ok(()) => attempt.follow(),
            Err(_) => attempt.stop(),
        }
    });
    reqwest::Client::builder()
        .user_agent(USER_AGENT)
        .timeout(Duration::from_secs(DEFAULT_FETCH_TIMEOUT_SECS))
        .connect_timeout(Duration::from_secs(10))
        .redirect(redirect)
        .tcp_nodelay(true)
        .build()
        .map_err(|error| UnifiedError::internal(format!("Cannot build HTTP client: {error}")))
}

// Validation helpers.

/// Normalize an ISO 3166-1 alpha-2 code: trim, upper-case, exactly
/// two ASCII letters.
pub(crate) fn normalize_country_code(raw: &str) -> UnifiedResult<String> {
    let code = raw.trim().to_ascii_uppercase();
    if code.len() != 2 || !code.bytes().all(|byte| byte.is_ascii_alphabetic()) {
        return Err(UnifiedError::bad_request(format!(
            "Country code must be an ISO 3166-1 alpha-2 code such as \"ID\", got \"{raw}\""
        )));
    }
    Ok(code)
}

/// Parse a YYYY-MM-DD date.
pub(crate) fn parse_date(raw: &str) -> UnifiedResult<NaiveDate> {
    NaiveDate::parse_from_str(raw.trim(), "%Y-%m-%d").map_err(|_| {
        UnifiedError::bad_request(format!("Date must be YYYY-MM-DD, got \"{raw}\""))
    })
}

/// Validate a provider id slug: lower-case letters, digits, hyphens.
pub(crate) fn validate_provider_id(raw: &str) -> UnifiedResult<String> {
    let id = raw.trim().to_ascii_lowercase();
    if id.is_empty()
        || id.len() > 64
        || !id
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
    {
        return Err(UnifiedError::bad_request(
            "Provider id must be 1-64 characters of lower-case letters, digits, or hyphens",
        ));
    }
    Ok(id)
}

/// Validate a schedule expression: either an interval in seconds
/// (a plain number, 60..=2592000) or five cron-like fields where
/// each field is `*`, `*/N`, or a plain number. The scheduler owns
/// the real interpretation; this only rejects nonsense early.
pub(crate) fn validate_schedule(raw: &str) -> UnifiedResult<String> {
    let schedule = raw.trim().to_string();
    if schedule.is_empty() {
        return Err(UnifiedError::bad_request("Schedule must not be empty"));
    }
    if let Ok(seconds) = schedule.parse::<u64>() {
        if (60..=2_592_000).contains(&seconds) {
            return Ok(schedule);
        }
        return Err(UnifiedError::bad_request(
            "Interval schedule must be between 60 and 2592000 seconds",
        ));
    }
    let fields: Vec<&str> = schedule.split_whitespace().collect();
    if fields.len() != 5 {
        return Err(UnifiedError::bad_request(
            "Schedule must be an interval in seconds or five cron-like fields (minute hour day month weekday)",
        ));
    }
    for field in fields {
        if field == "*" {
            continue;
        }
        if let Some(step) = field.strip_prefix("*/") {
            if step.parse::<u32>().is_ok_and(|value| value >= 1) {
                continue;
            }
            return Err(UnifiedError::bad_request(format!(
                "Invalid schedule step \"{field}\""
            )));
        }
        if field.parse::<u32>().is_err() {
            return Err(UnifiedError::bad_request(format!(
                "Invalid schedule field \"{field}\""
            )));
        }
    }
    Ok(schedule)
}

/// Validate a list of years: 1..=MAX_YEARS_PER_CONFIG entries, each
/// within MIN_SYNC_YEAR..=MAX_SYNC_YEAR, de-duplicated, sorted.
pub(crate) fn validate_years(years: &[i32]) -> UnifiedResult<Vec<i32>> {
    if years.is_empty() || years.len() > MAX_YEARS_PER_CONFIG {
        return Err(UnifiedError::bad_request(format!(
            "Years must list between 1 and {MAX_YEARS_PER_CONFIG} years"
        )));
    }
    let mut unique: Vec<i32> = Vec::new();
    for year in years {
        if !(MIN_SYNC_YEAR..=MAX_SYNC_YEAR).contains(year) {
            return Err(UnifiedError::bad_request(format!(
                "Year must be between {MIN_SYNC_YEAR} and {MAX_SYNC_YEAR}, got {year}"
            )));
        }
        if !unique.contains(year) {
            unique.push(*year);
        }
    }
    unique.sort_unstable();
    Ok(unique)
}

/// Default years for a new config: the current year and the next
/// DEFAULT_YEARS_AHEAD years.
pub(crate) fn default_years() -> Vec<i32> {
    let current = Utc::now().year();
    (current..=current + DEFAULT_YEARS_AHEAD).collect()
}

/// Validate a config's country list: non-empty, at most
/// MAX_COUNTRIES_PER_CONFIG, each a known, enabled country.
pub(crate) fn validate_config_countries(
    state: &HolidaysState,
    countries: &[String],
) -> UnifiedResult<Vec<String>> {
    if countries.is_empty() || countries.len() > MAX_COUNTRIES_PER_CONFIG {
        return Err(UnifiedError::bad_request(format!(
            "Countries must list between 1 and {MAX_COUNTRIES_PER_CONFIG} countries"
        )));
    }
    let mut unique: Vec<String> = Vec::new();
    for raw in countries {
        let code = normalize_country_code(raw)?;
        let Some(country) = state.countries.get(&code) else {
            return Err(UnifiedError::bad_request(format!(
                "Unknown country code \"{code}\""
            )));
        };
        if !country.enabled {
            return Err(UnifiedError::bad_request(format!(
                "Country \"{code}\" is disabled"
            )));
        }
        if !unique.contains(&code) {
            unique.push(code);
        }
    }
    unique.sort();
    Ok(unique)
}

/// Validate a provider reference on a config: the provider must
/// exist and be enabled.
pub(crate) fn load_enabled_provider(state: &HolidaysState, provider_id: &str) -> UnifiedResult<HolidayProvider> {
    let Some(provider) = state.providers.get(provider_id) else {
        return Err(UnifiedError::bad_request(format!(
            "Unknown provider \"{provider_id}\""
        )));
    };
    if !provider.enabled {
        return Err(UnifiedError::bad_request(format!(
            "Provider \"{provider_id}\" is disabled"
        )));
    }
    Ok(provider.clone())
}

/// Validate a fallback chain: at most MAX_FALLBACK_PROVIDERS ids,
/// each an existing enabled provider, distinct from the primary and
/// from each other.
pub(crate) fn validate_fallback_chain(
    state: &HolidaysState,
    primary_id: &str,
    fallback_ids: &[String],
) -> UnifiedResult<Vec<String>> {
    if fallback_ids.len() > MAX_FALLBACK_PROVIDERS {
        return Err(UnifiedError::bad_request(format!(
            "Fallback chain must list at most {MAX_FALLBACK_PROVIDERS} providers"
        )));
    }
    let mut unique: Vec<String> = Vec::new();
    for raw in fallback_ids {
        let id = raw.trim().to_string();
        if id == primary_id {
            return Err(UnifiedError::bad_request(
                "Fallback chain must not repeat the primary provider",
            ));
        }
        if unique.contains(&id) {
            return Err(UnifiedError::bad_request(format!(
                "Fallback chain repeats provider \"{id}\""
            )));
        }
        load_enabled_provider(state, &id)?;
        unique.push(id);
    }
    Ok(unique)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::modules::holidays::{
        DEFAULT_YEARS_AHEAD, MAX_SYNC_YEAR, MAX_YEARS_PER_CONFIG, MIN_SYNC_YEAR,
    };

    #[test]
    fn normalize_country_code_lowercase_and_trims() {
        assert_eq!(normalize_country_code("id").unwrap(), "ID");
        assert_eq!(normalize_country_code("  ID ").unwrap(), "ID");
    }

    #[test]
    fn normalize_country_code_rejects_wrong_length() {
        assert!(normalize_country_code("IDN").is_err());
        assert!(normalize_country_code("I").is_err());
        assert!(normalize_country_code("").is_err());
    }

    #[test]
    fn normalize_country_code_rejects_non_letters() {
        assert!(normalize_country_code("1D").is_err());
        assert!(normalize_country_code("I-").is_err());
    }

    #[test]
    fn parse_date_accepts_iso() {
        assert_eq!(
            parse_date("2026-12-25").unwrap(),
            NaiveDate::from_ymd_opt(2026, 12, 25).unwrap()
        );
        assert_eq!(
            parse_date(" 2026-01-01 ").unwrap(),
            NaiveDate::from_ymd_opt(2026, 1, 1).unwrap()
        );
    }

    #[test]
    fn parse_date_rejects_bad_month_and_format() {
        assert!(parse_date("2026-13-01").is_err());
        assert!(parse_date("25-12-2026").is_err());
        assert!(parse_date("2026/12/25").is_err());
    }

    #[test]
    fn is_forbidden_ip_rejects_ipv6_special_ranges() {
        assert!(is_forbidden_ip("2001:db8::1".parse().unwrap()));
        assert!(is_forbidden_ip("2001:2::1".parse().unwrap()));
        assert!(is_forbidden_ip("fec0::1".parse().unwrap()));
        assert!(is_forbidden_ip("fc00::1".parse().unwrap()));
        assert!(is_forbidden_ip("fe80::1".parse().unwrap()));
        assert!(is_forbidden_ip("::".parse().unwrap()));
        assert!(!is_forbidden_ip("2606:4700:4700::1111".parse().unwrap()));
        assert!(!is_forbidden_ip("2001:4860:4860::8888".parse().unwrap()));
    }

    #[test]
    fn is_forbidden_ip_rejects_ipv4_special_ranges() {
        assert!(is_forbidden_ip("192.0.2.1".parse().unwrap()));
        assert!(is_forbidden_ip("198.51.100.1".parse().unwrap()));
        assert!(is_forbidden_ip("203.0.113.1".parse().unwrap()));
        assert!(!is_forbidden_ip("8.8.8.8".parse().unwrap()));
    }

    #[test]
    fn validate_provider_id_lowercases() {
        assert_eq!(validate_provider_id("Nager-Date").unwrap(), "nager-date");
        assert_eq!(validate_provider_id("abc-123").unwrap(), "abc-123");
    }

    #[test]
    fn validate_provider_id_rejects_empty_and_spaces() {
        assert!(validate_provider_id("").is_err());
        assert!(validate_provider_id("has space").is_err());
        assert!(validate_provider_id("under_score").is_err());
        assert!(validate_provider_id(&"a".repeat(65)).is_err());
    }

    #[test]
    fn validate_years_sorts_and_dedupes() {
        assert_eq!(validate_years(&[2026, 2025]).unwrap(), vec![2025, 2026]);
        assert_eq!(validate_years(&[2026, 2026]).unwrap(), vec![2026]);
    }

    #[test]
    fn validate_years_rejects_empty_too_many_and_out_of_range() {
        assert!(validate_years(&[]).is_err());
        assert!(validate_years(&[2026, 2027, 2028, 2029, 2030, 2031]).is_err());
        assert!(validate_years(&[MIN_SYNC_YEAR - 1]).is_err());
        assert!(validate_years(&[MAX_SYNC_YEAR + 1]).is_err());
        assert_eq!(MAX_YEARS_PER_CONFIG, 5);
    }

    #[test]
    fn validate_schedule_accepts_interval_and_cron() {
        assert_eq!(validate_schedule("3600").unwrap(), "3600");
        assert_eq!(validate_schedule("0 9 * * 1").unwrap(), "0 9 * * 1");
        assert_eq!(validate_schedule("*/15 * * * *").unwrap(), "*/15 * * * *");
    }

    #[test]
    fn validate_schedule_rejects_bad_values() {
        assert!(validate_schedule("").is_err());
        assert!(validate_schedule("30").is_err());
        assert!(validate_schedule("0 9 * *").is_err());
        assert!(validate_schedule("*/0 * * * *").is_err());
        assert!(validate_schedule("nope").is_err());
    }

    #[test]
    fn default_years_covers_current_and_next() {
        let years = default_years();
        assert_eq!(years.len(), DEFAULT_YEARS_AHEAD as usize + 1);
        assert_eq!(years[0], Utc::now().year());
        assert_eq!(years[1], Utc::now().year() + 1);
    }
}
