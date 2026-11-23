// Unit tests: redaction helpers (pure functions).

use chrono::Utc;
use serde_json::json;

use super::*;

#[test]
fn redact_email_keeps_first_char_and_domain() {
    assert_eq!(redact_email("jane@example.com"), "j***@example.com");
    assert_eq!(redact_email("a@example.com"), "a***@example.com");
    assert_eq!(redact_email("not-an-email"), "***");
    assert_eq!(redact_email("  jane@example.com  "), "j***@example.com");
}

#[test]
fn redact_phone_keeps_last_two_digits() {
    assert_eq!(redact_phone("+62 812-3456-7890"), "+***90");
    assert_eq!(redact_phone("081234567890"), "***90");
    assert_eq!(redact_phone("123"), "***");
}

#[test]
fn redact_id_number_keeps_ends() {
    assert_eq!(redact_id_number("1234567890123456"), "12************56");
    assert_eq!(redact_id_number("1234"), "***");
    assert_eq!(redact_id_number("123456"), "12**56");
}

#[test]
fn redact_text_masks_email_phone_and_id_but_not_dates() {
    let input = "Reach jane@example.com or +62 812-3456-7890, ID 1234567890123456, on 2026-11-22 11:42:23.";
    let out = redact_text(input);
    assert!(out.contains("j***@example.com"), "{out}");
    assert!(out.contains("+***90"), "{out}");
    assert!(out.contains("12************56"), "{out}");
    assert!(out.contains("2026-11-22"), "{out}");
    assert!(!out.contains("jane@example.com"), "{out}");
    assert!(!out.contains("1234567890123456"), "{out}");
}

#[test]
fn redact_text_leaves_clean_text_alone() {
    assert_eq!(redact_text("Hello world, no PII here."), "Hello world, no PII here.");
}

#[test]
fn redact_value_redacts_pii_keys_and_nested_text() {
    let value = json!({
        "email": "jane@example.com",
        "phone": "+62 812-3456-7890",
        "name": "Jane Doe",
        "note": "Call jane@example.com please",
        "tags": ["plain", "jane@example.com"],
        "age": 41
    });
    let redacted = redact_value(&value, true);
    assert_eq!(redacted["email"], json!("j***@example.com"));
    assert_eq!(redacted["phone"], json!("+***90"));
    assert_eq!(redacted["name"], json!("J***"));
    assert_eq!(redacted["note"], json!("Call j***@example.com please"));
    assert_eq!(redacted["tags"][1], json!("j***@example.com"));
    assert_eq!(redacted["age"], json!(41));
}

#[test]
fn redact_value_keeps_names_when_not_flagged() {
    let value = json!({ "name": "Jane Doe", "email": "jane@example.com" });
    let redacted = redact_value(&value, false);
    assert_eq!(redacted["name"], json!("Jane Doe"));
    assert_eq!(redacted["email"], json!("j***@example.com"));
}

#[test]
fn is_pii_key_matches_markers() {
    assert!(is_pii_key("email_address"));
    assert!(is_pii_key("phone_number"));
    assert!(is_pii_key("home_address"));
    assert!(!is_pii_key("title"));
    assert!(!is_pii_key("status"));
}

#[test]
fn visibility_ordering_and_default() {
    assert_eq!(default_visibility(), Visibility::Private);
    assert!(Visibility::Private < Visibility::Company);
    assert!(Visibility::Company < Visibility::Public);
    assert_eq!(Visibility::parse("public"), Some(Visibility::Public));
    assert_eq!(Visibility::parse("bogus"), None);
}

#[test]
fn anonymization_is_deterministic_and_idempotent() {
    let state = PrivacyState::new();
    let first = state
        .anonymize_internal("user-1", &["crm".to_string()], &["email".to_string()], "test", "admin")
        .expect("anonymize");
    let second = state
        .anonymize_internal("user-1", &["crm".to_string()], &["email".to_string()], "test", "admin")
        .expect("anonymize again");
    assert_eq!(first.token, second.token);
    assert_eq!(first.field_tokens, second.field_tokens);
    assert_eq!(second.times_applied, 2);
    assert_ne!(first.token, "user-1");
    assert_eq!(first.token.len(), 64);
}

#[test]
fn consent_status_honours_withdrawal_and_expiry() {
    let now = Utc::now();
    let granted = ConsentRecord {
        id: new_id(),
        user_id: "u1".to_string(),
        consent_type: "marketing".to_string(),
        granted: true,
        granted_at: Some(now),
        withdrawn_at: None,
        expires_at: None,
        source: "api".to_string(),
        created_by: "u1".to_string(),
        created_at: now,
        updated_at: now,
    };
    assert_eq!(granted.status(now), "granted");
    let mut expired = granted.clone();
    expired.expires_at = Some(now - chrono::Duration::seconds(1));
    assert_eq!(expired.status(now), "expired");
    let mut withdrawn = granted.clone();
    withdrawn.granted = false;
    withdrawn.withdrawn_at = Some(now);
    assert_eq!(withdrawn.status(now), "withdrawn");
}

#[test]
fn current_notice_falls_back_to_generic() {
    let state = PrivacyState::new();
    let notice = state.current_notice("no-such-module");
    assert!(notice.generic);
    assert_eq!(notice.version, 0);
    assert_eq!(notice.text, GENERIC_NOTICE_TEXT);
}

#[test]
fn effective_visibility_defaults_to_private() {
    let state = PrivacyState::new();
    let effective = state.effective_visibility("no-such-module");
    assert_eq!(effective.visibility, Visibility::Private);
    assert_eq!(effective.source, "default");
}

#[test]
fn concurrent_grant_consent_creates_exactly_one_record() {
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicUsize, Ordering};
    let state = PrivacyState::new();
    let created = AtomicUsize::new(0);
    let ids = Mutex::new(Vec::new());
    std::thread::scope(|scope| {
        for _ in 0..8 {
            scope.spawn(|| {
                let (record, is_new) =
                    state.apply_consent_grant("user-1", "marketing", true, None, "api", "user-1");
                if is_new {
                    created.fetch_add(1, Ordering::SeqCst);
                }
                ids.lock().expect("ids lock poisoned").push(record.id);
            });
        }
    });
    assert_eq!(
        state.consents.len(),
        1,
        "exactly one consent record must exist"
    );
    assert_eq!(
        created.load(Ordering::SeqCst),
        1,
        "exactly one grant must report creation"
    );
    let ids = ids.lock().expect("ids lock poisoned");
    assert_eq!(ids.len(), 8);
    assert!(
        ids.iter().all(|id| *id == ids[0]),
        "all grants must return the same record id"
    );
    // Every history entry belongs to that single record.
    assert!(
        state
            .consent_history
            .iter()
            .all(|entry| entry.value().consent_id == ids[0])
    );
}

#[test]
fn regrant_after_withdraw_reuses_the_single_record() {
    let state = PrivacyState::new();
    let (first, is_new) =
        state.apply_consent_grant("user-1", "marketing", true, None, "api", "user-1");
    assert!(is_new);
    let withdrawn = state
        .withdraw_consent_record(&first.id, "user-1")
        .expect("withdraw");
    assert_eq!(withdrawn.status(Utc::now()), "withdrawn");
    let (again, is_new_again) =
        state.apply_consent_grant("user-1", "marketing", true, None, "api", "user-1");
    assert!(!is_new_again);
    assert_eq!(again.id, first.id);
    assert_eq!(again.status(Utc::now()), "granted");
    assert_eq!(state.consents.len(), 1);
}

#[test]
fn withdraw_already_withdrawn_consent_is_a_noop() {
    let state = PrivacyState::new();
    let (record, _) = state.apply_consent_grant("user-1", "marketing", true, None, "api", "user-1");
    let withdrawn = state
        .withdraw_consent_record(&record.id, "user-1")
        .expect("withdraw");
    assert!(!withdrawn.granted);
    assert!(withdrawn.withdrawn_at.is_some());
    let history_len = state.consent_history.len();
    let updated_at = withdrawn.updated_at;
    // Second withdraw: same record, unchanged, no new history.
    let again = state
        .withdraw_consent_record(&record.id, "user-1")
        .expect("withdraw again");
    assert_eq!(again.id, withdrawn.id);
    assert_eq!(again.updated_at, updated_at);
    assert!(!again.granted);
    assert_eq!(state.consent_history.len(), history_len);
}

#[test]
fn concurrent_fulfill_erasure_anonymizes_exactly_once() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let state = PrivacyState::new();
    let request = ErasureRequest {
        id: new_id(),
        user_id: "user-1".to_string(),
        modules: vec!["crm".to_string()],
        reason: "test".to_string(),
        status: "approved".to_string(),
        requested_by: "user-1".to_string(),
        reviewed_by: Some("admin".to_string()),
        reviewed_at: Some(Utc::now()),
        rejection_reason: None,
        fulfilled_at: None,
        anonymization_id: None,
        created_at: Utc::now(),
        updated_at: Utc::now(),
    };
    state
        .erasure_requests
        .insert(request.id.clone(), request.clone());
    let request_id = request.id.clone();
    let succeeded = AtomicUsize::new(0);
    let conflicted = AtomicUsize::new(0);
    std::thread::scope(|scope| {
        for _ in 0..8 {
            scope.spawn(
                || match state.fulfill_erasure_request(&request_id, "admin") {
                    Ok(_) => {
                        succeeded.fetch_add(1, Ordering::SeqCst);
                    }
                    Err(error) => {
                        assert_eq!(
                            error.code, 409,
                            "second fulfill must be a conflict: {}",
                            error.message
                        );
                        conflicted.fetch_add(1, Ordering::SeqCst);
                    }
                },
            );
        }
    });
    assert_eq!(
        succeeded.load(Ordering::SeqCst),
        1,
        "exactly one fulfill must succeed"
    );
    assert_eq!(
        conflicted.load(Ordering::SeqCst),
        7,
        "the other fulfills must conflict"
    );
    let stored = state
        .erasure_requests
        .get(&request_id)
        .expect("request still present");
    assert_eq!(stored.status, "fulfilled");
    assert!(stored.fulfilled_at.is_some());
    assert!(stored.anonymization_id.is_some());
    // Anonymization ran exactly once: times_applied is 1.
    let anonymization = state
        .anonymizations
        .get("user-1")
        .expect("anonymization record");
    assert_eq!(anonymization.times_applied, 1);
}
