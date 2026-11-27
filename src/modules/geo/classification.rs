//! geo - classification.

use std::net::IpAddr;



#[derive(Debug, Clone, Copy, PartialEq, Eq)]
// IP classification: which addresses are not publicly routable.
/// Address class. Anything other than `Public` is a bogon for Geo
/// purposes: no MaxMind record exists for it, so lookups skip the
/// database (settings overrides still apply).
pub enum IpClass {
    Public,
    Private,
    Loopback,
    LinkLocal,
    Multicast,
    Unspecified,
    Broadcast,
    Documentation,
    Benchmarking,
    Reserved,
    Shared,
    UniqueLocal,
    Discard,
    SiteLocal,
}


pub fn classify_ip(ip: IpAddr) -> IpClass {
    match ip {
        IpAddr::V4(addr) => {
            let octets = addr.octets();
            if octets == [0, 0, 0, 0] || octets[0] == 0 {
                IpClass::Unspecified
            } else if addr.is_loopback() {
                IpClass::Loopback
            } else if addr.is_private() {
                IpClass::Private
            } else if addr.is_link_local() {
                IpClass::LinkLocal
            } else if addr.is_multicast() {
                IpClass::Multicast
            } else if octets == [255, 255, 255, 255] {
                IpClass::Broadcast
            } else if (octets[0] == 192 && octets[1] == 0 && octets[2] == 2)
                || (octets[0] == 198 && octets[1] == 51 && octets[2] == 100)
                || (octets[0] == 203 && octets[1] == 0 && octets[2] == 113)
            {
                // TEST-NET-1/2/3 documentation ranges (RFC 5737).
                IpClass::Documentation
            } else if octets[0] == 198 && (18..=19).contains(&octets[1]) {
                // Benchmarking range 198.18.0.0/15 (RFC 2544).
                IpClass::Benchmarking
            } else if octets[0] == 100 && (64..=127).contains(&octets[1]) {
                // Shared address space / CGNAT 100.64.0.0/10 (RFC 6598).
                IpClass::Shared
            } else if octets[0] >= 240 {
                IpClass::Reserved
            } else {
                IpClass::Public
            }
        }
        IpAddr::V6(addr) => {
            let segments = addr.segments();
            if addr.is_unspecified() {
                IpClass::Unspecified
            } else if addr.is_loopback() {
                IpClass::Loopback
            } else if addr.is_multicast() {
                IpClass::Multicast
            } else if segments[0] & 0xfe00 == 0xfc00 {
                // Unique-local addresses fc00::/7 (RFC 4193).
                IpClass::UniqueLocal
            } else if segments[0] & 0xffc0 == 0xfe80 {
                // Link-local addresses fe80::/10 (RFC 4291).
                IpClass::LinkLocal
            } else if segments[0] == 0x2001 && segments[1] == 0x0db8 {
                // Documentation range 2001:db8::/32 (RFC 3849).
                IpClass::Documentation
            } else if segments[0] == 0x2001 && segments[1] == 0x0002 && segments[2] == 0 {
                // Benchmarking range 2001:2::/48 (RFC 5180).
                IpClass::Benchmarking
            } else if segments[0] == 0x0100 && segments[1] == 0 && segments[2] == 0 && segments[3] == 0 {
                // Discard-only range 100::/64 (RFC 6666).
                IpClass::Discard
            } else if segments[0] & 0xffc0 == 0xfec0 {
                // Deprecated site-local addresses fec0::/10 (RFC 3879).
                IpClass::SiteLocal
            } else {
                IpClass::Public
            }
        }
    }
}


/// `true` when the address is not publicly routable.
pub fn is_bogon(ip: IpAddr) -> bool {
    classify_ip(ip) != IpClass::Public
}


/// `true` when the address is publicly routable.
pub fn is_public_ip(ip: IpAddr) -> bool {
    classify_ip(ip) == IpClass::Public
}


/// Validate a MaxMind license-key string by shape: 40 characters in
/// three underscore-separated parts, a 6-character alphanumeric prefix,
/// an alphanumeric middle part, and the `mmk` suffix. This checks the
/// format only; it does not verify the key against MaxMind.
pub fn is_valid_maxmind_license_key(key: &str) -> bool {
    let key = key.trim();
    if key.len() != 40 {
        return false;
    }
    let parts: Vec<&str> = key.split('_').collect();
    if parts.len() != 3 {
        return false;
    }
    let (prefix, middle, suffix) = (parts[0], parts[1], parts[2]);
    prefix.len() == 6
        && !middle.is_empty()
        && suffix == "mmk"
        && prefix.chars().all(|c| c.is_ascii_alphanumeric())
        && middle.chars().all(|c| c.is_ascii_alphanumeric())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_ip_names_the_common_classes() {
        assert_eq!(classify_ip("8.8.8.8".parse().unwrap()), IpClass::Public);
        assert_eq!(classify_ip("127.0.0.1".parse().unwrap()), IpClass::Loopback);
        assert_eq!(classify_ip("10.0.0.1".parse().unwrap()), IpClass::Private);
        assert_eq!(classify_ip("192.168.0.1".parse().unwrap()), IpClass::Private);
        assert_eq!(
            classify_ip("169.254.1.1".parse().unwrap()),
            IpClass::LinkLocal
        );
        assert_eq!(classify_ip("0.0.0.0".parse().unwrap()), IpClass::Unspecified);
        assert_eq!(
            classify_ip("224.0.0.1".parse().unwrap()),
            IpClass::Multicast
        );
    }

    #[test]
    fn bogon_and_public_helpers_agree_with_classification() {
        assert!(is_bogon("127.0.0.1".parse().unwrap()));
        assert!(is_bogon("10.1.2.3".parse().unwrap()));
        assert!(!is_bogon("8.8.8.8".parse().unwrap()));
        assert!(is_public_ip("8.8.8.8".parse().unwrap()));
        assert!(!is_public_ip("192.168.1.1".parse().unwrap()));
    }

    #[test]
    fn maxmind_license_key_checks_shape_only() {
        let valid = format!("{}_{}_mmk", "Ab1234", "x".repeat(29));
        assert_eq!(valid.len(), 40);
        assert!(is_valid_maxmind_license_key(&valid));
        assert!(!is_valid_maxmind_license_key("short"));
        assert!(!is_valid_maxmind_license_key(&"a".repeat(40)));
        assert!(!is_valid_maxmind_license_key("Ab1234_middlepart_mmk_extra"));
    }
}
