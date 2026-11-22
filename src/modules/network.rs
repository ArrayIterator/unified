// Network module.
//
// Provides local network information, active-connection counts, Geo
// lookups (delegated to the GEO module through the `geo.lookup` hook
// event), and the DNS verifier library (`DnsVerifier`).
//
// DNS / mail / security wiring (documented here so the other modules can
// rely on it):
//   - `POST /api/v1/network/dns/check` runs a full DNS report and then
//     emits a `dns.check` hook event carrying the report, so the Domains,
//     Mail, and Security modules can observe every check without calling
//     this module directly.
//   - The Domains module verifies a domain by calling `DnsVerifier`
//     directly (async DNS cannot run inside a synchronous hook handler)
//     and emits `domain.verify`; the Mail module can reuse the same
//     SPF / DKIM / DMARC reports for sender authentication, and the
//     Security module can reuse `dns_health` for domain reputation.
//   - Geo lookups are delegated through the `geo.lookup` hook event:
//     the GEO module registers a synchronous handler for it, and this
//     module falls back to `crate::modules::geo::lookup_geo` when no
//     handler answered.

use crate::core::context::{AppContext, GeoInfo};
use crate::core::error::{UnifiedError, UnifiedResult};
use crate::core::hooks::{events, HookBus, HookContext};
use crate::core::json;
use crate::core::modules::{Module, ModuleRegistry};
use actix_web::{web, HttpResponse};
use hickory_proto::rr::{Name, RData, RecordType};
use hickory_resolver::{ResolverConfig, ResolverOpts, TokioResolver};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::net::IpAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

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
    fn new() -> Self {
        Self {
            http: AtomicU64::new(0),
            websocket: AtomicU64::new(0),
            total_accepted: AtomicU64::new(0),
        }
    }
}

static CONNECTION_STATS: OnceLock<ConnectionStats> = OnceLock::new();

pub fn connection_stats() -> &'static ConnectionStats {
    CONNECTION_STATS.get_or_init(ConnectionStats::new)
}

/// Record a newly accepted connection. `kind` is `"http"` or `"websocket"`.
pub fn note_connection_open(kind: &str) {
    let stats = connection_stats();
    match kind {
        "websocket" => stats.websocket.fetch_add(1, Ordering::SeqCst),
        _ => stats.http.fetch_add(1, Ordering::SeqCst),
    };
    stats.total_accepted.fetch_add(1, Ordering::SeqCst);
}

/// Record a closed connection. `kind` is `"http"` or `"websocket"`.
pub fn note_connection_closed(kind: &str) {
    let stats = connection_stats();
    match kind {
        "websocket" => {
            stats.websocket.fetch_sub(1, Ordering::SeqCst);
        }
        _ => {
            stats.http.fetch_sub(1, Ordering::SeqCst);
        }
    };
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
pub struct DnsVerifier {
    resolver: TokioResolver,
    ip_choice: IpChoice,
}

impl DnsVerifier {
    pub fn new() -> Self {
        Self::with_ip_choice(IpChoice::default())
    }

    pub fn with_ip_choice(ip_choice: IpChoice) -> Self {
        let mut opts = ResolverOpts::default();
        // Match the reference behaviour: fail fast instead of waiting on
        // the resolver default, and retry once on a transient failure.
        opts.timeout = Duration::from_secs(5);
        opts.attempts = 2;
        Self {
            resolver: TokioResolver::builder(ResolverConfig::default(), opts).build(),
            ip_choice,
        }
    }

    pub fn set_ip_choice(&mut self, ip_choice: IpChoice) {
        self.ip_choice = ip_choice;
    }

    pub fn ip_choice(&self) -> IpChoice {
        self.ip_choice
    }

    fn dns_name(domain: &str) -> UnifiedResult<Name> {
        let fqdn = if domain.ends_with('.') {
            domain.to_string()
        } else {
            format!("{domain}.")
        };
        fqdn.parse::<Name>()
            .map_err(|_| UnifiedError::bad_request(format!("Invalid domain name \"{domain}\"")))
    }

    async fn lookup_records(
        &self,
        domain: &str,
        record_type: RecordType,
    ) -> UnifiedResult<Vec<RData>> {
        let name = Self::dns_name(domain)?;
        let lookup = self
            .resolver
            .lookup(name, record_type)
            .await
            .map_err(|error| {
                UnifiedError::internal(format!("DNS lookup failed for {domain}: {error}"))
            })?;
        Ok(lookup.iter().cloned().collect())
    }

    pub async fn lookup_a(&self, domain: &str) -> UnifiedResult<Vec<IpAddr>> {
        let records = self.lookup_records(domain, RecordType::A).await?;
        let mut out = Vec::new();
        for record in records {
            if let RData::A(addr) = record {
                out.push(IpAddr::V4(addr.0));
            }
        }
        Ok(out)
    }

    pub async fn lookup_aaaa(&self, domain: &str) -> UnifiedResult<Vec<IpAddr>> {
        let records = self.lookup_records(domain, RecordType::AAAA).await?;
        let mut out = Vec::new();
        for record in records {
            if let RData::AAAA(addr) = record {
                out.push(IpAddr::V6(addr.0));
            }
        }
        Ok(out)
    }

    pub async fn lookup_mx(&self, domain: &str) -> UnifiedResult<Vec<MxRecord>> {
        let records = self.lookup_records(domain, RecordType::MX).await?;
        let mut out = Vec::new();
        for record in records {
            if let RData::MX(mx) = record {
                out.push(MxRecord {
                    exchange: mx.exchange().to_string(),
                    preference: mx.preference(),
                });
            }
        }
        out.sort_by_key(|mx| mx.preference);
        Ok(out)
    }

    pub async fn lookup_txt(&self, domain: &str) -> UnifiedResult<Vec<String>> {
        let records = self.lookup_records(domain, RecordType::TXT).await?;
        let mut out = Vec::new();
        for record in records {
            if let RData::TXT(txt) = record {
                let joined: String = txt
                    .txt_data()
                    .iter()
                    .map(|bytes| String::from_utf8_lossy(bytes).into_owned())
                    .collect();
                out.push(joined);
            }
        }
        Ok(out)
    }

    pub async fn lookup_ns(&self, domain: &str) -> UnifiedResult<Vec<String>> {
        let records = self.lookup_records(domain, RecordType::NS).await?;
        let mut out = Vec::new();
        for record in records {
            if let RData::NS(ns) = record {
                out.push(ns.to_string());
            }
        }
        Ok(out)
    }

    /// Resolve A / AAAA records honouring the verifier's address-family
    /// preference. Preferred families fall back to the other family when
    /// the first lookup fails or returns no addresses.
    pub async fn lookup_ip(&self, domain: &str) -> UnifiedResult<Vec<IpAddr>> {
        self.lookup_ip_with(domain, self.ip_choice).await
    }

    pub async fn lookup_ip_with(
        &self,
        domain: &str,
        choice: IpChoice,
    ) -> UnifiedResult<Vec<IpAddr>> {
        match choice {
            IpChoice::Ipv4Only => self.lookup_a(domain).await,
            IpChoice::Ipv6Only => self.lookup_aaaa(domain).await,
            IpChoice::Ipv4Preferred => match self.lookup_a(domain).await {
                Ok(addresses) if !addresses.is_empty() => Ok(addresses),
                _ => self.lookup_aaaa(domain).await,
            },
            IpChoice::Ipv6Preferred => match self.lookup_aaaa(domain).await {
                Ok(addresses) if !addresses.is_empty() => Ok(addresses),
                _ => self.lookup_a(domain).await,
            },
        }
    }

    /// First CNAME target for a domain, if it has one. Returns `Ok(None)`
    /// when the domain has no CNAME record.
    pub async fn lookup_cname(&self, domain: &str) -> UnifiedResult<Option<String>> {
        let records = match self.lookup_records(domain, RecordType::CNAME).await {
            Ok(records) => records,
            Err(_) => return Ok(None),
        };
        for record in records {
            if let RData::CNAME(target) = record {
                return Ok(Some(
                    target.to_string().trim_end_matches('.').to_string(),
                ));
            }
        }
        Ok(None)
    }

    /// Reverse (PTR) lookup for an IP address, via the standard
    /// `in-addr.arpa.` / `ip6.arpa.` names. Returns the PTR targets.
    pub async fn lookup_ptr(&self, ip: IpAddr) -> UnifiedResult<Vec<String>> {
        let name = match ip {
            IpAddr::V4(addr) => {
                let octets = addr.octets();
                format!(
                    "{}.{}.{}.{}.in-addr.arpa.",
                    octets[3], octets[2], octets[1], octets[0]
                )
            }
            IpAddr::V6(addr) => {
                let nibbles: Vec<String> = addr
                    .octets()
                    .iter()
                    .rev()
                    .flat_map(|byte| {
                        [
                            format!("{:x}", byte & 0x0f),
                            format!("{:x}", byte >> 4),
                        ]
                    })
                    .collect();
                format!("{}.ip6.arpa.", nibbles.join("."))
            }
        };
        let records = self.lookup_records(&name, RecordType::PTR).await?;
        let mut out = Vec::new();
        for record in records {
            if let RData::PTR(target) = record {
                out.push(target.to_string().trim_end_matches('.').to_string());
            }
        }
        Ok(out)
    }

    /// Parse a `key=value` TXT policy record (`v=spf1`, `v=DMARC1`, DKIM).
    fn parse_policy(record: &str) -> Vec<(String, String)> {
        record
            .split(';')
            .filter_map(|part| {
                let part = part.trim();
                let (key, value) = part.split_once('=')?;
                Some((key.trim().to_lowercase(), value.trim().to_string()))
            })
            .collect()
    }

    pub async fn check_spf(&self, domain: &str) -> SpfReport {
        let mut report = SpfReport {
            record: None,
            mechanisms: Vec::new(),
            lookup_count: 0,
            has_all: false,
            all_mechanism: None,
            valid: false,
            issues: Vec::new(),
        };
        let txt = match self.lookup_txt(domain).await {
            Ok(records) => records,
            Err(error) => {
                report.issues.push(format!("TXT lookup failed: {error}"));
                return report;
            }
        };
        let spf_records: Vec<&String> = txt
            .iter()
            .filter(|record| {
                record
                    .trim_start()
                    .to_lowercase()
                    .starts_with("v=spf1")
            })
            .collect();
        if spf_records.is_empty() {
            report.issues.push("No SPF record (v=spf1) found".to_string());
            return report;
        }
        if spf_records.len() > 1 {
            report
                .issues
                .push("Multiple SPF records found; only one is allowed".to_string());
        }
        let record = spf_records[0].clone();
        report.record = Some(record.clone());
        let mechanisms: Vec<String> = record
            .split_whitespace()
            .skip(1)
            .map(str::to_string)
            .collect();
        report.mechanisms = mechanisms.clone();
        // Mechanisms that trigger DNS lookups (RFC 7208 section 10.1).
        let lookup_mechanisms = ["include", "a", "mx", "ptr", "exists", "redirect"];
        for mechanism in &mechanisms {
            let bare = mechanism.trim_start_matches(['+', '-', '~', '?']);
            let name = bare.split(':').next().unwrap_or(bare);
            if lookup_mechanisms.contains(&name) {
                report.lookup_count += 1;
            }
            if bare == "all" || name == "all" {
                report.has_all = true;
                report.all_mechanism = Some(mechanism.clone());
            }
        }
        if report.lookup_count > 10 {
            report.issues.push(format!(
                "SPF exceeds the 10 DNS lookup limit ({} lookups)",
                report.lookup_count
            ));
        }
        if !report.has_all {
            report
                .issues
                .push("SPF record has no \"all\" mechanism".to_string());
        }
        if let Some(all) = &report.all_mechanism {
            if all.starts_with("+all") {
                report
                    .issues
                    .push("SPF uses \"+all\", which authorises every sender".to_string());
            }
        }
        report.valid = report.issues.is_empty();
        report
    }

    pub async fn check_dkim(&self, domain: &str, selector: Option<&str>) -> DkimReport {
        let mut report = DkimReport {
            checked: false,
            selector: selector.map(str::to_string),
            record: None,
            key_type: None,
            public_key_present: false,
            valid: false,
            issues: Vec::new(),
        };
        let Some(selector) = selector.filter(|value| !value.trim().is_empty()) else {
            report
                .issues
                .push("No DKIM selector supplied; pass \"selector\" to check DKIM".to_string());
            return report;
        };
        report.checked = true;
        let host = format!("{selector}._domainkey.{domain}");
        let txt = match self.lookup_txt(&host).await {
            Ok(records) => records,
            Err(error) => {
                report.issues.push(format!("DKIM TXT lookup failed: {error}"));
                return report;
            }
        };
        let Some(record) = txt.into_iter().next() else {
            report.issues.push(format!(
                "No DKIM TXT record found for selector \"{selector}\""
            ));
            return report;
        };
        report.record = Some(record.clone());
        let fields = Self::parse_policy(&record);
        let get = |key: &str| fields.iter().find(|(k, _)| k == key).map(|(_, v)| v.clone());
        if get("v").as_deref() != Some("DKIM1") {
            report
                .issues
                .push("DKIM record does not start with \"v=DKIM1\"".to_string());
        }
        report.key_type = get("k");
        match get("p") {
            Some(key) if !key.trim().is_empty() => report.public_key_present = true,
            _ => report
                .issues
                .push("DKIM record has no public key (\"p=\")".to_string()),
        }
        report.valid = report.issues.is_empty();
        report
    }

    pub async fn check_dmarc(&self, domain: &str) -> DmarcReport {
        let mut report = DmarcReport {
            record: None,
            policy: None,
            subdomain_policy: None,
            rua: None,
            percentage: None,
            valid: false,
            issues: Vec::new(),
        };
        let host = format!("_dmarc.{domain}");
        let txt = match self.lookup_txt(&host).await {
            Ok(records) => records,
            Err(error) => {
                report.issues.push(format!("DMARC TXT lookup failed: {error}"));
                return report;
            }
        };
        let Some(record) = txt
            .iter()
            .find(|record| record.trim_start().starts_with("v=DMARC1"))
            .cloned()
        else {
            report
                .issues
                .push("No DMARC record (v=DMARC1) found".to_string());
            return report;
        };
        report.record = Some(record.clone());
        let fields = Self::parse_policy(&record);
        let get = |key: &str| fields.iter().find(|(k, _)| k == key).map(|(_, v)| v.clone());
        report.policy = get("p");
        report.subdomain_policy = get("sp");
        report.rua = get("rua");
        report.percentage = get("pct").and_then(|value| value.parse::<u8>().ok());
        match report.policy.as_deref() {
            Some("none") | Some("quarantine") | Some("reject") => {}
            Some(other) => report
                .issues
                .push(format!("Unknown DMARC policy \"{other}\"")),
            None => report
                .issues
                .push("DMARC record has no policy (\"p=\")".to_string()),
        }
        if let Some(pct) = report.percentage {
            if pct > 100 {
                report
                    .issues
                    .push("DMARC pct must be between 0 and 100".to_string());
            }
        }
        report.valid = report.issues.is_empty();
        report
    }

    /// Normalise free-form domain input to its ASCII (IDNA / punycode)
    /// form. Accepts a bare domain, a domain with a trailing dot or a
    /// port, and a full URL (scheme, path, and query are stripped and
    /// only the host is kept). Rejects empty input, IP literals, and
    /// anything containing `@` (pass an email address to
    /// `domain_from_email` instead).
    pub fn normalize_domain_input(input: &str) -> UnifiedResult<String> {
        let input = input.trim();
        if input.is_empty() {
            return Err(UnifiedError::bad_request("Domain is empty"));
        }
        if input.contains('@') {
            return Err(UnifiedError::bad_request(
                "Input looks like an email address; pass the domain part only",
            ));
        }
        let host: String = if input.contains("://") || input.starts_with("//") {
            let url_text = if input.starts_with("//") {
                format!("https:{input}")
            } else {
                input.to_string()
            };
            let url = url::Url::parse(&url_text).map_err(|_| {
                UnifiedError::bad_request(format!("Invalid URL \"{input}\""))
            })?;
            url.host_str()
                .map(str::to_string)
                .ok_or_else(|| UnifiedError::bad_request(format!("URL \"{input}\" has no host")))?
        } else if input.contains('/') || input.contains('?') || input.contains('#') || input.contains(':') {
            let url = url::Url::parse(&format!("https://{input}")).map_err(|_| {
                UnifiedError::bad_request(format!("Invalid domain name \"{input}\""))
            })?;
            url.host_str()
                .map(str::to_string)
                .ok_or_else(|| {
                    UnifiedError::bad_request(format!("Invalid domain name \"{input}\""))
                })?
        } else {
            input.to_string()
        };
        let host = host.trim_end_matches('.');
        if host.is_empty() {
            return Err(UnifiedError::bad_request("Domain is empty"));
        }
        match url::Host::parse(host) {
            Ok(url::Host::Domain(ascii)) => Ok(ascii),
            Ok(_) => Err(UnifiedError::bad_request(
                "Input is an IP address, not a domain name",
            )),
            Err(_) => Err(UnifiedError::bad_request(format!(
                "Invalid domain name \"{input}\""
            ))),
        }
    }

    /// Extract the ASCII domain from an email address. Only the simple
    /// unquoted local-part form is supported (at most 64 characters, no
    /// leading/trailing or double dots, no whitespace or specials);
    /// quoted-string local parts and comments are rejected rather than
    /// half-parsed.
    pub fn domain_from_email(email: &str) -> UnifiedResult<String> {
        let email = email.trim();
        let parts: Vec<&str> = email.split('@').collect();
        if parts.len() != 2 {
            return Err(UnifiedError::bad_request(
                "Email address must contain exactly one \"@\"",
            ));
        }
        let (local, domain) = (parts[0], parts[1]);
        if local.is_empty() {
            return Err(UnifiedError::bad_request(
                "Email address has an empty local part",
            ));
        }
        if local.len() > 64 {
            return Err(UnifiedError::bad_request(
                "Email local part exceeds 64 characters",
            ));
        }
        if local.starts_with('.') || local.ends_with('.') || local.contains("..") {
            return Err(UnifiedError::bad_request(
                "Email local part must not start or end with a dot or contain double dots",
            ));
        }
        if local.chars().any(|c| {
            c.is_whitespace()
                || matches!(
                    c,
                    '<' | '>' | '(' | ')' | ',' | ';' | ':' | '"' | '[' | ']' | '\\'
                )
        }) {
            return Err(UnifiedError::bad_request(
                "Email local part contains an invalid character",
            ));
        }
        Self::normalize_domain_input(domain)
    }

    /// Domain syntax validation: labels, total length, TLD, and IDNA /
    /// punycode conversion (via the `url` crate, which applies IDNA).
    pub fn check_domain_syntax(domain: &str) -> DomainSyntaxReport {
        let mut issues = Vec::new();
        let raw = domain.trim();
        if raw.starts_with('.') {
            issues.push("Domain must not start with a dot".to_string());
        }
        let domain = raw.trim_end_matches('.').to_lowercase();
        let unicode = if raw.chars().any(|c| !c.is_ascii()) {
            Some(domain.clone())
        } else {
            None
        };
        // Fast path for localhost: a valid local name with no TLD.
        if domain == "localhost" {
            return DomainSyntaxReport {
                valid: issues.is_empty(),
                domain,
                punycode: Some("localhost".to_string()),
                unicode,
                is_local: true,
                issues,
            };
        }
        let punycode = match url::Host::parse(&domain) {
            Ok(url::Host::Domain(ascii)) => Some(ascii),
            Ok(_) => {
                issues.push("Domain is an IP address, not a hostname".to_string());
                None
            }
            Err(_) => {
                issues.push("Domain is not a valid hostname".to_string());
                None
            }
        };
        let checked = punycode.clone().unwrap_or_else(|| domain.clone());
        if checked.is_empty() {
            issues.push("Domain is empty".to_string());
        }
        if checked.len() > 253 {
            issues.push("Domain exceeds 253 characters".to_string());
        }
        let labels: Vec<&str> = checked.split('.').collect();
        if labels.len() < 2 {
            issues.push("Domain must contain at least one dot and a TLD".to_string());
        }
        for label in &labels {
            if label.is_empty() {
                issues.push("Domain contains an empty label".to_string());
            } else if label.len() > 63 {
                issues.push(format!("Label \"{label}\" exceeds 63 characters"));
            } else if label.starts_with('-') || label.ends_with('-') {
                issues.push(format!("Label \"{label}\" starts or ends with a hyphen"));
            } else if !label
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-')
            {
                issues.push(format!("Label \"{label}\" contains invalid characters"));
            }
        }
        let mut is_local = false;
        if let Some(tld) = labels.last() {
            is_local = LOCAL_SUFFIXES.contains(tld);
            let tld_ok = (tld.len() >= 2
                && tld.chars().all(|c| c.is_ascii_alphabetic()))
                || tld.starts_with("xn--");
            if !tld_ok {
                issues.push(format!("TLD \"{tld}\" is not valid"));
            }
        }
        DomainSyntaxReport {
            valid: issues.is_empty(),
            domain: domain.clone(),
            punycode,
            unicode,
            is_local,
            issues,
        }
    }

    pub async fn dns_health(&self, domain: &str) -> DnsHealthReport {
        let mut report = DnsHealthReport {
            resolver_latency_ms: None,
            resolver_ok: false,
            ns_records: Vec::new(),
            ns_ok: false,
            a_ok: false,
            issues: Vec::new(),
        };
        let started = Instant::now();
        match self.lookup_a(domain).await {
            Ok(addresses) => {
                report.resolver_latency_ms = Some(started.elapsed().as_millis());
                report.resolver_ok = true;
                report.a_ok = !addresses.is_empty();
                if addresses.is_empty() {
                    report
                        .issues
                        .push("Domain has no A records".to_string());
                }
            }
            Err(error) => {
                report
                    .issues
                    .push(format!("Resolver failed to answer A lookup: {error}"));
            }
        }
        match self.lookup_ns(domain).await {
            Ok(ns) => {
                report.ns_records = ns;
                report.ns_ok = !report.ns_records.is_empty();
                if report.ns_records.is_empty() {
                    report
                        .issues
                        .push("Domain has no NS records".to_string());
                }
            }
            Err(error) => {
                report
                    .issues
                    .push(format!("NS lookup failed: {error}"));
            }
        }
        report
    }

    /// Full report for a domain: syntax, A / AAAA / CNAME / MX / TXT,
    /// SPF, DKIM, DMARC, and resolver health. The input may be a bare
    /// domain or a URL; it is normalised to its ASCII form first.
    /// Individual lookup failures are recorded in the report instead of
    /// failing the whole request.
    pub async fn check_domain_full(
        &self,
        domain: &str,
        selector: Option<&str>,
    ) -> UnifiedResult<DnsReport> {
        let canonical = Self::normalize_domain_input(domain)?;
        let syntax = Self::check_domain_syntax(&canonical);
        if !syntax.valid {
            return Err(UnifiedError::bad_request(format!(
                "Invalid domain \"{domain}\": {}",
                syntax.issues.join("; ")
            )));
        }
        let a = self.lookup_a(&canonical).await.unwrap_or_default();
        let aaaa = self.lookup_aaaa(&canonical).await.unwrap_or_default();
        let cname = self.lookup_cname(&canonical).await.unwrap_or(None);
        let mx = self.lookup_mx(&canonical).await.unwrap_or_default();
        let txt = self.lookup_txt(&canonical).await.unwrap_or_default();
        let spf = self.check_spf(&canonical).await;
        let dkim = self.check_dkim(&canonical, selector).await;
        let dmarc = self.check_dmarc(&canonical).await;
        let health = self.dns_health(&canonical).await;
        Ok(DnsReport {
            domain: canonical,
            syntax,
            a,
            aaaa,
            cname,
            mx,
            txt,
            spf,
            dkim,
            dmarc,
            health,
        })
    }
}

// ---------------------------------------------------------------------------
// Hook support: answer `dns.verify` (from the SaaS module) with the
// module's full DNS check. Hook handlers are synchronous but the
// verifier is async, so the check runs on a dedicated worker thread
// with its own current-thread runtime and the caller blocks on the
// join — this never touches the caller's runtime, so it cannot deadlock
// or panic on `block_in_place` the way re-entering a runtime would.
// `valid` uses the same bar as the Domains module's own verification:
// the syntax must be valid and the domain must resolve (A or AAAA).
// ---------------------------------------------------------------------------

fn run_dns_check(domain: &str, selector: Option<&str>) -> UnifiedResult<DnsReport> {
    let domain = domain.to_string();
    let selector = selector.map(str::to_string);
    let worker = std::thread::Builder::new()
        .name("dns-verify".to_string())
        .spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(|error| {
                    UnifiedError::internal(format!("Cannot start DNS check runtime: {error}"))
                })?;
            runtime.block_on(async move {
                let verifier = DnsVerifier::new();
                verifier
                    .check_domain_full(&domain, selector.as_deref())
                    .await
            })
        });
    match worker {
        Ok(handle) => match handle.join() {
            Ok(result) => result,
            Err(_) => Err(UnifiedError::internal(
                "DNS check worker thread failed",
            )),
        },
        Err(error) => Err(UnifiedError::internal(format!(
            "Cannot start DNS check worker thread: {error}"
        ))),
    }
}

fn on_dns_verify(ctx: &mut HookContext) {
    let Some(domain) = ctx
        .get("domain")
        .and_then(|value| value.as_str())
        .map(str::to_string)
    else {
        ctx.add_result(serde_json::json!({
            "valid": false,
            "message": "dns.verify requires 'domain'",
        }));
        return;
    };
    let selector = ctx
        .get("selector")
        .and_then(|value| value.as_str())
        .map(str::to_string);
    match run_dns_check(&domain, selector.as_deref()) {
        Ok(report) => {
            let valid = report.syntax.valid && (!report.a.is_empty() || !report.aaaa.is_empty());
            let mut issues = report.syntax.issues.clone();
            issues.extend(report.health.issues.iter().cloned());
            let message = if issues.is_empty() {
                None
            } else {
                Some(issues.join("; "))
            };
            ctx.add_result(serde_json::json!({
                "valid": valid,
                "report": report,
                "message": message,
            }));
        }
        Err(error) => {
            // The only error `check_domain_full` returns is an invalid
            // domain syntax: answer with a syntax-only report so the
            // caller still gets the detail.
            let syntax = DnsVerifier::check_domain_syntax(&domain);
            ctx.add_result(serde_json::json!({
                "valid": false,
                "report": { "domain": domain, "syntax": syntax },
                "message": error.message,
            }));
        }
    }
}

// ---------------------------------------------------------------------------
// Request types and handlers.
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct GeoLookupRequest {
    ip: String,
}

#[derive(Debug, Deserialize)]
struct DnsCheckRequest {
    domain: String,
    selector: Option<String>,
}

#[derive(Debug, Serialize)]
struct InterfaceView {
    name: String,
    mac_address: String,
    ip_addresses: Vec<String>,
    received_bytes: u64,
    transmitted_bytes: u64,
}

async fn network_info(ctx: web::Data<Arc<AppContext>>) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let networks = sysinfo::Networks::new_with_refreshed_list();
        let mut interfaces = Vec::new();
        for (name, data) in networks.iter() {
            interfaces.push(InterfaceView {
                name: name.clone(),
                mac_address: data.mac_address().to_string(),
                ip_addresses: data
                    .ip_networks()
                    .iter()
                    .map(|network| format!("{}/{}", network.addr, network.prefix))
                    .collect(),
                received_bytes: data.received(),
                transmitted_bytes: data.transmitted(),
            });
        }
        interfaces.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(serde_json::json!({
            "hostname": sysinfo::System::host_name(),
            "uptime_seconds": sysinfo::System::uptime(),
            "interfaces": interfaces,
        }))
    })();
    json::respond(result, debug)
}

async fn geo_lookup_handler(
    ctx: web::Data<Arc<AppContext>>,
    body: web::Json<GeoLookupRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<GeoInfo> = (|| {
        let ip = body.ip.trim().to_string();
        // Delegate to the GEO module through the hook event first.
        let emitted = ctx.hooks.emit(
            events::GEO_LOOKUP,
            serde_json::json!({ "ip": ip }),
        );
        for result_value in emitted.results {
            // `GeoInfo` is Serialize-only, so rebuild it from the JSON
            // result instead of deserialising it.
            if result_value.get("error").is_some() {
                continue;
            }
            let info = GeoInfo {
                ip: result_value
                    .get("ip")
                    .and_then(Value::as_str)
                    .unwrap_or(&ip)
                    .to_string(),
                country_code: result_value
                    .get("country_code")
                    .and_then(Value::as_str)
                    .map(str::to_string),
                country_name: result_value
                    .get("country_name")
                    .and_then(Value::as_str)
                    .map(str::to_string),
                city: result_value
                    .get("city")
                    .and_then(Value::as_str)
                    .map(str::to_string),
                region: result_value
                    .get("region")
                    .and_then(Value::as_str)
                    .map(str::to_string),
                asn: result_value.get("asn").and_then(Value::as_u64).map(|v| v as u32),
                asn_organization: result_value
                    .get("asn_organization")
                    .and_then(Value::as_str)
                    .map(str::to_string),
                latitude: result_value.get("latitude").and_then(Value::as_f64),
                longitude: result_value.get("longitude").and_then(Value::as_f64),
                source: result_value
                    .get("source")
                    .and_then(Value::as_str)
                    .unwrap_or("unknown")
                    .to_string(),
            };
            if info.source != "unknown" || info.country_code.is_some() {
                ctx.set_geo(info.clone());
                return Ok(info);
            }
        }
        // Fallback when no hook handler answered: call the GEO lookup
        // directly and still update the global Geo variable.
        let info = crate::modules::geo::lookup_geo(&ctx, &ip)?;
        ctx.set_geo(info.clone());
        Ok(info)
    })();
    json::respond(result, debug)
}

async fn connections_handler(ctx: web::Data<Arc<AppContext>>) -> HttpResponse {
    let debug = ctx.is_debug();
    let stats = connection_stats();
    let http = stats.http.load(Ordering::SeqCst);
    let websocket = stats.websocket.load(Ordering::SeqCst);
    json::respond(
        Ok::<Value, UnifiedError>(serde_json::json!({
            "http": http,
            "websocket": websocket,
            "total_active": http + websocket,
            "total_accepted": stats.total_accepted.load(Ordering::SeqCst),
            "uptime_seconds": ctx.uptime_seconds(),
        })),
        debug,
    )
}

async fn dns_check_handler(
    ctx: web::Data<Arc<AppContext>>,
    body: web::Json<DnsCheckRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<DnsReport> = (|| async {
        let verifier = DnsVerifier::new();
        let report = verifier
            .check_domain_full(body.domain.trim(), body.selector.as_deref())
            .await?;
        // Publish the report on the hook bus so the Domains, Mail, and
        // Security modules can observe every DNS check.
        ctx.hooks.emit(
            "dns.check",
            serde_json::to_value(&report).unwrap_or(Value::Null),
        );
        Ok(report)
    })()
    .await;
    json::respond(result, debug)
}

// ---------------------------------------------------------------------------
// Module definition.
// ---------------------------------------------------------------------------

pub struct NetworkModule;

impl Module for NetworkModule {
    fn id(&self) -> &'static str {
        "network"
    }

    fn name(&self) -> &'static str {
        "Network"
    }

    fn description(&self) -> &'static str {
        "Network information, active connections, Geo lookups, and the DNS verifier library (A/AAAA/CNAME/PTR/MX/TXT, SPF, DKIM, DMARC, domain syntax with IDNA, IPv4/IPv6 preference, resolver health)."
    }

    fn dependencies(&self) -> Vec<&'static str> {
        vec!["geo"]
    }

    fn register_hooks(&self, bus: &HookBus) {
        bus.register("dns.verify", "network", Arc::new(on_dns_verify));
    }

    fn configure_routes(&self, cfg: &mut actix_web::web::ServiceConfig) {
        cfg.service(
            web::scope("/api/v1/network")
                .route("/info", web::get().to(network_info))
                .route("/geo-lookup", web::post().to(geo_lookup_handler))
                .route("/connections", web::get().to(connections_handler))
                .route("/dns/check", web::post().to(dns_check_handler)),
        );
    }
}

pub fn register(registry: &ModuleRegistry) {
    registry.register(Arc::new(NetworkModule));
}
