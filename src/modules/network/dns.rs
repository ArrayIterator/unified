// network module -- DNS verifier library and the dns.verify hook handler.

use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::HookContext;
use unified_core::json;
use hickory_proto::rr::{Name, RData, RecordType};
use hickory_resolver::config::{ResolverConfig, ResolverOpts};
use hickory_resolver::name_server::TokioConnectionProvider;
use hickory_resolver::{ResolveError, ResolveErrorKind, TokioResolver};
use std::net::IpAddr;
use std::time::{Duration, Instant};

use super::*;

/// Overall wall-clock budget for one full DNS check (`run_dns_check`).
/// The per-lookup resolver timeout is 5s, but a full check runs about
/// ten sequential lookups, so without this cap a slow check could block
/// its caller far longer than 5s.
pub const DNS_CHECK_TIMEOUT: Duration = Duration::from_secs(30);

pub struct DnsVerifier {
    pub(crate) resolver: TokioResolver,
    pub(crate) ip_choice: IpChoice,
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
            resolver: TokioResolver::builder_with_config(
                ResolverConfig::default(),
                TokioConnectionProvider::default(),
            )
            .with_options(opts)
            .build(),
            ip_choice,
        }
    }

    pub fn set_ip_choice(&mut self, ip_choice: IpChoice) {
        self.ip_choice = ip_choice;
    }

    pub fn ip_choice(&self) -> IpChoice {
        self.ip_choice
    }

    pub(crate) fn dns_name(domain: &str) -> UnifiedResult<Name> {
        let fqdn = if domain.ends_with('.') {
            domain.to_string()
        } else {
            format!("{domain}.")
        };
        fqdn.parse::<Name>()
            .map_err(|_| UnifiedError::bad_request(format!("Invalid domain name \"{domain}\"")))
    }

    /// True when a resolver error only means the name or the record
    /// type does not exist (NXDOMAIN / NODATA). Callers get an empty
    /// result for those, not a 500: a missing record is a normal
    /// answer, while genuine resolver failures (timeouts, refused
    /// queries, unreachable servers) still surface as internal errors.
    pub(crate) fn is_no_records_error(error: &ResolveError) -> bool {
        matches!(
            error.kind(),
            ResolveErrorKind::Proto(proto)
                if proto.is_nx_domain() || proto.is_no_records_found()
        )
    }

    pub(crate) async fn lookup_records(
        &self,
        domain: &str,
        record_type: RecordType,
    ) -> UnifiedResult<Vec<RData>> {
        let name = Self::dns_name(domain)?;
        let lookup = match self.resolver.lookup(name, record_type).await {
            Ok(lookup) => lookup,
            Err(error) if Self::is_no_records_error(&error) => return Ok(Vec::new()),
            Err(error) => {
                return Err(UnifiedError::internal(format!(
                    "DNS lookup failed for {domain}: {error}"
                )))
            }
        };
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
    pub(crate) fn parse_policy(record: &str) -> Vec<(String, String)> {
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

    /// Pick the DKIM record out of the TXT records at
    /// `<selector>._domainkey.<domain>`: the one starting with
    /// `v=DKIM1`. Several TXT records can coexist at that name, so
    /// taking the first one (as the old code did) caused false
    /// negatives whenever the DKIM record was not first.
    pub(crate) fn select_dkim_record(records: &[String]) -> Option<&String> {
        records
            .iter()
            .find(|record| record.trim_start().starts_with("v=DKIM1"))
    }

    /// Parse a DMARC `pct` value as an unsigned integer. Parsed as
    /// `u32` (not `u8`) so out-of-range values such as `pct=300` are
    /// detected and reported instead of silently failing to parse and
    /// being treated as absent.
    pub(crate) fn parse_dmarc_percentage(value: &str) -> Option<u32> {
        value.trim().parse::<u32>().ok()
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
        let Some(record) = Self::select_dkim_record(&txt).cloned() else {
            if txt.is_empty() {
                report.issues.push(format!(
                    "No DKIM TXT record found for selector \"{selector}\""
                ));
            } else {
                report.issues.push(format!(
                    "No DKIM record (v=DKIM1) found for selector \"{selector}\""
                ));
            }
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
        // Parse `pct` as `u32` so values above 255 (which `u8` cannot
        // hold) are still detected; `percentage` keeps its `u8` shape
        // and only carries values that fit, while out-of-range values
        // are flagged below instead of vanishing.
        let pct_value = get("pct").and_then(|value| Self::parse_dmarc_percentage(&value));
        report.percentage = pct_value.and_then(|value| u8::try_from(value).ok());
        match report.policy.as_deref() {
            Some("none") | Some("quarantine") | Some("reject") => {}
            Some(other) => report
                .issues
                .push(format!("Unknown DMARC policy \"{other}\"")),
            None => report
                .issues
                .push("DMARC record has no policy (\"p=\")".to_string()),
        }
        if let Some(pct) = pct_value {
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
// with its own current-thread runtime and the caller waits on a
// channel for the result -- this never touches the caller's runtime,
// so it cannot deadlock or panic on `block_in_place` the way
// re-entering a runtime would. The wait is bounded by
// `DNS_CHECK_TIMEOUT`: a full check runs about ten sequential lookups
// at up to 5s each, so without an overall cap a slow check could block
// the hook caller / HTTP handler for far longer than the per-lookup
// timeout suggests. On timeout the worker thread is detached and
// finishes on its own; only the caller stops waiting.
// `valid` uses the same bar as the Domains module's own verification:
// the syntax must be valid and the domain must resolve (A or AAAA).
// ---------------------------------------------------------------------------

/// Wait for a worker-thread result with an overall timeout. A timeout
/// or a dead worker is an internal error; the worker itself is not
/// killed, it is simply no longer waited on.
pub(crate) fn recv_with_timeout<T>(
    rx: std::sync::mpsc::Receiver<UnifiedResult<T>>,
    timeout: Duration,
) -> UnifiedResult<T> {
    match rx.recv_timeout(timeout) {
        Ok(result) => result,
        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => Err(UnifiedError::internal(format!(
            "DNS check timed out after {} seconds",
            timeout.as_secs()
        ))),
        Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
            Err(UnifiedError::internal("DNS check worker thread failed"))
        }
    }
}

pub(crate) fn run_dns_check(domain: &str, selector: Option<&str>) -> UnifiedResult<DnsReport> {
    let domain = domain.to_string();
    let selector = selector.map(str::to_string);
    let (tx, rx) = std::sync::mpsc::channel();
    let worker = std::thread::Builder::new()
        .name("dns-verify".to_string())
        .spawn(move || {
            let result = (|| {
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
            })();
            let _ = tx.send(result);
        });
    match worker {
        Ok(_) => recv_with_timeout(rx, DNS_CHECK_TIMEOUT),
        Err(error) => Err(UnifiedError::internal(format!(
            "Cannot start DNS check worker thread: {error}"
        ))),
    }
}

pub(crate) fn on_dns_verify(ctx: &mut HookContext) {
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

#[cfg(test)]
mod tests {
    use super::*;
    use hickory_proto::op::{Query, ResponseCode};
    use hickory_proto::ProtoError;
    use hickory_resolver::ResolveError;

    #[test]
    fn dns_name_accepts_plain_domain() {
        assert!(DnsVerifier::dns_name("example.com").is_ok());
    }

    #[test]
    fn dns_name_accepts_trailing_dot_fqdn() {
        assert!(DnsVerifier::dns_name("example.com.").is_ok());
    }

    #[test]
    fn dns_name_rejects_domain_with_space() {
        assert!(DnsVerifier::dns_name("bad domain").is_err());
    }

    #[test]
    fn select_dkim_record_picks_the_dkim1_record_not_the_first() {
        let records = vec![
            "google-site-verification=abc123".to_string(),
            "v=DKIM1; k=rsa; p=MIGfMA0GCSqGSIb3DQEBAQUAA4GNADCBiQKBgQC7".to_string(),
        ];
        let found = DnsVerifier::select_dkim_record(&records).expect("DKIM record found");
        assert!(found.starts_with("v=DKIM1"));
    }

    #[test]
    fn select_dkim_record_returns_none_without_a_dkim1_record() {
        let records = vec![
            "google-site-verification=abc123".to_string(),
            "v=spf1 include:_spf.example.com -all".to_string(),
        ];
        assert!(DnsVerifier::select_dkim_record(&records).is_none());
        assert!(DnsVerifier::select_dkim_record(&[]).is_none());
    }

    #[test]
    fn select_dkim_record_tolerates_leading_whitespace() {
        let records = vec!["  v=DKIM1; k=rsa; p=ABCD".to_string()];
        assert!(DnsVerifier::select_dkim_record(&records).is_some());
    }

    #[test]
    fn parse_dmarc_percentage_accepts_in_range_values() {
        assert_eq!(DnsVerifier::parse_dmarc_percentage("0"), Some(0));
        assert_eq!(DnsVerifier::parse_dmarc_percentage("100"), Some(100));
    }

    #[test]
    fn parse_dmarc_percentage_keeps_out_of_range_values_detectable() {
        // `pct=300` cannot fit in a `u8`; parsing as `u32` keeps it so
        // `check_dmarc` can flag it instead of treating it as absent.
        assert_eq!(DnsVerifier::parse_dmarc_percentage("300"), Some(300));
    }

    #[test]
    fn parse_dmarc_percentage_rejects_non_numeric_values() {
        assert_eq!(DnsVerifier::parse_dmarc_percentage("abc"), None);
        assert_eq!(DnsVerifier::parse_dmarc_percentage(""), None);
    }

    fn no_records_error(response_code: ResponseCode) -> ResolveError {
        let query = Query::query(
            "missing.example.".parse().expect("name parses"),
            RecordType::TXT,
        );
        ResolveError::from(ProtoError::nx_error(
            Box::new(query),
            None,
            None,
            None,
            response_code,
            true,
            None,
        ))
    }

    #[test]
    fn nxdomain_is_a_no_records_error() {
        assert!(DnsVerifier::is_no_records_error(&no_records_error(
            ResponseCode::NXDomain
        )));
    }

    #[test]
    fn nodata_is_a_no_records_error() {
        assert!(DnsVerifier::is_no_records_error(&no_records_error(
            ResponseCode::NoError
        )));
    }

    #[test]
    fn generic_resolver_error_is_not_a_no_records_error() {
        let error = ResolveError::from(ResolveErrorKind::Msg("boom".to_string()));
        assert!(!DnsVerifier::is_no_records_error(&error));
    }

    #[test]
    fn dns_check_timeout_is_thirty_seconds() {
        assert_eq!(DNS_CHECK_TIMEOUT, Duration::from_secs(30));
    }

    #[test]
    fn recv_with_timeout_returns_the_worker_result() {
        let (tx, rx) = std::sync::mpsc::channel();
        tx.send(Ok::<u32, UnifiedError>(42)).expect("send succeeds");
        let value = recv_with_timeout(rx, Duration::from_secs(5)).expect("result arrives");
        assert_eq!(value, 42);
    }

    #[test]
    fn recv_with_timeout_reports_a_slow_worker() {
        // The sender stays alive but never sends, so the wait must end
        // in a timeout error instead of blocking forever.
        let (tx, rx) = std::sync::mpsc::channel::<UnifiedResult<u32>>();
        let error =
            recv_with_timeout(rx, Duration::from_millis(1)).expect_err("slow worker must time out");
        assert_eq!(error.code, 500);
        assert!(error.message.contains("timed out"));
        drop(tx);
    }

    #[test]
    fn recv_with_timeout_reports_a_dead_worker() {
        let (tx, rx) = std::sync::mpsc::channel::<UnifiedResult<u32>>();
        drop(tx);
        let error =
            recv_with_timeout(rx, Duration::from_secs(5)).expect_err("dead worker must fail");
        assert_eq!(error.code, 500);
    }
}
