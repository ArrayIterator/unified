// Dynamic per-domain TLS storage: SNI resolution, runtime certificate
// swaps, and a self-signed fallback when auto-creation is enabled.

use crate::core::config::SslSection;
use crate::core::error::{UnifiedError, UnifiedResult};
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use rcgen::{
    CertificateParams, DistinguishedName, DnType, KeyPair, SanType, PKCS_ECDSA_P256_SHA256,
};
use rustls::client::verify_server_name;
use rustls::crypto::aws_lc_rs::sign::any_supported_type;
use rustls::server::{ClientHello, ParsedCertificate, ResolvesServerCert};
use rustls::sign::CertifiedKey;
use rustls::{Error as RustlsError, ServerConfig};
use rustls_pki_types::{CertificateDer, DnsName, PrivateKeyDer, ServerName};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::collections::HashSet;
use std::io::BufReader;
use std::net::IpAddr;
use std::sync::Arc;
use x509_certificate::{CapturedX509Certificate, X509Certificate};

/// Maximum number of auto-created fallback certificates kept in memory.
const MAX_AUTO_CERTS: usize = 8192;
/// Entries evicted when the auto-created cache is full.
const AUTO_EVICT: usize = 100;
const MAX_CERT_BYTES: u64 = 50 * 1024;
const MAX_KEY_BYTES: u64 = 10 * 1024;
/// Subject Alternative Name extension OID (2.5.29.17).
const SAN_OID: &[u32] = &[2, 5, 29, 17];

/// Dynamic per-domain SSL storage, used as the rustls certificate resolver.
pub struct SslStorage {
    auto_create: bool,
    factory_key: Arc<(KeyPair, PrivateKeyDer<'static>)>,
    /// Certificates loaded from configuration, keyed by name and SAN.
    defaults: DashMap<String, Arc<CertifiedKey>>,
    /// Certificates added at runtime (add / renew).
    dynamic: DashMap<String, Arc<CertifiedKey>>,
    /// Self-signed fallback certificates created on demand. Kept apart
    /// from `dynamic` so cache eviction can never drop a configured or
    /// renewed certificate, and so fallbacks lose to both in lookups.
    auto_created: DashMap<String, Arc<CertifiedKey>>,
    /// Previous live certificate per name, saved by `swap_certificate`
    /// before a replacement goes live, so `rollback_certificate` can
    /// restore it when a swap turns out to be bad.
    previous: DashMap<String, Arc<CertifiedKey>>,
}

/// Lowercase a DNS name and convert Unicode labels to ASCII (punycode).
/// A leading `*.` wildcard prefix is preserved.
pub fn to_ascii_name(name: &str) -> String {
    let name = name.trim().trim_end_matches('.').to_ascii_lowercase();
    match name.strip_prefix("*.") {
        Some(rest) => format!("*.{}", to_ascii_host(rest)),
        None => to_ascii_host(&name),
    }
}

fn to_ascii_host(host: &str) -> String {
    if host.is_ascii() {
        return host.to_string();
    }
    match url::Url::parse(&format!("https://{host}/")) {
        Ok(url) => url.host_str().unwrap_or(host).to_string(),
        Err(_) => host.to_string(),
    }
}

fn map_get(map: &DashMap<String, Arc<CertifiedKey>>, key: &str) -> Option<Arc<CertifiedKey>> {
    map.get(key).map(|entry| entry.value().clone())
}

fn rustls_err(message: String) -> RustlsError {
    RustlsError::General(message)
}

/// Common names and SANs (DNS + IP) of a DER certificate, lowercased.
fn cert_names(der: &[u8]) -> Result<Vec<String>, String> {
    let cert = X509Certificate::from_der(der).map_err(|e| format!("invalid certificate: {e}"))?;
    let mut names = HashSet::new();
    if let Some(cn) = cert.subject_common_name() {
        names.insert(to_ascii_name(&cn));
    }
    for ext in cert.iter_extensions() {
        if ext.id.as_ref() == SAN_OID {
            let mut sans = Vec::new();
            scan_sans(&ext.value.to_bytes(), &mut sans);
            for san in sans {
                names.insert(to_ascii_name(&san));
            }
        }
    }
    Ok(names.into_iter().collect())
}

/// Walk DER TLVs and collect DNS names (tag 0x82) and IP addresses (0x87).
/// Tolerates the SAN value being wrapped in an outer SEQUENCE or not.
fn scan_sans(data: &[u8], out: &mut Vec<String>) {
    let mut i = 0;
    while i + 2 <= data.len() {
        let tag = data[i];
        let Some((len, header)) = read_len(&data[i + 1..]) else {
            return;
        };
        let start = i + 1 + header;
        let end = start + len;
        if end > data.len() {
            return;
        }
        match tag {
            0x30 => scan_sans(&data[start..end], out),
            0x82 => {
                if let Ok(name) = std::str::from_utf8(&data[start..end]) {
                    out.push(name.to_string());
                }
            }
            0x87 => out.push(ip_from_bytes(&data[start..end])),
            _ => {}
        }
        i = end;
    }
}

fn read_len(bytes: &[u8]) -> Option<(usize, usize)> {
    let first = *bytes.first()?;
    if first < 0x80 {
        return Some((first as usize, 1));
    }
    let count = (first & 0x7f) as usize;
    if count == 0 || count > 4 || bytes.len() < 1 + count {
        return None;
    }
    let mut len = 0usize;
    for byte in &bytes[1..1 + count] {
        len = (len << 8) | *byte as usize;
    }
    Some((len, 1 + count))
}

fn ip_from_bytes(bytes: &[u8]) -> String {
    match bytes.len() {
        4 => format!("{}.{}.{}.{}", bytes[0], bytes[1], bytes[2], bytes[3]),
        16 => {
            let mut raw = [0u8; 16];
            raw.copy_from_slice(bytes);
            IpAddr::from(raw).to_string()
        }
        _ => String::new(),
    }
}

/// True when the private key matches the certificate: sign a message with
/// the key and verify the signature against the certificate public key.
fn key_matches(key: &KeyPair, cert_der: &[u8]) -> bool {
    let Ok(cert) = CapturedX509Certificate::from_der(cert_der) else {
        return false;
    };
    let message = b"unified-key-match";
    let Ok(signature) = key.sign(message) else {
        return false;
    };
    cert.verify_signed_data(message, &signature).is_ok()
}

/// True when `name` is covered by one of the certificate names: exact
/// match, or a wildcard name matching exactly one label.
fn cert_covers(names: &[String], name: &str) -> bool {
    for cert_name in names {
        if cert_name == name {
            return true;
        }
        if let Some(base) = cert_name.strip_prefix("*.") {
            if name != base
                && name.ends_with(&format!(".{base}"))
                && name.matches('.').count() == base.matches('.').count() + 1
            {
                return true;
            }
        }
    }
    false
}

fn load_pem_certs(pem: &[u8]) -> Result<Vec<CertificateDer<'static>>, RustlsError> {
    let mut certs = Vec::new();
    for item in rustls_pemfile::certs(&mut BufReader::new(pem)) {
        certs.push(
            item.map_err(|e| rustls_err(format!("invalid certificate PEM: {e}")))?,
        );
    }
    if certs.is_empty() {
        return Err(rustls_err("no certificate found in PEM data".to_string()));
    }
    Ok(certs)
}

fn load_key_pair(pem: &[u8]) -> Result<KeyPair, RustlsError> {
    let text = std::str::from_utf8(pem)
        .map_err(|_| rustls_err("private key is not valid UTF-8".to_string()))?;
    KeyPair::from_pem(text).map_err(|e| rustls_err(format!("invalid private key: {e}")))
}

fn certified_key(
    certs: Vec<CertificateDer<'static>>,
    key: &KeyPair,
) -> Result<CertifiedKey, RustlsError> {
    let key_der = PrivateKeyDer::Pkcs8(key.serialize_der().into());
    let signing_key = any_supported_type(&key_der)
        .map_err(|e| rustls_err(format!("unsupported private key: {e}")))?;
    Ok(CertifiedKey::new(certs, signing_key))
}

/// Validate a certificate chain against a key: non-empty, leaf has a CN
/// or SAN, and the key matches the leaf. Returns the leaf names.
fn validate_pair(
    certs: &[CertificateDer<'static>],
    key: &KeyPair,
) -> Result<Vec<String>, String> {
    if certs.is_empty() {
        return Err("no certificates found".to_string());
    }
    let names = cert_names(certs[0].as_ref())?;
    if names.is_empty() {
        return Err("certificate has no common name or subject alternative name".to_string());
    }
    if !key_matches(key, certs[0].as_ref()) {
        return Err("private key does not match the certificate".to_string());
    }
    Ok(names)
}

fn read_capped(path: &str, max: u64) -> Result<Vec<u8>, String> {
    let size = std::fs::metadata(path)
        .map_err(|e| format!("cannot read {path}: {e}"))?
        .len();
    if size > max {
        return Err(format!("{path} exceeds the size limit"));
    }
    std::fs::read(path).map_err(|e| format!("cannot read {path}: {e}"))
}

impl ResolvesServerCert for SslStorage {
    fn resolve(&self, client_hello: ClientHello<'_>) -> Option<Arc<CertifiedKey>> {
        let name = client_hello.server_name()?;
        self.lookup(name).or_else(|| {
            if self.auto_create {
                self.create_fallback(name)
            } else {
                None
            }
        })
    }
}

impl SslStorage {
    /// Load the default certificate and every per-domain certificate from
    /// configuration. Each entry is validated (key matches certificate,
    /// certificate has a CN or SAN); all bad entries are collected and
    /// returned as one error instead of failing on the first.
    pub fn from_config(ssl: &SslSection) -> UnifiedResult<Arc<Self>> {
        let storage = Arc::new(Self {
            auto_create: ssl.auto_create,
            factory_key: Arc::new(Self::generate_factory_key()),
            defaults: DashMap::new(),
            dynamic: DashMap::new(),
            auto_created: DashMap::new(),
            previous: DashMap::new(),
        });
        let mut bad: Vec<String> = Vec::new();
        if ssl.cert.is_empty() != ssl.key.is_empty() {
            bad.push("default certificate: cert and key must be set together".to_string());
        } else if !ssl.cert.is_empty() {
            if let Err(message) = storage.load_file_pair(&ssl.cert, &ssl.key, None) {
                bad.push(format!("default certificate: {message}"));
            }
        }
        for (domain, entry) in &ssl.domains {
            if entry.cert.is_empty() && entry.key.is_empty() {
                continue;
            }
            if entry.cert.is_empty() || entry.key.is_empty() {
                bad.push(format!("{domain}: cert and key must be set together"));
            } else if let Err(message) =
                storage.load_file_pair(&entry.cert, &entry.key, Some(domain))
            {
                bad.push(format!("{domain}: {message}"));
            }
        }
        if bad.is_empty() {
            Ok(storage)
        } else {
            Err(UnifiedError::bad_request(format!(
                "Invalid SSL configuration: {}",
                bad.join("; ")
            )))
        }
    }

    fn generate_factory_key() -> (KeyPair, PrivateKeyDer<'static>) {
        let key_pair = KeyPair::generate_for(&PKCS_ECDSA_P256_SHA256)
            .expect("ECDSA P-256 key generation must not fail");
        let key_der = PrivateKeyDer::Pkcs8(key_pair.serialize_der().into());
        (key_pair, key_der)
    }

    /// Load one cert/key file pair into the defaults map, indexed by the
    /// configured domain (when given) and by every name on the certificate.
    fn load_file_pair(
        &self,
        cert_path: &str,
        key_path: &str,
        domain: Option<&str>,
    ) -> Result<(), String> {
        let cert_pem = read_capped(cert_path, MAX_CERT_BYTES)?;
        let key_pem = read_capped(key_path, MAX_KEY_BYTES)?;
        let certs = load_pem_certs(&cert_pem).map_err(|e| e.to_string())?;
        let key = load_key_pair(&key_pem).map_err(|e| e.to_string())?;
        let names = validate_pair(&certs, &key)?;
        let certified =
            Arc::new(certified_key(certs, &key).map_err(|e| e.to_string())?);
        if let Some(domain) = domain {
            self.defaults.insert(to_ascii_name(domain), certified.clone());
        }
        for name in names {
            self.defaults.insert(name, certified.clone());
        }
        Ok(())
    }

    /// Look up a certificate by SNI name: exact match first (runtime
    /// entries win over configured ones, self-signed fallbacks lose to
    /// both), then wildcard suffixes walking up one label at a time.
    pub fn lookup(&self, name: &str) -> Option<Arc<CertifiedKey>> {
        let ascii = to_ascii_name(name);
        if let Some(cert) = map_get(&self.dynamic, &ascii)
            .or_else(|| map_get(&self.defaults, &ascii))
            .or_else(|| map_get(&self.auto_created, &ascii))
        {
            return Some(cert);
        }
        if ascii.parse::<IpAddr>().is_ok() {
            return None;
        }
        let mut rest = ascii.as_str();
        while let Some((_, parent)) = rest.split_once('.') {
            let wildcard = format!("*.{parent}");
            if let Some(cert) = map_get(&self.dynamic, &wildcard)
                .or_else(|| map_get(&self.defaults, &wildcard))
                .or_else(|| map_get(&self.auto_created, &wildcard))
            {
                return Some(cert);
            }
            rest = parent;
        }
        None
    }

    /// Alias for `lookup`, for callers that already hold a name.
    pub fn get(&self, name: &str) -> Option<Arc<CertifiedKey>> {
        self.lookup(name)
    }

    /// Expiry of the stored certificate for `name`, if one is stored.
    pub fn cert_expiry(&self, name: &str) -> Option<DateTime<Utc>> {
        let cert = self.lookup(name)?;
        let parsed = X509Certificate::from_der(cert.end_entity_cert().as_ref()).ok()?;
        Some(parsed.validity_not_after())
    }

    /// DNS and IP names on the stored leaf certificate for `name`, if
    /// one is stored. Used by ACME renewal so a renewal re-requests
    /// every name on the old certificate instead of only the domain
    /// that triggered the renewal.
    pub fn certificate_names(&self, name: &str) -> Option<Vec<String>> {
        let cert = self.lookup(name)?;
        cert_names(cert.end_entity_cert().as_ref()).ok()
    }

    pub fn is_auto_create(&self) -> bool {
        self.auto_create
    }

    /// Add a certificate for `name`. The name must be a valid DNS name
    /// (or a `*.` wildcard) and the certificate must cover it.
    pub fn add(&self, name: &str, ck: CertifiedKey) -> Result<(), RustlsError> {
        if let Some(base) = name.strip_prefix("*.") {
            DnsName::try_from(base)
                .map_err(|_| rustls_err("bad DNS name".to_string()))?;
            let names = cert_names(ck.end_entity_cert().as_ref())
                .map_err(rustls_err)?;
            if !cert_covers(&names, &to_ascii_name(name)) {
                return Err(rustls_err(format!(
                    "certificate does not cover {name}"
                )));
            }
        } else {
            let checked = DnsName::try_from(name)
                .map_err(|_| rustls_err("bad DNS name".to_string()))?
                .to_lowercase_owned();
            let server_name = ServerName::DnsName(checked);
            let parsed = ParsedCertificate::try_from(ck.end_entity_cert())?;
            verify_server_name(&parsed, &server_name)?;
        }
        self.dynamic
            .insert(to_ascii_name(name), Arc::new(ck));
        Ok(())
    }

    /// Remove a runtime certificate (configured defaults are untouched).
    /// A self-signed fallback cached for the same name is removed too.
    pub fn remove(&self, name: &str) {
        let ascii = to_ascii_name(name);
        self.dynamic.remove(&ascii);
        self.auto_created.remove(&ascii);
    }

    /// Replace the certificate for `name` with new PEM data. The key must
    /// match the new certificate and the certificate must cover `name`.
    pub fn renew(&self, name: &str, cert_pem: &[u8], key_pem: &[u8]) -> UnifiedResult<()> {
        let certs = load_pem_certs(cert_pem)
            .map_err(|e| UnifiedError::bad_request(format!("invalid certificate: {e}")))?;
        let key = load_key_pair(key_pem)
            .map_err(|e| UnifiedError::bad_request(format!("invalid private key: {e}")))?;
        let names = validate_pair(&certs, &key).map_err(UnifiedError::bad_request)?;
        let ascii = to_ascii_name(name);
        if !cert_covers(&names, &ascii) {
            return Err(UnifiedError::bad_request(format!(
                "new certificate does not cover {name}"
            )));
        }
        let certified = certified_key(certs, &key)
            .map_err(|e| UnifiedError::internal(format!("cannot use private key: {e}")))?;
        self.dynamic.insert(ascii, Arc::new(certified));
        Ok(())
    }

    /// Generate a self-signed fallback certificate for `name` (covering
    /// the name and its one-label wildcard) and cache it separately
    /// from configured and renewed certificates, so evicting old
    /// fallbacks can never drop a real certificate.
    fn create_fallback(&self, name: &str) -> Option<Arc<CertifiedKey>> {
        let ascii = to_ascii_name(name);
        let certified = Arc::new(self.generate_self_signed(&ascii).ok()?);
        if self.auto_created.len() >= MAX_AUTO_CERTS {
            let evict: Vec<String> = self
                .auto_created
                .iter()
                .take(AUTO_EVICT)
                .map(|entry| entry.key().clone())
                .collect();
            for key in evict {
                self.auto_created.remove(&key);
            }
        }
        self.auto_created.insert(ascii.clone(), certified.clone());
        if ascii.parse::<IpAddr>().is_err() {
            if let Some((_, parent)) = ascii.split_once('.') {
                self.auto_created
                    .insert(format!("*.{parent}"), certified.clone());
            }
        }
        Some(certified)
    }

    fn generate_self_signed(&self, name: &str) -> Result<CertifiedKey, RustlsError> {
        let (key_pair, _) = &*self.factory_key;
        let mut params = CertificateParams::default();
        params.distinguished_name = DistinguishedName::new();
        let base = name.strip_prefix("*.").unwrap_or(name);
        if let Ok(ip) = base.parse::<IpAddr>() {
            params
                .distinguished_name
                .push(DnType::CommonName, base);
            params.subject_alt_names = vec![SanType::IpAddress(ip)];
        } else {
            params
                .distinguished_name
                .push(DnType::CommonName, base);
            let dns = |value: &str| {
                value
                    .to_string()
                    .try_into()
                    .map_err(|_| rustls_err(format!("invalid DNS name: {value}")))
            };
            params.subject_alt_names = vec![
                SanType::DnsName(dns(base)?),
                SanType::DnsName(dns(&format!("*.{base}"))?),
            ];
        }
        params.is_ca = rcgen::IsCa::NoCa;
        let cert = params
            .self_signed(key_pair)
            .map_err(|e| rustls_err(format!("self-signed certificate failed: {e}")))?;
        certified_key(vec![cert.der().clone()], key_pair)
    }
}

/// Build a TLS server configuration that resolves certificates through
/// the given storage. No client authentication; ALPN offers h2 then http/1.1.
pub fn server_config(storage: &Arc<SslStorage>) -> UnifiedResult<ServerConfig> {
    let provider = match rustls::crypto::CryptoProvider::get_default() {
        Some(provider) => Arc::new(provider.clone()),
        None => Arc::new(rustls::crypto::aws_lc_rs::default_provider()),
    };
    let mut config = ServerConfig::builder_with_provider(provider)
        .with_protocol_versions(&[&rustls::version::TLS13, &rustls::version::TLS12])
        .map_err(|e| UnifiedError::internal(format!("TLS setup failed: {e}")))?
        .with_no_client_auth()
        .with_cert_resolver(Arc::clone(storage));
    config.alpn_protocols = vec![b"h2".to_vec(), b"http/1.1".to_vec()];
    Ok(config)
}

// ---------------------------------------------------------------------------
// Advanced certificate management: inspection, config validation,
// swap with rollback, and revocation.
//
// These helpers extend the storage above without changing any of its
// original behaviour: `from_config` still fails startup on a bad
// configured certificate, while `from_config_lenient` loads every valid
// entry, reports every invalid one with per-domain reasons, and lets
// the server keep running on the self-signed fallback for the bad
// domains. Private keys are never included in any value returned here:
// callers only ever see fingerprints and metadata.
// ---------------------------------------------------------------------------

/// Days before expiry at which a certificate counts as `expiring`.
pub const CERT_EXPIRING_DAYS: i64 = 30;

/// Status of one certificate: `valid`, `expiring`, `expired`,
/// `invalid`, or `self-signed`.
pub fn status_for(days_remaining: i64, self_signed: bool) -> &'static str {
    if days_remaining < 0 {
        "expired"
    } else if self_signed {
        "self-signed"
    } else if days_remaining <= CERT_EXPIRING_DAYS {
        "expiring"
    } else {
        "valid"
    }
}

/// SHA-256 fingerprint of one DER certificate, lowercase hex.
pub fn fingerprint_sha256(der: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(der);
    let digest = hasher.finalize();
    let mut out = String::with_capacity(digest.len() * 2);
    for byte in digest {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

/// Public metadata of one certificate. No private key material is ever
/// present here: only the fingerprint and the certificate metadata.
#[derive(Debug, Clone, Serialize)]
pub struct CertificateInfo {
    pub domain: String,
    pub subject: String,
    pub issuer: String,
    /// Common name plus every DNS / IP subject alternative name.
    pub sans: Vec<String>,
    pub not_before: String,
    pub not_after: String,
    pub days_remaining: i64,
    pub fingerprint_sha256: String,
    pub self_signed: bool,
    pub chain_length: usize,
    pub chain_complete: bool,
    /// `Some(true/false)` when a private key was checked against the
    /// leaf, `None` when no key was available (stored certificates).
    pub key_matches: Option<bool>,
    pub status: String,
    /// `configured`, `dynamic`, `fallback`, or `custom`.
    pub source: String,
}

/// Validation report for one configured or uploaded certificate.
#[derive(Debug, Clone, Serialize)]
pub struct CertificateValidation {
    pub domain: String,
    pub valid: bool,
    pub reasons: Vec<String>,
    pub info: Option<CertificateInfo>,
}

/// Check one parsed chain: every certificate must parse, and the chain
/// is complete when the leaf is self-signed or when at least one
/// intermediate is present and the certificates are ordered (the
/// issuer common name of each certificate matches the subject common
/// name of the next one). The common-name comparison is a heuristic:
/// authorities that omit the common name fall back to "ordered" when
/// both names are absent. Returns `(complete, reasons)`.
pub fn check_chain(certs: &[CertificateDer<'static>]) -> (bool, Vec<String>) {
    let mut reasons: Vec<String> = Vec::new();
    if certs.is_empty() {
        return (false, vec!["no certificates found".to_string()]);
    }
    let mut parsed: Vec<X509Certificate> = Vec::with_capacity(certs.len());
    for (index, der) in certs.iter().enumerate() {
        match X509Certificate::from_der(der.as_ref()) {
            Ok(cert) => parsed.push(cert),
            Err(error) => reasons.push(format!(
                "certificate {index} in the chain cannot be parsed: {error}"
            )),
        }
    }
    if !reasons.is_empty() {
        return (false, reasons);
    }
    let leaf = &parsed[0];
    let leaf_subject = leaf.subject_common_name();
    let leaf_issuer = leaf.issuer_common_name();
    let leaf_self_signed = leaf_subject.is_some()
        && leaf_subject == leaf_issuer;
    if parsed.len() == 1 {
        if leaf_self_signed {
            return (true, Vec::new());
        }
        return (
            false,
            vec![
                "chain is incomplete: the leaf certificate has no intermediate certificate bundled"
                    .to_string(),
            ],
        );
    }
    for index in 0..parsed.len() - 1 {
        let issuer = parsed[index].issuer_common_name();
        let subject = parsed[index + 1].subject_common_name();
        if let (Some(issuer), Some(subject)) = (issuer, subject) {
            if issuer != subject {
                reasons.push(format!(
                    "chain order problem: certificate {index} was issued by \"{issuer}\" but certificate {} belongs to \"{subject}\"",
                    index + 1
                ));
            }
        }
    }
    (reasons.is_empty(), reasons)
}

/// Build the public metadata of one leaf certificate.
fn info_from_leaf(
    domain: &str,
    der: &[u8],
    chain_length: usize,
    chain_complete: bool,
    key_matches: Option<bool>,
    source: &str,
) -> Result<CertificateInfo, String> {
    let cert = X509Certificate::from_der(der).map_err(|e| format!("invalid certificate: {e}"))?;
    let subject = cert.subject_common_name().unwrap_or_default();
    let issuer = cert.issuer_common_name().unwrap_or_default();
    let not_before = cert.validity_not_before();
    let not_after = cert.validity_not_after();
    let days_remaining = not_after.signed_duration_since(Utc::now()).num_days();
    let self_signed = !subject.is_empty() && subject == issuer;
    Ok(CertificateInfo {
        domain: to_ascii_name(domain),
        subject,
        issuer,
        sans: cert_names(der).unwrap_or_default(),
        not_before: not_before.to_rfc3339(),
        not_after: not_after.to_rfc3339(),
        days_remaining,
        fingerprint_sha256: fingerprint_sha256(der),
        self_signed,
        chain_length,
        chain_complete,
        key_matches,
        status: status_for(days_remaining, self_signed).to_string(),
        source: source.to_string(),
    })
}

/// Validate one certificate/key PEM pair for `expected_domain`.
/// Collects every problem instead of failing on the first: key
/// mismatch, expiry, not-yet-valid, SANs not covering the domain, and
/// an incomplete or mis-ordered chain.
pub fn validate_pem_pair(
    cert_pem: &[u8],
    key_pem: &[u8],
    expected_domain: Option<&str>,
) -> CertificateValidation {
    let domain = expected_domain.unwrap_or("").to_string();
    let mut reasons: Vec<String> = Vec::new();
    let certs = match load_pem_certs(cert_pem) {
        Ok(certs) => certs,
        Err(error) => {
            return CertificateValidation {
                domain,
                valid: false,
                reasons: vec![format!("invalid certificate PEM: {error}")],
                info: None,
            }
        }
    };
    let key = match load_key_pair(key_pem) {
        Ok(key) => key,
        Err(error) => {
            return CertificateValidation {
                domain,
                valid: false,
                reasons: vec![format!("invalid private key: {error}")],
                info: None,
            }
        }
    };
    let (chain_complete, mut chain_reasons) = check_chain(&certs);
    reasons.append(&mut chain_reasons);
    let names = match cert_names(certs[0].as_ref()) {
        Ok(names) => names,
        Err(error) => {
            reasons.push(error);
            Vec::new()
        }
    };
    if names.is_empty() {
        reasons.push(
            "certificate has no common name or subject alternative name".to_string(),
        );
    }
    let matches = key_matches(&key, certs[0].as_ref());
    if !matches {
        reasons.push("private key does not match the certificate".to_string());
    }
    if let Some(expected) = expected_domain {
        let ascii = to_ascii_name(expected);
        if !names.is_empty() && !cert_covers(&names, &ascii) {
            reasons.push(format!(
                "certificate subject alternative names ({}) do not cover {expected}",
                names.join(", ")
            ));
        }
    }
    let info = info_from_leaf(
        &domain,
        certs[0].as_ref(),
        certs.len(),
        chain_complete,
        Some(matches),
        "custom",
    )
    .ok();
    if let Some(info) = &info {
        if info.days_remaining < 0 {
            reasons.push(format!("certificate expired on {}", info.not_after));
        }
        if let Ok(not_before) = DateTime::parse_from_rfc3339(&info.not_before) {
            if not_before > Utc::now().into() {
                reasons.push(format!(
                    "certificate is not yet valid (valid from {})",
                    info.not_before
                ));
            }
        }
    }
    let valid = reasons.is_empty();
    CertificateValidation {
        domain,
        valid,
        reasons,
        info,
    }
}

/// Validate one certificate PEM without a private key (key-match is
/// reported as unknown).
pub fn validate_cert_pem(
    cert_pem: &[u8],
    expected_domain: Option<&str>,
) -> CertificateValidation {
    let domain = expected_domain.unwrap_or("").to_string();
    let certs = match load_pem_certs(cert_pem) {
        Ok(certs) => certs,
        Err(error) => {
            return CertificateValidation {
                domain,
                valid: false,
                reasons: vec![format!("invalid certificate PEM: {error}")],
                info: None,
            }
        }
    };
    let mut reasons: Vec<String> = Vec::new();
    let (chain_complete, mut chain_reasons) = check_chain(&certs);
    reasons.append(&mut chain_reasons);
    let names = cert_names(certs[0].as_ref()).unwrap_or_default();
    if names.is_empty() {
        reasons.push(
            "certificate has no common name or subject alternative name".to_string(),
        );
    }
    if let Some(expected) = expected_domain {
        let ascii = to_ascii_name(expected);
        if !names.is_empty() && !cert_covers(&names, &ascii) {
            reasons.push(format!(
                "certificate subject alternative names ({}) do not cover {expected}",
                names.join(", ")
            ));
        }
    }
    let info = info_from_leaf(
        &domain,
        certs[0].as_ref(),
        certs.len(),
        chain_complete,
        None,
        "custom",
    )
    .ok();
    if let Some(info) = &info {
        if info.days_remaining < 0 {
            reasons.push(format!("certificate expired on {}", info.not_after));
        }
    }
    let valid = reasons.is_empty();
    CertificateValidation {
        domain,
        valid,
        reasons,
        info,
    }
}

/// Validate one configured cert/key file pair for `domain`.
pub fn validate_config_entry(domain: &str, cert_path: &str, key_path: &str) -> CertificateValidation {
    let cert_pem = match read_capped(cert_path, MAX_CERT_BYTES) {
        Ok(pem) => pem,
        Err(message) => {
            return CertificateValidation {
                domain: domain.to_string(),
                valid: false,
                reasons: vec![message],
                info: None,
            }
        }
    };
    let key_pem = match read_capped(key_path, MAX_KEY_BYTES) {
        Ok(pem) => pem,
        Err(message) => {
            return CertificateValidation {
                domain: domain.to_string(),
                valid: false,
                reasons: vec![message],
                info: None,
            }
        }
    };
    validate_pem_pair(&cert_pem, &key_pem, Some(domain))
}

/// Validate every configured certificate: the default pair (reported
/// under the domain `default`) and every per-domain entry. Entries
/// with only one of cert/key set are reported, not skipped. The
/// result is sorted by domain so reports are deterministic.
pub fn validate_ssl_config(ssl: &SslSection) -> Vec<CertificateValidation> {
    let mut reports: Vec<CertificateValidation> = Vec::new();
    if ssl.cert.is_empty() != ssl.key.is_empty() {
        reports.push(CertificateValidation {
            domain: "default".to_string(),
            valid: false,
            reasons: vec!["default certificate: cert and key must be set together".to_string()],
            info: None,
        });
    } else if !ssl.cert.is_empty() {
        reports.push(validate_config_entry("default", &ssl.cert, &ssl.key));
    }
    let mut domains: Vec<(&String, &crate::core::config::DomainSsl)> =
        ssl.domains.iter().collect();
    domains.sort_by(|a, b| a.0.cmp(b.0));
    for (domain, entry) in domains {
        if entry.cert.is_empty() && entry.key.is_empty() {
            continue;
        }
        if entry.cert.is_empty() || entry.key.is_empty() {
            reports.push(CertificateValidation {
                domain: domain.clone(),
                valid: false,
                reasons: vec![format!("{domain}: cert and key must be set together")],
                info: None,
            });
        } else {
            reports.push(validate_config_entry(domain, &entry.cert, &entry.key));
        }
    }
    reports
}

impl SslStorage {
    /// Lenient loader: loads every valid configured certificate and
    /// returns the storage together with the validation report for
    /// every configured entry. Invalid entries are reported, never
    /// loaded, and never fail startup: SNI lookups for those domains
    /// fall back to the self-signed certificate when `auto_create`
    /// is enabled (see `create_fallback`), exactly as for a domain
    /// with no certificate at all.
    pub fn from_config_lenient(ssl: &SslSection) -> (Arc<Self>, Vec<CertificateValidation>) {
        let storage = Arc::new(Self {
            auto_create: ssl.auto_create,
            factory_key: Arc::new(Self::generate_factory_key()),
            defaults: DashMap::new(),
            dynamic: DashMap::new(),
            auto_created: DashMap::new(),
            previous: DashMap::new(),
        });
        let reports = validate_ssl_config(ssl);
        let valid: std::collections::HashSet<String> = reports
            .iter()
            .filter(|report| report.valid)
            .map(|report| report.domain.clone())
            .collect();
        if valid.contains("default") && !ssl.cert.is_empty() {
            let _ = storage.load_file_pair(&ssl.cert, &ssl.key, None);
        }
        for (domain, entry) in &ssl.domains {
            if valid.contains(domain) {
                let _ = storage.load_file_pair(&entry.cert, &entry.key, Some(domain));
            }
        }
        (storage, reports)
    }

    /// Public metadata of the live certificate for `name`, if one is
    /// stored (configured, dynamic, or self-signed fallback).
    pub fn certificate_info(&self, name: &str) -> Option<CertificateInfo> {
        let ascii = to_ascii_name(name);
        let source = if self.dynamic.contains_key(&ascii) {
            "dynamic"
        } else if self.defaults.contains_key(&ascii) {
            "configured"
        } else if self.auto_created.contains_key(&ascii) {
            "fallback"
        } else {
            // Wildcard lookup may have matched a parent entry.
            "dynamic"
        };
        let cert = self.lookup(name)?;
        let leaf = cert.end_entity_cert();
        let chain_length = cert.cert.len();
        let info = info_from_leaf(
            name,
            leaf.as_ref(),
            chain_length,
            chain_length > 1,
            None,
            source,
        )
        .ok()?;
        Some(CertificateInfo {
            chain_complete: chain_length > 1 || info.self_signed,
            ..info
        })
    }

    /// Public metadata of every stored certificate, de-duplicated by
    /// name. Runtime entries win over configured ones, and both win
    /// over self-signed fallbacks (the same precedence as `lookup`).
    pub fn list_certificates(&self) -> Vec<CertificateInfo> {
        let mut names: std::collections::HashSet<String> = std::collections::HashSet::new();
        for map in [&self.dynamic, &self.defaults, &self.auto_created] {
            for entry in map.iter() {
                names.insert(entry.key().clone());
            }
        }
        let mut out: Vec<CertificateInfo> = names
            .iter()
            .filter_map(|name| self.certificate_info(name))
            .collect();
        out.sort_by(|a, b| a.domain.cmp(&b.domain));
        out
    }

    /// Days until the stored certificate for `name` expires, if one
    /// is stored. Negative when already expired.
    pub fn days_until_expiry(&self, name: &str) -> Option<i64> {
        self.certificate_info(name).map(|info| info.days_remaining)
    }

    /// Public metadata of the previous live certificate for `name`,
    /// saved by the last swap, if one exists.
    pub fn previous_info(&self, name: &str) -> Option<CertificateInfo> {
        let ascii = to_ascii_name(name);
        let previous = self.previous.get(&ascii).map(|entry| entry.value().clone())?;
        let leaf = previous.end_entity_cert();
        info_from_leaf(
            name,
            leaf.as_ref(),
            previous.cert.len(),
            previous.cert.len() > 1,
            None,
            "previous",
        )
        .ok()
    }

    /// Replace the live certificate for `name` with new PEM data. The
    /// replacement is fully validated first (key matches, covers the
    /// name, parses); only then is the current live certificate saved
    /// as the rollback target and the new one installed. A validation
    /// failure leaves the live certificate untouched.
    pub fn swap_certificate(
        &self,
        name: &str,
        cert_pem: &[u8],
        key_pem: &[u8],
    ) -> UnifiedResult<()> {
        let certs = load_pem_certs(cert_pem)
            .map_err(|e| UnifiedError::bad_request(format!("invalid certificate: {e}")))?;
        let key = load_key_pair(key_pem)
            .map_err(|e| UnifiedError::bad_request(format!("invalid private key: {e}")))?;
        let names = validate_pair(&certs, &key).map_err(UnifiedError::bad_request)?;
        let ascii = to_ascii_name(name);
        if !cert_covers(&names, &ascii) {
            return Err(UnifiedError::bad_request(format!(
                "new certificate does not cover {name}"
            )));
        }
        let certified = certified_key(certs, &key)
            .map_err(|e| UnifiedError::internal(format!("cannot use private key: {e}")))?;
        let certified = Arc::new(certified);
        if let Some(current) = self.lookup(name) {
            self.previous.insert(ascii.clone(), current);
        }
        self.dynamic.insert(ascii, certified);
        Ok(())
    }

    /// Restore the previous live certificate for `name`, saved by the
    /// last `swap_certificate`. Fails honestly when no previous
    /// certificate exists.
    pub fn rollback_certificate(&self, name: &str) -> UnifiedResult<()> {
        let ascii = to_ascii_name(name);
        let previous = self
            .previous
            .get(&ascii)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| {
                UnifiedError::not_found(format!(
                    "no previous certificate stored for {name}; nothing to roll back to"
                ))
            })?;
        self.dynamic.insert(ascii, previous);
        Ok(())
    }

    /// Revoke the live certificate for `name`: stop serving it and
    /// return its metadata for the revocation record. Runtime and
    /// fallback certificates are removed; a certificate that only
    /// exists because it is configured in `app.ssl.domains` is
    /// config-owned, so revoking it requires removing it from the
    /// configuration instead of silently dropping the live entry.
    /// After revocation, SNI for the domain falls back to the
    /// self-signed certificate when `auto_create` is enabled, so the
    /// server keeps serving the domain.
    pub fn revoke_live(&self, name: &str) -> UnifiedResult<CertificateInfo> {
        let ascii = to_ascii_name(name);
        let info = self.certificate_info(name).ok_or_else(|| {
            UnifiedError::not_found(format!("no certificate stored for {name}"))
        })?;
        let in_dynamic = self.dynamic.contains_key(&ascii)
            || self.dynamic.iter().any(|entry| {
                cert_names_from_key(entry.value())
                    .map(|names| names.iter().any(|n| n == &ascii))
                    .unwrap_or(false)
            });
        let in_fallback = self.auto_created.contains_key(&ascii);
        if in_dynamic || in_fallback {
            self.dynamic.remove(&ascii);
            self.auto_created.remove(&ascii);
            return Ok(info);
        }
        if self.defaults.contains_key(&ascii) {
            return Err(UnifiedError::conflict(format!(
                "certificate for {name} is configured in app.ssl.domains; remove it from the configuration to revoke it"
            )));
        }
        Err(UnifiedError::not_found(format!(
            "no revocable certificate stored for {name}"
        )))
    }
}

/// Names on the leaf of an already-parsed certified key.
fn cert_names_from_key(key: &CertifiedKey) -> Option<Vec<String>> {
    cert_names(key.end_entity_cert().as_ref()).ok()
}
