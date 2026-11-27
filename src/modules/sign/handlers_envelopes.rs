//! HTTP handlers for sign envelopes: creation, sending,
//! status transitions, certificates, audit, and reminders.

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
    append_audit, caller, caller_company, created, envelope_view, err, load_envelope, ok, paginate,
    require_permission, require_tenant, require_text, resolve_tenant, validate_document_hashes,
    validate_document_ids, validate_reminder_schedule, validate_signer_input,
};
use super::state::SignState;
use super::types::{
    CreateEnvelopeRequest, DEFAULT_TOKEN_TTL_DAYS, EVENT_ENVELOPE_CANCELLED,
    EVENT_ENVELOPE_CREATED, EVENT_ENVELOPE_VOIDED, EVENT_REMINDER_DUE, EnvelopeStatus,
    MAX_SIGNERS_PER_ENVELOPE, MAX_TITLE_LEN, PERM_SIGN_MANAGE, PERM_SIGN_VIEW, SignEnvelope,
    SignSigner, SignerStatus, UpdateEnvelopeRequest, envelope_can_transition,
    generate_signing_token, hash_value, next_reminder_at, reminder_due, reminders_remaining,
    valid_signing_order,
};

/// Path carrying an envelope id.
#[derive(Debug, Deserialize)]
struct EnvelopePath {
    envelope_id: String,
}

/// Issue a fresh signing token for a signer: the raw token is
/// returned to the caller (once), only its hash is stored, with
/// a fresh expiry. Returns the raw token.
fn issue_token(state: &SignState, signer: &mut SignSigner) -> String {
    let token = generate_signing_token();
    signer.token_hash = hash_value(&token);
    signer.token_expires_at = Some(Utc::now() + Duration::days(DEFAULT_TOKEN_TTL_DAYS));
    signer.updated_at = Utc::now();
    state.signers.insert(signer.id, signer.clone());
    token
}

fn envelope_event(state: &SignState, event: &str, tenant_id: &str, envelope: &SignEnvelope) {
    state.emit(
        event,
        json!({
            "tenant_id": tenant_id,
            "envelope_id": envelope.id.to_string(),
            "title": envelope.title,
            "status": envelope.status.as_str(),
            "document_ids": envelope.document_ids,
        }),
    );
}

// Envelope handlers (authenticated)

pub(crate) async fn list_envelopes(
    state: web::Data<SignState>,
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let tenant_id = resolve_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_SIGN_VIEW)?;
        let params = query.into_inner();
        let status_filter = params
            .filters
            .get("status")
            .and_then(|raw| EnvelopeStatus::from_str(raw));
        let signer_filter = params.filters.get("signer").cloned();
        let document_filter = params.filters.get("document_id").cloned();
        let search = params.search.clone();
        let mut items: Vec<SignEnvelope> = state
            .envelopes
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|envelope| envelope.tenant_id == tenant_id)
            .map(|envelope| super::helpers::refresh_expiry(&state, envelope))
            .filter(|envelope| {
                if let Some(status) = status_filter {
                    if envelope.status != status {
                        return false;
                    }
                }
                if let Some(document_id) = &document_filter {
                    if !envelope.document_ids.iter().any(|id| id == document_id) {
                        return false;
                    }
                }
                if let Some(needle) = &signer_filter {
                    let needle = needle.to_lowercase();
                    let matches = state.signers_of(envelope.id).iter().any(|signer| {
                        signer.email.to_lowercase().contains(&needle)
                            || signer.name.to_lowercase().contains(&needle)
                    });
                    if !matches {
                        return false;
                    }
                }
                if let Some(needle) = &search {
                    let needle = needle.to_lowercase();
                    if !envelope.title.to_lowercase().contains(&needle)
                        && !envelope
                            .message
                            .as_deref()
                            .unwrap_or("")
                            .to_lowercase()
                            .contains(&needle)
                    {
                        return false;
                    }
                }
                true
            })
            .collect();
        items.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        let views: Vec<Value> = items
            .iter()
            .map(|item| envelope_view(&state, item))
            .collect();
        Ok(paginate(views, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn create_envelope(
    state: web::Data<SignState>,
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    body: web::Json<CreateEnvelopeRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let tenant_id = resolve_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_SIGN_MANAGE)?;
        let input = body.into_inner();
        let title = require_text(&input.title, "title", MAX_TITLE_LEN)?;
        validate_document_ids(&input.document_ids)?;
        let document_hashes =
            validate_document_hashes(&input.document_ids, &input.document_hashes)?;
        let signing_order = input
            .signing_order
            .unwrap_or_else(|| "sequential".to_string());
        if !valid_signing_order(&signing_order) {
            return Err(UnifiedError::bad_request(
                "signing_order must be sequential or parallel",
            ));
        }
        let reminder_schedule = input
            .reminder_schedule
            .unwrap_or_else(|| state.settings_for(&tenant_id).default_reminder_schedule);
        validate_reminder_schedule(&reminder_schedule)?;
        if let Some(expires_at) = input.expires_at {
            if expires_at <= Utc::now() {
                return Err(UnifiedError::bad_request(
                    "expires_at must be in the future",
                ));
            }
        }
        if input.signers.len() > MAX_SIGNERS_PER_ENVELOPE {
            return Err(UnifiedError::bad_request(format!(
                "At most {MAX_SIGNERS_PER_ENVELOPE} signers per envelope"
            )));
        }
        let message = match &input.message {
            Some(text) => Some(require_text(
                text,
                "message",
                super::types::MAX_MESSAGE_LEN,
            )?),
            None => None,
        };
        let mut seen_emails = std::collections::HashSet::new();
        let mut signer_rows: Vec<(String, String, String, i64)> = Vec::new();
        for (index, signer_input) in input.signers.iter().enumerate() {
            let (name, email, role) =
                validate_signer_input(&signer_input.name, &signer_input.email, &signer_input.role)?;
            if !seen_emails.insert(email.to_lowercase()) {
                return Err(UnifiedError::conflict(format!(
                    "Signer with email \"{email}\" already exists on this envelope"
                )));
            }
            let order_index = signer_input.order_index.unwrap_or(index as i64);
            if order_index < 0 {
                return Err(UnifiedError::bad_request(
                    "order_index must not be negative",
                ));
            }
            signer_rows.push((name, email, role, order_index));
        }
        if signer_rows
            .iter()
            .map(|row| row.3)
            .collect::<std::collections::HashSet<_>>()
            .len()
            != signer_rows.len()
        {
            return Err(UnifiedError::bad_request(
                "signer order_index values must be unique",
            ));
        }
        let now = Utc::now();
        let envelope = SignEnvelope {
            id: Uuid::new_v4(),
            tenant_id: tenant_id.clone(),
            title,
            message,
            document_ids: input.document_ids,
            document_hashes,
            signing_order,
            status: EnvelopeStatus::Draft,
            template_id: None,
            expires_at: input.expires_at,
            sent_at: None,
            completed_at: None,
            certificate_hash: None,
            reminder_schedule,
            created_by: caller.actor.clone(),
            created_at: now,
            updated_at: now,
        };
        state.envelopes.insert(envelope.id, envelope.clone());
        for (name, email, role, order_index) in signer_rows {
            let signer = SignSigner {
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
            state.signers.insert(signer.id, signer);
        }
        append_audit(
            &state,
            &tenant_id,
            envelope.id,
            None,
            "created",
            &caller.actor,
            Some(format!(
                "Envelope created with {} document(s)",
                envelope.document_ids.len()
            )),
        );
        envelope_event(&state, EVENT_ENVELOPE_CREATED, &tenant_id, &envelope);
        Ok(envelope_view(&state, &envelope))
    })();
    match result {
        Ok(data) => created(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn get_envelope(
    state: web::Data<SignState>,
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    path: web::Path<EnvelopePath>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let tenant_id = resolve_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_SIGN_VIEW)?;
        let envelope = load_envelope(&state, &tenant_id, &path.into_inner().envelope_id)?;
        Ok(envelope_view(&state, &envelope))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn update_envelope(
    state: web::Data<SignState>,
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    path: web::Path<EnvelopePath>,
    body: web::Json<UpdateEnvelopeRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let tenant_id = resolve_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_SIGN_MANAGE)?;
        let mut envelope = load_envelope(&state, &tenant_id, &path.into_inner().envelope_id)?;
        if envelope.status != EnvelopeStatus::Draft {
            return Err(UnifiedError::conflict(format!(
                "Envelopes can only be updated while draft (current status \"{}\")",
                envelope.status.as_str(),
            )));
        }
        let input = body.into_inner();
        if let Some(title) = input.title {
            envelope.title = require_text(&title, "title", MAX_TITLE_LEN)?;
        }
        if let Some(message) = input.message {
            envelope.message = match message {
                Some(text) => Some(require_text(
                    &text,
                    "message",
                    super::types::MAX_MESSAGE_LEN,
                )?),
                None => None,
            };
        }
        if let Some(document_ids) = input.document_ids {
            validate_document_ids(&document_ids)?;
            envelope.document_hashes =
                validate_document_hashes(&document_ids, &input.document_hashes)?;
            envelope.document_ids = document_ids;
        } else if input.document_hashes.is_some() {
            envelope.document_hashes =
                validate_document_hashes(&envelope.document_ids, &input.document_hashes)?;
        }
        if let Some(signing_order) = input.signing_order {
            if !valid_signing_order(&signing_order) {
                return Err(UnifiedError::bad_request(
                    "signing_order must be sequential or parallel",
                ));
            }
            envelope.signing_order = signing_order;
        }
        if let Some(expires_at) = input.expires_at {
            if let Some(expires_at) = expires_at {
                if expires_at <= Utc::now() {
                    return Err(UnifiedError::bad_request(
                        "expires_at must be in the future",
                    ));
                }
            }
            envelope.expires_at = expires_at;
        }
        if let Some(schedule) = input.reminder_schedule {
            validate_reminder_schedule(&schedule)?;
            envelope.reminder_schedule = schedule;
        }
        envelope.updated_at = Utc::now();
        state.envelopes.insert(envelope.id, envelope.clone());
        append_audit(
            &state,
            &tenant_id,
            envelope.id,
            None,
            "updated",
            &caller.actor,
            None,
        );
        Ok(envelope_view(&state, &envelope))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn delete_envelope(
    state: web::Data<SignState>,
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    path: web::Path<EnvelopePath>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let tenant_id = resolve_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_SIGN_MANAGE)?;
        let envelope = load_envelope(&state, &tenant_id, &path.into_inner().envelope_id)?;
        if matches!(
            envelope.status,
            EnvelopeStatus::Sent | EnvelopeStatus::PartiallySigned | EnvelopeStatus::Completed
        ) {
            return Err(UnifiedError::conflict(format!(
                "Envelopes cannot be deleted while \"{}\"",
                envelope.status.as_str(),
            )));
        }
        for signer in state.signers_of(envelope.id) {
            state.signers.remove(&signer.id);
        }
        state.audit.remove(&envelope.id);
        if let Some(certificate_hash) = &envelope.certificate_hash {
            state.certificates.remove(certificate_hash);
        }
        state.envelopes.remove(&envelope.id);
        Ok(json!({
            "deleted": true,
            "envelope_id": envelope.id.to_string(),
        }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn send_envelope(
    state: web::Data<SignState>,
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    path: web::Path<EnvelopePath>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let tenant_id = resolve_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_SIGN_MANAGE)?;
        let mut envelope = load_envelope(&state, &tenant_id, &path.into_inner().envelope_id)?;
        if envelope.status != EnvelopeStatus::Draft {
            return Err(UnifiedError::conflict(format!(
                "Only draft envelopes can be sent (current status \"{}\")",
                envelope.status.as_str(),
            )));
        }
        let signers = state.signers_of(envelope.id);
        if signers.is_empty() {
            return Err(UnifiedError::bad_request(
                "An envelope needs at least one signer before it can be sent",
            ));
        }
        // Every document must carry its SHA-256 before sending:
        // the hash is the tamper evidence the signers attest to.
        if envelope.document_hashes.iter().any(|hash| hash.is_empty()) {
            return Err(UnifiedError::bad_request(
                "Every document needs a document_hash before the envelope can be sent",
            ));
        }
        let settings = state.settings_for(&tenant_id);
        if envelope.expires_at.is_none() {
            envelope.expires_at =
                Some(Utc::now() + Duration::days(settings.default_expiry_days.max(1)));
        }
        let now = Utc::now();
        envelope.status = EnvelopeStatus::Sent;
        envelope.sent_at = Some(now);
        envelope.updated_at = now;
        state.envelopes.insert(envelope.id, envelope.clone());
        let mut signer_views: Vec<Value> = Vec::new();
        for mut signer in signers {
            let token = issue_token(&state, &mut signer);
            signer.status = SignerStatus::Sent;
            signer.updated_at = now;
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
            signer_views.push(super::helpers::signer_view_with_token(&signer, &token));
        }
        append_audit(
            &state,
            &tenant_id,
            envelope.id,
            None,
            "sent",
            &caller.actor,
            Some(format!("Envelope sent to {} signer(s)", signer_views.len())),
        );
        let mut view = envelope_view(&state, &envelope);
        if let Value::Object(map) = &mut view {
            map.insert("signers".to_string(), Value::Array(signer_views));
        }
        Ok(view)
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn cancel_envelope(
    state: web::Data<SignState>,
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    path: web::Path<EnvelopePath>,
) -> HttpResponse {
    transition_terminal(
        &state,
        req,
        ctx,
        path,
        EnvelopeStatus::Cancelled,
        "cancelled",
    )
    .await
}

pub(crate) async fn void_envelope(
    state: web::Data<SignState>,
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    path: web::Path<EnvelopePath>,
) -> HttpResponse {
    transition_terminal(&state, req, ctx, path, EnvelopeStatus::Voided, "voided").await
}

async fn transition_terminal(
    state: &web::Data<SignState>,
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    path: web::Path<EnvelopePath>,
    to: EnvelopeStatus,
    action: &str,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let tenant_id = resolve_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_SIGN_MANAGE)?;
        let mut envelope = load_envelope(state, &tenant_id, &path.into_inner().envelope_id)?;
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
        // Revoke every outstanding signing token by clearing the
        // stored hashes: the raw tokens no longer resolve.
        for mut signer in state.signers_of(envelope.id) {
            if matches!(
                signer.status,
                SignerStatus::Pending | SignerStatus::Sent | SignerStatus::Viewed
            ) {
                signer.token_hash = String::new();
                signer.token_expires_at = None;
                signer.updated_at = Utc::now();
                state.signers.insert(signer.id, signer);
            }
        }
        append_audit(
            state,
            &tenant_id,
            envelope.id,
            None,
            action,
            &caller.actor,
            None,
        );
        let event = match to {
            EnvelopeStatus::Cancelled => EVENT_ENVELOPE_CANCELLED,
            EnvelopeStatus::Voided => EVENT_ENVELOPE_VOIDED,
            _ => EVENT_ENVELOPE_CANCELLED,
        };
        envelope_event(state, event, &tenant_id, &envelope);
        Ok(envelope_view(state, &envelope))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn get_certificate(
    state: web::Data<SignState>,
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    path: web::Path<EnvelopePath>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let tenant_id = resolve_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_SIGN_VIEW)?;
        let envelope = load_envelope(&state, &tenant_id, &path.into_inner().envelope_id)?;
        let certificate_hash = envelope.certificate_hash.clone().ok_or_else(|| {
            UnifiedError::not_found(format!(
                "Certificate not found for envelope: {}",
                envelope.id
            ))
        })?;
        let certificate = state
            .certificates
            .get(&certificate_hash)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| {
                UnifiedError::not_found(format!(
                    "Certificate not found for envelope: {}",
                    envelope.id
                ))
            })?;
        let chain = super::types::verify_audit_chain(&state.audit_of(envelope.id));
        Ok(json!({
            "certificate": certificate,
            "audit_chain_valid": chain.0,
            "audit_events_checked": state.audit_of(envelope.id).len(),
        }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn list_audit(
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
        let action_filter = params.filters.get("action").cloned();
        let mut entries = state.audit_of(envelope.id);
        entries.sort_by_key(|entry| entry.created_at);
        if let Some(action) = &action_filter {
            entries.retain(|entry| entry.action == *action);
        }
        let (valid, broken_index) = super::types::verify_audit_chain(&entries);
        let mut page = paginate(
            entries
                .iter()
                .map(|entry| serde_json::to_value(entry).unwrap_or(Value::Null))
                .collect::<Vec<Value>>(),
            &params,
        )
        .to_json();
        if let Value::Object(map) = &mut page {
            map.insert("audit_chain_valid".to_string(), Value::Bool(valid));
            map.insert(
                "audit_chain_broken_index".to_string(),
                broken_index.map(Value::from).unwrap_or(Value::Null),
            );
        }
        Ok(page)
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

/// Send reminders now to every signer who has not signed yet.
/// Each reminder emits `sign.reminder.due` (for the
/// notifications module) and a `notification.requested`.
pub(crate) async fn remind_envelope(
    state: web::Data<SignState>,
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    path: web::Path<EnvelopePath>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let tenant_id = resolve_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_SIGN_MANAGE)?;
        let envelope = load_envelope(&state, &tenant_id, &path.into_inner().envelope_id)?;
        if !matches!(
            envelope.status,
            EnvelopeStatus::Sent | EnvelopeStatus::PartiallySigned
        ) {
            return Err(UnifiedError::conflict(format!(
                "Reminders can only be sent while an envelope is sent or partially signed (current status \"{}\")",
                envelope.status.as_str(),
            )));
        }
        let settings = state.settings_for(&tenant_id);
        if !settings.reminder_enabled {
            return Err(UnifiedError::conflict(
                "Reminders are disabled in this tenant's sign settings",
            ));
        }
        let now = Utc::now();
        let mut reminded = 0i64;
        for mut signer in state.signers_of(envelope.id) {
            if !matches!(
                signer.status,
                SignerStatus::Pending | SignerStatus::Sent | SignerStatus::Viewed
            ) {
                continue;
            }
            signer.reminders_sent += 1;
            signer.last_reminder_at = Some(now);
            signer.updated_at = now;
            state.signers.insert(signer.id, signer.clone());
            state.emit(
                EVENT_REMINDER_DUE,
                json!({
                    "tenant_id": tenant_id,
                    "envelope_id": envelope.id.to_string(),
                    "signer_id": signer.id.to_string(),
                    "email": signer.email,
                    "title": envelope.title,
                    "reminders_sent": signer.reminders_sent,
                    "expires_at": envelope.expires_at.map(|at| at.to_rfc3339()),
                }),
            );
            state.notify(
                "sign.reminder",
                &signer.email,
                &format!("Reminder: sign \"{}\"", envelope.title),
                &format!(
                    "This is a reminder to sign \"{}\". Use your signing link to open it.",
                    envelope.title
                ),
                None,
            );
            reminded += 1;
        }
        append_audit(
            &state,
            &tenant_id,
            envelope.id,
            None,
            "reminded",
            &caller.actor,
            Some(format!("Reminder sent to {reminded} signer(s)")),
        );
        Ok(json!({
            "envelope_id": envelope.id.to_string(),
            "reminded_signers": reminded,
        }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

/// Envelopes whose next reminder is due: sent or
/// partially-signed envelopes with pending signers, reminder
/// schedule enabled, and the computed next reminder time in the
/// past. Read-only: nothing is sent or emitted here.
pub(crate) async fn list_reminders_due(
    state: web::Data<SignState>,
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let tenant_id = resolve_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_SIGN_VIEW)?;
        let params = query.into_inner();
        let settings = state.settings_for(&tenant_id);
        if !settings.reminder_enabled {
            return Ok(paginate(Vec::<Value>::new(), &params).to_json());
        }
        let now = Utc::now();
        let mut items: Vec<Value> = state
            .envelopes
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|envelope| envelope.tenant_id == tenant_id)
            .map(|envelope| super::helpers::refresh_expiry(&state, envelope))
            .filter(|envelope| {
                matches!(
                    envelope.status,
                    EnvelopeStatus::Sent | EnvelopeStatus::PartiallySigned
                )
            })
            .filter_map(|envelope| {
                let signers = state.signers_of(envelope.id);
                let pending: Vec<&SignSigner> = signers
                    .iter()
                    .filter(|signer| {
                        matches!(
                            signer.status,
                            SignerStatus::Pending | SignerStatus::Sent | SignerStatus::Viewed
                        )
                    })
                    .collect();
                if pending.is_empty() {
                    return None;
                }
                let last_reminder = signers
                    .iter()
                    .filter_map(|signer| signer.last_reminder_at)
                    .max();
                let reminders_sent: i64 =
                    signers.iter().map(|signer| signer.reminders_sent).sum();
                let sent_at = envelope.sent_at?;
                if !reminder_due(
                    &envelope.reminder_schedule,
                    sent_at,
                    last_reminder,
                    reminders_sent,
                    now,
                ) {
                    return None;
                }
                let next = next_reminder_at(
                    &envelope.reminder_schedule,
                    sent_at,
                    last_reminder,
                    reminders_sent,
                );
                Some(json!({
                    "envelope_id": envelope.id.to_string(),
                    "title": envelope.title,
                    "status": envelope.status.as_str(),
                    "pending_signers": pending.len(),
                    "reminders_sent": reminders_sent,
                    "reminders_remaining": reminders_remaining(&envelope.reminder_schedule, reminders_sent),
                    "next_reminder_at": next.map(|at| at.to_rfc3339()),
                    "expires_at": envelope.expires_at.map(|at| at.to_rfc3339()),
                }))
            })
            .collect();
        items.sort_by(|a, b| {
            a.get("next_reminder_at")
                .and_then(Value::as_str)
                .cmp(&b.get("next_reminder_at").and_then(Value::as_str))
        });
        Ok(paginate(items, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}
