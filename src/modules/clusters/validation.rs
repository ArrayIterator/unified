// clusters — Validation helpers and small pure utilities.

use super::*;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::pagination::{PageResult, PaginationParams};
use actix_web::HttpRequest;
use serde::Serialize;
use std::collections::HashSet;
use std::sync::Arc;

pub(crate) fn require_non_empty(value: &str, field: &str) -> UnifiedResult<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err(UnifiedError::bad_request(format!("{field} is required")));
    }
    Ok(trimmed.to_string())
}

pub(crate) fn validate_base_url(value: &str) -> UnifiedResult<String> {
    let url = require_non_empty(value, "base_url")?;
    if url.len() > 512 {
        return Err(UnifiedError::bad_request("base_url is too long"));
    }
    if url.contains(char::is_whitespace) {
        return Err(UnifiedError::bad_request("base_url must not contain whitespace"));
    }
    if url.starts_with("https://") {
        // Fine: sync payloads (including peer secrets) travel encrypted.
    } else if url.starts_with("http://") {
        // Plain HTTP is only acceptable for a loopback peer (local
        // development or tests); anywhere else the payloads would
        // cross the network in cleartext.
        if !is_loopback_http(&url) {
            return Err(UnifiedError::bad_request(
                "base_url must use https://; plain http:// is only allowed for loopback hosts (localhost, 127.0.0.1, ::1)",
            ));
        }
    } else {
        return Err(UnifiedError::bad_request(
            "base_url must start with http:// or https://",
        ));
    }
    Ok(url.trim_end_matches('/').to_string())
}

/// Whether an `http://` URL points at a loopback host, the only case
/// where cleartext node traffic is acceptable.
fn is_loopback_http(url: &str) -> bool {
    let Some(rest) = url.strip_prefix("http://") else {
        return false;
    };
    let authority = rest.split('/').next().unwrap_or("");
    // Strip an optional `:port`, keeping IPv6 bracketed hosts intact.
    let host = if let Some(bracketed) = authority.strip_prefix('[') {
        match bracketed.split(']').next() {
            Some(host) => format!("[{host}]"),
            None => return false,
        }
    } else {
        authority.split(':').next().unwrap_or("").to_string()
    };
    matches!(host.as_str(), "localhost" | "127.0.0.1" | "[::1]")
}

/// Validate a sync domain against the registered domain registry
/// (admin-editable; seeded with `DEFAULT_SYNC_DOMAINS`).
pub(crate) fn validate_domain(state: &Arc<ClustersState>, domain: &str) -> UnifiedResult<String> {
    let domain = domain.trim().to_string();
    let known: Vec<String> = {
        let mut known: Vec<String> = state
            .domains
            .read()
            .expect("domains lock poisoned")
            .iter()
            .cloned()
            .collect();
        known.sort();
        known
    };
    if !known.iter().any(|known| known == &domain) {
        return Err(UnifiedError::bad_request(format!(
            "Unknown sync domain: {domain}. Known domains: {}",
            known.join(", ")
        )));
    }
    Ok(domain)
}

pub(crate) fn validate_domains(
    state: &Arc<ClustersState>,
    domains: Vec<String>,
) -> UnifiedResult<Vec<String>> {
    if domains.is_empty() {
        return Err(UnifiedError::bad_request("At least one sync domain is required"));
    }
    let mut seen: HashSet<String> = HashSet::new();
    let mut out: Vec<String> = Vec::new();
    for domain in domains {
        let domain = validate_domain(state, &domain)?;
        if seen.insert(domain.clone()) {
            out.push(domain);
        }
    }
    Ok(out)
}

/// A schedule is either an interval in seconds (greater than zero)
/// or five cron-like fields where each field is `*`, `*/N`, or a plain
/// number inside that field's real range.
pub(crate) fn valid_schedule(schedule: &str) -> bool {
    let schedule = schedule.trim();
    if schedule.is_empty() {
        return false;
    }
    if let Ok(seconds) = schedule.parse::<u64>() {
        return seconds > 0;
    }
    let fields: Vec<&str> = schedule.split_whitespace().collect();
    if fields.len() != 5 {
        return false;
    }
    // Real cron ranges: minute 0-59, hour 0-23, day 1-31, month 1-12,
    // weekday 0-7 (both 0 and 7 are Sunday).
    const MIN: [u32; 5] = [0, 0, 1, 1, 0];
    const MAX: [u32; 5] = [59, 23, 31, 12, 7];
    fields.iter().enumerate().all(|(index, field)| {
        if *field == "*" {
            return true;
        }
        if let Some(step) = field.strip_prefix("*/") {
            return step
                .parse::<u32>()
                .is_ok_and(|n| (1..=MAX[index]).contains(&n));
        }
        field
            .parse::<u32>()
            .is_ok_and(|n| (MIN[index]..=MAX[index]).contains(&n))
    })
}

pub(crate) fn paginate<T: Serialize + Clone>(
    items: Vec<T>,
    params: &PaginationParams,
) -> PageResult<T> {
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

/// Read and validate the idempotency key from the request headers.
/// Returns `Ok(None)` when no key was sent (the call runs normally),
/// `Ok(Some(key))` for a usable key, and an error for a malformed one.
pub(crate) fn idempotency_key_from(req: &HttpRequest) -> UnifiedResult<Option<String>> {
    let raw = req
        .headers()
        .get(HEADER_IDEMPOTENCY_KEY)
        .or_else(|| req.headers().get(HEADER_IDEMPOTENCY_KEY_ALT))
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let Some(key) = raw else {
        return Ok(None);
    };
    if key.len() > MAX_IDEMPOTENCY_KEY_LEN {
        return Err(UnifiedError::bad_request(
            "Idempotency-Key is too long (maximum 128 characters)",
        ));
    }
    if key.contains(char::is_whitespace) {
        return Err(UnifiedError::bad_request(
            "Idempotency-Key must not contain whitespace",
        ));
    }
    Ok(Some(key.to_string()))
}

pub(crate) fn valid_domain_name(domain: &str) -> bool {
    let domain = domain.trim();
    !domain.is_empty()
        && domain.len() <= 64
        && domain
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn require_non_empty_trims() {
        assert_eq!(require_non_empty("  x  ", "field").unwrap(), "x");
        assert!(require_non_empty("", "field").is_err());
        assert!(require_non_empty("   ", "field").is_err());
    }

    #[test]
    fn validate_base_url_shape() {
        assert_eq!(
            validate_base_url("https://a.example.com/").unwrap(),
            "https://a.example.com"
        );
        // Plain HTTP is rejected for non-loopback hosts: sync payloads
        // carry peer secrets and must not travel in cleartext.
        assert!(validate_base_url("http://10.0.0.1:8080").is_err());
        assert!(validate_base_url("http://a.example.com").is_err());
        // Loopback stays allowed for local development and tests.
        assert_eq!(
            validate_base_url("http://localhost:8080/").unwrap(),
            "http://localhost:8080"
        );
        assert_eq!(
            validate_base_url("http://127.0.0.1:8080").unwrap(),
            "http://127.0.0.1:8080"
        );
        assert_eq!(
            validate_base_url("http://[::1]:8080").unwrap(),
            "http://[::1]:8080"
        );
        assert!(validate_base_url("ftp://a.example.com").is_err());
        assert!(validate_base_url("https://a b.example.com").is_err());
        assert!(validate_base_url("").is_err());
        assert!(validate_base_url(&format!("https://{}", "a".repeat(510))).is_err());
    }

    #[test]
    fn valid_schedule_forms() {
        assert!(valid_schedule("60"));
        assert!(valid_schedule("3600"));
        // An interval of zero seconds would schedule a run that never
        // waits; it is not a valid schedule.
        assert!(!valid_schedule("0"));
        assert!(valid_schedule("* * * * *"));
        assert!(valid_schedule("*/5 * * * *"));
        assert!(valid_schedule("0 2 * * *"));
        assert!(valid_schedule("30 4 1 1 0"));
        assert!(valid_schedule("0 0 31 12 7"));
        assert!(valid_schedule("*/30 * * * *"));
        assert!(!valid_schedule(""));
        assert!(!valid_schedule("1 2 3"));
        assert!(!valid_schedule("*/0 * * * *"));
        assert!(!valid_schedule("a b c d e"));
        // Every cron field is checked against its real range.
        assert!(!valid_schedule("99 99 99 99 99"));
        assert!(!valid_schedule("60 * * * *"));
        assert!(!valid_schedule("* 24 * * *"));
        assert!(!valid_schedule("* * 0 * *"));
        assert!(!valid_schedule("* * 32 * *"));
        assert!(!valid_schedule("* * * 0 *"));
        assert!(!valid_schedule("* * * 13 *"));
        assert!(!valid_schedule("* * * * 8"));
        assert!(!valid_schedule("*/61 * * * *"));
        assert!(!valid_schedule("*/0 * * * *"));
    }

    #[test]
    fn valid_domain_name_rules() {
        assert!(valid_domain_name("config"));
        assert!(valid_domain_name("my_domain1"));
        assert!(!valid_domain_name(""));
        assert!(!valid_domain_name("Config"));
        assert!(!valid_domain_name("my-domain"));
        assert!(!valid_domain_name("has space"));
        assert!(!valid_domain_name(&"a".repeat(65)));
        assert!(valid_domain_name(&"a".repeat(64)));
    }

    #[test]
    fn validate_domain_against_registry() {
        let state = Arc::new(ClustersState::new());
        assert_eq!(validate_domain(&state, "config").unwrap(), "config");
        assert_eq!(validate_domain(&state, " modules ").unwrap(), "modules");
        assert!(validate_domain(&state, "nope").is_err());
        assert!(validate_domain(&state, "").is_err());
    }

    #[test]
    fn validate_domains_dedupes_and_requires_one() {
        let state = Arc::new(ClustersState::new());
        assert_eq!(
            validate_domains(&state, vec!["config".to_string(), "config".to_string()])
                .unwrap(),
            vec!["config".to_string()]
        );
        assert!(validate_domains(&state, Vec::new()).is_err());
        assert!(validate_domains(&state, vec!["nope".to_string()]).is_err());
    }
}
