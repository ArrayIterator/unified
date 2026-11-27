//! Cloudflare connector module. - DNS validation.
//!
//! Split from `src/modules/cloudflare.rs`; behaviour unchanged.

use super::accounts_zones::*;
use super::cache_handlers::*;
use super::client::*;
use super::constants::*;
use super::dns_handlers::*;
use super::helpers::*;
use super::module::*;
use super::requests::*;
use super::state::*;
use super::status_handlers::*;

use unified_core::error::{UnifiedError, UnifiedResult};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

/// Validate one DNS label: 1-63 characters, letters, digits, hyphens,
/// and a leading underscore for service labels (`_sip`, `_dmarc`).
/// Labels must not start or end with a hyphen.
pub(super) fn valid_dns_label(label: &str) -> bool {
    if label.is_empty() || label.len() > 63 {
        return false;
    }
    let body = label.strip_prefix('_').unwrap_or(label);
    if body.is_empty() {
        return false;
    }
    if body.starts_with('-') || body.ends_with('-') {
        return false;
    }
    body.chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-')
}

/// Validate a DNS name: `@` (zone apex), `*` (wildcard), a leading
/// `*.` wildcard label (for example `*.example.com`, which Cloudflare
/// accepts), or a dotted list of labels, at most 253 characters in
/// total. A wildcard is only allowed as the whole first label; a
/// `*` anywhere else (`foo.*.example.com`, `*foo.example.com`) is
/// rejected.
pub(super) fn valid_dns_name(name: &str) -> bool {
    let name = name.trim().trim_end_matches('.');
    if name == "@" || name == "*" {
        return true;
    }
    if name.is_empty() || name.len() > 253 {
        return false;
    }
    let mut labels = name.split('.');
    if labels.next() == Some("*") {
        // Leading wildcard: at least one real label must follow, and
        // every remaining label must be a valid DNS label.
        let rest: Vec<&str> = labels.collect();
        return !rest.is_empty() && rest.iter().all(|label| valid_dns_label(label));
    }
    name.split('.').all(valid_dns_label)
}

/// Validate the content of a DNS record for its type, and return a
/// normalized content string (trimmed). `priority` is the record's
/// priority field, used by MX and SRV.
pub(super) fn validate_record_content(
    record_type: &str,
    content: &str,
    priority: Option<u16>,
) -> UnifiedResult<String> {
    let content = content.trim();
    if content.is_empty() {
        return Err(UnifiedError::bad_request("content must not be empty"));
    }
    match record_type {
        "A" => {
            let address: Ipv4Addr = content.parse().map_err(|_| {
                UnifiedError::bad_request(format!("\"{content}\" is not a valid IPv4 address"))
            })?;
            if address.is_unspecified() || address.is_multicast() {
                return Err(UnifiedError::bad_request(
                    "A record content must be a usable unicast IPv4 address",
                ));
            }
        }
        "AAAA" => {
            let address: Ipv6Addr = content.parse().map_err(|_| {
                UnifiedError::bad_request(format!("\"{content}\" is not a valid IPv6 address"))
            })?;
            let ip = IpAddr::V6(address);
            if ip.is_unspecified() || ip.is_multicast() {
                return Err(UnifiedError::bad_request(
                    "AAAA record content must be a usable unicast IPv6 address",
                ));
            }
        }
        "CNAME" | "MX" => {
            if !valid_dns_name(content) {
                return Err(UnifiedError::bad_request(format!(
                    "\"{content}\" is not a valid hostname"
                )));
            }
            if content.parse::<IpAddr>().is_ok() {
                return Err(UnifiedError::bad_request(format!(
                    "{record_type} record content must be a hostname, not an IP address"
                )));
            }
            if record_type == "MX" && priority.is_none() {
                return Err(UnifiedError::bad_request(
                    "MX records require a priority (0-65535)",
                ));
            }
        }
        "TXT" => {
            if content.chars().count() > 65_535 {
                return Err(UnifiedError::bad_request(
                    "TXT record content must be at most 65535 characters",
                ));
            }
        }
        "SRV" => {
            // Content form: `priority weight port target`.
            let parts: Vec<&str> = content.split_whitespace().collect();
            if parts.len() != 4 {
                return Err(UnifiedError::bad_request(
                    "SRV record content must be \"priority weight port target\"",
                ));
            }
            for (index, part) in parts.iter().take(3).enumerate() {
                if part.parse::<u16>().is_err() {
                    let field = ["priority", "weight", "port"][index];
                    return Err(UnifiedError::bad_request(format!(
                        "SRV record {field} \"{part}\" is not a number between 0 and 65535"
                    )));
                }
            }
            if !valid_dns_name(parts[3]) {
                return Err(UnifiedError::bad_request(format!(
                    "SRV record target \"{}\" is not a valid hostname",
                    parts[3]
                )));
            }
        }
        "CAA" => {
            // Content form: `flags tag "value"`.
            let mut parts = content.splitn(3, ' ');
            let flags = parts.next().unwrap_or("");
            let tag = parts.next().unwrap_or("");
            let value = parts.next().unwrap_or("");
            let flags_ok = flags.parse::<u16>().is_ok_and(|flags| flags <= 255);
            let tag_ok = matches!(tag, "issue" | "issuewild" | "issuemail" | "iodef");
            let value_ok = value.len() >= 2
                && value.starts_with('"')
                && value.ends_with('"');
            if !flags_ok || !tag_ok || !value_ok {
                return Err(UnifiedError::bad_request(
                    "CAA record content must be `flags tag \"value\"` with flags 0-255 and tag issue, issuewild, issuemail, or iodef",
                ));
            }
        }
        _ => {
            return Err(UnifiedError::bad_request(format!(
                "Unsupported record type \"{record_type}\""
            )));
        }
    }
    Ok(content.to_string())
}

/// Validate a TTL: 1 means automatic, otherwise 60-86400 seconds.
pub(super) fn validate_ttl(ttl: u32) -> UnifiedResult<u32> {
    if ttl == 1 || (60..=86_400).contains(&ttl) {
        Ok(ttl)
    } else {
        Err(UnifiedError::bad_request(
            "TTL must be 1 (automatic) or between 60 and 86400 seconds",
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn valid_dns_label_rules() {
        assert!(valid_dns_label("www"));
        assert!(valid_dns_label("a-b"));
        assert!(valid_dns_label("_dmarc"));
        assert!(valid_dns_label("_sip"));
        assert!(!valid_dns_label(""));
        assert!(!valid_dns_label("_"));
        assert!(!valid_dns_label("-bad"));
        assert!(!valid_dns_label("bad-"));
        assert!(!valid_dns_label("a_b"));
        assert!(!valid_dns_label("has space"));
        assert!(!valid_dns_label(&"a".repeat(64)));
        assert!(valid_dns_label(&"a".repeat(63)));
    }

    #[test]
    fn valid_dns_name_rules() {
        assert!(valid_dns_name("@"));
        assert!(valid_dns_name("*"));
        assert!(valid_dns_name("example.com"));
        assert!(valid_dns_name("sub.example.com."));
        assert!(valid_dns_name("_dmarc.example.com"));
        assert!(!valid_dns_name(""));
        assert!(!valid_dns_name("bad-.com"));
        assert!(!valid_dns_name("exa mple.com"));
        assert!(!valid_dns_name(&format!("{}.com", "a".repeat(250))));
    }

    #[test]
    fn valid_dns_name_allows_leading_wildcard_label() {
        assert!(valid_dns_name("*.example.com"));
        assert!(valid_dns_name("*.sub.example.com"));
        assert!(valid_dns_name("*.example.com."));
        assert!(valid_dns_name("*._dmarc.example.com"));
    }

    #[test]
    fn valid_dns_name_rejects_wildcards_anywhere_else() {
        // Wildcard glued to a label, or in a non-leading position.
        assert!(!valid_dns_name("*example.com"));
        assert!(!valid_dns_name("foo.*.example.com"));
        assert!(!valid_dns_name("example.*.com"));
        assert!(!valid_dns_name("**.example.com"));
        assert!(!valid_dns_name("*.*.example.com"));
        // Leading wildcard still needs valid labels after it.
        assert!(!valid_dns_name("*.bad-.com"));
        assert!(!valid_dns_name("*.exa mple.com"));
    }

    #[test]
    fn validate_record_content_a_and_aaaa() {
        assert_eq!(
            validate_record_content("A", "  1.2.3.4  ", None).unwrap(),
            "1.2.3.4"
        );
        assert!(validate_record_content("A", "not-an-ip", None).is_err());
        assert!(validate_record_content("A", "0.0.0.0", None).is_err());
        assert!(validate_record_content("A", "224.0.0.1", None).is_err());
        assert!(validate_record_content("AAAA", "::1", None).is_ok());
        assert!(validate_record_content("AAAA", "1.2.3.4", None).is_err());
        assert!(validate_record_content("AAAA", "::", None).is_err());
        assert!(validate_record_content("A", "", None).is_err());
    }

    #[test]
    fn validate_record_content_names_and_mx() {
        assert!(validate_record_content("CNAME", "example.com", None).is_ok());
        assert!(validate_record_content("CNAME", "1.2.3.4", None).is_err());
        assert!(validate_record_content("MX", "mail.example.com", Some(10)).is_ok());
        assert!(validate_record_content("MX", "mail.example.com", None).is_err());
        assert!(validate_record_content("TXT", "hello", None).is_ok());
    }

    #[test]
    fn validate_record_content_srv_caa_and_unknown() {
        assert!(validate_record_content("SRV", "10 20 5060 sip.example.com", None).is_ok());
        assert!(validate_record_content("SRV", "10 20 sip.example.com", None).is_err());
        assert!(validate_record_content("SRV", "x 20 5060 sip.example.com", None).is_err());
        assert!(validate_record_content("CAA", "0 issue \"letsencrypt.org\"", None).is_ok());
        assert!(validate_record_content("CAA", "0 badtag \"x\"", None).is_err());
        assert!(validate_record_content("CAA", "300 issue \"x\"", None).is_err());
        assert!(validate_record_content("NS", "example.com", None).is_err());
    }

    #[test]
    fn validate_ttl_rules() {
        assert_eq!(validate_ttl(1).unwrap(), 1);
        assert_eq!(validate_ttl(60).unwrap(), 60);
        assert_eq!(validate_ttl(86_400).unwrap(), 86_400);
        assert!(validate_ttl(0).is_err());
        assert!(validate_ttl(59).is_err());
        assert!(validate_ttl(86_401).is_err());
    }
}
