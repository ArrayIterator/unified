// TOTP two-factor authentication (RFC 6238).
//
// Secrets are 160-bit random values encoded as base32 (the format
// authenticator apps expect). Codes are 6 digits, SHA-1, 30-second steps,
// with one step of clock skew allowed on verification.
//
// At rest, a confirmed secret is stored encrypted
// (`encrypt_secret` / `decrypt_secret` below): the stored value is
// `enc:v1:<base64(nonce || ciphertext || tag)>`, an HMAC-based
// authenticated encryption (CTR-mode keystream from HMAC-SHA256
// plus an HMAC tag, Encrypt-then-MAC) keyed by a key derived from
// the application secret + salt. It uses only the crates the tree
// already depends on (`hmac`, `sha2`, `rand`, `base64`); when the
// `aes-gcm` crate is added as a direct dependency, the internals of
// `encrypt_secret` / `decrypt_secret` can be swapped for AES-256-GCM
// behind the same signatures and stored format version. Values
// without the prefix are pre-encryption plaintext rows: they still
// verify (and are re-encrypted on the next successful login) so
// existing enrolments keep working.

use std::time::SystemTime;
use base64::Engine;
use hmac::{Hmac, Mac};
use sha2::{Digest, Sha256};
use totp_rs::{Algorithm, Secret, TOTP};

type HmacSha256 = Hmac<Sha256>;

/// Number of digits in a TOTP code.
pub const DIGITS: usize = 6;
/// Allowed clock skew in 30-second steps (one step each way).
pub const SKEW: u8 = 1;
/// Length of one TOTP time step in seconds.
pub const STEP: u64 = 30;

/// Prefix marking an encrypted TOTP secret at rest.
pub const ENCRYPTED_TOTP_PREFIX: &str = "enc:v1:";
/// Nonce length in bytes for the encrypted format.
const TOTP_NONCE_LEN: usize = 16;
/// Authentication tag length in bytes (HMAC-SHA256).
const TOTP_TAG_LEN: usize = 32;

/// Generate a new random base32 TOTP secret (160 bits, 32 characters).
pub fn generate_secret() -> String {
    Secret::generate_secret().to_encoded().to_string()
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
        Ok(totp) => totp.check(code, unix_seconds(time)),
        Err(_) => false,
    }
}

/// Generate the current TOTP code for a secret, if the secret is valid.
/// Only used for testing and diagnostics; never expose codes to callers
/// outside the authentication flow.
pub fn current_code(secret: &str) -> Option<String> {
    match build_totp(secret, None, "unified") {
        Ok(totp) => Some(totp.generate(unix_seconds(SystemTime::now()))),
        Err(_) => None,
    }
}

/// Derive the 32-byte key that encrypts TOTP secrets at rest from
/// the application secret + salt. Domain-separated from the JWT
/// signing key (`"{secret}:{salt}"`) so the two keys never coincide,
/// and from the API-key pepper. Both inputs must already satisfy the
/// startup key-length rules; empty inputs still derive a key, but a
/// started server never has them (config validation rejects that).
pub fn totp_encryption_key(secret_key: &str, salt_key: &str) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(b"unified-totp-enc-key-v1");
    hasher.update([0u8]);
    hasher.update(secret_key.as_bytes());
    hasher.update(b":");
    hasher.update(salt_key.as_bytes());
    let digest = hasher.finalize();
    let mut key = [0u8; 32];
    key.copy_from_slice(&digest);
    key
}

/// True when `stored` is an encrypted TOTP secret (`enc:v1:` prefix).
pub fn is_encrypted(stored: &str) -> bool {
    stored.starts_with(ENCRYPTED_TOTP_PREFIX)
}

/// Encrypt a plaintext TOTP secret for storage. Returns
/// `enc:v1:<base64(nonce || ciphertext || tag)>`, or `None` when the
/// secret is not valid UTF-8 (never for generated secrets).
///
/// Construction: a CTR-mode keystream where block `i` is
/// HMAC-SHA256(key, `"unified-totp-enc-v1" || nonce || i_be32`),
/// XORed with the plaintext, plus an Encrypt-then-MAC tag
/// HMAC-SHA256(key, `"unified-totp-tag-v1" || nonce || ciphertext`).
/// The nonce is fresh random per encryption, so the same secret
/// encrypts differently every time.
///
/// SWAP POINT (AES-256-GCM): this construction is sound
/// Encrypt-then-MAC with a fresh nonce and a constant-time tag
/// check before decryption, but it is NOT AES-256-GCM. When the
/// `aes-gcm` crate is added as a direct dependency, swap the
/// internals of `encrypt_secret` / `decrypt_secret` for
/// AES-256-GCM behind these same signatures, with two rules:
/// (1) mint a NEW format version (`enc:v2:`, 12-byte random nonce,
/// AAD binding the purpose string `"unified-totp-enc-v1"`) — never
/// reuse the `enc:v1:` prefix for a different construction, or old
/// rows would decrypt under the wrong algorithm; (2) keep
/// `decrypt_secret` dispatching on the prefix, so `enc:v1:` rows
/// keep verifying until they are re-encrypted on the next
/// successful login, exactly like the plaintext legacy rows do
/// today. The key derivation (`totp_encryption_key`) stays
/// unchanged. An independent crypto review is recommended before
/// treating either construction as final.
pub fn encrypt_secret(secret: &str, key: &[u8; 32]) -> Option<String> {
    let nonce: [u8; TOTP_NONCE_LEN] = rand::random();
    let ciphertext = apply_keystream(secret.as_bytes(), key, &nonce);
    let tag = compute_tag(key, &nonce, &ciphertext);
    let mut blob = Vec::with_capacity(TOTP_NONCE_LEN + ciphertext.len() + TOTP_TAG_LEN);
    blob.extend_from_slice(&nonce);
    blob.extend_from_slice(&ciphertext);
    blob.extend_from_slice(&tag);
    Some(format!(
        "{ENCRYPTED_TOTP_PREFIX}{}",
        base64::engine::general_purpose::STANDARD.encode(&blob)
    ))
}

/// Decrypt a stored TOTP secret. Returns the plaintext secret, or
/// `None` when an encrypted value fails authentication (tampered,
/// truncated, or the wrong key). A value without the `enc:v1:`
/// prefix is a pre-encryption plaintext row: it is returned as-is
/// so existing enrolments keep working until they are re-encrypted
/// (on the next successful login, or on re-enrolment).
///
/// The tag is verified in constant time BEFORE any decryption
/// (Encrypt-then-MAC): a tampered row never reaches the keystream.
/// See the SWAP POINT note on `encrypt_secret` for the AES-256-GCM
/// migration rules (new `enc:v2:` version, prefix dispatch here).
pub fn decrypt_secret(stored: &str, key: &[u8; 32]) -> Option<String> {
    let Some(encoded) = stored.strip_prefix(ENCRYPTED_TOTP_PREFIX) else {
        return Some(stored.to_string());
    };
    let blob = base64::engine::general_purpose::STANDARD
        .decode(encoded)
        .ok()?;
    if blob.len() < TOTP_NONCE_LEN + TOTP_TAG_LEN {
        return None;
    }
    let (nonce, rest) = blob.split_at(TOTP_NONCE_LEN);
    let (ciphertext, tag) = rest.split_at(rest.len() - TOTP_TAG_LEN);
    let expected_tag = compute_tag(key, nonce, ciphertext);
    if !constant_time_eq(&expected_tag, tag) {
        return None;
    }
    let plaintext = apply_keystream(ciphertext, key, nonce);
    String::from_utf8(plaintext).ok()
}

/// XOR `data` with the HMAC-CTR keystream for `nonce`. Symmetric:
/// the same call encrypts and decrypts.
fn apply_keystream(data: &[u8], key: &[u8; 32], nonce: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len());
    let mut counter: u32 = 0;
    for chunk in data.chunks(32) {
        let mut mac = HmacSha256::new_from_slice(key).expect("HMAC accepts any key length");
        mac.update(b"unified-totp-enc-v1");
        mac.update(nonce);
        mac.update(&counter.to_be_bytes());
        let block = mac.finalize().into_bytes();
        for (index, byte) in chunk.iter().enumerate() {
            out.push(byte ^ block[index]);
        }
        counter = counter.wrapping_add(1);
    }
    out
}

/// Authentication tag over `nonce || ciphertext`.
fn compute_tag(key: &[u8; 32], nonce: &[u8], ciphertext: &[u8]) -> [u8; 32] {
    let mut mac = HmacSha256::new_from_slice(key).expect("HMAC accepts any key length");
    mac.update(b"unified-totp-tag-v1");
    mac.update(nonce);
    mac.update(ciphertext);
    let tag = mac.finalize().into_bytes();
    let mut out = [0u8; 32];
    out.copy_from_slice(&tag);
    out
}

/// Constant-time equality for two byte slices. Returns `false`
/// immediately only when the lengths differ (tags are always 32
/// bytes, so no secret leaks through length).
fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    if left.len() != right.len() {
        return false;
    }
    let mut diff = 0u8;
    for index in 0..left.len() {
        diff |= left[index] ^ right[index];
    }
    diff == 0
}

/// Unix seconds for a `SystemTime` (0 when before the epoch);
/// totp-rs 5.7.2 takes seconds, not `SystemTime`.
fn unix_seconds(time: SystemTime) -> u64 {
    time.duration_since(SystemTime::UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
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

#[cfg(test)]
mod tests {
    use super::*;

    fn test_key() -> [u8; 32] {
        totp_encryption_key("test-secret-key-that-is-long-enough", "test-salt-key-16")
    }

    #[test]
    fn totp_encryption_key_is_domain_separated() {
        let key = test_key();
        assert_ne!(
            key,
            totp_encryption_key("other-secret-key-that-is-long-enough", "test-salt-key-16")
        );
        assert_ne!(
            key,
            totp_encryption_key("test-secret-key-that-is-long-enough", "other-salt-key-16")
        );
        assert_eq!(key, test_key());
    }

    #[test]
    fn encrypt_decrypt_round_trip() {
        let key = test_key();
        let secret = generate_secret();
        let stored = encrypt_secret(&secret, &key).expect("encryption succeeds");
        assert!(is_encrypted(&stored));
        assert_ne!(stored, secret);
        assert_eq!(
            decrypt_secret(&stored, &key).as_deref(),
            Some(secret.as_str())
        );
        // Fresh nonce: the same secret encrypts differently each time.
        let stored_again = encrypt_secret(&secret, &key).expect("encryption succeeds");
        assert_ne!(stored, stored_again);
        assert_eq!(
            decrypt_secret(&stored_again, &key).as_deref(),
            Some(secret.as_str())
        );
    }

    #[test]
    fn decrypt_rejects_tampered_ciphertext_and_wrong_key() {
        let key = test_key();
        let stored = encrypt_secret(&generate_secret(), &key).expect("encryption succeeds");
        // Flip one character inside the base64 payload (past the prefix).
        let mut tampered = stored.clone();
        let byte_index = ENCRYPTED_TOTP_PREFIX.len() + 4;
        let byte = tampered.as_bytes()[byte_index];
        let replacement = if byte == b'A' { b'B' } else { b'A' };
        tampered.replace_range(
            byte_index..byte_index + 1,
            &(replacement as char).to_string(),
        );
        assert!(decrypt_secret(&tampered, &key).is_none());
        // A different key cannot decrypt either.
        let other_key =
            totp_encryption_key("another-secret-key-that-is-long-enough", "test-salt-key-16");
        assert!(decrypt_secret(&stored, &other_key).is_none());
        // Malformed stored values fail closed.
        assert!(decrypt_secret("enc:v1:not-base64!!", &key).is_none());
        assert!(decrypt_secret("enc:v1:AAAA", &key).is_none());
    }

    #[test]
    fn decrypt_passes_through_legacy_plaintext() {
        let key = test_key();
        let secret = generate_secret();
        assert!(!is_encrypted(&secret));
        assert_eq!(
            decrypt_secret(&secret, &key).as_deref(),
            Some(secret.as_str())
        );
    }

    #[test]
    fn encrypted_secret_verifies_totp_codes() {
        let key = test_key();
        let secret = generate_secret();
        let stored = encrypt_secret(&secret, &key).expect("encryption succeeds");
        let recovered = decrypt_secret(&stored, &key).expect("decryption succeeds");
        let code = current_code(&recovered).expect("code generates");
        assert!(verify(&recovered, &code));
        assert!(!verify(&stored, &code));
    }
}
