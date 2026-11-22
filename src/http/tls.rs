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
    /// Certificates added at runtime (add / renew / auto-created).
    dynamic: DashMap<String, Arc<CertifiedKey>>,
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
    /// entries win over configured ones), then wildcard suffixes.
    pub fn lookup(&self, name: &str) -> Option<Arc<CertifiedKey>> {
        let ascii = to_ascii_name(name);
        if let Some(cert) = map_get(&self.dynamic, &ascii)
            .or_else(|| map_get(&self.defaults, &ascii))
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
    pub fn remove(&self, name: &str) {
        let ascii = to_ascii_name(name);
        self.dynamic.remove(&ascii);
        self.defaults.remove(&ascii);
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
    /// the name and its one-label wildcard) and cache it.
    fn create_fallback(&self, name: &str) -> Option<Arc<CertifiedKey>> {
        let ascii = to_ascii_name(name);
        let certified = Arc::new(self.generate_self_signed(&ascii).ok()?);
        if self.dynamic.len() >= MAX_AUTO_CERTS {
            let evict: Vec<String> = self
                .dynamic
                .iter()
                .take(AUTO_EVICT)
                .map(|entry| entry.key().clone())
                .collect();
            for key in evict {
                self.dynamic.remove(&key);
            }
        }
        self.dynamic.insert(ascii.clone(), certified.clone());
        if ascii.parse::<IpAddr>().is_err() {
            if let Some((_, parent)) = ascii.split_once('.') {
                self.dynamic
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
