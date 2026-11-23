//! Auth Manager - JWT helpers: base64url, HMAC signing, header and claims types.

use crate::auth::{password as auth_password, totp as auth_totp};
use unified_core::context::AppContext;
use unified_core::error::UnifiedError;
use unified_core::hardening::{shared_state, trusted_client_ip_for_http_request};
use unified_core::hooks::events;
use unified_core::json as api_json;
use unified_core::modules::{Module, ModuleRegistry, RouteInfo};
use unified_core::db::models::User;
use unified_core::db::DbPool;
use actix_web::http::header;
use actix_web::{web, HttpRequest, HttpResponse};
use base64::Engine;
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use diesel::prelude::*;
use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use sha2::Sha256;
use std::sync::Arc;
use std::time::Duration;
use uuid::Uuid;
use super::{providers::*, state::*, handlers_providers::*, login::*, tokens::*, recovery::*, totp::*, google::*, module::*};

// ---------------------------------------------------------------------------
// JWT helpers
// ---------------------------------------------------------------------------

/// Clock-skew leeway for JWT time claims, in seconds. A token whose
/// `iat` is further in the future than this is rejected as
/// not-yet-valid, which also bounds how far a verifier's clock may
/// lag the issuer's.
pub(crate) const JWT_CLOCK_SKEW_LEEWAY_SECS: i64 = 60;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct JwtHeader {
    pub(crate) alg: String,
    pub(crate) typ: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct JwtClaims {
    pub(crate) sub: String,
    pub(crate) iss: String,
    pub(crate) iat: i64,
    pub(crate) exp: i64,
    pub(crate) jti: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) scopes: Vec<String>,
}

pub(crate) fn base64url_encode(data: &[u8]) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(data)
}

pub(crate) fn base64url_decode(data: &str) -> Result<Vec<u8>, UnifiedError> {
    base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(data)
        .map_err(|_| UnifiedError::bad_request("token is not valid base64url"))
}

pub(crate) fn hmac_sign(key: &[u8], message: &str) -> Vec<u8> {
    let mut mac = HmacSha256::new_from_slice(key).expect("HMAC accepts any key length");
    mac.update(message.as_bytes());
    mac.finalize().into_bytes().to_vec()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64url_encode_known_vector() {
        assert_eq!(base64url_encode(b"hello"), "aGVsbG8");
        assert_eq!(base64url_encode(b""), "");
        // No padding and no '+' or '/' in the output.
        assert_eq!(base64url_encode(&[0xfb, 0xff]), "-_8");
    }

    #[test]
    fn base64url_roundtrip() {
        for data in [b"".as_slice(), b"a", b"hello world", &[0u8, 1, 2, 250, 255]] {
            let encoded = base64url_encode(data);
            assert_eq!(base64url_decode(&encoded).expect("roundtrip"), data);
        }
    }

    #[test]
    fn base64url_decode_rejects_invalid() {
        assert!(base64url_decode("!!!").is_err());
        assert!(base64url_decode("aGVsbG8=").is_err());
    }

    #[test]
    fn hmac_sign_is_deterministic_and_keyed() {
        let first = hmac_sign(b"key", "message");
        let second = hmac_sign(b"key", "message");
        assert_eq!(first, second);
        assert_eq!(first.len(), 32);
        assert_ne!(first, hmac_sign(b"other-key", "message"));
        assert_ne!(first, hmac_sign(b"key", "other message"));
    }
}
