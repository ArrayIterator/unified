// clusters — Hashing, signatures, and secret handling.

use super::*;
use unified_core::error::{UnifiedError, UnifiedResult};
use actix_web::HttpRequest;
use chrono::{DateTime, Utc};
use hmac::Mac;
use sha2::{Digest, Sha256};
use std::sync::Arc;
use std::time::Duration;
use uuid::Uuid;

// ---------------------------------------------------------------------------
// Small helpers.
// ---------------------------------------------------------------------------

pub(crate) fn now() -> DateTime<Utc> {
    Utc::now()
}

fn hex_encode(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for &byte in bytes {
        out.push(HEX[(byte >> 4) as usize] as char);
        out.push(HEX[(byte & 0x0f) as usize] as char);
    }
    out
}

fn hex_decode(text: &str) -> Option<Vec<u8>> {
    let text = text.trim();
    if text.len() % 2 != 0 {
        return None;
    }
    let mut out = Vec::with_capacity(text.len() / 2);
    let bytes = text.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        let high = hex_value(bytes[index])?;
        let low = hex_value(bytes[index + 1])?;
        out.push((high << 4) | low);
        index += 2;
    }
    Some(out)
}

fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

fn sha256_hex(data: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(data);
    hex_encode(&hasher.finalize())
}

/// Constant-time equality for signature comparison.
fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    if left.len() != right.len() {
        return false;
    }
    let mut diff: u8 = 0;
    for index in 0..left.len() {
        diff |= left[index] ^ right[index];
    }
    diff == 0
}

/// A masked fingerprint of a stored secret hash. The raw secret and the
/// full hash are never exposed; only this prefix/suffix is returned.
pub(crate) fn secret_fingerprint(secret_hash_hex: &str) -> String {
    let chars: Vec<char> = secret_hash_hex.chars().collect();
    if chars.len() < 12 {
        return "sha256:masked".to_string();
    }
    let first: String = chars.iter().take(8).collect();
    let last: String = chars.iter().skip(chars.len() - 4).collect();
    format!("sha256:{first}...{last}")
}

/// Generate a new node secret. Returned to the caller exactly once; only
/// its SHA-256 hash is stored.
pub(crate) fn generate_secret() -> String {
    format!("cls_{}_{}", Uuid::new_v4().as_simple(), Uuid::new_v4().as_simple())
}

pub(crate) fn hash_secret(secret: &str) -> String {
    sha256_hex(secret.as_bytes())
}

// ---------------------------------------------------------------------------
// Node-to-node HMAC authentication.
//
// Signature input: `{unix_timestamp}.{nonce}.{sha256_hex(body)}`, signed
// with HMAC-SHA256 keyed by the raw bytes of the stored secret hash
// (SHA-256 of the secret). Both sides derive the same key: the sender
// from the secret it was given at registration, the receiver from the
// stored hash. Requests older than the replay window or with a reused
// nonce are rejected. Comparison is constant-time.
// ---------------------------------------------------------------------------

fn sign_request(secret_hash_hex: &str, timestamp: i64, nonce: &str, body: &[u8]) -> String {
    let key = hex_decode(secret_hash_hex).unwrap_or_default();
    let body_hash = sha256_hex(body);
    let canonical = format!("{timestamp}.{nonce}.{body_hash}");
    let mut mac = HmacSha256::new_from_slice(&key).expect("HMAC key must accept any length");
    mac.update(canonical.as_bytes());
    hex_encode(&mac.finalize().into_bytes())
}

fn header_value(req: &HttpRequest, name: &str) -> Option<String> {
    req.headers()
        .get(name)
        .and_then(|value| value.to_str().ok())
        .map(str::to_string)
}

/// Validate the HMAC signature on an incoming node-to-node request and
/// return the authenticated node. Consumes the nonce on success.
pub(crate) fn verify_node_request(
    state: &Arc<ClustersState>,
    req: &HttpRequest,
    body: &[u8],
) -> UnifiedResult<NodeState> {
    let node_id_text = header_value(req, HEADER_NODE_ID)
        .ok_or_else(|| UnifiedError::unauthorized("Missing node identity header"))?;
    let node_id: Uuid = node_id_text
        .parse()
        .map_err(|_| UnifiedError::unauthorized("Invalid node identity header"))?;
    let timestamp_text = header_value(req, HEADER_TIMESTAMP)
        .ok_or_else(|| UnifiedError::unauthorized("Missing request timestamp header"))?;
    let timestamp: i64 = timestamp_text
        .parse()
        .map_err(|_| UnifiedError::unauthorized("Invalid request timestamp header"))?;
    let nonce = header_value(req, HEADER_NONCE)
        .ok_or_else(|| UnifiedError::unauthorized("Missing request nonce header"))?;
    if nonce.trim().is_empty() || nonce.len() > 128 {
        return Err(UnifiedError::unauthorized("Invalid request nonce header"));
    }
    let signature_text = header_value(req, HEADER_SIGNATURE)
        .ok_or_else(|| UnifiedError::unauthorized("Missing request signature header"))?;

    let node = state
        .nodes
        .read()
        .expect("nodes lock poisoned")
        .get(&node_id)
        .cloned()
        .ok_or_else(|| UnifiedError::unauthorized("Unknown node"))?;
    if node.status == "revoked" {
        return Err(UnifiedError::unauthorized("Node credentials have been revoked"));
    }

    let now_unix = Utc::now().timestamp();
    if (now_unix - timestamp).abs() > state.settings().replay_window_secs {
        return Err(UnifiedError::unauthorized("Request timestamp outside the replay window"));
    }

    let expected = sign_request(&node.secret_hash, timestamp, &nonce, body);
    let expected_bytes = hex_decode(&expected).unwrap_or_default();
    let provided_bytes = hex_decode(&signature_text)
        .ok_or_else(|| UnifiedError::unauthorized("Invalid request signature header"))?;
    if !constant_time_eq(&expected_bytes, &provided_bytes) {
        return Err(UnifiedError::unauthorized("Invalid request signature"));
    }

    // The nonce is consumed only after the signature has verified: a
    // request that fails verification must not burn its nonce, or a
    // later legitimate retry with the same nonce would be rejected as
    // a replay. The check-and-insert stays under one lock hold so two
    // concurrent requests cannot both spend the same nonce.
    {
        let mut nonces = state.used_nonces.write().expect("nonce lock poisoned");
        let cutoff = Utc::now() - chrono::Duration::seconds(state.settings().replay_window_secs);
        nonces.retain(|_, seen| *seen > cutoff);
        if nonces.contains_key(&nonce) {
            return Err(UnifiedError::unauthorized("Request nonce has already been used"));
        }
        nonces.insert(nonce.clone(), Utc::now());
    }
    Ok(node)
}

pub(crate) fn join_url(base_url: &str, path: &str) -> String {
    format!("{}{}", base_url.trim_end_matches('/'), path)
}

pub(crate) fn signed_headers(
    _state: &Arc<ClustersState>,
    plan: &SyncPlanState,
    body: &[u8],
) -> UnifiedResult<Vec<(String, String)>> {
    let timestamp = Utc::now().timestamp();
    let nonce = Uuid::new_v4().to_string();
    let signature = sign_request(&plan.peer_secret_hash, timestamp, &nonce, body);
    Ok(vec![
        (HEADER_NODE_ID.to_string(), plan.local_node_id.clone()),
        (HEADER_TIMESTAMP.to_string(), timestamp.to_string()),
        (HEADER_NONCE.to_string(), nonce),
        (HEADER_SIGNATURE.to_string(), signature),
    ])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_round_trip() {
        assert_eq!(hex_encode(&[0x00, 0xff, 0x10, 0xab]), "00ff10ab");
        assert_eq!(hex_encode(&[]), "");
        assert_eq!(hex_decode("00ff10ab"), Some(vec![0x00, 0xff, 0x10, 0xab]));
        assert_eq!(hex_decode(" 00FF "), Some(vec![0x00, 0xff]));
        assert_eq!(hex_decode(""), Some(vec![]));
    }

    #[test]
    fn hex_decode_rejects_bad_input() {
        assert_eq!(hex_decode("0"), None);
        assert_eq!(hex_decode("zz"), None);
        assert_eq!(hex_decode("00f"), None);
        assert_eq!(hex_decode("00fg"), None);
    }

    #[test]
    fn hex_value_mapping() {
        assert_eq!(hex_value(b'0'), Some(0));
        assert_eq!(hex_value(b'9'), Some(9));
        assert_eq!(hex_value(b'a'), Some(10));
        assert_eq!(hex_value(b'f'), Some(15));
        assert_eq!(hex_value(b'A'), Some(10));
        assert_eq!(hex_value(b'F'), Some(15));
        assert_eq!(hex_value(b'z'), None);
        assert_eq!(hex_value(b' '), None);
    }

    #[test]
    fn sha256_known_vectors() {
        assert_eq!(
            sha256_hex(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn constant_time_eq_behaviour() {
        assert!(constant_time_eq(b"abc", b"abc"));
        assert!(constant_time_eq(b"", b""));
        assert!(!constant_time_eq(b"abc", b"abd"));
        assert!(!constant_time_eq(b"abc", b"ab"));
        assert!(!constant_time_eq(b"abc", b"abcd"));
    }

    #[test]
    fn secret_fingerprint_masks_short_hashes() {
        assert_eq!(
            secret_fingerprint(&"a".repeat(64)),
            "sha256:aaaaaaaa...aaaa"
        );
        assert_eq!(secret_fingerprint("abc"), "sha256:masked");
        assert_eq!(secret_fingerprint(""), "sha256:masked");
    }

    #[test]
    fn generate_secret_shape_and_uniqueness() {
        let first = generate_secret();
        let second = generate_secret();
        assert!(first.starts_with("cls_"));
        assert_eq!(first.len(), 69);
        assert_ne!(first, second);
    }

    #[test]
    fn hash_secret_matches_sha256() {
        assert_eq!(hash_secret("hello"), sha256_hex(b"hello"));
    }

    fn signed_request(
        node_id: Uuid,
        timestamp: i64,
        nonce: &str,
        signature: &str,
    ) -> HttpRequest {
        actix_web::test::TestRequest::post()
            .uri("/api/v1/clusters/sync/receive")
            .insert_header((HEADER_NODE_ID, node_id.to_string()))
            .insert_header((HEADER_TIMESTAMP, timestamp.to_string()))
            .insert_header((HEADER_NONCE, nonce))
            .insert_header((HEADER_SIGNATURE, signature))
            .to_http_request()
    }

    #[test]
    fn verify_node_request_does_not_burn_nonce_on_bad_signature() {
        let state = Arc::new(ClustersState::new());
        let secret_hash = hash_secret("test-secret");
        let node = NodeState {
            id: Uuid::now_v7(),
            cluster_id: Uuid::now_v7(),
            display_name: "peer".to_string(),
            base_url: "https://peer.example.com".to_string(),
            role: "member".to_string(),
            status: "active".to_string(),
            secret_hash: secret_hash.clone(),
            secret_fingerprint: secret_fingerprint(&secret_hash),
            health: "unknown".to_string(),
            consecutive_failures: 0,
            last_seen_at: None,
            last_latency_ms: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        };
        let node_id = node.id;
        state
            .nodes
            .write()
            .expect("nodes lock poisoned")
            .insert(node_id, node);
        let body = br#"{"hello":"world"}"#;
        let timestamp = Utc::now().timestamp();
        let nonce = "test-nonce-1";

        // A request with a bad signature fails and must not consume
        // the nonce: the same nonce with the right signature must
        // still verify afterwards.
        let bad = signed_request(node_id, timestamp, nonce, &"00".repeat(32));
        assert!(verify_node_request(&state, &bad, body).is_err());
        assert!(!state
            .used_nonces
            .read()
            .expect("nonce lock poisoned")
            .contains_key(nonce));

        let signature = sign_request(&secret_hash, timestamp, nonce, body);
        let good = signed_request(node_id, timestamp, nonce, &signature);
        let authed = verify_node_request(&state, &good, body).expect("valid request verifies");
        assert_eq!(authed.id, node_id);
        assert!(state
            .used_nonces
            .read()
            .expect("nonce lock poisoned")
            .contains_key(nonce));

        // Replaying the now-consumed nonce is rejected.
        let replay = signed_request(node_id, timestamp, nonce, &signature);
        assert!(verify_node_request(&state, &replay, body).is_err());
    }
    #[test]
    fn hex_encode_matches_reference_for_every_byte_value() {
        // Exhaustive oracle: the lookup-table encoding must agree
        // with plain per-byte formatting for all 256 byte values.
        // (`format!` is the reference here, in test code only.)
        for byte in 0..=u8::MAX {
            assert_eq!(hex_encode(&[byte]), format!("{byte:02x}"));
        }
        assert_eq!(hex_encode(&[0xde, 0xad, 0xbe, 0xef]), "deadbeef");
    }

}
