// Domains module - TLS probing and certificate parsing.
//
// Split from `src/modules/domains.rs`; the module root re-exports the public API of this module.

use chrono::{DateTime, Utc};
use diesel::prelude::*;
use rustls::client::danger::ServerCertVerifier;
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{ClientConfig, ClientConnection, DigitallySignedStruct, SignatureScheme};
use std::sync::Arc;
use std::time::Duration;

// ---------------------------------------------------------------------------
// TLS probe: connect to port 443, complete a TLS handshake, and read the
// leaf certificate's expiry date.
//
// The probe only needs to *read* the certificate, not trust it (the
// domain may be mid-renewal), so it uses a verifier that accepts any
// certificate and then parses the presented leaf certificate itself.
// The blocking socket work runs in `spawn_blocking`.
// ---------------------------------------------------------------------------

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
            SignatureScheme::RSA_PKCS1_SHA256,
            SignatureScheme::RSA_PKCS1_SHA384,
            SignatureScheme::RSA_PKCS1_SHA512,
            SignatureScheme::ECDSA_NISTP256_SHA256,
            SignatureScheme::ECDSA_NISTP384_SHA384,
            SignatureScheme::ECDSA_NISTP521_SHA512,
            SignatureScheme::RSA_PSS_SHA256,
            SignatureScheme::RSA_PSS_SHA384,
            SignatureScheme::RSA_PSS_SHA512,
            SignatureScheme::ED25519,
        ]
    }
}

/// Probe TLS on `domain:443`. Returns `(reachable, certificate_expiry)`.
/// The expiry is best-effort: when the certificate cannot be parsed the
/// probe still reports reachability.
pub(crate) async fn probe_tls(domain: &str) -> (bool, Option<DateTime<Utc>>) {
    let domain = domain.to_string();
    tokio::task::spawn_blocking(move || probe_tls_blocking(&domain))
        .await
        .unwrap_or((false, None))
}

/// Resolve `domain:443` to every socket address it has. A literal
/// IP yields one address; a hostname yields all A/AAAA results in
/// resolver order.
pub(crate) fn resolve_tls_addrs(domain: &str) -> Vec<std::net::SocketAddr> {
    if let Ok(addr) = format!("{domain}:443").parse::<std::net::SocketAddr>() {
        return vec![addr];
    }
    // Hostname, not a literal address: resolve it first.
    match std::net::ToSocketAddrs::to_socket_addrs(&(domain, 443)) {
        Ok(addrs) => addrs.collect(),
        Err(_) => Vec::new(),
    }
}

pub(crate) fn probe_tls_blocking(domain: &str) -> (bool, Option<DateTime<Utc>>) {
    let timeout = Duration::from_secs(5);
    // Try every resolved address until one connects: an IPv6-first
    // host whose service is IPv4-only (or vice versa) must not be
    // reported unreachable because the first address failed.
    for address in resolve_tls_addrs(domain) {
        if let Some(result) = probe_tls_at(domain, address, timeout) {
            return result;
        }
    }
    (false, None)
}

/// One TLS probe attempt against a single address. Returns `None`
/// when the TCP connect itself fails, so the caller can try the
/// next address; a completed handshake always yields `Some`.
fn probe_tls_at(
    domain: &str,
    address: std::net::SocketAddr,
    timeout: Duration,
) -> Option<(bool, Option<DateTime<Utc>>)> {
    let Ok(mut stream) = std::net::TcpStream::connect_timeout(&address, timeout) else {
        return None;
    };
    let _ = stream.set_read_timeout(Some(timeout));
    let _ = stream.set_write_timeout(Some(timeout));
    let config = Arc::new(
        ClientConfig::builder()
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(AcceptAnyCertificate))
            .with_no_client_auth(),
    );
    let server_name: ServerName = match domain.to_owned().try_into() {
        Ok(name) => name,
        Err(_) => return None,
    };
    let Ok(mut connection) = ClientConnection::new(config, server_name) else {
        return None;
    };
    // Drive the handshake to completion.
    loop {
        if connection.is_handshaking() {
            match connection.complete_io(&mut stream) {
                Ok(_) => {}
                Err(_) => return None,
            }
        } else {
            break;
        }
    }
    let certificates = connection.peer_certificates().unwrap_or_default().to_vec();
    let mut expiry = None;
    if let Some(leaf) = certificates.first() {
        expiry = certificate_expiry(leaf.as_ref());
    }
    Some((true, expiry))
}

/// Extract the `not_after` expiry from a DER certificate. Returns `None`
/// when the certificate cannot be parsed.
pub(crate) fn certificate_expiry(der: &[u8]) -> Option<DateTime<Utc>> {
    let certificate = x509_certificate::X509Certificate::from_der(der).ok()?;
    Some(certificate.validity_not_after())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn certificate_expiry_rejects_invalid_der() {
        assert_eq!(certificate_expiry(b"not a certificate"), None);
        assert_eq!(certificate_expiry(b""), None);
    }

    #[test]
    fn resolve_tls_addrs_returns_literal_address() {
        let addrs = resolve_tls_addrs("127.0.0.1");
        assert_eq!(addrs.len(), 1);
        assert_eq!(addrs[0].to_string(), "127.0.0.1:443");
    }

    #[test]
    fn resolve_tls_addrs_returns_empty_for_unresolvable_name() {
        assert!(resolve_tls_addrs("invalid.invalid").is_empty());
    }

    #[test]
    fn probe_tls_blocking_reports_refused_port_unreachable() {
        // Nothing listens on loopback 443 in this environment, so the
        // TCP connect is refused immediately on the single address.
        assert_eq!(probe_tls_blocking("127.0.0.1"), (false, None));
    }

    #[test]
    fn probe_tls_blocking_reports_unresolvable_name_unreachable() {
        assert_eq!(probe_tls_blocking("invalid.invalid"), (false, None));
    }
}
