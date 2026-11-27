// Newsletter module - in-memory state.
//
// Split from `src/modules/newsletter.rs`; the module root re-exports the public API of this module.

use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::HookBus;
use actix_web::HttpRequest;
use base64::Engine;
use chrono::Utc;
use dashmap::DashMap;
use hmac::{Hmac, Mac};
use serde_json::{json, Value};
use sha2::Sha256;
use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock, RwLock};
use uuid::Uuid;

use super::constants::EVENT_ROLE_CHECK;
use super::helpers::{hex_encode, signatures_equal};
use super::types::{Campaign, NewsletterList, Subscriber, TrackingEvent};

// In-memory stores (same pattern as the other modules in this tree).

pub(crate) static LISTS: OnceLock<DashMap<String, NewsletterList>> = OnceLock::new();
pub(crate) static SUBSCRIBERS: OnceLock<DashMap<String, Subscriber>> = OnceLock::new();
pub(crate) static CAMPAIGNS: OnceLock<DashMap<String, Campaign>> = OnceLock::new();
pub(crate) static TRACKING_EVENTS: OnceLock<DashMap<String, TrackingEvent>> = OnceLock::new();
/// Campaign id -> distinct normalized click-target URLs recorded when
/// campaign HTML was personalized. The click tracker only redirects
/// to a URL in this set (or to the list's own domain).
pub(crate) static CLICK_URLS: OnceLock<DashMap<String, HashSet<String>>> = OnceLock::new();
/// Keys already counted as a unique open/click, in the form
/// `campaign_id\0subscriber_id\0kind`. Claimed atomically through the
/// map entry API so concurrent duplicate events count only once.
pub(crate) static SEEN_TRACKING: OnceLock<DashMap<String, ()>> = OnceLock::new();

pub(crate) fn lists_store() -> &'static DashMap<String, NewsletterList> {
    LISTS.get_or_init(DashMap::new)
}

pub(crate) fn subscribers_store() -> &'static DashMap<String, Subscriber> {
    SUBSCRIBERS.get_or_init(DashMap::new)
}

pub(crate) fn campaigns_store() -> &'static DashMap<String, Campaign> {
    CAMPAIGNS.get_or_init(DashMap::new)
}

pub(crate) fn tracking_store() -> &'static DashMap<String, TrackingEvent> {
    TRACKING_EVENTS.get_or_init(DashMap::new)
}

pub(crate) fn click_urls_store() -> &'static DashMap<String, HashSet<String>> {
    CLICK_URLS.get_or_init(DashMap::new)
}

pub(crate) fn seen_tracking_store() -> &'static DashMap<String, ()> {
    SEEN_TRACKING.get_or_init(DashMap::new)
}

// Caller identity and capability checks (N2).
//
// Same pattern as the backup and workflows modules: the caller
// identity comes from the `X-User-Id` and `X-Role` headers, and the
// capability is checked through the `role.check` hook, failing closed
// when no handler answers.

/// Caller identity: the server-verified caller (user id plus
/// server-resolved role, both required for capability checks).
pub(crate) struct Caller {
    pub(crate) actor: String,
    pub(crate) role: String,
}

pub(crate) fn caller(req: &HttpRequest) -> UnifiedResult<Caller> {
    let verified = unified_core::caller::require_caller(req)?;
    let actor = verified.user_id.clone();
    let role = verified.role().to_string();
    Ok(Caller { actor, role })
}

/// Ask the roles module whether `role` holds `permission` through
/// the `role.check` hook. Fails closed: no hook bus, or no answer,
/// means not allowed.
pub(crate) fn check_capability(state: &NewsletterState, role: &str, permission: &str) -> bool {
    let Some(hooks) = state.hooks() else {
        return false;
    };
    let context = hooks.emit(
        EVENT_ROLE_CHECK,
        json!({ "role": role, "permission": permission, "allowed": false }),
    );
    context
        .get("allowed")
        .and_then(|value| value.as_bool())
        .unwrap_or(false)
}

pub(crate) fn require_permission(
    state: &NewsletterState,
    role: &str,
    permission: &str,
) -> UnifiedResult<()> {
    if check_capability(state, role, permission) {
        Ok(())
    } else {
        Err(UnifiedError::forbidden(format!(
            "Role \"{role}\" does not hold permission \"{permission}\""
        )))
    }
}

/// Read the caller from the request headers and require `permission`.
/// Returns the caller on success; handlers turn the error into a
/// 401/403 response.
pub(crate) fn authorize(
    req: &HttpRequest,
    state: &NewsletterState,
    permission: &str,
) -> UnifiedResult<Caller> {
    let found = caller(req)?;
    require_permission(state, &found.role, permission)?;
    Ok(found)
}
// Tenant edge guard (C3 pilot 4, gate-only).

/// Fail-closed tenant gate: resolves the tenant context the
/// tenant-resolution middleware attached to the request. A request
/// with no resolved tenant context is rejected (401) and never falls
/// back to the `public` schema (the shared `TenantContext` contract
/// in `unified_core::tenancy`). Every tenant-data handler in this
/// module calls this FIRST, before `caller` / `authorize` or any
/// capability check.
///
/// This module's records carry no `company_id` / `tenant_id` scope
/// field, so the guard is a fail-closed gate only: no per-record
/// company check is applied. Real partitioning of these records
/// needs a `tenant_id` on the record (C11 follow-up).
pub(crate) fn require_tenant(
    req: &HttpRequest,
) -> UnifiedResult<unified_core::tenancy::TenantContext> {
    unified_core::tenancy::TenantContext::from_http_request(req)
}

/// Company scope of the verified caller: the tenant context's
/// company first, the server-resolved caller company second; a
/// mismatch between the two is a 403. The `X-Company-Id` header is
/// never read. Resolved in every gated handler so a tenant/caller
/// mismatch fails closed even though these records carry no company
/// field to filter on (gate-only; see `require_tenant`).
pub(crate) fn caller_company(
    tenant: &unified_core::tenancy::TenantContext,
    req: &HttpRequest,
) -> UnifiedResult<Option<String>> {
    let verified = unified_core::caller::caller_from(req)
        .company_id
        .filter(|value| !value.is_empty());
    match (&tenant.company_id, &verified) {
        (Some(tenant_company), Some(verified_company)) if tenant_company != verified_company => {
            Err(UnifiedError::forbidden(
                "Verified caller company does not match the tenant company scope",
            ))
        }
        (Some(tenant_company), _) => Ok(Some(tenant_company.clone())),
        (None, verified) => Ok(verified.clone()),
    }
}

// State.

pub struct NewsletterState {
    pub(crate) hooks: RwLock<Option<Arc<HookBus>>>,
    pub(crate) debug: AtomicBool,
    /// Per-process secret for signing confirmation and unsubscribe
    /// tokens. Tokens stop verifying after a restart (the stores are
    /// in-memory too, so nothing outlives the process anyway).
    pub(crate) token_secret: String,
}

impl NewsletterState {
    pub(crate) fn new() -> Self {
        Self {
            hooks: RwLock::new(None),
            debug: AtomicBool::new(false),
            token_secret: Uuid::new_v4().to_string(),
        }
    }

    pub(crate) fn set_hooks(&self, hooks: Arc<HookBus>) {
        *self.hooks.write().expect("newsletter hooks lock poisoned") = Some(hooks);
    }

    pub(crate) fn hooks(&self) -> Option<Arc<HookBus>> {
        self.hooks
            .read()
            .expect("newsletter hooks lock poisoned")
            .clone()
    }

    pub(crate) fn is_debug(&self) -> bool {
        self.debug.load(Ordering::SeqCst)
    }

    pub(crate) fn emit(&self, event: &str, data: Value) {
        if let Some(hooks) = self.hooks() {
            hooks.emit(event, data);
        }
    }

    /// Build a signed token for a subscriber: base64url of
    /// `subscriber_id:expiry_unix:purpose`, plus a hex HMAC-SHA256
    /// over that payload.
    pub(crate) fn sign_token(&self, subscriber_id: &str, purpose: &str, ttl_secs: i64) -> String {
        let expiry = Utc::now().timestamp() + ttl_secs;
        let payload = format!("{subscriber_id}:{expiry}:{purpose}");
        let mut mac = Hmac::<Sha256>::new_from_slice(self.token_secret.as_bytes())
            .expect("HMAC key length is always valid");
        mac.update(payload.as_bytes());
        let signature = mac.finalize().into_bytes();
        let encoded = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(payload.as_bytes());
        format!("{encoded}.{}", hex_encode(&signature))
    }

    /// Verify a signed token and return (subscriber_id, purpose).
    pub(crate) fn verify_token(&self, token: &str) -> UnifiedResult<(String, String)> {
        let (encoded, signature) = token.split_once('.').ok_or_else(|| {
            UnifiedError::bad_request("Invalid token format")
        })?;
        let payload = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(encoded)
            .map_err(|_| UnifiedError::bad_request("Invalid token encoding"))?;
        let payload = String::from_utf8(payload)
            .map_err(|_| UnifiedError::bad_request("Invalid token encoding"))?;
        let mut mac = Hmac::<Sha256>::new_from_slice(self.token_secret.as_bytes())
            .expect("HMAC key length is always valid");
        mac.update(payload.as_bytes());
        let expected = hex_encode(&mac.finalize().into_bytes());
        if !signatures_equal(signature, &expected) {
            return Err(UnifiedError::unauthorized("Invalid token signature"));
        }
        let mut parts = payload.split(':');
        let (Some(subscriber_id), Some(expiry), Some(purpose)) =
            (parts.next(), parts.next(), parts.next())
        else {
            return Err(UnifiedError::bad_request("Invalid token payload"));
        };
        // Reject trailing segments: a token is exactly
        // `subscriber_id:expiry:purpose`, so a fourth segment means
        // the payload was extended after signing.
        if parts.next().is_some() {
            return Err(UnifiedError::bad_request("Invalid token payload"));
        }
        let expiry: i64 = expiry
            .parse()
            .map_err(|_| UnifiedError::bad_request("Invalid token expiry"))?;
        if Utc::now().timestamp() > expiry {
            return Err(UnifiedError::unauthorized("Token has expired"));
        }
        Ok((subscriber_id.to_string(), purpose.to_string()))
    }
}

#[cfg(test)]
mod tests {

    /// Build a request carrying the server-verified caller the auth
    /// middleware would attach for this user and role. The old
    /// self-asserted identity headers grant nothing,
    /// so tests attach the verified caller to the request
    /// extensions instead.
    fn verified_request(user_id: &str, role: &str) -> actix_web::HttpRequest {
        use actix_web::HttpMessage;
        let req = actix_web::test::TestRequest::default().to_http_request();
        req.extensions_mut().insert(
            unified_core::auth::Caller::verified(test_user_uuid(user_id), Vec::new())
                .with_roles(vec![role.to_string()]),
        );
        req
    }

    /// Build a request carrying a verified caller with no roles
    /// resolved: the caller resolves, but holds the anonymous role.
    /// This is the "no role" case in the new model (roles are
    /// resolved server-side, never claimed in a header).
    fn verified_request_without_roles(user_id: &str) -> actix_web::HttpRequest {
        use actix_web::HttpMessage;
        let req = actix_web::test::TestRequest::default().to_http_request();
        req.extensions_mut()
            .insert(unified_core::auth::Caller::verified(
                test_user_uuid(user_id),
                Vec::new(),
            ));
        req
    }

    /// Deterministic test-user identity: distinct names map to
    /// distinct UUIDs (FNV-1a folded into the low bits), so
    /// ownership tests can still tell two users apart. A value
    /// that already parses as a UUID is used as-is.
    fn test_user_uuid(name: &str) -> uuid::Uuid {
        if let Ok(id) = uuid::Uuid::parse_str(name) {
            return id;
        }
        let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
        for byte in name.bytes() {
            hash ^= u64::from(byte);
            hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        }
        uuid::Uuid::from_u128(u128::from(hash) | 0x0000_0000_0000_4000_8000_0000_0000_0000)
    }

    use super::*;

    #[test]
    fn token_sign_verify_round_trip() {
        let state = NewsletterState::new();
        let token = state.sign_token("sub-1", "confirm", 3600);
        assert_eq!(
            state.verify_token(&token).unwrap(),
            ("sub-1".to_string(), "confirm".to_string())
        );
    }

    #[test]
    fn token_verify_rejects_tampered_malformed_and_expired() {
        let state = NewsletterState::new();
        let token = state.sign_token("sub-1", "confirm", 3600);
        assert!(state.verify_token(&format!("{token}x")).is_err());
        assert!(state.verify_token("no-dot-here").is_err());
        let expired = state.sign_token("sub-1", "confirm", -10);
        assert!(state.verify_token(&expired).is_err());
    }

    #[test]
    fn token_verify_rejects_trailing_payload_segments() {
        // A token whose payload carries a fourth segment must fail
        // even though the signature is valid for that payload: the
        // format is exactly `subscriber_id:expiry:purpose`.
        let state = NewsletterState::new();
        let expiry = Utc::now().timestamp() + 3600;
        let payload = format!("sub-1:{expiry}:confirm:junk");
        let mut mac = Hmac::<Sha256>::new_from_slice(state.token_secret.as_bytes())
            .expect("HMAC key length is always valid");
        mac.update(payload.as_bytes());
        let signature = hex_encode(&mac.finalize().into_bytes());
        let encoded =
            base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(payload.as_bytes());
        let token = format!("{encoded}.{signature}");
        assert!(state.verify_token(&token).is_err());
    }

    #[test]
    fn caller_requires_a_verified_caller() {
        use actix_web::test::TestRequest;
        let missing_both = TestRequest::default().to_http_request();
        assert!(caller(&missing_both).is_err());
        let missing_role = TestRequest::default().to_http_request();
        assert!(caller(&missing_role).is_err());
        let ok = verified_request("user-1", "admin");
        let found = caller(&ok).unwrap();
        assert_eq!(found.actor, test_user_uuid("user-1").to_string());
        assert_eq!(found.role, "admin");
    }

    #[test]
    fn require_permission_fails_closed_without_a_role_check_handler() {
        // No hook bus is attached, so the check must refuse.
        let state = NewsletterState::new();
        assert!(!check_capability(&state, "admin", "newsletter:manage"));
        assert!(require_permission(&state, "admin", "newsletter:manage").is_err());
    }

    #[test]
    fn authorize_accepts_when_role_check_allows() {
        use actix_web::test::TestRequest;
        let state = NewsletterState::new();
        let bus = Arc::new(HookBus::new());
        bus.register(
            "role.check",
            "test",
            Arc::new(|context: &mut unified_core::hooks::HookContext| {
                context.set("allowed", serde_json::Value::Bool(true));
            }),
        );
        state.set_hooks(bus);
        let req = verified_request("user-1", "admin");
        assert!(authorize(&req, &state, "newsletter:manage").is_ok());
        // A request without identity headers is refused even when
        // the role check would allow the role.
        let anonymous = TestRequest::default().to_http_request();
        assert!(authorize(&anonymous, &state, "newsletter:manage").is_err());
    }
}
