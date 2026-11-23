//! SaaS Domains - hostname, target and label validation.
//!
//! Part of the `saas_domains` module split; see `src/modules/saas_domains.rs`.

use unified_core::error::UnifiedError;
use unified_core::error::UnifiedResult;
use regex::Regex;
use std::net::IpAddr;
use std::net::Ipv6Addr;
use std::sync::OnceLock;

use super::settings::SaasDomainsSettings;
use super::store::aliases_store;
use super::store::product_subdomains_store;

// ---------------------------------------------------------------------------
// Validation helpers.
// ---------------------------------------------------------------------------

/// TLDs that can never appear on the public internet: RFC 6761
/// special-use names plus suffixes commonly used for private
/// networks. A CNAME target ending in one of these is rejected
/// unless the target is itself a domain this system already
/// verified (a verified alias or a verified product subdomain),
/// which is the only case where such a target can legitimately
/// resolve here. This is a denylist, not a full public-suffix-list
/// check: the `publicsuffix` crate is not a dependency of this
/// project, so the list covers the special-use and private suffixes
/// named in the defect rather than every registrable suffix.
const NON_PUBLIC_TLDS: &[&str] = &[
    "local",
    "localhost",
    "internal",
    "intranet",
    "lan",
    "home",
    "corp",
    "invalid",
    "example",
    "test",
    "onion",
    "localdomain",
];

/// True when `domain` is a domain this system already verified: a
/// verified alias, or the host (or attached custom domain) of an
/// active, verified product subdomain.
fn is_verified_domain(domain: &str) -> bool {
    if aliases_store().iter().any(|entry| {
        let row = entry.value();
        row.domain == domain && row.status == "verified"
    }) {
        return true;
    }
    product_subdomains_store().iter().any(|entry| {
        let row = entry.value();
        row.verified_at.is_some()
            && row.status == "active"
            && (row.host == domain || row.custom_domain.as_deref() == Some(domain))
    })
}

/// True when an IPv6 address is in the documentation range
/// `2001:db8::/32` (RFC 3849). The standard library has no
/// `is_documentation` helper for IPv6, so the /32 prefix is
/// checked directly.
fn is_documentation_v6(ip: &Ipv6Addr) -> bool {
    let segments = ip.segments();
    segments[0] == 0x2001 && segments[1] == 0x0db8
}

fn domain_regex() -> &'static Regex {
    static REGEX: OnceLock<Regex> = OnceLock::new();
    REGEX.get_or_init(|| {
        Regex::new(r"^(?=.{1,253}$)[a-z0-9]([a-z0-9-]{0,61}[a-z0-9])?(\.[a-z0-9]([a-z0-9-]{0,61}[a-z0-9])?)+$")
            .expect("domain regex must compile")
    })
}

/// Validate a hostname: valid labels, at least one dot, alphabetic TLD.
pub(crate) fn validate_hostname(domain: &str) -> UnifiedResult<()> {
    let domain = domain.trim().to_lowercase();
    if !domain_regex().is_match(&domain) {
        return Err(UnifiedError::bad_request(format!(
            "Domain '{domain}' is not a valid hostname"
        )));
    }
    let tld = domain.rsplit('.').next().unwrap_or("");
    if tld.len() < 2 || !tld.chars().all(|c| c.is_ascii_alphabetic()) {
        return Err(UnifiedError::bad_request(format!(
            "Domain '{domain}' must end with an alphabetic top-level domain"
        )));
    }
    Ok(())
}

pub(crate) fn normalize_domain(raw: &str) -> UnifiedResult<String> {
    let domain = raw.trim().to_lowercase();
    validate_hostname(&domain)?;
    Ok(domain)
}

/// Validate an alias target value for its type. CNAME targets must
/// be valid hostnames whose TLD is a public suffix (or a domain
/// this system already verified); A targets must be valid public
/// IP literals — private, loopback, link-local, unspecified,
/// multicast, CGNAT shared-address space (`100.64.0.0/10`), and
/// IPv6 documentation addresses are rejected so an alias can never
/// point a tenant at internal infrastructure.
pub(crate) fn validate_target(target_type: &str, target_value: &str) -> UnifiedResult<String> {
    match target_type {
        "cname_subdomain" | "cname_server" => {
            let value = target_value.trim().to_lowercase();
            validate_hostname(&value)?;
            let tld = value.rsplit('.').next().unwrap_or("");
            if NON_PUBLIC_TLDS.contains(&tld) && !is_verified_domain(&value) {
                return Err(UnifiedError::bad_request(format!(
                    "CNAME target '{value}' ends in the non-public suffix '.{tld}'; use a public domain or a domain already verified here"
                )));
            }
            Ok(value)
        }
        "a_record" => {
            let value = target_value.trim().to_string();
            let address: IpAddr = value.parse().map_err(|_| {
                UnifiedError::bad_request(format!(
                    "A-record target '{value}' is not a valid IP address literal"
                ))
            })?;
            let rejected = match address {
                IpAddr::V4(ip) => {
                    ip.is_private()
                        || ip.is_loopback()
                        || ip.is_link_local()
                        || ip.is_unspecified()
                        || ip.is_multicast()
                        || ip.is_broadcast()
                        || unified_core::net::ipv4_is_shared(&ip)
                }
                IpAddr::V6(ip) => {
                    ip.is_loopback()
                        || ip.is_unspecified()
                        || ip.is_multicast()
                        || ip.is_unique_local()
                        || ip.is_unicast_link_local()
                        || is_documentation_v6(&ip)
                }
            };
            if rejected {
                return Err(UnifiedError::bad_request(format!(
                    "A-record target '{value}' is not a public IP address"
                )));
            }
            Ok(value)
        }
        other => Err(UnifiedError::bad_request(format!(
            "Unknown alias target type '{other}'"
        ))),
    }
}

/// Validate a subdomain label against the configured pattern and
/// the configured reserved-label blocklist.
pub(crate) fn validate_label(label_raw: &str, cfg: &SaasDomainsSettings) -> UnifiedResult<String> {
    let label = label_raw.trim().to_lowercase();
    if !cfg.label_regex().is_match(&label) {
        return Err(UnifiedError::bad_request(
            "Subdomain label must match the configured label pattern (1-63 characters, letters, digits, hyphens, never starting or ending with a hyphen)",
        ));
    }
    if cfg
        .reserved_labels
        .iter()
        .any(|blocked| blocked.trim().to_lowercase() == label)
    {
        return Err(UnifiedError::bad_request(format!(
            "Subdomain label '{label}' is reserved"
        )));
    }
    Ok(label)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use uuid::Uuid;

    #[test]
    fn hostname_validation() {
        assert!(validate_hostname("example.com").is_ok());
        assert!(validate_hostname("app.example.co.uk").is_ok());
        assert!(validate_hostname("EXAMPLE.COM").is_ok());
        assert!(validate_hostname("localhost").is_err());
        assert!(validate_hostname("bad_host.com").is_err());
        assert!(validate_hostname("").is_err());
        assert!(validate_hostname("example.c").is_err());
    }

    #[test]
    fn normalize_domain_trims_and_lowercases() {
        assert_eq!(normalize_domain("  Example.COM ").unwrap(), "example.com");
        assert!(normalize_domain("not a host").is_err());
    }

    #[test]
    fn alias_target_validation() {
        assert_eq!(
            validate_target("cname_subdomain", "App.Example.com").unwrap(),
            "app.example.com"
        );
        assert_eq!(validate_target("a_record", "8.8.8.8").unwrap(), "8.8.8.8");
        assert!(validate_target("a_record", "10.0.0.1").is_err());
        assert!(validate_target("a_record", "127.0.0.1").is_err());
        assert!(validate_target("a_record", "192.168.1.1").is_err());
        assert!(validate_target("a_record", "not-an-ip").is_err());
        assert!(validate_target("mx_record", "example.com").is_err());
    }

    #[test]
    fn a_record_rejects_cgnat_shared_space() {
        // 100.64.0.0/10 is CGNAT shared-address space, not public.
        assert!(validate_target("a_record", "100.64.0.1").is_err());
        assert!(validate_target("a_record", "100.127.255.255").is_err());
        // Just past the /10 boundary is public again.
        assert!(validate_target("a_record", "100.128.0.1").is_ok());
        assert!(validate_target("a_record", "100.63.255.255").is_ok());
    }

    #[test]
    fn a_record_rejects_ipv6_documentation_range() {
        assert!(validate_target("a_record", "2001:db8::1").is_err());
        assert!(validate_target("a_record", "2001:db8:85a3::8a2e:370:7334").is_err());
        assert!(validate_target("a_record", "2606:4700:4700::1111").is_ok());
        assert!(validate_target("a_record", "2001:4860:4860::8888").is_ok());
    }

    #[test]
    fn cname_target_rejects_non_public_suffixes() {
        assert!(validate_target("cname_subdomain", "app.local").is_err());
        assert!(validate_target("cname_subdomain", "app.internal").is_err());
        assert!(validate_target("cname_subdomain", "app.example").is_err());
        assert!(validate_target("cname_subdomain", "app.test").is_err());
        assert!(validate_target("cname_subdomain", "app.lan").is_err());
        assert!(validate_target("cname_server", "edge.localhost").is_err());
        assert!(validate_target("cname_subdomain", "app.example.com").is_ok());
        assert!(validate_target("cname_subdomain", "app.example.co.uk").is_ok());
    }

    #[test]
    fn cname_target_allows_already_verified_domain() {
        // An already-verified alias domain is the escape hatch for a
        // non-public suffix: the target is known to resolve here.
        let now = Utc::now();
        let alias = crate::modules::saas_domains::types::DomainAlias {
            id: Uuid::now_v7(),
            domain: "edge.local".to_string(),
            company_id: Uuid::now_v7(),
            target_type: "cname_subdomain".to_string(),
            target_value: "app.example.com".to_string(),
            status: "verified".to_string(),
            site_id: None,
            verified_at: Some(now),
            created_at: now,
            updated_at: now,
        };
        aliases_store().insert(alias.id, alias);
        assert_eq!(
            validate_target("cname_subdomain", "edge.local").unwrap(),
            "edge.local"
        );
        // An unverified alias with the same suffix is no escape hatch.
        aliases_store().remove(&alias.id);
        assert!(validate_target("cname_subdomain", "edge.local").is_err());
    }

    #[test]
    fn label_validation_uses_settings() {
        let cfg = SaasDomainsSettings::default();
        assert_eq!(validate_label("my-app", &cfg).unwrap(), "my-app");
        assert_eq!(validate_label("  Shop1 ", &cfg).unwrap(), "shop1");
        assert!(validate_label("www", &cfg).is_err());
        assert!(validate_label("admin", &cfg).is_err());
        assert!(validate_label("-bad", &cfg).is_err());
        assert!(validate_label("bad-", &cfg).is_err());
        assert!(validate_label("bad_label", &cfg).is_err());
        assert!(validate_label("", &cfg).is_err());
    }
}
