//! IoT external module.

use super::*;


// Validation helpers (pure, unit-tested)

fn is_valid_tenant_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.')
}

pub(crate) fn validate_tenant_id(value: &str) -> UnifiedResult<()> {
    if !is_valid_tenant_id(value) {
        return Err(UnifiedError::bad_request(
            "tenant_id is invalid: 1-128 characters, letters, digits, '-', '_' or '.'",
        ));
    }
    Ok(())
}

fn is_valid_device_key(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.')
}

pub(crate) fn validate_device_key(value: &str) -> UnifiedResult<()> {
    if !is_valid_device_key(value) {
        return Err(UnifiedError::bad_request(
            "device_key is invalid: 1-64 characters, letters, digits, '-', '_' or '.'",
        ));
    }
    Ok(())
}

pub(crate) fn is_valid_metric_name(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.' || c == '/')
}

pub(crate) fn validate_text(value: &str, field: &str, max_len: usize) -> UnifiedResult<()> {
    if value.is_empty() || value.len() > max_len {
        return Err(UnifiedError::bad_request(format!(
            "{field} must be 1-{max_len} characters"
        )));
    }
    Ok(())
}

pub(crate) fn validate_json_object_size(value: &Value, field: &str, max_bytes: usize) -> UnifiedResult<()> {
    if !value.is_object() {
        return Err(UnifiedError::bad_request(format!(
            "{field} must be a JSON object"
        )));
    }
    let size = serde_json::to_string(value)
        .map(|s| s.len())
        .unwrap_or(usize::MAX);
    if size > max_bytes {
        return Err(UnifiedError::bad_request(format!(
            "{field} exceeds the {max_bytes} byte limit"
        )));
    }
    Ok(())
}

/// Generate a random per-device API key. The returned value is the
/// only copy of the plaintext; callers must show it once and drop it.
pub(crate) fn generate_device_api_key() -> String {
    let bytes: [u8; 32] = rand::random();
    URL_SAFE_NO_PAD.encode(bytes)
}

/// SHA-256 hex of a device API key. Only this hash is stored.
pub(crate) fn hash_device_key(key: &str) -> String {
    let digest = Sha256::digest(key.as_bytes());
    let mut out = String::with_capacity(digest.len() * 2);
    for byte in digest {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

/// Constant-time equality for equal-length secrets. Lengths differ
/// only when the stored hash is corrupt, which is also a mismatch;
/// the comparison itself never short-circuits on content.
pub(crate) fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff: u8 = 0;
    for i in 0..a.len() {
        diff |= a[i] ^ b[i];
    }
    diff == 0
}

pub(crate) fn now_utc() -> DateTime<Utc> {
    Utc::now()
}

pub(crate) fn new_id() -> String {
    uuid::Uuid::new_v4().to_string()
}
