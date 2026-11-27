//! geo - cidr.

use unified_core::error::{UnifiedError, UnifiedResult};
use std::net::IpAddr;



#[derive(Debug, Clone, Copy)]
// CIDR parsing and containment (IPv4 and IPv6, implemented with std only).
pub struct Cidr {
    pub network: IpAddr,
    pub prefix: u8,
}


impl Cidr {
    pub fn parse(input: &str) -> UnifiedResult<Self> {
        let input = input.trim();
        let (addr_text, prefix_text) = match input.split_once('/') {
            Some((addr, prefix)) => (addr, Some(prefix)),
            None => (input, None),
        };
        let addr: IpAddr = addr_text.parse().map_err(|_| {
            UnifiedError::bad_request(format!("Invalid IP address in CIDR \"{input}\""))
        })?;
        let max_prefix: u8 = match addr {
            IpAddr::V4(_) => 32,
            IpAddr::V6(_) => 128,
        };
        let prefix: u8 = match prefix_text {
            Some(text) => text.parse().map_err(|_| {
                UnifiedError::bad_request(format!("Invalid prefix in CIDR \"{input}\""))
            })?,
            None => max_prefix,
        };
        if prefix > max_prefix {
            return Err(UnifiedError::bad_request(format!(
                "Prefix {prefix} exceeds maximum {max_prefix} for {addr}"
            )));
        }
        Ok(Self {
            network: masked(addr, prefix),
            prefix,
        })
    }

    pub fn contains(&self, ip: IpAddr) -> bool {
        match (self.network, ip) {
            (IpAddr::V4(network), IpAddr::V4(addr)) => {
                masked(IpAddr::V4(addr), self.prefix) == self.network
            }
            (IpAddr::V6(network), IpAddr::V6(addr)) => {
                masked(IpAddr::V6(addr), self.prefix) == self.network
            }
            _ => false,
        }
    }

    /// Normalised `address/prefix` form.
    pub fn to_cidr_string(&self) -> String {
        format!("{}/{}", self.network, self.prefix)
    }
}


pub(crate) fn masked(ip: IpAddr, prefix: u8) -> IpAddr {
    match ip {
        IpAddr::V4(addr) => {
            let bits = u32::from(addr);
            let mask: u32 = if prefix == 0 {
                0
            } else {
                u32::MAX << (32 - prefix)
            };
            IpAddr::V4((bits & mask).into())
        }
        IpAddr::V6(addr) => {
            let bits = u128::from(addr);
            let mask: u128 = if prefix == 0 {
                0
            } else {
                u128::MAX << (128 - prefix)
            };
            IpAddr::V6((bits & mask).into())
        }
    }
}


pub(crate) fn broadcast(ip: IpAddr, prefix: u8) -> IpAddr {
    match ip {
        IpAddr::V4(addr) => {
            let bits = u32::from(addr);
            let mask: u32 = if prefix == 0 {
                0
            } else {
                u32::MAX << (32 - prefix)
            };
            IpAddr::V4((bits | !mask).into())
        }
        IpAddr::V6(addr) => {
            let bits = u128::from(addr);
            let mask: u128 = if prefix == 0 {
                0
            } else {
                u128::MAX << (128 - prefix)
            };
            IpAddr::V6((bits | !mask).into())
        }
    }
}


impl Cidr {
    /// First and last address of the block (network and broadcast).
    pub fn range(&self) -> (IpAddr, IpAddr) {
        (self.network, broadcast(self.network, self.prefix))
    }

    /// Build the single CIDR block that covers exactly `start..=end`.
    /// Ranges that are not one aligned block (for example `10.0.0.0`
    /// to `10.0.0.5`) are rejected instead of being widened into a
    /// block that covers addresses outside the range.
    pub fn from_range(start: IpAddr, end: IpAddr) -> UnifiedResult<Self> {
        let (start_num, end_num, max_prefix) = match (start, end) {
            (IpAddr::V4(a), IpAddr::V4(b)) => {
                (u32::from(a) as u128, u32::from(b) as u128, 32u8)
            }
            (IpAddr::V6(a), IpAddr::V6(b)) => (u128::from(a), u128::from(b), 128u8),
            _ => {
                return Err(UnifiedError::bad_request(
                    "range start and end must be the same IP version",
                ));
            }
        };
        if start_num > end_num {
            return Err(UnifiedError::bad_request(
                "range start must not be greater than range end",
            ));
        }
        let diff = start_num ^ end_num;
        let prefix: u8 = if diff == 0 {
            max_prefix
        } else if max_prefix == 32 {
            // Count leading zeros in 32-bit space: the values were
            // widened to u128, whose leading zeros would over-count.
            (diff as u32).leading_zeros() as u8
        } else {
            diff.leading_zeros() as u8
        };
        let candidate = Cidr {
            network: masked(start, prefix),
            prefix,
        };
        if candidate.network != start || broadcast(candidate.network, prefix) != end {
            return Err(UnifiedError::bad_request(
                "range is not exactly one CIDR block",
            ));
        }
        Ok(candidate)
    }
}


/// Numeric form of an IP address (IPv4 as the low 32 bits).
pub fn ip_to_u128(ip: IpAddr) -> u128 {
    match ip {
        IpAddr::V4(addr) => u32::from(addr) as u128,
        IpAddr::V6(addr) => u128::from(addr),
    }
}


/// Inverse of `ip_to_u128`. `is_v6` selects the address family.
pub fn u128_to_ip(value: u128, is_v6: bool) -> UnifiedResult<IpAddr> {
    if is_v6 {
        Ok(IpAddr::V6(value.into()))
    } else if value <= u32::MAX as u128 {
        Ok(IpAddr::V4((value as u32).into()))
    } else {
        Err(UnifiedError::bad_request(
            "value does not fit in an IPv4 address",
        ))
    }
}


/// Normalise an IPv4 address written with leading zeros
/// (`010.000.0.1` -> `10.0.0.1`). Rust's standard parser rejects
/// leading zeros outright, so this handles the legacy form explicitly.
pub fn normalize_ipv4(input: &str) -> UnifiedResult<String> {
    let parts: Vec<&str> = input.trim().split('.').collect();
    if parts.len() != 4 {
        return Err(UnifiedError::bad_request(format!(
            "Invalid IPv4 address \"{input}\""
        )));
    }
    let mut out: Vec<String> = Vec::with_capacity(4);
    for part in parts {
        if part.is_empty() || !part.bytes().all(|b| b.is_ascii_digit()) {
            return Err(UnifiedError::bad_request(format!(
                "Invalid IPv4 address \"{input}\""
            )));
        }
        let stripped = part.trim_start_matches('0');
        let digits = if stripped.is_empty() { "0" } else { stripped };
        let value: u16 = digits.parse().map_err(|_| {
            UnifiedError::bad_request(format!("Invalid IPv4 address \"{input}\""))
        })?;
        if value > 255 {
            return Err(UnifiedError::bad_request(format!(
                "Invalid IPv4 address \"{input}\""
            )));
        }
        out.push(value.to_string());
    }
    Ok(out.join("."))
}


/// Collapse an IPv4-mapped IPv6 address (`::ffff:8.8.8.8`) to its IPv4
/// form so CIDR overrides and MaxMind lookups match it. Other
/// addresses are returned unchanged.
pub fn normalize_ip(ip: IpAddr) -> IpAddr {
    match ip {
        IpAddr::V6(addr) => match addr.to_ipv4_mapped() {
            Some(v4) => IpAddr::V4(v4),
            None => ip,
        },
        IpAddr::V4(_) => ip,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cidr_parse_normalises_network_address() {
        let cidr = Cidr::parse("192.168.1.5/24").expect("valid CIDR");
        assert_eq!(cidr.to_cidr_string(), "192.168.1.0/24");
        assert!(cidr.contains("192.168.1.200".parse().unwrap()));
        assert!(!cidr.contains("192.168.2.1".parse().unwrap()));
        assert!(!cidr.contains("2001:db8::1".parse().unwrap()));
    }

    #[test]
    fn cidr_parse_defaults_to_host_prefix() {
        let v4 = Cidr::parse("10.0.0.7").expect("bare IPv4");
        assert_eq!(v4.to_cidr_string(), "10.0.0.7/32");
        let v6 = Cidr::parse("2001:db8::1").expect("bare IPv6");
        assert_eq!(v6.prefix, 128);
        assert!(v6.contains("2001:db8::1".parse().unwrap()));
        assert!(!v6.contains("2001:db8::2".parse().unwrap()));
    }

    #[test]
    fn cidr_parse_rejects_bad_input() {
        assert!(Cidr::parse("not-an-ip/24").is_err());
        assert!(Cidr::parse("192.168.1.0/33").is_err());
        assert!(Cidr::parse("192.168.1.0/abc").is_err());
        assert!(Cidr::parse("2001:db8::1/129").is_err());
    }

    #[test]
    fn ip_to_u128_round_trips_both_families() {
        let v4: IpAddr = "8.8.4.4".parse().unwrap();
        assert_eq!(u128_to_ip(ip_to_u128(v4), false).unwrap(), v4);
        let v6: IpAddr = "2001:db8::1".parse().unwrap();
        assert_eq!(u128_to_ip(ip_to_u128(v6), true).unwrap(), v6);
    }

    #[test]
    fn normalize_ipv4_strips_leading_zeros() {
        assert_eq!(normalize_ipv4("010.000.0.1").unwrap(), "10.0.0.1");
        assert_eq!(normalize_ipv4("192.168.1.1").unwrap(), "192.168.1.1");
        assert!(normalize_ipv4("256.1.1.1").is_err());
        assert!(normalize_ipv4("192.168.1").is_err());
        assert!(normalize_ipv4("192.168.1.x").is_err());
    }

    #[test]
    fn normalize_ip_collapses_ipv4_mapped_ipv6() {
        let mapped: IpAddr = "::ffff:8.8.8.8".parse().unwrap();
        assert_eq!(normalize_ip(mapped), "8.8.8.8".parse().unwrap());
        let plain: IpAddr = "2001:db8::1".parse().unwrap();
        assert_eq!(normalize_ip(plain), plain);
    }
}
