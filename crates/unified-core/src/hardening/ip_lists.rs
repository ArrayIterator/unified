// hardening - ip_lists.
// CIDR ranges and allow/block lists.
//

use super::*;
use crate::error::UnifiedError;
use crate::hooks::HookBus;
use crate::json;
use crate::middleware::{Middleware, MiddlewareResult, NextFn};
use actix_web::HttpResponse;
use actix_web::dev::ServiceRequest;
use actix_web::http::header::{self, HeaderValue};
use base64::Engine;
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

// CIDR ranges

/// One parsed CIDR range (or a bare IP, stored as a /32 or /128).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize)]
pub struct CidrRange {
    pub network: IpAddr,
    pub prefix: u8,
    /// Normalized `network/prefix` form, used as the map key.
    pub cidr: String,
}

/// Parse an IP address or CIDR range. A bare IP becomes a host route
/// (/32 for IPv4, /128 for IPv6). Returns a human-readable error.
pub fn parse_cidr(input: &str) -> Result<CidrRange, String> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return Err("IP list entry must not be empty".to_string());
    }
    let (ip_text, prefix) = match trimmed.split_once('/') {
        Some((ip_text, prefix_text)) => {
            let ip: IpAddr = ip_text.parse().map_err(|_| {
                format!("\"{trimmed}\" is not a valid IP address or CIDR range")
            })?;
            let max_prefix = match ip {
                IpAddr::V4(_) => 32,
                IpAddr::V6(_) => 128,
            };
            let prefix: u8 = prefix_text.parse().map_err(|_| {
                format!("\"{trimmed}\" has an invalid CIDR prefix")
            })?;
            if prefix > max_prefix {
                return Err(format!(
                    "\"{trimmed}\" has a prefix larger than {max_prefix}"
                ));
            }
            (ip_text, prefix)
        }
        None => {
            let ip: IpAddr = trimmed
                .parse()
                .map_err(|_| format!("\"{trimmed}\" is not a valid IP address or CIDR range"))?;
            let prefix = match ip {
                IpAddr::V4(_) => 32,
                IpAddr::V6(_) => 128,
            };
            (trimmed, prefix)
        }
    };
    let ip: IpAddr = ip_text.parse().expect("IP parsed above");
    let network = mask_address(ip, prefix);
    Ok(CidrRange {
        network,
        prefix,
        cidr: format!("{network}/{prefix}"),
    })
}

fn mask_address(ip: IpAddr, prefix: u8) -> IpAddr {
    match ip {
        IpAddr::V4(addr) => {
            let bits = u32::from(addr);
            let mask = if prefix == 0 {
                0u32
            } else {
                u32::MAX << (32 - prefix)
            };
            IpAddr::V4((bits & mask).into())
        }
        IpAddr::V6(addr) => {
            let bits = u128::from(addr);
            let mask = if prefix == 0 {
                0u128
            } else {
                u128::MAX << (128 - prefix)
            };
            IpAddr::V6((bits & mask).into())
        }
    }
}

impl CidrRange {
    /// True when `ip` falls inside this range.
    pub fn contains(&self, ip: IpAddr) -> bool {
        match (self.network, ip) {
            (IpAddr::V4(_), IpAddr::V4(_)) | (IpAddr::V6(_), IpAddr::V6(_)) => {
                mask_address(ip, self.prefix) == self.network
            }
            _ => false,
        }
    }
}

/// One IP list record, as returned by the list endpoints.
#[derive(Debug, Clone, Serialize)]
pub struct IpListEntry {
    pub list: String,
    pub cidr: String,
    pub network: String,
    pub prefix: u8,
    pub added_at: DateTime<Utc>,
}

impl HardeningState {
    pub(crate) fn replace_ip_list(&self, list: &str, entries: &[String]) {
        let map = if list == "allow" {
            &self.allowlist
        } else {
            &self.blocklist
        };
        map.clear();
        for entry in entries {
            if let Ok(range) = parse_cidr(entry) {
                map.insert(
                    range.cidr.clone(),
                    IpListEntry {
                        list: list.to_string(),
                        cidr: range.cidr.clone(),
                        network: range.network.to_string(),
                        prefix: range.prefix,
                        added_at: Utc::now(),
                    },
                );
            }
        }
    }

    // -- IP lists ------------------------------------------------------

    pub fn is_allowlisted(&self, ip: IpAddr) -> bool {
        self.allowlist
            .iter()
            .any(|entry| parse_cidr(&entry.value().cidr).is_ok_and(|range| range.contains(ip)))
    }

    pub fn is_blocklisted(&self, ip: IpAddr) -> bool {
        self.blocklist
            .iter()
            .any(|entry| parse_cidr(&entry.value().cidr).is_ok_and(|range| range.contains(ip)))
    }

    pub fn ip_list_entries(&self, list: &str) -> Vec<IpListEntry> {
        let map = if list == "allow" {
            &self.allowlist
        } else {
            &self.blocklist
        };
        let mut entries: Vec<IpListEntry> =
            map.iter().map(|entry| entry.value().clone()).collect();
        entries.sort_by(|a, b| a.cidr.cmp(&b.cidr));
        entries
    }

    pub fn add_ip_entry(&self, list: &str, cidr_text: &str) -> Result<IpListEntry, String> {
        if list != "allow" && list != "block" {
            return Err("list must be \"allow\" or \"block\"".to_string());
        }
        let range = parse_cidr(cidr_text)?;
        let entry = IpListEntry {
            list: list.to_string(),
            cidr: range.cidr.clone(),
            network: range.network.to_string(),
            prefix: range.prefix,
            added_at: Utc::now(),
        };
        if list == "allow" {
            self.allowlist.insert(range.cidr.clone(), entry.clone());
        } else {
            self.blocklist.insert(range.cidr.clone(), entry.clone());
        }
        Ok(entry)
    }

    pub fn remove_ip_entry(&self, list: &str, cidr_text: &str) -> Result<bool, String> {
        if list != "allow" && list != "block" {
            return Err("list must be \"allow\" or \"block\"".to_string());
        }
        let range = parse_cidr(cidr_text)?;
        let removed = if list == "allow" {
            self.allowlist.remove(&range.cidr).is_some()
        } else {
            self.blocklist.remove(&range.cidr).is_some()
        };
        Ok(removed)
    }
}
