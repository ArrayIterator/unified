// E-signature module for the Unified server (`crates/external-esignature`).
//
// An e-signature workflow, evidence, and audit layer: signature
// requests with ordered signers, HMAC-signed expiring signing links,
// a hash-chained audit trail, reminders, and JSON certificates of
// completion.
//
// What this module is NOT: it does not render, modify, or seal PDF
// bytes. It records *that* a document (identified by its SHA-256
// hash) was signed, by whom, in what order, and when - and it proves
// the record afterwards with the hash-chained audit trail and the
// completion certificate. Producing a signed PDF from the certificate
// is a separate rendering step, outside this crate.
//
// Persistence model: in-memory DashMap store, authoritative for the
// running process (the same pattern as the calculator module's
// in-memory store). A database migration is the persistence target
// for requests, audit events, and certificates; until it lands, a
// restart loses the in-memory state. Nothing is ever written to
// disk by this crate.
//
// Security model:
//   - No hard-coded secrets. Signing-link HMAC keys come only from
//     the `ESIGNATURE_SECRET` environment variable (at least 32
//     characters). The secret is never logged and never appears in
//     any response.
//   - Signing tokens are HMAC-SHA256 over `v1.<request id>.<signer
//     id>.<expiry unix>`, expire with the request, and are
//     single-purpose: one token signs exactly one request as exactly
//     one signer.
//   - Every read and write is tenant-scoped. `tenant_id` is required
//     on writes (request body) and on reads (query parameter); a
//     request from another tenant is answered 404, never 403, so a
//     caller cannot probe for other tenants' requests.
//   - Callback URLs must be absolute http/https URLs without
//     credentials, and literal private/loopback/link-local IP hosts
//     are rejected.
//   - Audit events are hash-chained: each event stores the SHA-256
//     of the previous event hash plus its own payload, so tampering
//     with any event breaks the chain and is detected by the verify
//     endpoint.
//
// Events emitted on the hook bus:
//   - `esignature.request.sent` -- a request was sent
//     (`{"request_id", "tenant_id", "document_id", "signers",
//     "expires_at"}`); the mail/notifications modules deliver the
//     signing links.
//   - `esignature.request.completed` -- every signer signed
//     (`{"request_id", "tenant_id", "document_id",
//     "certificate_hash", "completed_at"}`).
//   - `esignature.request.declined` -- a signer declined
//     (`{"request_id", "tenant_id", "signer_id", "email", "reason"}`).
//   - `esignature.request.expired` -- the request passed its expiry
//     with signers still pending (`{"request_id", "tenant_id",
//     "document_id"}`).
//   - `esignature.reminder.requested` -- a reminder was recorded
//     (`{"request_id", "tenant_id", "pending_signers",
//     "expires_at"}`); the mail/notifications modules deliver it.
//     This module never sends email itself.
// Events consumed:
//   - `contracts.signed` -- emitted by the in-tree contracts module
//     (`src/modules/contracts.rs`: `contracts.signed` with
//     `{"contract_id", "title", "content_hash", "signed_at"}`; the
//     name is NOT in `unified_core::hooks::events`, it is verified
//     from the contracts module source). When a completed request
//     matches the contract (same `contract_id` when the request was
//     created with one, or the same document SHA-256 as the sealed
//     content hash), the request is linked back to that contract
//     and a `linked` audit event is appended.

use actix_web::{web, HttpRequest, HttpResponse};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::Sha256;
use std::net::IpAddr;
use std::sync::Arc;
use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::{HookBus, HookContext};
use unified_core::json::{error_response, success_response};
use unified_core::modules::{Module, ModuleRegistry, RouteInfo};
use unified_core::sharing::{constant_time_eq, sha256_hex};
use uuid::Uuid;

/// Module id. Also the URL prefix: `/api/v1/esignature/`.
pub const MODULE_ID: &str = "esignature";
/// Environment variable holding the HMAC secret for signing links.
pub const SECRET_ENV_VAR: &str = "ESIGNATURE_SECRET";
/// Minimum length of the HMAC secret, in characters.
pub const MIN_SECRET_LEN: usize = 32;
/// Minimum interval between two reminders for the same request.
pub const REMINDER_MIN_INTERVAL_SECS: i64 = 3600;
/// Maximum number of signers on one request.
pub const MAX_SIGNERS: usize = 50;

/// Event names emitted by this module.
pub mod events {
    pub const REQUEST_SENT: &str = "esignature.request.sent";
    pub const REQUEST_COMPLETED: &str = "esignature.request.completed";
    pub const REQUEST_DECLINED: &str = "esignature.request.declined";
    pub const REQUEST_EXPIRED: &str = "esignature.request.expired";
    pub const REMINDER_REQUESTED: &str = "esignature.reminder.requested";
}

/// `contracts.signed`, emitted by the in-tree contracts module. This
/// name is not in `unified_core::hooks::events`; it is verified from
/// `src/modules/contracts.rs` and `signing_links.rs`.
pub const EVENT_CONTRACTS_SIGNED: &str = "contracts.signed";

/// Genesis previous-hash for the first audit event of a request: 64
/// zeroes, the same shape as a SHA-256 hex digest.
const GENESIS_HASH: &str =
    "0000000000000000000000000000000000000000000000000000000000000000";

// Split from the parent file: the implementation now lives
// in the submodules below; the public paths are unchanged
// (this file re-exports everything that used to live here).
pub mod types;
pub mod crypto;
pub mod state;
pub mod handlers;
pub mod module;
pub use crypto::{verify_audit_events};
pub use module::{EsignatureModule, register, module};
pub use state::{EsignatureState};
pub use types::{RequestStatus, SignerStatus, SignatureRecord, Signer, SignatureRequest, AuditEvent, CertificateSigner, Certificate, SigningLink, ChainVerification, CertificateVerification, SignerInput, CreateRequestInput, SignInput, DeclineInput};
pub(crate) use crypto::{audit_hash_input, effective_status, issue_token, signing_secret, valid_email, valid_sha256_hex, valid_tenant_id, validate_callback_url, verify_token};
pub(crate) use handlers::{audit_handler, ceremony_handler, certificate_handler, create_request_handler, decline_handler, get_request_handler, list_requests_handler, remind_handler, send_request_handler, sign_handler, status_name, user_agent, verify_audit_handler, verify_certificate_handler};
pub(crate) use types::{ListQuery, TenantQuery};

// Unit tests

#[cfg(test)]
mod tests {
    use super::*;

    const TEST_SECRET: &[u8] = b"test-secret-that-is-long-enough-1234";

    fn test_input() -> CreateRequestInput {
        CreateRequestInput {
            tenant_id: "tenant-1".to_string(),
            document_id: "doc-1".to_string(),
            document_sha256: "a".repeat(64),
            title: "Service agreement".to_string(),
            signers: vec![
                SignerInput {
                    name: "Alice".to_string(),
                    email: "alice@example.com".to_string(),
                    role: "signer".to_string(),
                    order: 0,
                },
                SignerInput {
                    name: "Bob".to_string(),
                    email: "bob@example.com".to_string(),
                    role: "approver".to_string(),
                    order: 1,
                },
            ],
            expires_at: Utc::now() + chrono::Duration::days(7),
            callback_url: Some("https://example.com/hooks/esignature".to_string()),
            contract_id: None,
        }
    }

    fn sign_input(name: &str, hash_char: char) -> SignInput {
        SignInput {
            typed_name: name.to_string(),
            signature_hash: hash_char.to_string().repeat(64),
        }
    }

    fn token_from_link(link: &SigningLink) -> String {
        link.signing_url
            .strip_prefix("/api/v1/esignature/sign/")
            .expect("signing_url has the expected prefix")
            .to_string()
    }

    #[test]
    fn token_round_trip_and_tamper_detection() {
        let expires_at = Utc::now() + chrono::Duration::days(7);
        let token = issue_token(TEST_SECRET, "req-1", "signer-1", expires_at);
        let (request_id, signer_id, _) =
            verify_token(&token, TEST_SECRET).expect("fresh token verifies");
        assert_eq!(request_id, "req-1");
        assert_eq!(signer_id, "signer-1");
        // A tampered signature must not verify.
        let tampered = format!("{}0", &token[..token.len() - 1]);
        assert!(verify_token(&tampered, TEST_SECRET).is_err());
        // A token verified with the wrong secret must not verify.
        assert!(verify_token(&token, b"another-secret-that-is-long-enough-1").is_err());
    }

    #[test]
    fn token_expiry_is_enforced() {
        let expired_at = Utc::now() - chrono::Duration::hours(1);
        let token = issue_token(TEST_SECRET, "req-1", "signer-1", expired_at);
        let err = verify_token(&token, TEST_SECRET).expect_err("expired token must fail");
        assert_eq!(err.code, 410);
    }

    #[test]
    fn lifecycle_send_sign_in_order_completes_with_certificate() {
        let state = EsignatureState::new();
        let bus = HookBus::new();
        let request = state.create_request(test_input()).expect("valid input");
        assert_eq!(request.status, RequestStatus::Draft);
        let (sent, links) = state
            .send_request(&request.id, "tenant-1", TEST_SECRET, &bus)
            .expect("draft sends");
        assert_eq!(sent.status, RequestStatus::Sent);
        assert_eq!(links.len(), 2);
        let alice_token = token_from_link(&links[0]);
        let bob_token = token_from_link(&links[1]);
        let (partial, certificate) = state
            .record_signature(
                &alice_token,
                TEST_SECRET,
                sign_input("Alice", 'b'),
                "203.0.113.1",
                "test-agent",
                &bus,
            )
            .expect("first signer signs");
        assert_eq!(partial.status, RequestStatus::PartiallySigned);
        assert!(certificate.is_none());
        let (done, certificate) = state
            .record_signature(
                &bob_token,
                TEST_SECRET,
                sign_input("Bob", 'c'),
                "203.0.113.2",
                "test-agent",
                &bus,
            )
            .expect("second signer signs");
        assert_eq!(done.status, RequestStatus::Completed);
        let certificate = certificate.expect("completion issues a certificate");
        assert_eq!(certificate.request_id, request.id);
        assert_eq!(certificate.signers.len(), 2);
        let verification = state
            .verify_certificate(&certificate.certificate_hash)
            .expect("certificate verifies");
        assert!(verification.valid);
        let chain = state
            .verify_audit_chain(&request.id, "tenant-1", None)
            .expect("audit chain loads");
        assert!(chain.valid);
        assert!(chain.events_checked >= 5);
    }

    #[test]
    fn out_of_order_signing_is_rejected() {
        let state = EsignatureState::new();
        let bus = HookBus::new();
        let request = state.create_request(test_input()).expect("valid input");
        let (_, links) = state
            .send_request(&request.id, "tenant-1", TEST_SECRET, &bus)
            .expect("draft sends");
        let bob_token = token_from_link(&links[1]);
        let err = state
            .record_signature(
                &bob_token,
                TEST_SECRET,
                sign_input("Bob", 'c'),
                "203.0.113.2",
                "test-agent",
                &bus,
            )
            .expect_err("second signer must wait for the first");
        assert_eq!(err.code, 409);
    }

    #[test]
    fn invalid_inputs_are_rejected() {
        let state = EsignatureState::new();
        let mut bad_email = test_input();
        bad_email.signers[0].email = "not-an-email".to_string();
        assert_eq!(
            state.create_request(bad_email).expect_err("bad email").code,
            400
        );
        let mut bad_hash = test_input();
        bad_hash.document_sha256 = "xyz".to_string();
        assert_eq!(
            state.create_request(bad_hash).expect_err("bad hash").code,
            400
        );
        let mut past_expiry = test_input();
        past_expiry.expires_at = Utc::now() - chrono::Duration::hours(1);
        assert_eq!(
            state
                .create_request(past_expiry)
                .expect_err("past expiry")
                .code,
            400
        );
        let mut no_signers = test_input();
        no_signers.signers.clear();
        assert_eq!(
            state
                .create_request(no_signers)
                .expect_err("no signers")
                .code,
            400
        );
        let mut private_callback = test_input();
        private_callback.callback_url =
            Some("http://192.168.1.10/hooks/esignature".to_string());
        assert_eq!(
            state
                .create_request(private_callback)
                .expect_err("private callback IP")
                .code,
            400
        );
    }

    #[test]
    fn expiry_is_computed_on_read() {
        let state = EsignatureState::new();
        let bus = HookBus::new();
        let request = state.create_request(test_input()).expect("valid input");
        state
            .send_request(&request.id, "tenant-1", TEST_SECRET, &bus)
            .expect("draft sends");
        // Move the expiry into the past without touching the status.
        if let Some(mut stored) = state.requests.get_mut(&request.id) {
            stored.expires_at = Utc::now() - chrono::Duration::hours(1);
        }
        let read = state
            .load_for_tenant(&request.id, "tenant-1", None)
            .expect("request loads");
        assert_eq!(read.status, RequestStatus::Expired);
        let chain = state
            .verify_audit_chain(&request.id, "tenant-1", None)
            .expect("audit chain loads");
        assert!(chain.valid);
        assert!(state
            .audit_events(&request.id)
            .iter()
            .any(|event| event.kind == "expired"));
    }

    #[test]
    fn hash_chain_detects_tampering() {
        let state = EsignatureState::new();
        let request = state.create_request(test_input()).expect("valid input");
        let events = state.audit_events(&request.id);
        assert!(verify_audit_events(&events).valid);
        if let Some(mut chain) = state.audit.get_mut(&request.id) {
            chain[0].details = "tampered".to_string();
        }
        let tampered = state.audit_events(&request.id);
        let verification = verify_audit_events(&tampered);
        assert!(!verification.valid);
        assert_eq!(verification.broken_index, Some(0));
    }

    #[test]
    fn reminder_is_rate_limited_to_one_per_hour() {
        let state = EsignatureState::new();
        let bus = HookBus::new();
        let request = state.create_request(test_input()).expect("valid input");
        state
            .send_request(&request.id, "tenant-1", TEST_SECRET, &bus)
            .expect("draft sends");
        state
            .remind(&request.id, "tenant-1", &bus)
            .expect("first reminder records");
        let err = state
            .remind(&request.id, "tenant-1", &bus)
            .expect_err("second reminder within the hour must fail");
        assert_eq!(err.code, 429);
    }

    #[test]
    fn tenant_isolation_returns_not_found() {
        let state = EsignatureState::new();
        let request = state.create_request(test_input()).expect("valid input");
        let err = state
            .load_for_tenant(&request.id, "other-tenant", None)
            .expect_err("other tenant must not see the request");
        assert_eq!(err.code, 404);
        assert!(state.list_requests("other-tenant", None, None).is_empty());
        assert_eq!(state.list_requests("tenant-1", None, None).len(), 1);
    }

    #[test]
    fn decline_ends_the_request() {
        let state = EsignatureState::new();
        let bus = HookBus::new();
        let request = state.create_request(test_input()).expect("valid input");
        let (_, links) = state
            .send_request(&request.id, "tenant-1", TEST_SECRET, &bus)
            .expect("draft sends");
        let alice_token = token_from_link(&links[0]);
        let declined = state
            .decline_via_token(
                &alice_token,
                TEST_SECRET,
                DeclineInput {
                    reason: "Wrong document version".to_string(),
                },
                &bus,
            )
            .expect("pending signer declines");
        assert_eq!(declined.status, RequestStatus::Declined);
        let bob_token = token_from_link(&links[1]);
        assert!(state
            .record_signature(
                &bob_token,
                TEST_SECRET,
                sign_input("Bob", 'c'),
                "203.0.113.2",
                "test-agent",
                &bus,
            )
            .is_err());
    }
}
