// load_balancer -- Name, domain, weight and health-check validation.

use unified_core::config::AppConfig;
use unified_core::error::{UnifiedError, UnifiedResult};

use super::constants::*;
use super::types::*;

// Validation.

/// Split a `host:port` (or `[ipv6]:port`) value. Returns
/// `(host, port)` or `None` when the value is malformed.
pub fn parse_host_port(value: &str) -> Option<(String, u16)> {
    let value = value.trim();
    if value.is_empty() {
        return None;
    }
    if let Some(rest) = value.strip_prefix('[') {
        let (host, port) = rest.split_once("]:")?;
        if host.is_empty() || port.is_empty() {
            return None;
        }
        let port: u16 = port.parse().ok()?;
        if port == 0 {
            return None;
        }
        Some((host.to_string(), port))
    } else {
        let (host, port) = value.rsplit_once(':')?;
        if host.is_empty() || host.contains(':') || port.is_empty() {
            return None;
        }
        let port: u16 = port.parse().ok()?;
        if port == 0 {
            return None;
        }
        Some((host.to_string(), port))
    }
}

pub(crate) fn validate_host_port(value: &str, field: &str) -> UnifiedResult<()> {
    let (host, _port) = parse_host_port(value).ok_or_else(|| {
        UnifiedError::bad_request(format!(
            "{field} must be a valid \"host:port\" address, got \"{value}\""
        ))
    })?;
    if host.chars().any(|c| {
        c.is_whitespace() || c == '/' || c == '?' || c == '@' || c == '#' || c == '[' || c == ']'
    }) {
        return Err(UnifiedError::bad_request(format!(
            "{field} host contains an invalid character: \"{value}\""
        )));
    }
    Ok(())
}

pub(crate) fn validate_name(name: &str, field: &str) -> UnifiedResult<String> {
    let name = name.trim().to_string();
    if name.is_empty() {
        return Err(UnifiedError::bad_request(format!(
            "{field} must not be empty"
        )));
    }
    if name.len() > 200 {
        return Err(UnifiedError::bad_request(format!(
            "{field} must be at most 200 characters"
        )));
    }
    Ok(name)
}

pub(crate) fn validate_domain(domain: &str) -> UnifiedResult<String> {
    let domain = domain.trim().trim_end_matches('.').to_lowercase();
    if domain.is_empty() || domain.len() > 253 {
        return Err(UnifiedError::bad_request(
            "Domain must not be empty and must be at most 253 characters",
        ));
    }
    // A wildcard is allowed only as the entire left-most label
    // (`*.example.com`). A bare `*`, or `*` anywhere else in a
    // label (`*example.com`, `exam*ple.com`, `example.*.com`),
    // is not a domain.
    let labels: Vec<&str> = domain.split('.').collect();
    for (index, label) in labels.iter().enumerate() {
        if label.is_empty() || label.len() > 63 {
            return Err(UnifiedError::bad_request(format!(
                "Invalid domain name \"{domain}\""
            )));
        }
        if *label == "*" {
            if index != 0 || labels.len() < 2 {
                return Err(UnifiedError::bad_request(format!(
                    "Invalid domain name \"{domain}\""
                )));
            }
            continue;
        }
        if label.contains('*')
            || label.starts_with('-')
            || label.ends_with('-')
            || !label
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-')
        {
            return Err(UnifiedError::bad_request(format!(
                "Invalid domain name \"{domain}\""
            )));
        }
    }
    Ok(domain)
}

pub(crate) fn validate_health_check(config: &HealthCheckConfig) -> UnifiedResult<()> {
    if matches!(
        config.kind,
        HealthCheckKind::Http | HealthCheckKind::Https
    ) {
        if !config.path.starts_with('/') {
            return Err(UnifiedError::bad_request(
                "Health check path must start with \"/\"",
            ));
        }
        if config.path.len() > 512 {
            return Err(UnifiedError::bad_request(
                "Health check path must be at most 512 characters",
            ));
        }
    }
    if !(MIN_HEALTH_INTERVAL_SECS..=MAX_HEALTH_INTERVAL_SECS).contains(&config.interval_secs) {
        return Err(UnifiedError::bad_request(format!(
            "Health check interval must be between {MIN_HEALTH_INTERVAL_SECS} and {MAX_HEALTH_INTERVAL_SECS} seconds"
        )));
    }
    if config.timeout_secs < 1 || config.timeout_secs > MAX_HEALTH_TIMEOUT_SECS {
        return Err(UnifiedError::bad_request(format!(
            "Health check timeout must be between 1 and {MAX_HEALTH_TIMEOUT_SECS} seconds"
        )));
    }
    if config.timeout_secs >= config.interval_secs {
        return Err(UnifiedError::bad_request(
            "Health check timeout must be smaller than the check interval",
        ));
    }
    if config.healthy_threshold < 1 || config.healthy_threshold > MAX_THRESHOLD {
        return Err(UnifiedError::bad_request(format!(
            "Healthy threshold must be between 1 and {MAX_THRESHOLD}"
        )));
    }
    if config.unhealthy_threshold < 1 || config.unhealthy_threshold > MAX_THRESHOLD {
        return Err(UnifiedError::bad_request(format!(
            "Unhealthy threshold must be between 1 and {MAX_THRESHOLD}"
        )));
    }
    if let Some(status) = config.expected_status {
        if !(100..=599).contains(&status) {
            return Err(UnifiedError::bad_request(
                "Expected HTTP status must be between 100 and 599",
            ));
        }
    }
    Ok(())
}

pub(crate) fn validate_weight(weight: u32) -> UnifiedResult<u32> {
    if !(1..=MAX_WEIGHT).contains(&weight) {
        return Err(UnifiedError::bad_request(format!(
            "Backend weight must be between 1 and {MAX_WEIGHT}"
        )));
    }
    Ok(weight)
}

/// A balancer with TLS termination must reference a domain that has
/// a certificate, unless automatic certificate creation is enabled
/// (then the storage creates a fallback on first handshake).
pub(crate) fn validate_tls_reference(
    config: &AppConfig,
    tls_mode: TlsMode,
    domain: &Option<String>,
) -> UnifiedResult<()> {
    if tls_mode != TlsMode::Terminate {
        return Ok(());
    }
    let Some(domain) = domain.as_deref() else {
        return Err(UnifiedError::bad_request(
            "TLS mode \"terminate\" requires a domain",
        ));
    };
    if config.app.ssl.domains.contains_key(domain) || config.app.ssl.auto_create {
        return Ok(());
    }
    Err(UnifiedError::unprocessable(format!(
        "No certificate is configured for domain \"{domain}\"; add it to app.ssl.domains or enable app.ssl.auto_create"
    )))
}

// Tests for the pure validation helpers in this file.
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_host_port_handles_ipv4_ipv6_and_invalid() {
        assert_eq!(
            parse_host_port("127.0.0.1:8080"),
            Some(("127.0.0.1".to_string(), 8080))
        );
        assert_eq!(parse_host_port("[::1]:443"), Some(("::1".to_string(), 443)));
        assert_eq!(parse_host_port(""), None);
        assert_eq!(parse_host_port("host:0"), None);
        assert_eq!(parse_host_port("no-port"), None);
        assert_eq!(parse_host_port("example.com:65536"), None);
        assert_eq!(parse_host_port("a:b:c"), None);
    }

    #[test]
    fn weight_enforces_bounds() {
        assert!(validate_weight(0).is_err());
        assert_eq!(validate_weight(1).unwrap(), 1);
        assert_eq!(validate_weight(100).unwrap(), 100);
        assert!(validate_weight(101).is_err());
    }

    #[test]
    fn name_is_trimmed_and_length_limited() {
        assert_eq!(validate_name("  ok  ", "name").unwrap(), "ok");
        assert!(validate_name("", "name").is_err());
        assert!(validate_name(&"x".repeat(201), "name").is_err());
    }

    #[test]
    fn domain_is_normalized() {
        assert_eq!(validate_domain("Example.COM.").unwrap(), "example.com");
        assert!(validate_domain("").is_err());
        assert!(validate_domain("-bad.com").is_err());
        assert!(validate_domain("a..b").is_err());
        assert!(validate_domain(".example.com").is_err());
    }

    #[test]
    fn host_port_value_must_be_host_colon_port() {
        assert!(validate_host_port("127.0.0.1:8080", "endpoint").is_ok());
        assert!(validate_host_port("no-port", "endpoint").is_err());
        assert!(validate_host_port("a b:8080", "endpoint").is_err());
    }

    #[test]
    fn health_check_default_is_valid() {
        assert!(validate_health_check(&HealthCheckConfig::default()).is_ok());
    }

    #[test]
    fn health_check_rejects_bad_values() {
        let mut config = HealthCheckConfig::default();
        config.path = "health".to_string();
        assert!(validate_health_check(&config).is_err());

        let mut config = HealthCheckConfig::default();
        config.interval_secs = 0;
        assert!(validate_health_check(&config).is_err());

        let mut config = HealthCheckConfig::default();
        config.timeout_secs = config.interval_secs;
        assert!(validate_health_check(&config).is_err());

        let mut config = HealthCheckConfig::default();
        config.healthy_threshold = 0;
        assert!(validate_health_check(&config).is_err());
    }

    #[test]
    fn domain_allows_only_leftmost_wildcard_label() {
        assert_eq!(
            validate_domain("*.example.com").unwrap(),
            "*.example.com"
        );
        assert_eq!(validate_domain("example.com").unwrap(), "example.com");
        // Bare `*` is not a domain, and `*` anywhere else is rejected.
        assert!(validate_domain("*").is_err());
        assert!(validate_domain("*example.com").is_err());
        assert!(validate_domain("exam*ple.com").is_err());
        assert!(validate_domain("example.*.com").is_err());
        assert!(validate_domain("*.example.*").is_err());
    }

    #[test]
    fn domain_validates_each_label() {
        assert!(validate_domain("exa_mple.com").is_err());
        assert!(validate_domain("example-.com").is_err());
        assert!(validate_domain("example.-com").is_err());
        assert!(validate_domain(&format!("{}.com", "a".repeat(64))).is_err());
        assert_eq!(
            validate_domain(&format!("{}.com", "a".repeat(63))).unwrap(),
            format!("{}.com", "a".repeat(63))
        );
        assert!(validate_domain("ex ample.com").is_err());
    }

    #[test]
    fn host_port_rejects_userinfo_and_url_characters() {
        assert!(validate_host_port("user@host:8080", "endpoint").is_err());
        assert!(validate_host_port("user:pass@host:8080", "endpoint").is_err());
        assert!(validate_host_port("host:8080/path", "endpoint").is_err());
        assert!(validate_host_port("host:8080?x=1", "endpoint").is_err());
        assert!(validate_host_port("host#frag:8080", "endpoint").is_err());
        assert!(validate_host_port("127.0.0.1:8080", "endpoint").is_ok());
        assert!(validate_host_port("[::1]:8080", "endpoint").is_ok());
    }

    #[test]
    fn health_check_https_requires_leading_slash_path() {
        let mut config = HealthCheckConfig::default();
        config.kind = HealthCheckKind::Https;
        assert!(validate_health_check(&config).is_ok());
        config.path = "health".to_string();
        assert!(validate_health_check(&config).is_err());
    }

    #[test]
    fn tls_reference_requires_certificate_for_termination() {
        let config = AppConfig::default();
        assert!(validate_tls_reference(&config, TlsMode::Http, &None).is_ok());
        assert!(validate_tls_reference(&config, TlsMode::Terminate, &None).is_err());
        assert!(
            validate_tls_reference(
                &config,
                TlsMode::Terminate,
                &Some("example.com".to_string())
            )
            .is_err()
        );
    }
}
