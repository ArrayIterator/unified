//! TLS certificate inspection and the SSL checker tool.

use unified_core::context::{AppContext};
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::json;
use unified_core::sharing;
use chrono::{DateTime, Utc};
use rustls::client::danger::ServerCertVerifier;
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{ClientConfig, ClientConnection, DigitallySignedStruct, SignatureScheme};
use serde_json::{json, Value};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::sync::{Arc};
use std::time::{Duration, Instant};
use super::types::ToolResult;
use super::validation::{clamp_connect_timeout, resolve_and_guard, validate_domain};

// SSL/TLS checker.

/// Accept-any verifier used ONLY by the inspection tool: the checker
/// must report untrusted or expired chains, not refuse the handshake.
/// Chain trust is reported separately in the `validation_notes`.
/// (Same shape as the domains module's `AcceptAnyCertificate`.)
#[derive(Debug)]
pub(crate) struct AcceptAnyCertificate;

impl ServerCertVerifier for AcceptAnyCertificate {
    fn verify_server_cert(
        &self,
        _end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        Ok(rustls::client::danger::ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
    }

    fn verify_tls13_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        vec![
            SignatureScheme::ECDSA_NISTP256_SHA256,
            SignatureScheme::ECDSA_NISTP384_SHA384,
            SignatureScheme::ECDSA_NISTP521_SHA512,
            SignatureScheme::ED25519,
            SignatureScheme::RSA_PSS_SHA256,
            SignatureScheme::RSA_PSS_SHA384,
            SignatureScheme::RSA_PSS_SHA512,
            SignatureScheme::RSA_PKCS1_SHA256,
            SignatureScheme::RSA_PKCS1_SHA384,
            SignatureScheme::RSA_PKCS1_SHA512,
        ]
    }
}

/// Read one TLV (tag + length + value) from `der` starting at
/// `offset`. Returns (tag, value slice, next offset).
pub(crate) fn der_tlv<'a>(der: &'a [u8], offset: usize) -> Option<(u8, &'a [u8], usize)> {
    if offset + 2 > der.len() {
        return None;
    }
    let tag = der[offset];
    let mut pos = offset + 1;
    let first_len = der[pos] as usize;
    pos += 1;
    let len = if first_len & 0x80 == 0 {
        first_len
    } else {
        let bytes = first_len & 0x7f;
        if bytes == 0 || bytes > 4 || pos + bytes > der.len() {
            return None;
        }
        let mut len = 0usize;
        for byte in &der[pos..pos + bytes] {
            len = (len << 8) | (*byte as usize);
        }
        pos += bytes;
        len
    };
    if pos + len > der.len() {
        return None;
    }
    Some((tag, &der[pos..pos + len], pos + len))
}

/// Extract DNS and IP subject alternative names from a DER
/// certificate with a small bounded walk: find the SAN extension
/// (OID 2.5.29.17), then read its GeneralNames. dNSName is context
/// tag [2] (0x82), iPAddress is [7] (0x87). Returns an empty list
/// when the extension is absent or unparseable.
pub(crate) fn extract_sans(der: &[u8]) -> Vec<String> {
    // OID 2.5.29.17 encoded: 06 03 55 1D 11
    let oid: [u8; 5] = [0x06, 0x03, 0x55, 0x1d, 0x11];
    let mut sans: Vec<String> = Vec::new();
    let mut search_from = 0usize;
    while let Some(found) = der[search_from..]
        .windows(oid.len())
        .position(|window| window == oid)
    {
        let oid_pos = search_from + found;
        // The extension value is the next TLV after the OID; it is an
        // OCTET STRING wrapping a SEQUENCE of GeneralNames.
        let mut pos = oid_pos + oid.len();
        // Optional critical BOOLEAN may sit between OID and value.
        if let Some((tag, _value, next)) = der_tlv(der, pos) {
            if tag == 0x01 {
                pos = next;
            }
        }
        let Some((tag, value, _next)) = der_tlv(der, pos) else {
            break;
        };
        if tag != 0x04 {
            search_from = oid_pos + oid.len();
            continue;
        }
        // `value` is the DER of the SEQUENCE of GeneralNames.
        let Some((seq_tag, seq_value, _)) = der_tlv(value, 0) else {
            break;
        };
        if seq_tag != 0x30 {
            break;
        }
        let mut inner = 0usize;
        while let Some((tag, name_value, next)) = der_tlv(seq_value, inner) {
            match tag {
                0x82 => {
                    if let Ok(name) = std::str::from_utf8(name_value) {
                        sans.push(name.to_string());
                    }
                }
                0x87 => {
                    let ip = match name_value.len() {
                        4 => IpAddr::V4(Ipv4Addr::new(
                            name_value[0],
                            name_value[1],
                            name_value[2],
                            name_value[3],
                        ))
                        .to_string(),
                        16 => {
                            let mut octets = [0u8; 16];
                            octets.copy_from_slice(name_value);
                            IpAddr::V6(Ipv6Addr::from(octets)).to_string()
                        }
                        _ => String::new(),
                    };
                    if !ip.is_empty() {
                        sans.push(ip);
                    }
                }
                _ => {}
            }
            inner = next;
        }
        break;
    }
    sans.sort();
    sans.dedup();
    sans
}

pub(crate) fn parse_certificate_der(der: &[u8]) -> Value {
    let fingerprint = sharing::sha256_hex_bytes(der);
    let Ok(certificate) = x509_certificate::X509Certificate::from_der(der) else {
        return json!({
            "fingerprint_sha256": fingerprint,
            "parse_error": "Certificate DER could not be parsed",
        });
    };
    let tbs = &certificate.tbs_certificate();
    let not_before =
        DateTime::<Utc>::from(tbs.validity.not_before.clone()).to_rfc3339();
    let not_after = DateTime::<Utc>::from(tbs.validity.not_after.clone()).to_rfc3339();
    let serial = tbs
        .serial_number
        .as_slice()
        .iter()
        .map(|byte| format!("{byte:02X}"))
        .collect::<String>();
    json!({
        "subject": format!("{:?}", tbs.subject),
        "issuer": format!("{:?}", tbs.issuer),
        "serial": serial,
        "not_before": not_before,
        "not_after": not_after,
        "sans": extract_sans(der),
        "fingerprint_sha256": fingerprint,
    })
}

/// Blocking TLS handshake (same synchronous shape as the domains
/// module's probe): TCP connect, rustls handshake with an
/// accept-any verifier, then read the peer chain.
pub(crate) fn tls_handshake_blocking(
    addr: SocketAddr,
    server_name_text: &str,
    connect_timeout: Duration,
) -> UnifiedResult<(Vec<Vec<u8>>, Option<String>, Option<String>)> {
    let mut stream = std::net::TcpStream::connect_timeout(&addr, connect_timeout)
        .map_err(|error| UnifiedError::bad_request(format!("TCP connect failed: {error}")))?;
    let _ = stream.set_read_timeout(Some(connect_timeout));
    let _ = stream.set_write_timeout(Some(connect_timeout));
    let config = Arc::new(
        ClientConfig::builder()
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(AcceptAnyCertificate))
            .with_no_client_auth(),
    );
    let server_name: ServerName = server_name_text
        .to_owned()
        .try_into()
        .map_err(|_| UnifiedError::bad_request("Invalid TLS server name"))?;
    let mut connection = ClientConnection::new(config, server_name)
        .map_err(|error| UnifiedError::internal(format!("Cannot start TLS handshake: {error}")))?;
    while connection.is_handshaking() {
        connection
            .complete_io(&mut stream)
            .map_err(|error| UnifiedError::bad_request(format!("TLS handshake failed: {error}")))?;
    }
    let cipher = connection
        .negotiated_cipher_suite()
        .map(|suite| format!("{:?}", suite.suite()));
    let certs: Vec<Vec<u8>> = connection
        .peer_certificates()
        .unwrap_or_default()
        .iter()
        .map(|cert| cert.to_vec())
        .collect();
    // rustls 0.23 exposes the negotiated version through
    // `connection.protocol_version()`; fall back to the cipher suite
    // string when it is unavailable.
    let version = format!("{:?}", connection.protocol_version());
    Ok((certs, Some(version), cipher))
}

pub(crate) async fn tool_ssl_check(
    ctx: &AppContext,
    target: &str,
    port: u16,
    timeout_secs: u64,
    allow_private: bool,
) -> UnifiedResult<ToolResult> {
    let started = Instant::now();
    let domain = validate_domain(target)?;
    let connect_timeout = clamp_connect_timeout(Some(timeout_secs));
    let addrs = resolve_and_guard(&domain, allow_private).await?;
    // Reuse the certificates module's live data first for domains
    // the server already manages: no network probe is needed then.
    let mut managed: Option<Value> = None;
    if let Some(storage) = ctx.tls() {
        for info in storage.list_certificates() {
            if info.domain.eq_ignore_ascii_case(&domain) {
                managed = Some(json!({
                    "domain": info.domain,
                    "subject": info.subject,
                    "issuer": info.issuer,
                    "sans": info.sans,
                    "not_before": info.not_before,
                    "not_after": info.not_after,
                    "days_remaining": info.days_remaining,
                    "fingerprint_sha256": info.fingerprint_sha256,
                    "self_signed": info.self_signed,
                    "chain_length": info.chain_length,
                    "chain_complete": info.chain_complete,
                    "source": "certificates module (managed domain)",
                }));
                break;
            }
        }
    }
    let addr = SocketAddr::new(addrs[0], port);
    let tls_domain = domain.clone();
    let (certs, version, cipher) = tokio::task::spawn_blocking(move || {
        tls_handshake_blocking(addr, &tls_domain, connect_timeout)
    })
    .await
    .map_err(|error| UnifiedError::internal(format!("TLS probe task failed: {error}")))??;
    let chain: Vec<Value> = certs.iter().map(|der| parse_certificate_der(der)).collect();
    let mut notes: Vec<String> = Vec::new();
    let mut days_remaining: Option<i64> = None;
    let mut expired = false;
    if let Some(leaf) = chain.first() {
        if let Some(not_after) = leaf.get("not_after").and_then(|v| v.as_str()) {
            if let Ok(expiry) = DateTime::parse_from_rfc3339(not_after) {
                let days = (expiry.with_timezone(&Utc) - Utc::now()).num_days();
                days_remaining = Some(days);
                if days < 0 {
                    expired = true;
                    notes.push("The leaf certificate has expired.".to_string());
                } else if days <= 30 {
                    notes.push(format!("The leaf certificate expires in {days} day(s)."));
                }
            }
        }
        if let (Some(subject), Some(issuer)) = (
            leaf.get("subject").and_then(|v| v.as_str()),
            leaf.get("issuer").and_then(|v| v.as_str()),
        ) {
            if subject == issuer {
                notes.push("The leaf certificate is self-signed (subject equals issuer).".to_string());
            }
        }
        let sans: Vec<String> = leaf
            .get("sans")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default();
        if !sans.is_empty()
            && !sans.iter().any(|san| {
                san.eq_ignore_ascii_case(&domain)
                    || (san.starts_with("*.") && domain.ends_with(&san[1..]))
            })
        {
            notes.push("The domain is not listed in the leaf certificate SANs.".to_string());
        }
    } else {
        notes.push("The server presented no certificate chain.".to_string());
    }
    if chain.len() > 1 {
        notes.push(format!("Chain length: {} certificates.", chain.len()));
    }
    notes.push(
        "Chain trust was NOT validated against a root store by this probe (accept-any verifier); the notes above are structural checks only."
            .to_string(),
    );
    let outcome = if expired || chain.is_empty() {
        "failed"
    } else {
        "success"
    };
    Ok(ToolResult {
        tool: "ssl_check".to_string(),
        target: format!("{domain}:{port}"),
        outcome: outcome.to_string(),
        duration_ms: started.elapsed().as_millis() as u64,
        method: Some("rustls handshake".to_string()),
        result: json!({
            "domain": domain,
            "port": port,
            "protocol_version": version,
            "cipher_suite": cipher,
            "chain": chain,
            "days_remaining": days_remaining,
            "expired": expired,
            "managed_certificate": managed,
        }),
        notes,
    })
}
