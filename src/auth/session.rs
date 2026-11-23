// Stateless signed session tokenizer (HMAC-SHA256, tamper-proof).
//
// A token is a 208-character hex string encoding a 104-byte payload:
//
//   bytes  0..16   random nonce (16 bytes, from a UUID v4)
//   bytes 16..48   inner signature: HMAC-SHA256(key, user id bytes)
//   bytes 48..56   issued-at timestamp (u64 big-endian, Unix seconds)
//   bytes 56..64   expires-at timestamp (u64 big-endian, Unix seconds)
//   bytes 64..72   user id (u64 big-endian)
//   bytes 72..104  outer signature: HMAC-SHA256(key, bytes 0..72)
//
// The inner signature binds the token to its user id; the outer signature
// covers the whole payload, so no field can be altered without the key.
// Verification needs no database lookup. The signing key is
// `secret_key` + `salt_key` from the application configuration.

use unified_core::config::AppConfig;
use unified_core::error::{UnifiedError, UnifiedResult};
use chrono::Utc;
use hmac::{Hmac, Mac};
use sha2::Sha256;
use std::fmt;
use std::time::Duration;
use uuid::Uuid;

type HmacSha256 = Hmac<Sha256>;

/// Length of a token string in hexadecimal characters (104 bytes).
pub const TOKEN_LENGTH: usize = 208;

/// The verified data extracted from a session token.
#[derive(Debug, Clone)]
pub struct SessionPayload {
    /// The full 208-character hex token string.
    pub token: String,
    /// The numeric id of the authenticated user.
    pub user_id: u64,
    /// Unix timestamp (seconds) when the token was issued.
    pub timestamp: u64,
    /// Unix timestamp (seconds) when the token expires.
    pub expired_at: u64,
    /// The 16-byte random nonce embedded in the token.
    pub random: [u8; 16],
}

impl SessionPayload {
    /// True when the token is expired, malformed in time, or was issued
    /// in the future. A token issued in the current second is valid.
    pub fn is_expired(&self) -> bool {
        let now = Utc::now().timestamp() as u64;
        self.timestamp == 0
            || self.expired_at == 0
            || self.timestamp > now
            || self.expired_at <= now
    }

    /// True when the token is still valid but expires within `duration`,
    /// meaning the caller should issue a fresh token.
    pub fn is_need_renew(&self, duration: Duration) -> bool {
        let now = Utc::now().timestamp() as u64;
        self.expired_at > now && self.expired_at - now <= duration.as_secs()
    }

    /// Seconds remaining before expiry; zero when already expired.
    pub fn remaining_seconds(&self) -> u64 {
        let now = Utc::now().timestamp() as u64;
        self.expired_at.saturating_sub(now)
    }
}

impl fmt::Display for SessionPayload {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.token)
    }
}

/// Stateless session tokenizer providing integrity protection via nested
/// HMAC-SHA256. Thread-safe: it only holds the secret and salt bytes.
#[derive(Debug, Clone)]
pub struct SessionTokenizer {
    secret: Vec<u8>,
    salt: Vec<u8>,
}

impl SessionTokenizer {
    /// Length of a token string in hexadecimal characters.
    pub const LENGTH: usize = TOKEN_LENGTH;

    /// Create a tokenizer from a secret and a salt string.
    pub fn new<Secret: AsRef<str>, Salt: AsRef<str>>(secret: Secret, salt: Salt) -> Self {
        Self {
            secret: secret.as_ref().as_bytes().to_vec(),
            salt: salt.as_ref().as_bytes().to_vec(),
        }
    }

    /// Create a tokenizer from the application configuration, using
    /// `app.secret_key` as the secret and `app.salt_key` as the salt.
    pub fn from_config(config: &AppConfig) -> Self {
        Self::new(&config.app.secret_key, &config.app.salt_key)
    }

    /// The combined signing key: secret bytes followed by salt bytes.
    fn combined_key(&self) -> Vec<u8> {
        [self.secret.as_slice(), self.salt.as_slice()].concat()
    }

    /// Issue a signed token for `user_id` that is valid for `duration`.
    pub fn generate(&self, user_id: u64, duration: Duration) -> UnifiedResult<SessionPayload> {
        let key = self.combined_key();
        let random: [u8; 16] = *Uuid::new_v4().as_bytes();
        let now = Utc::now().timestamp() as u64;
        let expired_at = now.saturating_add(duration.as_secs());

        let time_8 = now.to_be_bytes();
        let exp_8 = expired_at.to_be_bytes();
        let user_8 = user_id.to_be_bytes();

        // Inner signature binds the user id to the key.
        let id_binding = hmac_sha256(&key, &user_8)?;

        let mut buffer = [0u8; 104];
        buffer[0..16].copy_from_slice(&random);
        buffer[16..48].copy_from_slice(&id_binding);
        buffer[48..56].copy_from_slice(&time_8);
        buffer[56..64].copy_from_slice(&exp_8);
        buffer[64..72].copy_from_slice(&user_8);

        // Outer signature covers the whole payload (bytes 0..72).
        let final_sign = hmac_sha256(&key, &buffer[0..72])?;
        buffer[72..104].copy_from_slice(&final_sign);

        Ok(SessionPayload {
            token: to_hex(&buffer),
            user_id,
            timestamp: now,
            expired_at,
            random,
        })
    }

    /// Verify a token string and return its payload.
    ///
    /// Fails when the length or hex encoding is wrong, when the outer
    /// signature does not match (tampered payload), when the inner
    /// signature does not match the embedded user id, or when the
    /// token is expired (issued in the future, or past `expired_at`).
    /// Expiry is enforced here, in `parse()` itself, so no caller
    /// can accept an expired token by forgetting to call
    /// `SessionPayload::is_expired` on the result.
    pub fn parse(&self, token: &str) -> UnifiedResult<SessionPayload> {
        if token.len() != Self::LENGTH {
            return Err(UnifiedError::unauthorized(
                "Invalid session token: incorrect length",
            ));
        }
        let bytes = from_hex(token)?;
        if bytes.len() != 104 {
            return Err(UnifiedError::unauthorized(
                "Invalid session token: incorrect length",
            ));
        }

        let key = self.combined_key();
        let payload_part = &bytes[0..72];
        let outer_signature = &bytes[72..104];

        // 1. Verify the outer signature over the whole payload.
        let mut mac_outer = HmacSha256::new_from_slice(&key).map_err(|error| {
            UnifiedError::internal(format!("HMAC initialisation failed: {error}"))
        })?;
        mac_outer.update(payload_part);
        mac_outer
            .verify_slice(outer_signature)
            .map_err(|_| UnifiedError::unauthorized("Session token integrity check failed"))?;

        // 2. Extract the components.
        let random: [u8; 16] = payload_part[0..16]
            .try_into()
            .map_err(|_| UnifiedError::unauthorized("Invalid session token: bad nonce"))?;
        let inner_signature = &payload_part[16..48];
        let timestamp = u64::from_be_bytes(
            payload_part[48..56]
                .try_into()
                .map_err(|_| UnifiedError::unauthorized("Invalid session token: bad timestamp"))?,
        );
        let expired_at = u64::from_be_bytes(
            payload_part[56..64]
                .try_into()
                .map_err(|_| UnifiedError::unauthorized("Invalid session token: bad expiry"))?,
        );
        let user_8: [u8; 8] = payload_part[64..72]
            .try_into()
            .map_err(|_| UnifiedError::unauthorized("Invalid session token: bad user id"))?;
        let user_id = u64::from_be_bytes(user_8);

        // 3. Verify the inner signature binds this user id.
        let mut mac_inner = HmacSha256::new_from_slice(&key).map_err(|error| {
            UnifiedError::internal(format!("HMAC initialisation failed: {error}"))
        })?;
        mac_inner.update(&user_8);
        mac_inner
            .verify_slice(inner_signature)
            .map_err(|_| UnifiedError::unauthorized("Session token identity check failed"))?;

        let payload = SessionPayload {
            token: token.to_string(),
            user_id,
            timestamp,
            expired_at,
            random,
        };
        // Expiry is enforced here: a token whose time window has
        // closed (or that claims a future issue time) is rejected
        // even though its signatures are valid.
        if payload.is_expired() {
            return Err(UnifiedError::unauthorized("Session token has expired"));
        }
        Ok(payload)
    }
}

/// Compute HMAC-SHA256 over `data` with `key`.
fn hmac_sha256(key: &[u8], data: &[u8]) -> UnifiedResult<[u8; 32]> {
    let mut mac = HmacSha256::new_from_slice(key)
        .map_err(|error| UnifiedError::internal(format!("HMAC initialisation failed: {error}")))?;
    mac.update(data);
    let output = mac.finalize().into_bytes();
    let mut result = [0u8; 32];
    result.copy_from_slice(&output);
    Ok(result)
}

/// Encode bytes as lowercase hex.
fn to_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for &byte in bytes {
        out.push(HEX[(byte >> 4) as usize] as char);
        out.push(HEX[(byte & 0x0f) as usize] as char);
    }
    out
}

/// Decode a lowercase (or uppercase) hex string into bytes.
fn from_hex(hex: &str) -> UnifiedResult<Vec<u8>> {
    if hex.len() % 2 != 0 {
        return Err(UnifiedError::unauthorized(
            "Invalid session token: odd-length hex string",
        ));
    }
    let bytes = hex.as_bytes();
    let mut out = Vec::with_capacity(hex.len() / 2);
    let mut index = 0;
    while index < bytes.len() {
        let high = hex_value(bytes[index])?;
        let low = hex_value(bytes[index + 1])?;
        out.push((high << 4) | low);
        index += 2;
    }
    Ok(out)
}

/// Value of one hex digit.
fn hex_value(byte: u8) -> UnifiedResult<u8> {
    match byte {
        b'0'..=b'9' => Ok(byte - b'0'),
        b'a'..=b'f' => Ok(byte - b'a' + 10),
        b'A'..=b'F' => Ok(byte - b'A' + 10),
        _ => Err(UnifiedError::unauthorized(
            "Invalid session token: non-hex character",
        )),
    }
}
