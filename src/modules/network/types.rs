// network module -- Shared types, records, request bodies, and constants.

use serde::{Deserialize, Serialize};
use std::net::IpAddr;
use std::sync::atomic::{AtomicU64, Ordering};

// ---------------------------------------------------------------------------
// Active-connection counters.
//
// There is no shared connection-stats extension on `AppContext`, so the
// counts live in a module-level static. The HTTP server and WebSocket
// layers call `note_connection_open` / `note_connection_closed` whenever
// a connection is accepted or dropped.
// ---------------------------------------------------------------------------

pub struct ConnectionStats {
    pub http: AtomicU64,
    pub websocket: AtomicU64,
    pub total_accepted: AtomicU64,
}

impl ConnectionStats {
    pub(crate) fn new() -> Self {
        Self {
            http: AtomicU64::new(0),
            websocket: AtomicU64::new(0),
            total_accepted: AtomicU64::new(0),
        }
    }
}

// ---------------------------------------------------------------------------
// DNS verifier library.
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
pub struct MxRecord {
    pub exchange: String,
    pub preference: u16,
}

#[derive(Debug, Clone, Serialize)]
pub struct SpfReport {
    pub record: Option<String>,
    pub mechanisms: Vec<String>,
    pub lookup_count: usize,
    pub has_all: bool,
    pub all_mechanism: Option<String>,
    pub valid: bool,
    pub issues: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct DkimReport {
    pub checked: bool,
    pub selector: Option<String>,
    pub record: Option<String>,
    pub key_type: Option<String>,
    pub public_key_present: bool,
    pub valid: bool,
    pub issues: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct DmarcReport {
    pub record: Option<String>,
    pub policy: Option<String>,
    pub subdomain_policy: Option<String>,
    pub rua: Option<String>,
    pub percentage: Option<u8>,
    pub valid: bool,
    pub issues: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct DomainSyntaxReport {
    pub valid: bool,
    pub domain: String,
    /// Punycode (IDNA / ASCII) form of the domain.
    pub punycode: Option<String>,
    /// Unicode form, present only when the input contained non-ASCII
    /// characters (the display form of an internationalised domain).
    pub unicode: Option<String>,
    /// `true` for `localhost` and special-use / local suffixes
    /// (`test`, `example`, `invalid`, `local`, `internal`, `arpa`).
    pub is_local: bool,
    pub issues: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct DnsHealthReport {
    pub resolver_latency_ms: Option<u128>,
    pub resolver_ok: bool,
    pub ns_records: Vec<String>,
    pub ns_ok: bool,
    pub a_ok: bool,
    pub issues: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct DnsReport {
    pub domain: String,
    pub syntax: DomainSyntaxReport,
    pub a: Vec<IpAddr>,
    pub aaaa: Vec<IpAddr>,
    pub cname: Option<String>,
    pub mx: Vec<MxRecord>,
    pub txt: Vec<String>,
    pub spf: SpfReport,
    pub dkim: DkimReport,
    pub dmarc: DmarcReport,
    pub health: DnsHealthReport,
}

/// Permissions guarding the network routes. Reads (`/info`,
/// `/connections`) require `network.view`; lookups and checks
/// (`/geo-lookup`, `/dns/check`) require `network.manage` because they
/// perform work against caller-supplied input.
pub(crate) const PERM_NETWORK_VIEW: &str = "network.view";
pub(crate) const PERM_NETWORK_MANAGE: &str = "network.manage";

/// Which address family a lookup prefers when both exist.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum IpChoice {
    /// Try IPv4 first, fall back to IPv6.
    #[default]
    Ipv4Preferred,
    /// Try IPv6 first, fall back to IPv4.
    Ipv6Preferred,
    Ipv4Only,
    Ipv6Only,
}

/// Special-use and local suffixes (RFC 6761 and common local use).
/// These resolve locally, if at all; they are never public domains.
pub const LOCAL_SUFFIXES: &[&str] = &[
    "localhost",
    "localdomain",
    "local",
    "arpa",
    "test",
    "invalid",
    "example",
    "internal",
];

/// Full DNS verifier: A / AAAA / MX / TXT lookups plus SPF, DKIM, DMARC,
/// domain-syntax (including IDNA / punycode), and resolver health checks.

#[derive(Debug, Deserialize)]
// ---------------------------------------------------------------------------
// Request types and handlers.
// ---------------------------------------------------------------------------

pub(crate) struct GeoLookupRequest {
    pub(crate) ip: String,
}

#[derive(Debug, Deserialize)]
pub(crate) struct DnsCheckRequest {
    pub(crate) domain: String,
    pub(crate) selector: Option<String>,
}

#[derive(Debug, Serialize)]
pub(crate) struct InterfaceView {
    pub(crate) name: String,
    pub(crate) mac_address: String,
    pub(crate) ip_addresses: Vec<String>,
    pub(crate) received_bytes: u64,
    pub(crate) transmitted_bytes: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn connection_stats_start_at_zero() {
        let stats = ConnectionStats::new();
        assert_eq!(stats.http.load(Ordering::SeqCst), 0);
        assert_eq!(stats.websocket.load(Ordering::SeqCst), 0);
        assert_eq!(stats.total_accepted.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn ip_choice_defaults_to_ipv4_preferred() {
        assert_eq!(IpChoice::default(), IpChoice::Ipv4Preferred);
    }

    #[test]
    fn local_suffixes_cover_well_known_names() {
        assert!(LOCAL_SUFFIXES.contains(&"localhost"));
        assert!(LOCAL_SUFFIXES.contains(&"invalid"));
        assert!(LOCAL_SUFFIXES.contains(&"internal"));
    }

    #[test]
    fn mx_record_serializes() {
        let record = MxRecord {
            exchange: "mail.example.com".to_string(),
            preference: 10,
        };
        let text = serde_json::to_string(&record).expect("record serializes");
        assert!(text.contains("mail.example.com"));
    }
}
