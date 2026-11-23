//! HTTP handlers for sign templates: reusable signing
//! templates and building envelopes from them.

use actix_web::{HttpRequest, HttpResponse, web};
use chrono::{Duration, Utc};
use serde::Deserialize;
use serde_json::Value;
use std::sync::Arc;
use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::pagination::PaginationParams;
use uuid::Uuid;

use super::helpers::{
    append_audit, caller, caller_company, created, err, load_template, ok, paginate,
    require_permission, require_tenant, require_text, resolve_tenant, validate_document_hashes,
    validate_document_ids, validate_reminder_schedule, validate_signer_input,
    validate_template_fields, validate_template_roles,
};
use super::state::SignState;
use super::types::{
    CreateTemplateRequest, EVENT_ENVELOPE_CREATED, EnvelopeStatus, MAX_TITLE_LEN, PERM_SIGN_MANAGE,
    PERM_SIGN_VIEW, SignEnvelope, SignSigner, SignTemplate, SignerStatus, UpdateTemplateRequest,
    UseTemplateRequest, valid_signing_order,
};

/// Path carrying a template id.
#[derive(Debug, Deserialize)]
struct TemplatePath {
    template_id: String,
}

pub(crate) async fn list_templates(
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
        let search = params.search.clone();
        let mut items: Vec<SignTemplate> = state
            .templates
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|template| template.tenant_id == tenant_id)
            .filter(|template| {
                if let Some(needle) = &search {
                    let needle = needle.to_lowercase();
                    template.name.to_lowercase().contains(&needle)
                        || template
                            .description
                            .as_deref()
                            .unwrap_or("")
                            .to_lowercase()
                            .contains(&needle)
                } else {
                    true
                }
            })
            .collect();
        items.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        let views: Vec<Value> = items
            .iter()
            .map(|item| serde_json::to_value(item).unwrap_or(Value::Null))
            .collect();
        Ok(paginate(views, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn create_template(
    state: web::Data<SignState>,
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    body: web::Json<CreateTemplateRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let tenant_id = resolve_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_SIGN_MANAGE)?;
        let input = body.into_inner();
        let name = require_text(&input.name, "name", MAX_TITLE_LEN)?;
        if state
            .templates
            .iter()
            .any(|entry| entry.value().tenant_id == tenant_id && entry.value().name == name)
        {
            return Err(UnifiedError::conflict(format!(
                "Template with name \"{name}\" already exists"
            )));
        }
        validate_document_ids(&input.document_ids)?;
        validate_template_roles(&input.signer_roles)?;
        validate_template_fields(&input.fields)?;
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
        if let Some(days) = input.expires_in_days {
            if days < 1 {
                return Err(UnifiedError::bad_request(
                    "expires_in_days must be at least 1",
                ));
            }
        }
        let description = match &input.description {
            Some(text) => Some(require_text(
                text,
                "description",
                super::types::MAX_MESSAGE_LEN,
            )?),
            None => None,
        };
        let now = Utc::now();
        let template = SignTemplate {
            id: Uuid::new_v4(),
            tenant_id: tenant_id.clone(),
            name,
            description,
            document_ids: input.document_ids,
            signer_roles: input.signer_roles,
            fields: input.fields,
            signing_order,
            reminder_schedule,
            expires_in_days: input.expires_in_days,
            created_by: caller.actor.clone(),
            created_at: now,
            updated_at: now,
        };
        state.templates.insert(template.id, template.clone());
        Ok(serde_json::to_value(template).unwrap_or(Value::Null))
    })();
    match result {
        Ok(data) => created(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn get_template(
    state: web::Data<SignState>,
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    path: web::Path<TemplatePath>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let tenant_id = resolve_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_SIGN_VIEW)?;
        let template = load_template(&state, &tenant_id, &path.into_inner().template_id)?;
        Ok(serde_json::to_value(template).unwrap_or(Value::Null))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn update_template(
    state: web::Data<SignState>,
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    path: web::Path<TemplatePath>,
    body: web::Json<UpdateTemplateRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let tenant_id = resolve_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_SIGN_MANAGE)?;
        let mut template = load_template(&state, &tenant_id, &path.into_inner().template_id)?;
        let input = body.into_inner();
        if let Some(name) = input.name {
            let name = require_text(&name, "name", MAX_TITLE_LEN)?;
            if state.templates.iter().any(|entry| {
                entry.value().tenant_id == tenant_id
                    && entry.value().name == name
                    && entry.value().id != template.id
            }) {
                return Err(UnifiedError::conflict(format!(
                    "Template with name \"{name}\" already exists"
                )));
            }
            template.name = name;
        }
        if let Some(description) = input.description {
            template.description = match description {
                Some(text) => Some(require_text(
                    &text,
                    "description",
                    super::types::MAX_MESSAGE_LEN,
                )?),
                None => None,
            };
        }
        if let Some(document_ids) = input.document_ids {
            validate_document_ids(&document_ids)?;
            template.document_ids = document_ids;
        }
        if let Some(roles) = input.signer_roles {
            validate_template_roles(&roles)?;
            template.signer_roles = roles;
        }
        if let Some(fields) = input.fields {
            validate_template_fields(&fields)?;
            template.fields = fields;
        }
        if let Some(signing_order) = input.signing_order {
            if !valid_signing_order(&signing_order) {
                return Err(UnifiedError::bad_request(
                    "signing_order must be sequential or parallel",
                ));
            }
            template.signing_order = signing_order;
        }
        if let Some(schedule) = input.reminder_schedule {
            validate_reminder_schedule(&schedule)?;
            template.reminder_schedule = schedule;
        }
        if let Some(expires_in_days) = input.expires_in_days {
            if let Some(days) = expires_in_days {
                if days < 1 {
                    return Err(UnifiedError::bad_request(
                        "expires_in_days must be at least 1",
                    ));
                }
            }
            template.expires_in_days = expires_in_days;
        }
        template.updated_at = Utc::now();
        state.templates.insert(template.id, template.clone());
        Ok(serde_json::to_value(template).unwrap_or(Value::Null))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn delete_template(
    state: web::Data<SignState>,
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    path: web::Path<TemplatePath>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let tenant_id = resolve_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_SIGN_MANAGE)?;
        let template = load_template(&state, &tenant_id, &path.into_inner().template_id)?;
        // Envelopes built from the template keep their own
        // snapshot, so deleting the template never orphans one.
        state.templates.remove(&template.id);
        Ok(serde_json::json!({
            "deleted": true,
            "template_id": template.id.to_string(),
        }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

/// Build a draft envelope from a template. Signers come from
/// the template roles (using their default name/email when
/// set) merged with signers supplied here, matched by role.
/// Every role without a default needs a supplied signer.
pub(crate) async fn use_template(
    state: web::Data<SignState>,
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    path: web::Path<TemplatePath>,
    body: web::Json<UseTemplateRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let tenant_id = resolve_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_SIGN_MANAGE)?;
        let template = load_template(&state, &tenant_id, &path.into_inner().template_id)?;
        let input = body.into_inner();
        let title = require_text(&input.title, "title", MAX_TITLE_LEN)?;
        let document_ids = input
            .document_ids
            .unwrap_or_else(|| template.document_ids.clone());
        validate_document_ids(&document_ids)?;
        let document_hashes = validate_document_hashes(&document_ids, &input.document_hashes)?;
        let message = match &input.message {
            Some(text) => Some(require_text(
                text,
                "message",
                super::types::MAX_MESSAGE_LEN,
            )?),
            None => None,
        };
        if let Some(expires_at) = input.expires_at {
            if expires_at <= Utc::now() {
                return Err(UnifiedError::bad_request(
                    "expires_at must be in the future",
                ));
            }
        }
        let mut signer_rows: Vec<(String, String, String, i64)> = Vec::new();
        let mut seen_emails = std::collections::HashSet::new();
        for role in &template.signer_roles {
            let supplied = input.signers.iter().find(|signer| signer.role == role.role);
            let (name, email) = match supplied {
                Some(signer) => (signer.name.clone(), signer.email.clone()),
                None => match (&role.name, &role.email) {
                    (Some(name), Some(email)) => (name.clone(), email.clone()),
                    _ => {
                        return Err(UnifiedError::bad_request(format!(
                            "Role \"{}\" needs a signer: supply name and email, or set a default on the template",
                            role.role
                        )));
                    }
                },
            };
            let (name, email, role_name) = validate_signer_input(&name, &email, &role.role)?;
            if !seen_emails.insert(email.to_lowercase()) {
                return Err(UnifiedError::conflict(format!(
                    "Signer with email \"{email}\" already exists on this envelope"
                )));
            }
            signer_rows.push((name, email, role_name, role.order_index));
        }
        // Signers supplied for roles the template does not have
        // are appended after the template roles, in order.
        let mut next_order = signer_rows.iter().map(|row| row.3).max().unwrap_or(-1) + 1;
        for signer_input in &input.signers {
            if template
                .signer_roles
                .iter()
                .any(|role| role.role == signer_input.role)
            {
                continue;
            }
            let (name, email, role) =
                validate_signer_input(&signer_input.name, &signer_input.email, &signer_input.role)?;
            if !seen_emails.insert(email.to_lowercase()) {
                return Err(UnifiedError::conflict(format!(
                    "Signer with email \"{email}\" already exists on this envelope"
                )));
            }
            signer_rows.push((name, email, role, next_order));
            next_order += 1;
        }
        if signer_rows.is_empty() {
            return Err(UnifiedError::bad_request(
                "A template with no signer roles needs at least one signer",
            ));
        }
        let expires_at = match input.expires_at {
            Some(expires_at) => Some(expires_at),
            None => template
                .expires_in_days
                .map(|days| Utc::now() + Duration::days(days.max(1))),
        };
        let now = Utc::now();
        let envelope = SignEnvelope {
            id: Uuid::new_v4(),
            tenant_id: tenant_id.clone(),
            title,
            message,
            document_ids,
            document_hashes,
            signing_order: template.signing_order.clone(),
            status: EnvelopeStatus::Draft,
            template_id: Some(template.id),
            expires_at,
            sent_at: None,
            completed_at: None,
            certificate_hash: None,
            reminder_schedule: template.reminder_schedule.clone(),
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
            Some(format!("Envelope created from template {}", template.id)),
        );
        state.emit(
            EVENT_ENVELOPE_CREATED,
            serde_json::json!({
                "tenant_id": tenant_id,
                "envelope_id": envelope.id.to_string(),
                "title": envelope.title,
                "status": envelope.status.as_str(),
                "template_id": template.id.to_string(),
                "document_ids": envelope.document_ids,
            }),
        );
        Ok(super::helpers::envelope_view(&state, &envelope))
    })();
    match result {
        Ok(data) => created(data),
        Err(error) => err(error, debug),
    }
}
