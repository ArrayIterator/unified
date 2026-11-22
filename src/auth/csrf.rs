// CSRF protection.
//
// A token binds a session id to an expiry time with HMAC-SHA256:
//
//   token = base64url( expiry_be_bytes || HMAC(secret, session_id || ":" || expiry_be_bytes) )
//
// The token is 40 bytes (8-byte big-endian Unix expiry + 32-byte
// signature), encoded as 54 base64url characters without padding. The
// expiry is embedded in the token, so validation needs no server-side
// state. The secret is `secret_key` + `salt_key` from the application
// configuration; the lifetime comes from `app.csrf_duration` (minutes).

use crate::core::config::AppConfig;
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use chrono::Utc;
use hmac::{Hmac, Mac};
use sha2::Sha256;
use std::time::Duration;

type HmacSha256 = Hmac<Sha256>;

/// Token size in bytes: 8-byte expiry + 32-byte HMAC-SHA256 signature.
const TOKEN_BYTES: usize = 40;
/// Size of the big-endian expiry prefix inside a token.
const EXPIRY_BYTES: usize = 8;

/// CSRF token manager. Thread-safe: it only holds the secret bytes and
/// the token lifetime.
#[derive(Debug, Clone)]
pub struct CsrfManager {
    secret: Vec<u8>,
    duration: Duration,
}

impl CsrfManager {
    /// Create a manager from raw secret bytes and a token lifetime.
    pub fn new(secret: impl AsRef<[u8]>, duration: Duration) -> Self {
        Self {
            secret: secret.as_ref().to_vec(),
            duration,
        }
    }

    /// Create a manager from the application configuration: the secret is
    /// `app.secret_key` + `app.salt_key`, and the lifetime is
    /// `app.csrf_duration` minutes.
    pub fn from_config(config: &AppConfig) -> Self {
        let mut secret = Vec::with_capacity(
            config.app.secret_key.len() + config.app.salt_key.len(),
        );
        secret.extend_from_slice(config.app.secret_key.as_bytes());
        secret.extend_from_slice(config.app.salt_key.as_bytes());
        Self::new(
            secret,
            Duration::from_secs(u64::from(config.app.csrf_duration) * 60),
        )
    }

    /// The token lifetime this manager was configured with.
    pub fn duration(&self) -> Duration {
        self.duration
    }

    /// Generate a CSRF token bound to `session_id`.
    pub fn generate(&self, session_id: &str) -> String {
        let expires_at = Utc::now().timestamp().saturating_add(self.duration.as_secs() as i64);
        let expiry_bytes = expires_at.to_be_bytes();
        let signature = self.sign(session_id, &expiry_bytes);
        let mut token_bytes = Vec::with_capacity(TOKEN_BYTES);
        token_bytes.extend_from_slice(&expiry_bytes);
        token_bytes.extend_from_slice(&signature);
        URL_SAFE_NO_PAD.encode(token_bytes)
    }

    /// Validate a token for `session_id`.
    ///
    /// Returns `false` when the token is malformed, expired, bound to a
    /// different session, or has a mismatching signature. The signature
    /// comparison runs in constant time.
    pub fn validate(&self, token: &str, session_id: &str) -> bool {
        let decoded = match URL_SAFE_NO_PAD.decode(token) {
            Ok(decoded) => decoded,
            Err(_) => return false,
        };
        if decoded.len() != TOKEN_BYTES {
            return false;
        }
        let (expiry_bytes, signature) = decoded.split_at(EXPIRY_BYTES);
        let expires_at = i64::from_be_bytes(
            expiry_bytes
                .try_into()
                .expect("expiry prefix is exactly 8 bytes"),
        );
        if expires_at <= Utc::now().timestamp() {
            return false;
        }
        let expected = self.sign(session_id, expiry_bytes);
        constant_time_eq(&expected, signature)
    }

    /// HMAC-SHA256 over `session_id || ":" || expiry_bytes`.
    fn sign(&self, session_id: &str, expiry_bytes: &[u8]) -> [u8; 32] {
        // HMAC accepts keys of any length, so initialisation cannot fail.
        let mut mac = HmacSha256::new_from_slice(&self.secret)
            .expect("HMAC-SHA256 accepts keys of any length");
        mac.update(session_id.as_bytes());
        mac.update(b":");
        mac.update(expiry_bytes);
        let output = mac.finalize().into_bytes();
        let mut result = [0u8; 32];
        result.copy_from_slice(&output);
        result
    }
}

/// Constant-time equality for two byte slices of equal length.
/// Returns `false` immediately only when the lengths differ (token
/// signatures are always 32 bytes, so no secret leaks through length).
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for index in 0..a.len() {
        diff |= a[index] ^ b[index];
    }
    diff == 0
}
