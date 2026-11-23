// Marketing Automation — In-memory state and its constructors.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::{HookBus, HookContext};
use unified_core::json as api_json;
use unified_core::modules::{Module, ModuleRegistry, RouteInfo, RouteVersionSupport};
use unified_core::pagination::{PageResult, PaginationParams};
use unified_core::sharing::constant_time_eq;
use actix_web::{web, HttpRequest, HttpResponse};
use base64::Engine;
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::Sha256;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, RwLock};
use uuid::Uuid;

use super::constants::*;
use super::types::*;
use super::helpers::*;
use super::validation::*;
use super::service::*;
use super::service_sequences::*;
use super::handlers_campaigns::*;
use super::handlers_segments::*;
use super::handlers_sequences::*;
use super::handlers_scores::*;
use super::handlers_audience::*;
use super::hooks::*;
use super::module::*;

// ---------------------------------------------------------------------------
// State.
// ---------------------------------------------------------------------------

pub struct MarketingState {
    pub(crate) hooks: RwLock<Option<Arc<HookBus>>>,
    pub(crate) debug: AtomicBool,
    pub(crate) settings: RwLock<MarketingSettings>,
    pub(crate) campaigns: DashMap<String, MarketingCampaign>,
    pub(crate) variants: DashMap<String, CampaignVariant>,
    pub(crate) segments: DashMap<String, Segment>,
    pub(crate) segment_history: DashMap<String, Vec<SegmentSizeSnapshot>>,
    pub(crate) segment_members: DashMap<String, Vec<String>>,
    pub(crate) sequences: DashMap<String, Sequence>,
    pub(crate) steps: DashMap<String, SequenceStep>,
    pub(crate) enrollments: DashMap<String, Enrollment>,
    pub(crate) scores: DashMap<String, ContactScore>,
    pub(crate) score_rules: DashMap<String, ScoreRule>,
    pub(crate) sends: DashMap<String, MarketingSend>,
    pub(crate) audience: DashMap<String, AudienceContact>,
    pub(crate) suppressions: DashMap<String, Suppression>,
    pub(crate) consent_cache: DashMap<String, ConsentCacheEntry>,
    pub(crate) tracking_events: DashMap<String, TrackingEvent>,
    /// Per-process secret for signing recipient unsubscribe tokens.
    /// Tokens stop verifying after a restart (the stores are
    /// in-memory too, so nothing outlives the process anyway).
    /// Same pattern as the newsletter module's token secret.
    pub(crate) token_secret: String,
}

impl MarketingState {
    pub(crate) fn new() -> Self {
        Self {
            hooks: RwLock::new(None),
            debug: AtomicBool::new(false),
            settings: RwLock::new(MarketingSettings::default()),
            campaigns: DashMap::new(),
            variants: DashMap::new(),
            segments: DashMap::new(),
            segment_history: DashMap::new(),
            segment_members: DashMap::new(),
            sequences: DashMap::new(),
            steps: DashMap::new(),
            enrollments: DashMap::new(),
            scores: DashMap::new(),
            score_rules: DashMap::new(),
            sends: DashMap::new(),
            audience: DashMap::new(),
            suppressions: DashMap::new(),
            consent_cache: DashMap::new(),
            tracking_events: DashMap::new(),
            token_secret: Uuid::new_v4().to_string(),
        }
    }

    pub(crate) fn set_hooks(&self, hooks: Arc<HookBus>) {
        *self.hooks.write().expect("marketing hooks lock poisoned") = Some(hooks);
    }

    pub(crate) fn hooks(&self) -> Option<Arc<HookBus>> {
        self.hooks
            .read()
            .expect("marketing hooks lock poisoned")
            .clone()
    }

    pub(crate) fn is_debug(&self) -> bool {
        self.debug.load(Ordering::SeqCst)
    }

    pub(crate) fn settings_snapshot(&self) -> MarketingSettings {
        self.settings
            .read()
            .expect("marketing settings lock poisoned")
            .clone()
    }

    pub(crate) fn emit(&self, event: &str, data: Value) {
        if let Some(hooks) = self.hooks() {
            hooks.emit(event, data);
        }
    }

    /// Build a signed unsubscribe token for a recipient: base64url
    /// of `email:expiry_unix:campaign_id:unsubscribe`, plus a hex
    /// HMAC-SHA256 over that payload. `campaign_id` is empty for a
    /// global unsubscribe. Email templates link to
    /// POST /api/v1/marketing/unsubscribe with this token, so a
    /// recipient can unsubscribe without admin credentials; the
    /// token binds the address (and campaign, when given) and
    /// expires after `ttl_secs`.
    pub(crate) fn sign_unsubscribe_token(
        &self,
        email: &str,
        campaign_id: Option<&str>,
        ttl_secs: i64,
    ) -> String {
        let expiry = Utc::now().timestamp() + ttl_secs;
        let payload = format!(
            "{}:{}:{}:unsubscribe",
            email.trim().to_lowercase(),
            expiry,
            campaign_id.unwrap_or("")
        );
        let mut mac = Hmac::<Sha256>::new_from_slice(self.token_secret.as_bytes())
            .expect("HMAC key length is always valid");
        mac.update(payload.as_bytes());
        let signature = mac.finalize().into_bytes();
        let encoded =
            base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(payload.as_bytes());
        format!("{encoded}.{}", hex_encode(&signature))
    }

    /// Verify a signed unsubscribe token and return the
    /// (email, campaign_id) it was minted for (`None` campaign
    /// means a global unsubscribe).
    pub(crate) fn verify_unsubscribe_token(
        &self,
        token: &str,
    ) -> UnifiedResult<(String, Option<String>)> {
        let (encoded, signature) = token
            .split_once('.')
            .ok_or_else(|| UnifiedError::bad_request("Invalid token format"))?;
        let payload = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(encoded)
            .map_err(|_| UnifiedError::bad_request("Invalid token encoding"))?;
        let payload = String::from_utf8(payload)
            .map_err(|_| UnifiedError::bad_request("Invalid token encoding"))?;
        let mut mac = Hmac::<Sha256>::new_from_slice(self.token_secret.as_bytes())
            .expect("HMAC key length is always valid");
        mac.update(payload.as_bytes());
        let expected = hex_encode(&mac.finalize().into_bytes());
        if !constant_time_eq(signature.as_bytes(), expected.as_bytes()) {
            return Err(UnifiedError::unauthorized("Invalid token signature"));
        }
        let mut parts = payload.split(':');
        let (Some(email), Some(expiry), Some(campaign_id), Some(purpose)) = (
            parts.next(),
            parts.next(),
            parts.next(),
            parts.next(),
        ) else {
            return Err(UnifiedError::bad_request("Invalid token payload"));
        };
        if purpose != "unsubscribe" {
            return Err(UnifiedError::bad_request(
                "This token is not an unsubscribe token",
            ));
        }
        let expiry: i64 = expiry
            .parse()
            .map_err(|_| UnifiedError::bad_request("Invalid token expiry"))?;
        if Utc::now().timestamp() > expiry {
            return Err(UnifiedError::unauthorized("Token has expired"));
        }
        Ok((
            email.to_string(),
            if campaign_id.is_empty() {
                None
            } else {
                Some(campaign_id.to_string())
            },
        ))
    }
}

pub(crate) fn hex_encode(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for &byte in bytes {
        out.push(HEX[(byte >> 4) as usize] as char);
        out.push(HEX[(byte & 0x0f) as usize] as char);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unsubscribe_token_sign_verify_round_trip_and_rejections() {
        let state = MarketingState::new();
        // Global token round-trips to (email, None).
        let token = state.sign_unsubscribe_token("Ana@Example.com", None, 3600);
        assert_eq!(
            state.verify_unsubscribe_token(&token).unwrap(),
            ("ana@example.com".to_string(), None)
        );
        // Per-campaign token binds the campaign id.
        let campaign_token =
            state.sign_unsubscribe_token("ana@example.com", Some("camp-1"), 3600);
        assert_eq!(
            state.verify_unsubscribe_token(&campaign_token).unwrap(),
            ("ana@example.com".to_string(), Some("camp-1".to_string()))
        );
        // A token minted for one address never verifies as another.
        assert_ne!(
            state
                .verify_unsubscribe_token(&campaign_token)
                .unwrap()
                .0,
            "bob@example.com"
        );
        // Tampered payload and tampered signature are both rejected.
        let mut tampered = token.clone();
        tampered.replace_range(0..1, "A");
        assert!(state.verify_unsubscribe_token(&tampered).is_err());
        let tampered_sig = format!("{}.{}", token.split_once('.').unwrap().0, "00");
        assert!(state.verify_unsubscribe_token(&tampered_sig).is_err());
        // Malformed tokens are rejected, and an expired token is
        // rejected even though its signature is valid.
        assert!(state.verify_unsubscribe_token("not-a-token").is_err());
        assert!(state.verify_unsubscribe_token("!!!.00").is_err());
        let expired = state.sign_unsubscribe_token("ana@example.com", None, -10);
        assert!(state.verify_unsubscribe_token(&expired).is_err());
        // A token signed under another secret does not verify here.
        let other = MarketingState::new();
        let foreign = other.sign_unsubscribe_token("ana@example.com", None, 3600);
        assert!(state.verify_unsubscribe_token(&foreign).is_err());
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
