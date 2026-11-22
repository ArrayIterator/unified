// TOTP two-factor authentication (RFC 6238).
//
// Secrets are 160-bit random values encoded as base32 (the format
// authenticator apps expect). Codes are 6 digits, SHA-1, 30-second steps,
// with one step of clock skew allowed on verification.

use std::time::SystemTime;
use totp_rs::{Algorithm, Secret, TOTP};

/// Number of digits in a TOTP code.
pub const DIGITS: usize = 6;
/// Allowed clock skew in 30-second steps (one step each way).
pub const SKEW: u8 = 1;
/// Length of one TOTP time step in seconds.
pub const STEP: u64 = 30;

/// Generate a new random base32 TOTP secret (160 bits, 32 characters).
pub fn generate_secret() -> String {
    Secret::generate_secret().to_encoded()
}

/// Build the `otpauth://` provisioning URI for a secret, so it can be
/// rendered as a QR code or opened by an authenticator app.
///
/// Falls back to a manually built URI when the secret cannot be decoded
/// (for example when it was typed in by hand with a typo).
pub fn provisioning_uri(secret: &str, account: &str, issuer: &str) -> String {
    match build_totp(secret, Some(issuer), account) {
        Ok(totp) => totp.get_url(),
        Err(_) => fallback_uri(secret, account, issuer),
    }
}

/// Verify a 6-digit TOTP code against a base32 secret, allowing one step
/// of clock skew. Returns `false` for malformed secrets or codes.
pub fn verify(secret: &str, code: &str) -> bool {
    if code.len() != DIGITS || !code.bytes().all(|byte| byte.is_ascii_digit()) {
        return false;
    }
    match build_totp(secret, None, "unified") {
        Ok(totp) => totp.check_current(code).unwrap_or(false),
        Err(_) => false,
    }
}

/// Verify a TOTP code against a secret at a specific point in time
/// (useful for tests and for replaying a code that arrived late).
pub fn verify_at(secret: &str, code: &str, time: SystemTime) -> bool {
    if code.len() != DIGITS || !code.bytes().all(|byte| byte.is_ascii_digit()) {
        return false;
    }
    match build_totp(secret, None, "unified") {
        Ok(totp) => totp.check(code, time).unwrap_or(false),
        Err(_) => false,
    }
}

/// Generate the current TOTP code for a secret, if the secret is valid.
/// Only used for testing and diagnostics; never expose codes to callers
/// outside the authentication flow.
pub fn current_code(secret: &str) -> Option<String> {
    match build_totp(secret, None, "unified") {
        Ok(totp) => Some(totp.generate(SystemTime::now())),
        Err(_) => None,
    }
}

/// Build a `TOTP` instance for a base32 secret.
fn build_totp(secret: &str, issuer: Option<&str>, account: &str) -> Result<TOTP, String> {
    let bytes = Secret::Encoded(secret.to_string())
        .to_bytes()
        .map_err(|error| format!("Invalid TOTP secret: {error}"))?;
    TOTP::new(
        Algorithm::SHA1,
        DIGITS,
        SKEW,
        STEP,
        bytes,
        issuer.map(str::to_string),
        account.to_string(),
    )
    .map_err(|error| format!("Invalid TOTP configuration: {error}"))
}

/// Manually built provisioning URI, used when the secret cannot be
/// decoded into a `TOTP` instance.
fn fallback_uri(secret: &str, account: &str, issuer: &str) -> String {
    format!(
        "otpauth://totp/{}:{}?secret={}&issuer={}&algorithm=SHA1&digits=6&period=30",
        percent_encode(issuer),
        percent_encode(account),
        secret,
        percent_encode(issuer),
    )
}

/// Percent-encode a URI component (RFC 3986 unreserved set kept as-is).
fn percent_encode(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for &byte in value.as_bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char)
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}
