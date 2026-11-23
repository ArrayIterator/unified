//! Caller, permission, loading, view, and lifecycle helpers.

use super::state::SignState;
use super::types::{
    CertificateSigner, EVENT_ENVELOPE_COMPLETED, EVENT_ENVELOPE_EXPIRED, EnvelopeStatus,
    GENESIS_HASH, MAX_DOCUMENT_ID_LEN, MAX_DOCUMENTS_PER_ENVELOPE, MAX_EMAIL_LEN,
    MAX_FIELD_KEY_LEN, MAX_FIELD_LABEL_LEN, MAX_FIELDS_PER_TEMPLATE, MAX_NAME_LEN, MAX_REASON_LEN,
    MAX_ROLE_LEN, MAX_ROLES_PER_TEMPLATE, PERM_SIGN_MANAGE, ReminderSchedule, SignAuditEntry,
    SignCertificate, SignEnvelope, SignSigner, SignTemplate, SignerStatus, all_signers_signed,
    compute_audit_hash, compute_certificate_hash, effective_envelope_status,
    envelope_can_transition, hash_value, valid_email, valid_sha256_hex,
};
use actix_web::{HttpRequest, HttpResponse};
use chrono::{Duration, Utc};
use serde::Serialize;
use serde_json::{Value, json};
use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::json;
use unified_core::pagination::{PageResult, PaginationParams};
use uuid::Uuid;

/// Caller identity: the server-verified caller (user id plus
/// server-resolved role, both required for capability checks),
/// exactly like the forums module.
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
/// the `role.check` hook. Fails closed: no answer means not allowed.
pub(crate) fn check_capability(ctx: &AppContext, role: &str, permission: &str) -> bool {
    let context = ctx.hooks.emit(
        "role.check",
        json!({ "role": role, "permission": permission, "allowed": false }),
    );
    context
        .get("allowed")
        .and_then(|value| value.as_bool())
        .unwrap_or(false)
}

pub(crate) fn require_permission(
    ctx: &AppContext,
    role: &str,
    permission: &str,
) -> UnifiedResult<()> {
    if check_capability(ctx, role, permission) {
        Ok(())
    } else {
        Err(UnifiedError::forbidden(format!(
            "Role \"{role}\" does not hold permission \"{permission}\""
        )))
    }
}

/// True when the caller holds the manage capability.
pub(crate) fn can_manage(ctx: &AppContext, role: &str) -> bool {
    check_capability(ctx, role, PERM_SIGN_MANAGE)
}

/// Resolve the tenant id for this request from the middleware's
/// resolved tenant context. Fails closed: a request with no
/// resolved tenant is rejected, and there is never a fallback to
/// the control plane.
pub(crate) fn resolve_tenant(req: &HttpRequest) -> UnifiedResult<String> {
    unified_core::tenancy::TenantContext::from_http_request(req)
        .map(|context| context.tenant_id.to_string())
}

// ---------------------------------------------------------------------------
// Tenant edge guard (C3 pilot 5, task 319).
// ---------------------------------------------------------------------------

/// Fail-closed tenant gate: resolves the tenant context the
/// tenant-resolution middleware attached to the request. A request
/// with no resolved tenant context is rejected (401) and never falls
/// back to the `public` schema (the shared `TenantContext` contract
/// in `unified_core::tenancy`). Every tenant-data handler in this
/// module calls this FIRST, before `caller` or any capability check.
///
/// The records in this module carry a required `tenant_id` (not a
/// `company_id`): partitioning is by tenant, through the pre-existing
/// `load_*` helpers (cross-tenant row reads as 404) and the list
/// filters. `caller_company` below is therefore the mismatch-only
/// company check (pilot 4 group D idiom), not a strict
/// `require_company` gate: a tenant/caller company mismatch is 403,
/// but a write is never refused only because no company resolves —
/// the write is still fully tenant-partitioned by `tenant_id`.
pub(crate) fn require_tenant(
    req: &HttpRequest,
) -> UnifiedResult<unified_core::tenancy::TenantContext> {
    unified_core::tenancy::TenantContext::from_http_request(req)
}

/// The company scope the caller acts in: the tenant context's
/// company first, the verified caller's server-resolved company
/// otherwise. Fails closed (403) when both are present and disagree
/// — a request must not carry one company in its tenant context and
/// another in its verified identity. The `X-Company-Id` header is
/// NEVER trusted here: only the verified caller (company resolved
/// from the database at authentication time) and the
/// middleware-resolved tenant context are sources (header trust was
/// removed by task 317; this helper must never reintroduce it).
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

pub(crate) fn parse_uuid(raw: &str, what: &str) -> UnifiedResult<Uuid> {
    Uuid::parse_str(raw).map_err(|_| UnifiedError::bad_request(format!("Invalid {what} id: {raw}")))
}

pub(crate) fn require_text(value: &str, field: &str, max: usize) -> UnifiedResult<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err(UnifiedError::bad_request(format!(
            "{field} must not be empty"
        )));
    }
    if trimmed.len() > max {
        return Err(UnifiedError::bad_request(format!(
            "{field} must be at most {max} characters"
        )));
    }
    Ok(trimmed.to_string())
}

pub(crate) fn optional_text(
    raw: &Option<String>,
    field: &str,
    max: usize,
) -> UnifiedResult<Option<String>> {
    match raw {
        Some(text) => Ok(Some(require_text(text, field, max)?)),
        None => Ok(None),
    }
}

pub(crate) fn ok<T: Serialize>(data: T) -> HttpResponse {
    json::success_response(data, 200)
}

pub(crate) fn created<T: Serialize>(data: T) -> HttpResponse {
    json::success_response(data, 201)
}

pub(crate) fn err(error: UnifiedError, debug: bool) -> HttpResponse {
    json::error_response(&error, debug)
}

pub(crate) fn paginate<T: Serialize + Clone>(
    items: Vec<T>,
    params: &PaginationParams,
) -> PageResult<T> {
    let pagination = params.normalized();
    let total = items.len() as u64;
    let start = ((pagination.page - 1) * pagination.per_page) as usize;
    let page_items: Vec<T> = items
        .into_iter()
        .skip(start)
        .take(pagination.per_page as usize)
        .collect();
    PageResult::new(page_items, total, &pagination)
}

// ---------------------------------------------------------------------------
// Validation
// ---------------------------------------------------------------------------

/// Validate a document id list: at least one document, at most
/// the module bound, every id non-empty and within the length
/// bound, no duplicates.
pub(crate) fn validate_document_ids(ids: &[String]) -> UnifiedResult<()> {
    if ids.is_empty() {
        return Err(UnifiedError::bad_request(
            "document_ids must contain at least one document",
        ));
    }
    if ids.len() > MAX_DOCUMENTS_PER_ENVELOPE {
        return Err(UnifiedError::bad_request(format!(
            "At most {MAX_DOCUMENTS_PER_ENVELOPE} documents per envelope"
        )));
    }
    let mut seen = std::collections::HashSet::new();
    for id in ids {
        let trimmed = id.trim();
        if trimmed.is_empty() || trimmed.len() > MAX_DOCUMENT_ID_LEN {
            return Err(UnifiedError::bad_request(
                "document_ids must be non-empty ids of at most 200 characters",
            ));
        }
        if !seen.insert(trimmed.to_string()) {
            return Err(UnifiedError::bad_request(format!(
                "document_ids must not repeat document \"{trimmed}\""
            )));
        }
    }
    Ok(())
}

/// Validate document hashes: when present they must align with
/// the document list by position and each must be a SHA-256 hex
/// digest.
pub(crate) fn validate_document_hashes(
    ids: &[String],
    hashes: &Option<Vec<String>>,
) -> UnifiedResult<Vec<String>> {
    match hashes {
        None => Ok(vec![String::new(); ids.len()]),
        Some(hashes) => {
            if hashes.len() != ids.len() {
                return Err(UnifiedError::bad_request(
                    "document_hashes must align with document_ids by position",
                ));
            }
            for hash in hashes {
                if !hash.is_empty() && !valid_sha256_hex(hash) {
                    return Err(UnifiedError::bad_request(
                        "document_hashes must be SHA-256 hex digests",
                    ));
                }
            }
            Ok(hashes.clone())
        }
    }
}

/// Validate a reminder schedule supplied as data: the interval
/// must be at least one day, the start delay must not be
/// negative, and the cap (when set) must be non-negative.
pub(crate) fn validate_reminder_schedule(schedule: &ReminderSchedule) -> UnifiedResult<()> {
    if schedule.interval_days < 1 {
        return Err(UnifiedError::bad_request(
            "reminder_schedule.interval_days must be at least 1",
        ));
    }
    if schedule.start_after_days < 0 {
        return Err(UnifiedError::bad_request(
            "reminder_schedule.start_after_days must not be negative",
        ));
    }
    if let Some(max) = schedule.max_reminders {
        if max < 0 {
            return Err(UnifiedError::bad_request(
                "reminder_schedule.max_reminders must not be negative",
            ));
        }
    }
    Ok(())
}

/// Validate one signer input: name and role within bounds, a
/// valid email.
pub(crate) fn validate_signer_input(
    name: &str,
    email: &str,
    role: &str,
) -> UnifiedResult<(String, String, String)> {
    let name = require_text(name, "name", MAX_NAME_LEN)?;
    let email = require_text(email, "email", MAX_EMAIL_LEN)?;
    if !valid_email(&email) {
        return Err(UnifiedError::bad_request(
            "email must be a valid email address",
        ));
    }
    let role = require_text(role, "role", MAX_ROLE_LEN)?;
    Ok((name, email, role))
}

/// Validate template fields: keys unique and within bounds,
/// labels within bounds, field types non-empty.
pub(crate) fn validate_template_fields(
    fields: &[super::types::TemplateField],
) -> UnifiedResult<()> {
    if fields.len() > MAX_FIELDS_PER_TEMPLATE {
        return Err(UnifiedError::bad_request(format!(
            "At most {MAX_FIELDS_PER_TEMPLATE} fields per template"
        )));
    }
    let mut seen = std::collections::HashSet::new();
    for field in fields {
        let key = require_text(&field.key, "field key", MAX_FIELD_KEY_LEN)?;
        let _label = require_text(&field.label, "field label", MAX_FIELD_LABEL_LEN)?;
        if field.field_type.trim().is_empty() {
            return Err(UnifiedError::bad_request("field_type must not be empty"));
        }
        if !seen.insert(key) {
            return Err(UnifiedError::bad_request(format!(
                "template fields must not repeat key \"{key}\""
            )));
        }
        if let Some(page) = field.page {
            if page < 0 {
                return Err(UnifiedError::bad_request("field page must not be negative"));
            }
        }
    }
    Ok(())
}

/// Validate template signer roles: roles unique and within
/// bounds, order indexes non-negative, default emails valid.
pub(crate) fn validate_template_roles(
    roles: &[super::types::TemplateSignerRole],
) -> UnifiedResult<()> {
    if roles.len() > MAX_ROLES_PER_TEMPLATE {
        return Err(UnifiedError::bad_request(format!(
            "At most {MAX_ROLES_PER_TEMPLATE} signer roles per template"
        )));
    }
    let mut seen = std::collections::HashSet::new();
    for role in roles {
        let name = require_text(&role.role, "role", MAX_ROLE_LEN)?;
        if !seen.insert(name) {
            return Err(UnifiedError::bad_request(format!(
                "template signer roles must not repeat role \"{}\"",
                role.role
            )));
        }
        if role.order_index < 0 {
            return Err(UnifiedError::bad_request(
                "role order_index must not be negative",
            ));
        }
        if let Some(email) = &role.email {
            let email = require_text(email, "email", MAX_EMAIL_LEN)?;
            if !valid_email(&email) {
                return Err(UnifiedError::bad_request(
                    "email must be a valid email address",
                ));
            }
        }
        if let Some(name) = &role.name {
            let _ = require_text(name, "name", MAX_NAME_LEN)?;
        }
    }
    Ok(())
}

pub(crate) fn validate_reason(raw: &Option<String>) -> UnifiedResult<Option<String>> {
    optional_text(raw, "reason", MAX_REASON_LEN)
}

// ---------------------------------------------------------------------------
// Tenant-scoped loaders
// ---------------------------------------------------------------------------
//
// Every loader checks the row's `tenant_id` against the caller's
// tenant. A row from another tenant reads as 404, never 403, so
// its existence does not leak across tenants.

fn tenant_match(row_tenant_id: &str, tenant_id: &str, id: Uuid, what: &str) -> UnifiedResult<()> {
    if row_tenant_id != tenant_id {
        return Err(UnifiedError::not_found(format!("{what} not found: {id}")));
    }
    Ok(())
}

/// Load an envelope and refresh its expiry: a sent or
/// partially-signed envelope past its expiry is stored as
/// expired (with an audit entry and an event) the first time it
/// is read afterwards.
pub(crate) fn load_envelope(
    state: &SignState,
    tenant_id: &str,
    raw_id: &str,
) -> UnifiedResult<SignEnvelope> {
    let id = parse_uuid(raw_id, "envelope")?;
    let envelope = state
        .envelopes
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Envelope not found: {id}")))?;
    tenant_match(&envelope.tenant_id, tenant_id, id, "Envelope")?;
    Ok(refresh_expiry(state, envelope))
}

pub(crate) fn load_signer(
    state: &SignState,
    tenant_id: &str,
    raw_id: &str,
) -> UnifiedResult<SignSigner> {
    let id = parse_uuid(raw_id, "signer")?;
    let signer = state
        .signers
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Signer not found: {id}")))?;
    tenant_match(&signer.tenant_id, tenant_id, id, "Signer")?;
    Ok(signer)
}

pub(crate) fn load_template(
    state: &SignState,
    tenant_id: &str,
    raw_id: &str,
) -> UnifiedResult<SignTemplate> {
    let id = parse_uuid(raw_id, "template")?;
    let template = state
        .templates
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Template not found: {id}")))?;
    tenant_match(&template.tenant_id, tenant_id, id, "Template")?;
    Ok(template)
}

/// Resolve a raw signing token to its signer. Unknown, expired,
/// and revoked tokens all answer 404 with the same message, so
/// tokens cannot be probed apart. The token is the credential
/// here, so no tenant context is needed — possession of the
/// unguessable token is the authorization.
pub(crate) fn load_signer_by_token(
    state: &SignState,
    token: &str,
) -> UnifiedResult<(SignEnvelope, SignSigner)> {
    let token_hash = hash_value(token);
    let now = Utc::now();
    let signer = state
        .signers
        .iter()
        .map(|entry| entry.value().clone())
        .find(|signer| signer.token_hash == token_hash)
        .ok_or_else(|| UnifiedError::not_found("Signing link not found"))?;
    if signer
        .token_expires_at
        .map(|expires_at| expires_at <= now)
        .unwrap_or(true)
    {
        return Err(UnifiedError::not_found("Signing link not found"));
    }
    let envelope = state
        .envelopes
        .get(&signer.envelope_id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found("Signing link not found"))?;
    let envelope = refresh_expiry(state, envelope);
    if envelope.status.is_terminal() {
        return Err(UnifiedError::not_found("Signing link not found"));
    }
    Ok((envelope, signer))
}

// ---------------------------------------------------------------------------
// Audit trail and lifecycle
// ---------------------------------------------------------------------------

/// Append one hash-chained audit entry to an envelope's trail
/// and return it.
pub(crate) fn append_audit(
    state: &SignState,
    tenant_id: &str,
    envelope_id: Uuid,
    signer_id: Option<Uuid>,
    action: &str,
    actor_id: &str,
    detail: Option<String>,
) -> SignAuditEntry {
    let prev_hash = state
        .audit_of(envelope_id)
        .last()
        .map(|entry| entry.hash.clone())
        .unwrap_or_else(|| GENESIS_HASH.to_string());
    let entry = SignAuditEntry {
        id: Uuid::new_v4(),
        tenant_id: tenant_id.to_string(),
        envelope_id,
        signer_id,
        action: action.to_string(),
        actor_id: actor_id.to_string(),
        detail: detail.clone(),
        prev_hash: prev_hash.clone(),
        hash: String::new(),
        created_at: Utc::now(),
    };
    let hash = compute_audit_hash(
        &prev_hash,
        entry.id,
        envelope_id,
        action,
        actor_id,
        entry.created_at,
        detail.as_deref().unwrap_or(""),
    );
    let mut entry = entry;
    entry.hash = hash;
    state
        .audit
        .entry(envelope_id)
        .or_insert_with(Vec::new)
        .push(entry.clone());
    entry
}

/// Refresh expiry on a loaded envelope: when a live envelope is
/// past its expiry it is stored as expired, audited, and the
/// expiry event is emitted. Returns the (possibly updated)
/// envelope.
pub(crate) fn refresh_expiry(state: &SignState, envelope: SignEnvelope) -> SignEnvelope {
    let effective = effective_envelope_status(envelope.status, envelope.expires_at, Utc::now());
    if effective == EnvelopeStatus::Expired && envelope.status != EnvelopeStatus::Expired {
        let mut updated = envelope.clone();
        updated.status = EnvelopeStatus::Expired;
        updated.updated_at = Utc::now();
        state.envelopes.insert(updated.id, updated.clone());
        append_audit(
            state,
            &envelope.tenant_id,
            envelope.id,
            None,
            "expired",
            "system",
            Some("Envelope passed its expiry with signers still pending".to_string()),
        );
        state.emit(
            EVENT_ENVELOPE_EXPIRED,
            json!({
                "tenant_id": envelope.tenant_id,
                "envelope_id": envelope.id.to_string(),
                "title": envelope.title,
            }),
        );
        return updated;
    }
    envelope
}

/// Complete an envelope when every signer has signed: store the
/// completion time and certificate, audit the completion, and
/// emit the completion event. Returns the certificate when the
/// envelope completed here, `None` otherwise.
pub(crate) fn complete_envelope_if_ready(
    state: &SignState,
    envelope: &SignEnvelope,
) -> Option<SignCertificate> {
    if !matches!(
        envelope.status,
        EnvelopeStatus::Sent | EnvelopeStatus::PartiallySigned
    ) {
        return None;
    }
    let signers = state.signers_of(envelope.id);
    if !all_signers_signed(&signers) {
        return None;
    }
    let now = Utc::now();
    let certificate_hash =
        compute_certificate_hash(envelope.id, &envelope.document_hashes, &signers, now);
    let audit = state.audit_of(envelope.id);
    let audit_head_hash = audit
        .last()
        .map(|entry| entry.hash.clone())
        .unwrap_or_else(|| GENESIS_HASH.to_string());
    let certificate = SignCertificate {
        certificate_hash: certificate_hash.clone(),
        envelope_id: envelope.id,
        tenant_id: envelope.tenant_id.clone(),
        title: envelope.title.clone(),
        document_ids: envelope.document_ids.clone(),
        document_hashes: envelope.document_hashes.clone(),
        signers: signers
            .iter()
            .filter(|signer| signer.status == SignerStatus::Signed)
            .map(|signer| {
                let capture = signer.signature.clone();
                CertificateSigner {
                    signer_id: signer.id,
                    name: signer.name.clone(),
                    email: signer.email.clone(),
                    role: signer.role.clone(),
                    order_index: signer.order_index,
                    signed_at: signer.signed_at.unwrap_or(now),
                    method: capture
                        .as_ref()
                        .map(|capture| capture.method.clone())
                        .unwrap_or_default(),
                    signature_hash: capture
                        .as_ref()
                        .map(|capture| capture.signature_hash.clone())
                        .unwrap_or_default(),
                    document_hash: capture
                        .as_ref()
                        .map(|capture| capture.document_hash.clone())
                        .unwrap_or_default(),
                }
            })
            .collect(),
        audit_head_hash,
        audit_event_count: audit.len() as i64,
        completed_at: now,
        issued_at: now,
    };
    state
        .certificates
        .insert(certificate_hash.clone(), certificate.clone());
    let mut updated = envelope.clone();
    updated.status = EnvelopeStatus::Completed;
    updated.completed_at = Some(now);
    updated.certificate_hash = Some(certificate_hash.clone());
    updated.updated_at = now;
    state.envelopes.insert(updated.id, updated);
    append_audit(
        state,
        &envelope.tenant_id,
        envelope.id,
        None,
        "completed",
        "system",
        Some(format!(
            "All signers signed; certificate {certificate_hash}"
        )),
    );
    state.emit(
        EVENT_ENVELOPE_COMPLETED,
        json!({
            "tenant_id": envelope.tenant_id,
            "envelope_id": envelope.id.to_string(),
            "title": envelope.title,
            "certificate_hash": certificate_hash,
            "completed_at": now.to_rfc3339(),
        }),
    );
    Some(certificate)
}

/// Move an envelope to a new status through the status machine.
/// Rejects transitions the machine does not allow.
pub(crate) fn transition_envelope(
    state: &SignState,
    envelope: &mut SignEnvelope,
    to: EnvelopeStatus,
) -> UnifiedResult<()> {
    if !envelope_can_transition(envelope.status, to) {
        return Err(UnifiedError::conflict(format!(
            "Envelope cannot move from \"{}\" to \"{}\"",
            envelope.status.as_str(),
            to.as_str()
        )));
    }
    envelope.status = to;
    envelope.updated_at = Utc::now();
    state.envelopes.insert(envelope.id, envelope.clone());
    Ok(())
}

// ---------------------------------------------------------------------------
// Views
// ---------------------------------------------------------------------------

/// A signer view never includes the token hash. The raw token is
/// returned separately (and only once) by the send / resend /
/// add flows, under the `token` key.
pub(crate) fn signer_view(signer: &SignSigner) -> Value {
    serde_json::to_value(signer).unwrap_or(Value::Null)
}

/// A signer view with the raw token attached, for the single
/// response that returns it.
pub(crate) fn signer_view_with_token(signer: &SignSigner, token: &str) -> Value {
    let mut value = signer_view(signer);
    if let Value::Object(map) = &mut value {
        map.insert("token".to_string(), Value::String(token.to_string()));
    }
    value
}

/// An envelope view with its signers attached, so a single read
/// shows the whole signing story.
pub(crate) fn envelope_view(state: &SignState, envelope: &SignEnvelope) -> Value {
    let mut value = serde_json::to_value(envelope).unwrap_or(Value::Null);
    if let Value::Object(map) = &mut value {
        let signers: Vec<Value> = state
            .signers_of(envelope.id)
            .iter()
            .map(signer_view)
            .collect();
        map.insert("signers".to_string(), Value::Array(signers));
    }
    value
}

/// The token-gated sign page: the envelope summary, the
/// signer's own record, the other signers' progress (names and
/// statuses only, no emails), and the capture methods the
/// tenant allows. Never includes token hashes.
pub(crate) fn sign_page_view(
    state: &SignState,
    envelope: &SignEnvelope,
    signer: &SignSigner,
) -> Value {
    let settings = state.settings_for(&envelope.tenant_id);
    let others: Vec<Value> = state
        .signers_of(envelope.id)
        .iter()
        .filter(|other| other.id != signer.id)
        .map(|other| {
            json!({
                "name": other.name,
                "role": other.role,
                "order_index": other.order_index,
                "status": other.status.as_str(),
            })
        })
        .collect();
    json!({
        "envelope": {
            "id": envelope.id.to_string(),
            "title": envelope.title,
            "message": envelope.message,
            "document_ids": envelope.document_ids,
            "document_hashes": envelope.document_hashes,
            "signing_order": envelope.signing_order,
            "status": envelope.status.as_str(),
            "expires_at": envelope.expires_at.map(|at| at.to_rfc3339()),
        },
        "signer": signer_view(signer),
        "other_signers": others,
        "signature_types": settings.signature_types,
        "require_view_before_sign": settings.require_view_before_sign,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn pagination_params(page: Option<u32>, per_page: Option<u32>) -> PaginationParams {
        PaginationParams {
            page,
            per_page,
            sort: None,
            order: None,
            search: None,
            filters: HashMap::new(),
        }
    }

    #[test]
    fn document_ids_validation() {
        assert!(validate_document_ids(&["doc-1".to_string()]).is_ok());
        assert!(validate_document_ids(&[]).is_err());
        assert!(validate_document_ids(&["".to_string()]).is_err());
        assert!(validate_document_ids(&["doc-1".to_string(), "doc-1".to_string()]).is_err());
        let many: Vec<String> = (0..MAX_DOCUMENTS_PER_ENVELOPE + 1)
            .map(|i| format!("doc-{i}"))
            .collect();
        assert!(validate_document_ids(&many).is_err());
    }

    #[test]
    fn document_hashes_must_align() {
        let ids = vec!["doc-1".to_string(), "doc-2".to_string()];
        assert_eq!(
            validate_document_hashes(&ids, &None).unwrap(),
            vec![String::new(), String::new()]
        );
        assert!(validate_document_hashes(&ids, &Some(vec!["a".repeat(64)])).is_err());
        assert!(
            validate_document_hashes(&ids, &Some(vec!["zz".to_string(), "a".repeat(64)])).is_err()
        );
        assert!(
            validate_document_hashes(&ids, &Some(vec!["a".repeat(64), "b".repeat(64)])).is_ok()
        );
    }

    #[test]
    fn reminder_schedule_validation() {
        assert!(validate_reminder_schedule(&ReminderSchedule::default()).is_ok());
        assert!(
            validate_reminder_schedule(&ReminderSchedule {
                interval_days: 0,
                ..ReminderSchedule::default()
            })
            .is_err()
        );
        assert!(
            validate_reminder_schedule(&ReminderSchedule {
                start_after_days: -1,
                ..ReminderSchedule::default()
            })
            .is_err()
        );
        assert!(
            validate_reminder_schedule(&ReminderSchedule {
                max_reminders: Some(-1),
                ..ReminderSchedule::default()
            })
            .is_err()
        );
    }

    #[test]
    fn signer_input_validation() {
        assert!(validate_signer_input("Alice", "alice@example.com", "signer").is_ok());
        assert!(validate_signer_input("", "alice@example.com", "signer").is_err());
        assert!(validate_signer_input("Alice", "not-an-email", "signer").is_err());
        assert!(validate_signer_input("Alice", "alice@example.com", "").is_err());
    }

    #[test]
    fn paginate_bounds() {
        let items: Vec<i64> = (1..=5).collect();
        let first = paginate(items.clone(), &pagination_params(Some(1), Some(2)));
        assert_eq!(first.items, vec![1, 2]);
        assert_eq!(first.pagination.total, 5);
        let past_end = paginate(items.clone(), &pagination_params(Some(4), Some(2)));
        assert!(past_end.items.is_empty());
        assert_eq!(past_end.pagination.total, 5);
    }

    #[test]
    fn signer_view_hides_token_hash() {
        let signer = SignSigner {
            id: Uuid::new_v4(),
            tenant_id: "tenant-1".to_string(),
            envelope_id: Uuid::new_v4(),
            name: "Alice".to_string(),
            email: "alice@example.com".to_string(),
            role: "signer".to_string(),
            order_index: 0,
            status: SignerStatus::Sent,
            token_hash: "secret-hash".to_string(),
            token_expires_at: None,
            viewed_at: None,
            signed_at: None,
            declined_at: None,
            decline_reason: None,
            signature: None,
            reminders_sent: 0,
            last_reminder_at: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        };
        let view = signer_view(&signer);
        assert_eq!(view.get("token_hash"), None);
        assert_eq!(
            view.get("email").and_then(Value::as_str),
            Some("alice@example.com")
        );
        let with_token = signer_view_with_token(&signer, "raw-token");
        assert_eq!(
            with_token.get("token").and_then(Value::as_str),
            Some("raw-token")
        );
    }

    #[test]
    fn token_lookup_rejects_unknown_and_expired() {
        let state = SignState::default();
        assert!(load_signer_by_token(&state, "a".repeat(48).as_str()).is_err());
        let envelope_id = Uuid::new_v4();
        let signer = SignSigner {
            id: Uuid::new_v4(),
            tenant_id: "tenant-1".to_string(),
            envelope_id,
            name: "Alice".to_string(),
            email: "alice@example.com".to_string(),
            role: "signer".to_string(),
            order_index: 0,
            status: SignerStatus::Sent,
            token_hash: hash_value(&"b".repeat(48)),
            token_expires_at: Some(Utc::now() - Duration::hours(1)),
            viewed_at: None,
            signed_at: None,
            declined_at: None,
            decline_reason: None,
            signature: None,
            reminders_sent: 0,
            last_reminder_at: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        };
        state.signers.insert(signer.id, signer);
        state.envelopes.insert(
            envelope_id,
            SignEnvelope {
                id: envelope_id,
                tenant_id: "tenant-1".to_string(),
                title: "Agreement".to_string(),
                message: None,
                document_ids: vec!["doc-1".to_string()],
                document_hashes: vec!["a".repeat(64)],
                signing_order: "parallel".to_string(),
                status: EnvelopeStatus::Sent,
                template_id: None,
                expires_at: Some(Utc::now() + Duration::days(7)),
                sent_at: Some(Utc::now()),
                completed_at: None,
                certificate_hash: None,
                reminder_schedule: ReminderSchedule::default(),
                created_by: "user-1".to_string(),
                created_at: Utc::now(),
                updated_at: Utc::now(),
            },
        );
        // Expired token reads as not found, same as unknown.
        assert!(load_signer_by_token(&state, &"b".repeat(48)).is_err());
    }
}

#[cfg(test)]
mod tenant_guard_tests {
    use super::*;
    use actix_web::test::TestRequest;

    fn tenant_request(company_id: &str, caller_company_id: Option<&str>) -> HttpRequest {
        let req = TestRequest::default().to_http_request();
        req.extensions_mut()
            .insert(unified_core::middleware::TenantContext {
                tenant_id: uuid::Uuid::now_v7().to_string(),
                company_id: company_id.to_string(),
                site_id: String::new(),
                kind: unified_core::middleware::TenantKind::Subdomain,
                host: "tenant.example.test".to_string(),
                scope: None,
                product_id: None,
                package_id: None,
                product_subdomain_id: None,
            });
        let mut caller =
            unified_core::auth::Caller::verified(test_user_uuid("alice"), Vec::new());
        if let Some(company) = caller_company_id {
            caller = caller.with_company(company.to_string());
        }
        req.extensions_mut().insert(caller);
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

    #[test]
    fn require_tenant_fails_closed_without_resolved_context() {
        let bare = TestRequest::default().to_http_request();
        assert!(require_tenant(&bare).is_err());
    }

    #[test]
    fn require_tenant_resolves_from_middleware_context() {
        let req = tenant_request("acme", None);
        let tenant = require_tenant(&req).expect("tenant context must resolve");
        assert!(!tenant.tenant_id.to_string().is_empty());
        assert_eq!(
            caller_company(&tenant, &req).unwrap(),
            Some("acme".to_string())
        );
    }

    #[test]
    fn caller_company_prefers_verified_caller_when_tenant_has_none() {
        let req = tenant_request("", Some("acme"));
        let tenant = require_tenant(&req).expect("tenant context must resolve");
        assert_eq!(
            caller_company(&tenant, &req).unwrap(),
            Some("acme".to_string())
        );
    }

    #[test]
    fn caller_company_rejects_verified_caller_mismatch() {
        let req = tenant_request("acme", Some("other"));
        let tenant = require_tenant(&req).expect("tenant context must resolve");
        assert!(caller_company(&tenant, &req).is_err());
    }

    #[test]
    fn caller_company_ignores_the_x_company_id_header() {
        // A forged header naming another company must not move
        // the scope: only the tenant context and the verified
        // caller count.
        let req = TestRequest::default()
            .insert_header(("x-company-id", "other"))
            .to_http_request();
        req.extensions_mut()
            .insert(unified_core::middleware::TenantContext {
                tenant_id: uuid::Uuid::now_v7().to_string(),
                company_id: "acme".to_string(),
                site_id: String::new(),
                kind: unified_core::middleware::TenantKind::Subdomain,
                host: "tenant.example.test".to_string(),
                scope: None,
                product_id: None,
                package_id: None,
                product_subdomain_id: None,
            });
        req.extensions_mut().insert(
            unified_core::auth::Caller::verified(test_user_uuid("alice"), Vec::new()),
        );
        let tenant = require_tenant(&req).expect("tenant context must resolve");
        assert_eq!(
            caller_company(&tenant, &req).unwrap(),
            Some("acme".to_string())
        );
    }
}
