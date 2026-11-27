//! HTTP handlers for sign signers: management, token resend,
//! and the public token-gated sign page, signing, decline, and
//! certificate verification.

use actix_web::{HttpRequest, HttpResponse, web};
use chrono::{Duration, Utc};
use serde::Deserialize;
use serde_json::{Value, json};
use std::sync::Arc;
use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::pagination::PaginationParams;
use uuid::Uuid;

use super::helpers::{
    append_audit, caller, caller_company, complete_envelope_if_ready, created, envelope_view, err,
    load_envelope, load_signer, load_signer_by_token, ok, paginate, require_permission,
    require_tenant, require_text, resolve_tenant, sign_page_view, signer_view,
    signer_view_with_token, validate_reason, validate_signer_input,
};
use super::state::SignState;
use super::types::{
    DEFAULT_TOKEN_TTL_DAYS, DeclineRequest, EVENT_ENVELOPE_DECLINED, EVENT_SIGNER_RESENT,
    EVENT_SIGNER_SIGNED, EnvelopeStatus, MAX_NAME_LEN, MAX_SIGNERS_PER_ENVELOPE, PERM_SIGN_MANAGE,
    PERM_SIGN_VIEW, SIGNING_TOKEN_LEN, SignRequest, SignSigner, SignatureCapture, SignerInput,
    SignerStatus, generate_signing_token, hash_value, signer_can_transition, signer_may_sign,
    valid_sha256_hex, valid_signature_method,
};

/// Path carrying an envelope id.
#[derive(Debug, Deserialize)]
struct EnvelopePath {
    envelope_id: String,
}

/// Path carrying an envelope id and a signer id.
#[derive(Debug, Deserialize)]
struct SignerPath {
    envelope_id: String,
    signer_id: String,
}

/// Path carrying a raw signing token.
#[derive(Debug, Deserialize)]
struct TokenPath {
    token: String,
}

/// Path carrying a certificate hash.
#[derive(Debug, Deserialize)]
struct CertificatePath {
    certificate_hash: String,
}

/// Validate a raw token's shape before lookup: 48 alphanumeric
/// characters. Anything else is a 404, same as unknown.
fn valid_token_shape(token: &str) -> bool {
    token.len() == SIGNING_TOKEN_LEN && token.chars().all(|c| c.is_ascii_alphanumeric())
}

/// Issue (or rotate) a signer's token. Returns the raw token;
/// only its hash is stored.
fn issue_token(state: &SignState, signer: &mut SignSigner) -> String {
    let token = generate_signing_token();
    signer.token_hash = hash_value(&token);
    signer.token_expires_at = Some(Utc::now() + Duration::days(DEFAULT_TOKEN_TTL_DAYS));
    signer.updated_at = Utc::now();
    state.signers.insert(signer.id, signer.clone());
    token
}

fn signer_event(state: &SignState, event: &str, tenant_id: &str, signer: &SignSigner) {
    state.emit(
        event,
        json!({
            "tenant_id": tenant_id,
            "envelope_id": signer.envelope_id.to_string(),
            "signer_id": signer.id.to_string(),
            "email": signer.email,
            "status": signer.status.as_str(),
        }),
    );
}

// Signer handlers (authenticated)

pub(crate) async fn list_signers(
    state: web::Data<SignState>,
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    path: web::Path<EnvelopePath>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let tenant_id = resolve_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_SIGN_VIEW)?;
        let envelope = load_envelope(&state, &tenant_id, &path.into_inner().envelope_id)?;
        let params = query.into_inner();
        let status_filter = params
            .filters
            .get("status")
            .and_then(|raw| SignerStatus::from_str(raw));
        let items: Vec<Value> = state
            .signers_of(envelope.id)
            .iter()
            .filter(|signer| {
                if let Some(status) = status_filter {
                    if signer.status != status {
                        return false;
                    }
                }
                true
            })
            .map(signer_view)
            .collect();
        Ok(paginate(items, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn add_signer(
    state: web::Data<SignState>,
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    path: web::Path<EnvelopePath>,
    body: web::Json<SignerInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let tenant_id = resolve_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_SIGN_MANAGE)?;
        let mut envelope = load_envelope(&state, &tenant_id, &path.into_inner().envelope_id)?;
        if envelope.status.is_terminal() {
            return Err(UnifiedError::conflict(format!(
                "Signers cannot be added while an envelope is \"{}\"",
                envelope.status.as_str(),
            )));
        }
        let existing = state.signers_of(envelope.id);
        if existing.len() >= MAX_SIGNERS_PER_ENVELOPE {
            return Err(UnifiedError::bad_request(format!(
                "At most {MAX_SIGNERS_PER_ENVELOPE} signers per envelope"
            )));
        }
        let input = body.into_inner();
        let (name, email, role) = validate_signer_input(&input.name, &input.email, &input.role)?;
        if existing
            .iter()
            .any(|signer| signer.email.to_lowercase() == email.to_lowercase())
        {
            return Err(UnifiedError::conflict(format!(
                "Signer with email \"{email}\" already exists on this envelope"
            )));
        }
        let order_index = match input.order_index {
            Some(index) => {
                if index < 0 {
                    return Err(UnifiedError::bad_request(
                        "order_index must not be negative",
                    ));
                }
                if existing.iter().any(|signer| signer.order_index == index) {
                    return Err(UnifiedError::bad_request(format!(
                        "order_index {index} is already taken on this envelope"
                    )));
                }
                index
            }
            None => {
                existing
                    .iter()
                    .map(|signer| signer.order_index)
                    .max()
                    .unwrap_or(-1)
                    + 1
            }
        };
        let now = Utc::now();
        let mut signer = SignSigner {
            id: Uuid::new_v4(),
            tenant_id: tenant_id.clone(),
            envelope_id: envelope.id,
            name,
            email,
            role,
            order_index,
            status: SignerStatus::Pending,
            token_hash: String::new(),
            token_expires_at: None,
            viewed_at: None,
            signed_at: None,
            declined_at: None,
            decline_reason: None,
            signature: None,
            reminders_sent: 0,
            last_reminder_at: None,
            created_at: now,
            updated_at: now,
        };
        // A signer added to an already-sent envelope is sent
        // immediately, with a fresh token.
        let mut token: Option<String> = None;
        if envelope.status == EnvelopeStatus::Sent
            || envelope.status == EnvelopeStatus::PartiallySigned
        {
            token = Some(issue_token(&state, &mut signer));
            signer.status = SignerStatus::Sent;
            state.signers.insert(signer.id, signer.clone());
            state.notify(
                "sign.envelope.sent",
                &signer.email,
                &format!("Signature requested: {}", envelope.title),
                &format!(
                    "You have been asked to sign \"{}\". Use your signing link to open it.",
                    envelope.title
                ),
                None,
            );
        } else {
            state.signers.insert(signer.id, signer.clone());
        }
        append_audit(
            &state,
            &tenant_id,
            envelope.id,
            Some(signer.id),
            "signer_added",
            &caller.actor,
            Some(format!("Signer {} added", signer.email)),
        );
        envelope.updated_at = now;
        state.envelopes.insert(envelope.id, envelope);
        match token {
            Some(token) => Ok(signer_view_with_token(&signer, &token)),
            None => Ok(signer_view(&signer)),
        }
    })();
    match result {
        Ok(data) => created(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn get_signer(
    state: web::Data<SignState>,
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    path: web::Path<SignerPath>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let tenant_id = resolve_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_SIGN_VIEW)?;
        let path = path.into_inner();
        let envelope = load_envelope(&state, &tenant_id, &path.envelope_id)?;
        let signer = load_signer(&state, &tenant_id, &path.signer_id)?;
        if signer.envelope_id != envelope.id {
            return Err(UnifiedError::not_found(format!(
                "Signer not found: {}",
                signer.id
            )));
        }
        Ok(signer_view(&signer))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn update_signer(
    state: web::Data<SignState>,
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    path: web::Path<SignerPath>,
    body: web::Json<SignerInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let tenant_id = resolve_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_SIGN_MANAGE)?;
        let path = path.into_inner();
        let envelope = load_envelope(&state, &tenant_id, &path.envelope_id)?;
        if envelope.status != EnvelopeStatus::Draft {
            return Err(UnifiedError::conflict(format!(
                "Signers can only be updated while an envelope is draft (current status \"{}\")",
                envelope.status.as_str(),
            )));
        }
        let mut signer = load_signer(&state, &tenant_id, &path.signer_id)?;
        if signer.envelope_id != envelope.id {
            return Err(UnifiedError::not_found(format!(
                "Signer not found: {}",
                signer.id
            )));
        }
        let input = body.into_inner();
        let (name, email, role) = validate_signer_input(&input.name, &input.email, &input.role)?;
        if state.signers_of(envelope.id).iter().any(|other| {
            other.id != signer.id && other.email.to_lowercase() == email.to_lowercase()
        }) {
            return Err(UnifiedError::conflict(format!(
                "Signer with email \"{email}\" already exists on this envelope"
            )));
        }
        if let Some(order_index) = input.order_index {
            if order_index < 0 {
                return Err(UnifiedError::bad_request(
                    "order_index must not be negative",
                ));
            }
            if state
                .signers_of(envelope.id)
                .iter()
                .any(|other| other.id != signer.id && other.order_index == order_index)
            {
                return Err(UnifiedError::bad_request(format!(
                    "order_index {order_index} is already taken on this envelope"
                )));
            }
            signer.order_index = order_index;
        }
        signer.name = name;
        signer.email = email;
        signer.role = role;
        signer.updated_at = Utc::now();
        state.signers.insert(signer.id, signer.clone());
        append_audit(
            &state,
            &tenant_id,
            envelope.id,
            Some(signer.id),
            "signer_updated",
            &caller.actor,
            None,
        );
        Ok(signer_view(&signer))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn delete_signer(
    state: web::Data<SignState>,
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    path: web::Path<SignerPath>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let tenant_id = resolve_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_SIGN_MANAGE)?;
        let path = path.into_inner();
        let envelope = load_envelope(&state, &tenant_id, &path.envelope_id)?;
        let signer = load_signer(&state, &tenant_id, &path.signer_id)?;
        if signer.envelope_id != envelope.id {
            return Err(UnifiedError::not_found(format!(
                "Signer not found: {}",
                signer.id
            )));
        }
        let removable = envelope.status == EnvelopeStatus::Draft
            || (matches!(
                envelope.status,
                EnvelopeStatus::Sent | EnvelopeStatus::PartiallySigned
            ) && matches!(signer.status, SignerStatus::Pending | SignerStatus::Sent));
        if !removable {
            return Err(UnifiedError::conflict(format!(
                "Signer cannot be removed while the envelope is \"{}\" and the signer is \"{}\"",
                envelope.status.as_str(),
                signer.status.as_str(),
            )));
        }
        state.signers.remove(&signer.id);
        append_audit(
            &state,
            &tenant_id,
            envelope.id,
            Some(signer.id),
            "signer_removed",
            &caller.actor,
            Some(format!("Signer {} removed", signer.email)),
        );
        Ok(json!({
            "deleted": true,
            "signer_id": signer.id.to_string(),
        }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

/// Rotate a signer's token: the old token stops working, a new
/// raw token is returned once, and the signer is notified.
pub(crate) async fn resend_signer(
    state: web::Data<SignState>,
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    path: web::Path<SignerPath>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let tenant_id = resolve_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_SIGN_MANAGE)?;
        let path = path.into_inner();
        let envelope = load_envelope(&state, &tenant_id, &path.envelope_id)?;
        if !matches!(
            envelope.status,
            EnvelopeStatus::Sent | EnvelopeStatus::PartiallySigned
        ) {
            return Err(UnifiedError::conflict(format!(
                "Signing links can only be resent while an envelope is sent or partially signed (current status \"{}\")",
                envelope.status.as_str(),
            )));
        }
        let mut signer = load_signer(&state, &tenant_id, &path.signer_id)?;
        if signer.envelope_id != envelope.id {
            return Err(UnifiedError::not_found(format!(
                "Signer not found: {}",
                signer.id
            )));
        }
        if !matches!(
            signer.status,
            SignerStatus::Pending | SignerStatus::Sent | SignerStatus::Viewed
        ) {
            return Err(UnifiedError::conflict(format!(
                "Signing links cannot be resent to a signer who is \"{}\"",
                signer.status.as_str(),
            )));
        }
        let token = issue_token(&state, &mut signer);
        signer.status = SignerStatus::Sent;
        signer.updated_at = Utc::now();
        state.signers.insert(signer.id, signer.clone());
        append_audit(
            &state,
            &tenant_id,
            envelope.id,
            Some(signer.id),
            "resent",
            &caller.actor,
            Some(format!("Signing link resent to {}", signer.email)),
        );
        signer_event(&state, EVENT_SIGNER_RESENT, &tenant_id, &signer);
        state.notify(
            "sign.envelope.sent",
            &signer.email,
            &format!("Signature requested: {}", envelope.title),
            &format!(
                "You have been asked to sign \"{}\". Use your new signing link to open it.",
                envelope.title
            ),
            None,
        );
        Ok(signer_view_with_token(&signer, &token))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

// Public token-gated handlers (no auth - the token is the credential)

/// Sign page data for one signing token. The first view records
/// the view (status `viewed`, audit `viewed`); later views are
/// idempotent reads.
pub(crate) async fn get_sign_page(
    state: web::Data<SignState>,
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    path: web::Path<TokenPath>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let _ = &req;
    let result: UnifiedResult<Value> = (|| {
        let token = path.into_inner().token;
        if !valid_token_shape(&token) {
            return Err(UnifiedError::not_found("Signing link not found"));
        }
        let (envelope, mut signer) = load_signer_by_token(&state, &token)?;
        if signer.status == SignerStatus::Sent {
            signer.status = SignerStatus::Viewed;
            signer.viewed_at = Some(Utc::now());
            signer.updated_at = Utc::now();
            state.signers.insert(signer.id, signer.clone());
            append_audit(
                &state,
                &envelope.tenant_id,
                envelope.id,
                Some(signer.id),
                "viewed",
                &signer.email,
                None,
            );
        }
        Ok(sign_page_view(&state, &envelope, &signer))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

/// Record a signature through a signing token. Only the hashes
/// are accepted: the signature hash and the hash of the document
/// as the signer saw it, which must match the hash recorded for
/// that document on the envelope.
pub(crate) async fn post_sign(
    state: web::Data<SignState>,
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    path: web::Path<TokenPath>,
    body: web::Json<SignRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let token = path.into_inner().token;
        if !valid_token_shape(&token) {
            return Err(UnifiedError::not_found("Signing link not found"));
        }
        let (mut envelope, mut signer) = load_signer_by_token(&state, &token)?;
        let input = body.into_inner();
        if !valid_signature_method(&input.method) {
            return Err(UnifiedError::bad_request(
                "method must be drawn, typed, or uploaded",
            ));
        }
        let settings = state.settings_for(&envelope.tenant_id);
        if !settings
            .signature_types
            .iter()
            .any(|kind| kind == &input.method)
        {
            return Err(UnifiedError::bad_request(format!(
                "Signature method \"{}\" is not allowed for this tenant",
                input.method
            )));
        }
        if !valid_sha256_hex(&input.signature_hash) {
            return Err(UnifiedError::bad_request(
                "signature_hash must be a SHA-256 hex digest",
            ));
        }
        if !valid_sha256_hex(&input.document_hash) {
            return Err(UnifiedError::bad_request(
                "document_hash must be a SHA-256 hex digest",
            ));
        }
        let typed_name = if input.method == "typed" {
            Some(require_text(
                input.typed_name.as_deref().unwrap_or(""),
                "typed_name",
                MAX_NAME_LEN,
            )?)
        } else {
            match &input.typed_name {
                Some(name) => Some(require_text(name, "typed_name", MAX_NAME_LEN)?),
                None => None,
            }
        };
        // The signed document must be one of the envelope's
        // documents, and the hash must match the hash recorded
        // for it at send time.
        let document_index = envelope
            .document_ids
            .iter()
            .position(|id| *id == input.document_id)
            .ok_or_else(|| {
                UnifiedError::bad_request(format!(
                    "document_id \"{}\" is not part of this envelope",
                    input.document_id
                ))
            })?;
        let recorded_hash = envelope.document_hashes.get(document_index).cloned();
        if recorded_hash.as_deref() != Some(input.document_hash.as_str()) {
            return Err(UnifiedError::conflict(
                "document_hash does not match the hash recorded for this document",
            ));
        }
        if settings.require_view_before_sign && signer.viewed_at.is_none() {
            return Err(UnifiedError::conflict(
                "The document must be viewed before it can be signed",
            ));
        }
        let signers = state.signers_of(envelope.id);
        if !signer_may_sign(&signers, &signer, &envelope.signing_order) {
            return Err(UnifiedError::conflict(
                "It is not this signer's turn yet in the sequential signing order",
            ));
        }
        if !signer_can_transition(signer.status, SignerStatus::Signed) {
            return Err(UnifiedError::conflict(format!(
                "Signer cannot sign while \"{}\"",
                signer.status.as_str(),
            )));
        }
        let ip_hash = hash_value(
            req.peer_addr()
                .map(|addr| addr.ip().to_string())
                .unwrap_or_default()
                .as_str(),
        );
        let user_agent_hash = hash_value(
            req.headers()
                .get("user-agent")
                .and_then(|value| value.to_str().ok())
                .unwrap_or(""),
        );
        let now = Utc::now();
        signer.signature = Some(SignatureCapture {
            method: input.method.clone(),
            signature_hash: input.signature_hash,
            document_hash: input.document_hash,
            typed_name,
            ip_hash,
            user_agent_hash,
            signed_at: now,
        });
        signer.status = SignerStatus::Signed;
        signer.signed_at = Some(now);
        // The token is single-use for signing: clear it once the
        // signature is recorded.
        signer.token_hash = String::new();
        signer.token_expires_at = None;
        signer.updated_at = now;
        state.signers.insert(signer.id, signer.clone());
        if envelope.status == EnvelopeStatus::Sent {
            envelope.status = EnvelopeStatus::PartiallySigned;
            envelope.updated_at = now;
            state.envelopes.insert(envelope.id, envelope.clone());
        }
        append_audit(
            &state,
            &envelope.tenant_id,
            envelope.id,
            Some(signer.id),
            "signed",
            &signer.email,
            Some(format!(
                "Signer {} signed document {} with method {}",
                signer.email, input.document_id, input.method
            )),
        );
        signer_event(&state, EVENT_SIGNER_SIGNED, &envelope.tenant_id, &signer);
        let certificate = complete_envelope_if_ready(&state, &envelope);
        let envelope = state
            .envelopes
            .get(&envelope.id)
            .map(|entry| entry.value().clone())
            .unwrap_or(envelope);
        Ok(json!({
            "envelope": envelope_view(&state, &envelope),
            "certificate_hash": certificate.map(|certificate| certificate.certificate_hash),
        }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

/// Decline through a signing token. The envelope is declined
/// and every outstanding token stops working.
pub(crate) async fn post_decline(
    state: web::Data<SignState>,
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    path: web::Path<TokenPath>,
    body: web::Json<DeclineRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let _ = &req;
    let result: UnifiedResult<Value> = (|| {
        let token = path.into_inner().token;
        if !valid_token_shape(&token) {
            return Err(UnifiedError::not_found("Signing link not found"));
        }
        let (mut envelope, mut signer) = load_signer_by_token(&state, &token)?;
        let reason = validate_reason(&body.into_inner().reason)?;
        if !signer_can_transition(signer.status, SignerStatus::Declined) {
            return Err(UnifiedError::conflict(format!(
                "Signer cannot decline while \"{}\"",
                signer.status.as_str(),
            )));
        }
        let now = Utc::now();
        signer.status = SignerStatus::Declined;
        signer.declined_at = Some(now);
        signer.decline_reason = reason.clone();
        signer.token_hash = String::new();
        signer.token_expires_at = None;
        signer.updated_at = now;
        state.signers.insert(signer.id, signer.clone());
        envelope.status = EnvelopeStatus::Declined;
        envelope.updated_at = now;
        state.envelopes.insert(envelope.id, envelope.clone());
        for mut other in state.signers_of(envelope.id) {
            if other.id != signer.id
                && matches!(
                    other.status,
                    SignerStatus::Pending | SignerStatus::Sent | SignerStatus::Viewed
                )
            {
                other.token_hash = String::new();
                other.token_expires_at = None;
                other.updated_at = now;
                state.signers.insert(other.id, other);
            }
        }
        append_audit(
            &state,
            &envelope.tenant_id,
            envelope.id,
            Some(signer.id),
            "declined",
            &signer.email,
            reason.clone(),
        );
        state.emit(
            EVENT_ENVELOPE_DECLINED,
            json!({
                "tenant_id": envelope.tenant_id,
                "envelope_id": envelope.id.to_string(),
                "title": envelope.title,
                "signer_id": signer.id.to_string(),
                "email": signer.email,
                "reason": reason,
            }),
        );
        Ok(envelope_view(&state, &envelope))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

/// Public certificate verification: recompute the certificate
/// hash from the stored signing record and verify the audit
/// chain it is bound to.
pub(crate) async fn verify_certificate(
    state: web::Data<SignState>,
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    path: web::Path<CertificatePath>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let _ = &req;
    let result: UnifiedResult<Value> = (|| {
        let certificate_hash = path.into_inner().certificate_hash;
        if !valid_sha256_hex(&certificate_hash) {
            return Err(UnifiedError::not_found("Certificate not found"));
        }
        let certificate = state
            .certificates
            .get(&certificate_hash)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Certificate not found"))?;
        let envelope = state
            .envelopes
            .get(&certificate.envelope_id)
            .map(|entry| entry.value().clone());
        let (valid, broken_index) = match &envelope {
            Some(envelope) => {
                let recomputed = super::types::compute_certificate_hash(
                    envelope.id,
                    &envelope.document_hashes,
                    &state.signers_of(envelope.id),
                    certificate.completed_at,
                );
                let chain = super::types::verify_audit_chain(&state.audit_of(envelope.id));
                (
                    recomputed == certificate.certificate_hash && chain.0,
                    chain.1,
                )
            }
            None => (false, None),
        };
        Ok(json!({
            "valid": valid,
            "certificate": certificate,
            "audit_chain_broken_index": broken_index,
        }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}
