//! Hook handlers for document deletion and user deletion.

use super::state::SignState;
use super::types::{
    DELETED_USER_ID, DELETED_USER_NAME, EVENT_ENVELOPE_VOIDED, EnvelopeStatus, SignerStatus,
};
use chrono::Utc;
use serde_json::json;
use unified_core::hooks::HookContext;

/// Id of the document a hook payload refers to. Payloads carry
/// it under `document_id`, falling back to `id` and to
/// `reference_id`, the shapes other modules use for
/// cross-module references.
fn document_id_from(ctx: &HookContext) -> Option<String> {
    for key in ["document_id", "id", "reference_id"] {
        if let Some(raw) = ctx.get(key).and_then(|value| value.as_str()) {
            if !raw.is_empty() {
                return Some(raw.to_string());
            }
        }
    }
    None
}

/// `documents.deleted` voids every live envelope that references
/// the deleted document, revokes its outstanding signing tokens,
/// and records the void in the audit trail. Completed envelopes
/// keep their certificate: the signature evidence survives the
/// document.
pub(crate) fn on_document_deleted(state: SignState, ctx: &mut HookContext) {
    let Some(document_id) = document_id_from(ctx) else {
        ctx.add_result(
            json!({ "sign": "skipped", "reason": "documents.deleted carries no document id" }),
        );
        return;
    };
    let now = Utc::now();
    let mut voided = 0i64;
    for mut entry in state.envelopes.iter_mut() {
        let envelope = entry.value_mut();
        if envelope.status.is_terminal() {
            continue;
        }
        if !envelope.document_ids.iter().any(|id| *id == document_id) {
            continue;
        }
        let envelope_id = envelope.id;
        let tenant_id = envelope.tenant_id.clone();
        envelope.status = EnvelopeStatus::Voided;
        envelope.updated_at = now;
        for mut signer in state.signers.iter_mut() {
            if signer.value().envelope_id == envelope_id
                && matches!(
                    signer.value().status,
                    SignerStatus::Pending | SignerStatus::Sent | SignerStatus::Viewed
                )
            {
                signer.value_mut().token_hash = String::new();
                signer.value_mut().token_expires_at = None;
                signer.value_mut().updated_at = now;
            }
        }
        drop(entry);
        super::helpers::append_audit(
            &state,
            &tenant_id,
            envelope_id,
            None,
            "voided",
            "system",
            Some(format!("Document {document_id} was deleted")),
        );
        state.emit(
            EVENT_ENVELOPE_VOIDED,
            json!({
                "tenant_id": tenant_id,
                "envelope_id": envelope_id.to_string(),
                "document_id": document_id,
                "reason": "document_deleted",
            }),
        );
        voided += 1;
    }
    ctx.add_result(json!({
        "sign": "document_deleted",
        "document_id": document_id,
        "voided_envelopes": voided,
    }));
}

/// `user.deleted` carries `user_id` (falling back to `id`, the
/// same shape the forums module reads). The user's envelopes and
/// templates keep their history but lose the identity
/// (`created_by` becomes `deleted-user`); signers created for
/// that user are anonymized to `Deleted user` with a
/// per-signer anonymized email, so the signing record stays
/// auditable without the identity.
pub(crate) fn on_user_deleted(state: SignState, ctx: &mut HookContext) {
    let user_id = ctx
        .get("user_id")
        .and_then(|value| value.as_str())
        .map(str::to_string)
        .or_else(|| {
            ctx.get("id")
                .and_then(|value| value.as_str())
                .map(str::to_string)
        });
    let Some(user_id) = user_id else {
        ctx.add_result(json!({ "sign": "skipped", "reason": "user.deleted carries no user_id" }));
        return;
    };
    let now = Utc::now();
    let mut anonymized_envelopes = 0i64;
    for mut entry in state.envelopes.iter_mut() {
        if entry.value().created_by == user_id {
            entry.value_mut().created_by = DELETED_USER_ID.to_string();
            entry.value_mut().updated_at = now;
            anonymized_envelopes += 1;
        }
    }
    let mut anonymized_templates = 0i64;
    for mut entry in state.templates.iter_mut() {
        if entry.value().created_by == user_id {
            entry.value_mut().created_by = DELETED_USER_ID.to_string();
            entry.value_mut().updated_at = now;
            anonymized_templates += 1;
        }
    }
    // Signers do not carry a user id: a signer row belongs to a
    // user when its email matches the deleted user's email, which
    // the payload may carry under `email`. Without an email the
    // signer rows cannot be matched, so they are left untouched.
    let mut anonymized_signers = 0i64;
    if let Some(email) = ctx.get("email").and_then(|value| value.as_str()) {
        let email_lower = email.to_lowercase();
        for mut entry in state.signers.iter_mut() {
            if entry.value().email.to_lowercase() == email_lower {
                let signer_id = entry.value().id;
                entry.value_mut().name = DELETED_USER_NAME.to_string();
                entry.value_mut().email = format!("deleted-{signer_id}@invalid");
                entry.value_mut().updated_at = now;
                anonymized_signers += 1;
            }
        }
    }
    let mut anonymized_audit = 0i64;
    for mut entries in state.audit.iter_mut() {
        for audit_entry in entries.value_mut().iter_mut() {
            if audit_entry.actor_id == user_id {
                audit_entry.actor_id = DELETED_USER_ID.to_string();
                anonymized_audit += 1;
            }
        }
    }
    ctx.add_result(json!({
        "sign": "user_deleted",
        "user_id": user_id,
        "anonymized_envelopes": anonymized_envelopes,
        "anonymized_templates": anonymized_templates,
        "anonymized_signers": anonymized_signers,
        "anonymized_audit_entries": anonymized_audit,
    }));
}
