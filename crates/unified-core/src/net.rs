//! IPv4 address classification using only stable `std` APIs.
//!
//! The `std` predicates `Ipv4Addr::is_benchmarking`, `is_reserved`,
//! `is_shared`, and `is_global` are not stable on this toolchain, so
//! the same ranges are checked here with octet patterns. The ranges
//! follow the `std` definitions: benchmarking is 198.18.0.0/15,
//! reserved is 240.0.0.0/4, shared address space is 100.64.0.0/10,
//! and global means none of the special-purpose ranges.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

/// True for 198.18.0.0/15 (benchmarking), mirroring
/// `Ipv4Addr::is_benchmarking`.
pub fn ipv4_is_benchmarking(addr: &Ipv4Addr) -> bool {
    matches!(addr.octets(), [198, 18 | 19, _, _])
}

/// True for 240.0.0.0/4 (reserved), mirroring `Ipv4Addr::is_reserved`.
pub fn ipv4_is_reserved(addr: &Ipv4Addr) -> bool {
    addr.octets()[0] >= 240
}

/// True for 100.64.0.0/10 (shared address space), mirroring
/// `Ipv4Addr::is_shared`.
pub fn ipv4_is_shared(addr: &Ipv4Addr) -> bool {
    matches!(addr.octets(), [100, 64..=127, _, _])
}

/// True when the address is globally routable: none of the
/// special-purpose ranges, mirroring `Ipv4Addr::is_global`.
pub fn ipv4_is_global(addr: &Ipv4Addr) -> bool {
    !(addr.is_private()
        || addr.is_loopback()
        || addr.is_link_local()
        || addr.is_multicast()
        || addr.is_broadcast()
        || addr.is_documentation()
        || ipv4_is_benchmarking(addr)
        || ipv4_is_reserved(addr)
        || ipv4_is_shared(addr)
        || addr.is_unspecified())
}

/// True when the IPv6 address is globally routable, mirroring
/// `Ipv6Addr::is_global`: none of unspecified, loopback, multicast,
/// unique-local (fc00::/7), link-local (fe80::/10), site-local
/// (fec0::/10), documentation (2001:db8::/32), or benchmarking
/// (2001:2::/48).
pub fn ipv6_is_global(addr: &Ipv6Addr) -> bool {
    let segments = addr.segments();
    !(addr.is_unspecified()
        || addr.is_loopback()
        || addr.is_multicast()
        || addr.is_unique_local()
        || (segments[0] & 0xffc0) == 0xfe80
        || (segments[0] & 0xffc0) == 0xfec0
        || (segments[0] == 0x2001 && segments[1] == 0x0db8)
        || (segments[0] == 0x2001 && segments[1] == 0x0002 && segments[2] == 0))
}

/// `IpAddr` dispatch for the global-routability check.
pub fn ip_is_global(ip: &IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => ipv4_is_global(v4),
        IpAddr::V6(v6) => ipv6_is_global(v6),
    }
}
