//! eSignature external module (split from lib.rs; moved verbatim).

use super::*;


// ---------------------------------------------------------------------------
// Validation helpers
// ---------------------------------------------------------------------------

pub(crate) fn valid_email(email: &str) -> bool {
    if email.is_empty() || email.len() > 254 {
        return false;
    }
    if email.chars().any(|c| c.is_whitespace()) {
        return false;
    }
    let mut parts = email.split('@');
    let local = parts.next().unwrap_or("");
    let domain = parts.next().unwrap_or("");
    if parts.next().is_some() {
        return false;
    }
    if local.is_empty() || domain.is_empty() {
        return false;
    }
    if !domain.contains('.') || domain.starts_with('.') || domain.ends_with('.') {
        return false;
    }
    if local.starts_with('.') || local.ends_with('.') {
        return false;
    }
    true
}

pub(crate) fn valid_sha256_hex(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|b| b.is_ascii_hexdigit())
}

pub(crate) fn valid_tenant_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

/// Stable equivalents of the `Ipv4Addr` classification methods that are
/// still unstable behind the `ip` feature (rust-lang/rust#27709) and
/// therefore unusable on stable Rust: `is_benchmarking`,
/// `is_reserved`, `is_shared`. The predicates below copy the standard
/// library's definitions exactly:
/// - benchmarking: `198.18.0.0/15` (RFC 2544, errata 423).
/// - shared (CGNAT): `100.64.0.0/10` (RFC 6598).
/// - reserved: `240.0.0.0/4` excluding the `255.255.255.255` broadcast
///   address (RFC 1112).
const fn ipv4_is_benchmarking(octets: [u8; 4]) -> bool {
    octets[0] == 198 && (octets[1] & 0xfe) == 18
}

const fn ipv4_is_shared(octets: [u8; 4]) -> bool {
    octets[0] == 100 && (octets[1] & 0xc0) == 0x40
}

fn ipv4_is_reserved(octets: [u8; 4]) -> bool {
    (octets[0] & 0xf0) == 0xf0 && octets != [255, 255, 255, 255]
}

fn ip_is_private_or_reserved(ip: &IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            v4.is_private()
                || v4.is_loopback()
                || v4.is_link_local()
                || v4.is_unspecified()
                || v4.is_multicast()
                || ipv4_is_benchmarking(v4.octets())
                || ipv4_is_reserved(v4.octets())
                || ipv4_is_shared(v4.octets())
        }
        IpAddr::V6(v6) => {
            v6.is_loopback()
                || v6.is_unspecified()
                || v6.is_multicast()
                || v6.is_unique_local()
                // fe80::/10 link-local.
                || (v6.segments()[0] & 0xffc0) == 0xfe80
        }
    }
}

/// Callback URLs must be absolute http/https URLs, must not carry
/// credentials, and must not point at localhost or at a literal
/// private, loopback, or link-local IP. Note: a hostname that
/// *resolves* to a private IP is not rejected here (no DNS lookup
/// is done); deployments that follow callback URLs must resolve
/// them through a DNS allowlist of their own.
pub(crate) fn validate_callback_url(raw: &str) -> UnifiedResult<()> {
    let parsed = url::Url::parse(raw)
        .map_err(|_| UnifiedError::bad_request("callback_url must be a valid absolute URL"))?;
    if parsed.scheme() != "http" && parsed.scheme() != "https" {
        return Err(UnifiedError::bad_request(
            "callback_url must use http or https",
        ));
    }
    if !parsed.username().is_empty() || parsed.password().is_some() {
        return Err(UnifiedError::bad_request(
            "callback_url must not contain credentials",
        ));
    }
    let host = parsed
        .host_str()
        .ok_or_else(|| UnifiedError::bad_request("callback_url must have a host"))?;
    if host.eq_ignore_ascii_case("localhost") || host.ends_with(".localhost") {
        return Err(UnifiedError::bad_request(
            "callback_url must not point at localhost",
        ));
    }
    if let Ok(ip) = host.parse::<IpAddr>() {
        if ip_is_private_or_reserved(&ip) {
            return Err(UnifiedError::bad_request(
                "callback_url must not point at a private or loopback IP",
            ));
        }
    }
    Ok(())
}

fn hex_encode(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        out.push(HEX[(b >> 4) as usize] as char);
        out.push(HEX[(b & 0x0f) as usize] as char);
    }
    out
}

// ---------------------------------------------------------------------------
// Signing-link tokens (HMAC-SHA256, expiring, single-purpose)
// ---------------------------------------------------------------------------

/// Read the HMAC secret from the environment. There is no default
/// and no fallback: a missing or too-short secret is a startup/
/// operator error, surfaced as 500, never silently replaced.
pub(crate) fn signing_secret() -> UnifiedResult<Vec<u8>> {
    match std::env::var(SECRET_ENV_VAR) {
        Ok(value) if value.len() >= MIN_SECRET_LEN => Ok(value.into_bytes()),
        Ok(_) => Err(UnifiedError::internal(
            "ESIGNATURE_SECRET is set but too short; use at least 32 characters",
        )),
        Err(_) => Err(UnifiedError::internal(
            "ESIGNATURE_SECRET is not configured",
        )),
    }
}

/// Issue a signing token: `base64url(payload).hex(hmac)`, where the
/// payload is `v1.<request id>.<signer id>.<expiry unix>`. The token
/// expires with the request.
pub(crate) fn issue_token(
    secret: &[u8],
    request_id: &str,
    signer_id: &str,
    expires_at: DateTime<Utc>,
) -> String {
    let payload = format!(
        "v1.{request_id}.{signer_id}.{}",
        expires_at.timestamp()
    );
    let mut mac = Hmac::<Sha256>::new_from_slice(secret)
        .expect("HMAC accepts any key length, so this cannot fail");
    mac.update(payload.as_bytes());
    let signature = hex_encode(&mac.finalize().into_bytes());
    format!("{}.{}", URL_SAFE_NO_PAD.encode(payload.as_bytes()), signature)
}

/// Verify a signing token. Returns `(request_id, signer_id,
/// expires_at)`. A bad signature is 401; an expired token is 410.
pub(crate) fn verify_token(
    token: &str,
    secret: &[u8],
) -> UnifiedResult<(String, String, DateTime<Utc>)> {
    let (encoded_payload, presented_sig) = token
        .split_once('.')
        .ok_or_else(|| UnifiedError::unauthorized("invalid signing token"))?;
    let payload_bytes = URL_SAFE_NO_PAD
        .decode(encoded_payload)
        .map_err(|_| UnifiedError::unauthorized("invalid signing token"))?;
    let payload = String::from_utf8(payload_bytes)
        .map_err(|_| UnifiedError::unauthorized("invalid signing token"))?;
    let mut parts = payload.split('.');
    let version = parts.next().unwrap_or("");
    let request_id = parts.next().unwrap_or("").to_string();
    let signer_id = parts.next().unwrap_or("").to_string();
    let expiry_raw = parts.next().unwrap_or("");
    if version != "v1" || request_id.is_empty() || signer_id.is_empty() || parts.next().is_some()
    {
        return Err(UnifiedError::unauthorized("invalid signing token"));
    }
    let expiry_unix: i64 = expiry_raw
        .parse()
        .map_err(|_| UnifiedError::unauthorized("invalid signing token"))?;
    let expires_at = DateTime::from_timestamp(expiry_unix, 0)
        .ok_or_else(|| UnifiedError::unauthorized("invalid signing token"))?;
    let mut mac = Hmac::<Sha256>::new_from_slice(secret)
        .expect("HMAC accepts any key length, so this cannot fail");
    mac.update(payload.as_bytes());
    let expected_sig = hex_encode(&mac.finalize().into_bytes());
    if !constant_time_eq(expected_sig.as_bytes(), presented_sig.as_bytes()) {
        return Err(UnifiedError::unauthorized("invalid signing token"));
    }
    if Utc::now() > expires_at {
        return Err(UnifiedError::new("signing token has expired", 410));
    }
    Ok((request_id, signer_id, expires_at))
}

// ---------------------------------------------------------------------------
// Hash-chained audit trail
// ---------------------------------------------------------------------------

/// Hash input for one audit event. Kept in a single function so the
/// append path and the verify path can never drift apart.
pub(crate) fn audit_hash_input(
    prev_hash: &str,
    id: &str,
    request_id: &str,
    kind: &str,
    actor: &str,
    created_at: DateTime<Utc>,
    details: &str,
) -> String {
    format!(
        "{prev_hash}|{id}|{request_id}|{kind}|{actor}|{}|{details}",
        created_at.to_rfc3339()
    )
}

/// Recompute and check a whole audit chain. Returns the first broken
/// index and a reason when the chain does not verify.
pub fn verify_audit_events(events: &[AuditEvent]) -> ChainVerification {
    let mut prev = GENESIS_HASH.to_string();
    for (index, event) in events.iter().enumerate() {
        if event.prev_hash != prev {
            return ChainVerification {
                valid: false,
                events_checked: index,
                broken_index: Some(index),
                reason: Some(
                    "prev_hash does not match the previous event hash".to_string(),
                ),
            };
        }
        let expected = sha256_hex(&audit_hash_input(
            &event.prev_hash,
            &event.id,
            &event.request_id,
            &event.kind,
            &event.actor,
            event.created_at,
            &event.details,
        ));
        if !constant_time_eq(expected.as_bytes(), event.hash.as_bytes()) {
            return ChainVerification {
                valid: false,
                events_checked: index,
                broken_index: Some(index),
                reason: Some("event hash does not match its payload".to_string()),
            };
        }
        prev = event.hash.clone();
    }
    ChainVerification {
        valid: true,
        events_checked: events.len(),
        broken_index: None,
        reason: None,
    }
}

/// The status a request reports right now. A `sent` or
/// `partially_signed` request past its expiry is `expired`; every
/// other stored status reports as-is.
pub(crate) fn effective_status(req: &SignatureRequest, now: DateTime<Utc>) -> RequestStatus {
    match req.status {
        RequestStatus::Sent | RequestStatus::PartiallySigned if now > req.expires_at => {
            RequestStatus::Expired
        }
        other => other,
    }
}
