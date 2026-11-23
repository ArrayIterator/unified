//! Mail - HTTP handlers.
//!
//! Part of the `mail` module split; see `src/modules/mail.rs`.

use actix_web::HttpRequest;
use actix_web::HttpResponse;
use actix_web::web;
use chrono::Utc;
use lettre::Address;
use serde::Deserialize;
use serde_json::Value;
use serde_json::json;
use std::collections::HashMap;
use std::sync::Arc;
use unified_core::error::UnifiedError;
use unified_core::json as api_json;
use unified_core::pagination::PageResult;
use unified_core::pagination::PaginationParams;
use uuid::Uuid;

use super::service::deliver_sync;
use super::service::parse_mailbox;
use super::service::sendmail_binary;
use super::service::validate_header_value;
use super::state::MailState;
use super::types::CONNECTOR_LOCAL;
use super::types::CONNECTOR_LOG;
use super::types::CONNECTOR_SMTP;
use super::types::EVENT_MAIL_FAILED;
use super::types::EVENT_MAIL_SENT;
use super::types::EVENT_TEMPLATE_RENDER;
use super::types::MailLogEntry;
use super::types::OutgoingMessage;
use super::types::SmtpSettings;
use super::types::SmtpSettingsInput;
use super::{caller_company, require_tenant};

// ---------------------------------------------------------------------------
// HTTP handlers
// ---------------------------------------------------------------------------

pub(crate) async fn get_settings(
    state: web::Data<Arc<MailState>>,
    req: HttpRequest,
) -> HttpResponse {
    // C2: SMTP settings (including the stored password) are admin-only;
    // no verified caller, no read.
    let _tenant = require_tenant(&req)?;
    let _scope = caller_company(&_tenant, &req)?;
    if let Err(error) = unified_core::caller::require_caller(&req) {
        return api_json::error_response(&error, state.is_debug());
    }
    match state.settings_snapshot() {
        Some(settings) => {
            api_json::success_response(json!({ "configured": true, "settings": settings }), 200)
        }
        None => api_json::success_response(json!({ "configured": false, "settings": null }), 200),
    }
}

pub(crate) async fn update_settings(
    state: web::Data<Arc<MailState>>,
    req: HttpRequest,
    body: web::Json<SmtpSettingsInput>,
) -> HttpResponse {
    let debug = state.is_debug();
    // C2: overwriting SMTP host/credentials is admin-only; no
    // verified caller, no write.
    let _tenant = require_tenant(&req)?;
    let _scope = caller_company(&_tenant, &req)?;
    if let Err(error) = unified_core::caller::require_caller(&req) {
        return api_json::error_response(&error, debug);
    }
    let input = body.into_inner();
    if input.host.trim().is_empty() {
        return api_json::error_response(
            &UnifiedError::bad_request("SMTP host must not be empty"),
            debug,
        );
    }
    let port = input
        .port
        .unwrap_or(if input.tls.unwrap_or(true) { 587 } else { 25 });
    if port == 0 {
        return api_json::error_response(
            &UnifiedError::bad_request("SMTP port must be between 1 and 65535"),
            debug,
        );
    }
    if let Err(error) = input.sender_email.parse::<Address>() {
        return api_json::error_response(
            &UnifiedError::bad_request(format!(
                "sender_email is not a valid email address: {error}"
            )),
            debug,
        );
    }
    let connector = input
        .connector
        .clone()
        .unwrap_or_else(|| CONNECTOR_SMTP.to_string());
    if !matches!(
        connector.as_str(),
        CONNECTOR_SMTP | CONNECTOR_LOCAL | CONNECTOR_LOG
    ) {
        return api_json::error_response(
            &UnifiedError::bad_request(format!(
                "connector must be one of: {CONNECTOR_SMTP}, {CONNECTOR_LOCAL}, {CONNECTOR_LOG}"
            )),
            debug,
        );
    }
    if let Err(error) = validate_header_value(
        "sender_name",
        input.sender_name.as_deref().unwrap_or(""),
    ) {
        return api_json::error_response(&error, debug);
    }
    let previous_password = state
        .settings_snapshot()
        .map(|settings| settings.password)
        .unwrap_or_default();
    let settings = SmtpSettings {
        host: input.host.trim().to_string(),
        port,
        tls: input.tls.unwrap_or(true),
        username: input.username.unwrap_or_default(),
        // An omitted password keeps the previously stored one; an empty
        // string clears it.
        password: input.password.unwrap_or(previous_password),
        sender_name: input.sender_name.unwrap_or_default(),
        sender_email: input.sender_email.trim().to_string(),
        connector,
        updated_at: Utc::now().to_rfc3339(),
    };
    {
        let mut stored = state.settings.write().expect("mail settings lock poisoned");
        *stored = Some(settings.clone());
    }
    api_json::success_response(json!({ "configured": true, "settings": settings }), 200)
}

#[derive(Debug, Deserialize)]
struct SendMailInput {
    to: String,
    subject: Option<String>,
    body_text: Option<String>,
    body_html: Option<String>,
    template_id: Option<String>,
    variables: Option<HashMap<String, Value>>,
}

pub(crate) async fn send_mail(
    state: web::Data<Arc<MailState>>,
    body: web::Json<SendMailInput>,
    req: HttpRequest,
) -> HttpResponse {
    let _tenant = require_tenant(&req)?;
    let _scope = caller_company(&_tenant, &req)?;
    let state: Arc<MailState> = state.get_ref().clone();
    let debug = state.is_debug();
    let input = body.into_inner();

    if let Err(error) = parse_mailbox(&input.to) {
        return api_json::error_response(&error, debug);
    }
    let mut subject = input.subject.clone().unwrap_or_default();
    let mut body_html = input.body_html.clone();
    let body_text = input.body_text.clone();

    // Render the requested template through the Email Templates module.
    if let Some(template_id) = &input.template_id {
        match state.hooks() {
            Some(hooks) => {
                let rendered = hooks.emit(
                    EVENT_TEMPLATE_RENDER,
                    json!({
                        "template_id": template_id,
                        "variables": input.variables.clone().unwrap_or_default(),
                    }),
                );
                match rendered.results.first() {
                    Some(result) if result.get("error").is_none() => {
                        if let Some(html) = result.get("rendered_html").and_then(|v| v.as_str()) {
                            body_html = Some(html.to_string());
                        }
                        if subject.is_empty() {
                            if let Some(rendered_subject) =
                                result.get("subject").and_then(|v| v.as_str())
                            {
                                subject = rendered_subject.to_string();
                            }
                        }
                    }
                    Some(result) => {
                        let reason = result
                            .get("error")
                            .and_then(|v| v.as_str())
                            .unwrap_or("template render failed");
                        return api_json::error_response(
                            &UnifiedError::new(format!("Template render failed: {reason}"), 502),
                            debug,
                        );
                    }
                    None => {
                        return api_json::error_response(
                            &UnifiedError::new(
                                "Email template service did not answer the template.render_requested event",
                                503,
                            ),
                            debug,
                        );
                    }
                }
            }
            None => {
                return api_json::error_response(
                    &UnifiedError::new("Email template service is unavailable (no hook bus)", 503),
                    debug,
                );
            }
        }
    }
    // The subject may come from the template render above, so it is
    // checked here as well as in deliver_sync: a CR or LF would inject
    // arbitrary headers into the local-sendmail message.
    if let Err(error) = validate_header_value("subject", &subject) {
        return api_json::error_response(&error, debug);
    }
    if subject.trim().is_empty() {
        return api_json::error_response(
            &UnifiedError::bad_request("Provide a subject or a template_id that renders one"),
            debug,
        );
    }
    if body_text.is_none() && body_html.is_none() {
        return api_json::error_response(
            &UnifiedError::bad_request("Provide body_text or body_html (or a template_id)"),
            debug,
        );
    }

    let settings = match state.settings_snapshot() {
        Some(settings) => settings,
        None => {
            return api_json::error_response(
                &UnifiedError::new("SMTP settings are not configured", 503),
                debug,
            );
        }
    };
    let msg = OutgoingMessage {
        to: input.to.clone(),
        subject: subject.clone(),
        body_text,
        body_html,
        template_id: input.template_id.clone(),
    };
    let log_id = Uuid::now_v7().to_string();
    state.append_log(MailLogEntry {
        id: log_id.clone(),
        to: msg.to.clone(),
        subject: msg.subject.clone(),
        status: "queued".to_string(),
        connector: settings.connector.clone(),
        template_id: msg.template_id.clone(),
        error: None,
        created_at: Utc::now().to_rfc3339(),
    });

    let outcome = tokio::task::spawn_blocking(move || deliver_sync(&settings, &msg)).await;
    let outcome = match outcome {
        Ok(outcome) => outcome,
        Err(join_error) => Err(UnifiedError::internal(format!(
            "Mail delivery task failed: {join_error}"
        ))),
    };
    match outcome {
        Ok(()) => {
            state.update_log_status(&log_id, "sent", None);
            if let Some(hooks) = state.hooks() {
                hooks.emit(
                    EVENT_MAIL_SENT,
                    json!({ "job_id": log_id, "to": input.to, "subject": subject }),
                );
            }
            api_json::success_response(
                json!({ "sent": true, "log_id": log_id, "connector": state.settings_snapshot().map(|s| s.connector).unwrap_or_else(|| CONNECTOR_SMTP.to_string()) }),
                200,
            )
        }
        Err(error) => {
            state.update_log_status(&log_id, "failed", Some(error.message.clone()));
            if let Some(hooks) = state.hooks() {
                hooks.emit(
                    EVENT_MAIL_FAILED,
                    json!({ "job_id": log_id, "to": input.to, "error": error.message }),
                );
            }
            api_json::error_response(&error, debug)
        }
    }
}

#[derive(Debug, Deserialize)]
struct TestMailInput {
    to: String,
}

pub(crate) async fn test_mail(
    state: web::Data<Arc<MailState>>,
    body: web::Json<TestMailInput>,
    req: HttpRequest,
) -> HttpResponse {
    let _tenant = require_tenant(&req)?;
    let _scope = caller_company(&_tenant, &req)?;
    let state: Arc<MailState> = state.get_ref().clone();
    let input = body.into_inner();
    if let Err(error) = parse_mailbox(&input.to) {
        return api_json::error_response(&error, state.is_debug());
    }
    let settings = match state.settings_snapshot() {
        Some(settings) => settings,
        None => {
            // Diagnostic endpoint: report the missing configuration as
            // data instead of an error envelope.
            return api_json::success_response(
                json!({ "sent": false, "connector": null, "error": "SMTP settings are not configured" }),
                200,
            );
        }
    };
    let connector = settings.connector.clone();
    let msg = OutgoingMessage {
        to: input.to.clone(),
        subject: "Unified test message".to_string(),
        body_text: Some("This is a test message sent from the Unified Mail Manager.".to_string()),
        body_html: Some(
            "<p>This is a test message sent from the Unified Mail Manager.</p>".to_string(),
        ),
        template_id: None,
    };
    let outcome = tokio::task::spawn_blocking(move || deliver_sync(&settings, &msg)).await;
    let (sent, error) = match outcome {
        Ok(Ok(())) => (true, None),
        Ok(Err(error)) => (false, Some(error.message)),
        Err(join_error) => (
            false,
            Some(format!("Mail delivery task failed: {join_error}")),
        ),
    };
    state.append_log(MailLogEntry {
        id: Uuid::now_v7().to_string(),
        to: input.to,
        subject: "Unified test message".to_string(),
        status: if sent { "sent" } else { "failed" }.to_string(),
        connector: connector.clone(),
        template_id: None,
        error: error.clone(),
        created_at: Utc::now().to_rfc3339(),
    });
    api_json::success_response(
        json!({ "sent": sent, "connector": connector, "error": error }),
        200,
    )
}

pub(crate) async fn list_log(
    state: web::Data<Arc<MailState>>,
    query: web::Query<PaginationParams>,
    req: HttpRequest,
) -> HttpResponse {
    let _tenant = require_tenant(&req)?;
    let _scope = caller_company(&_tenant, &req)?;
    let pagination = query.normalized();
    let mut entries = state.log.read().expect("mail log lock poisoned").clone();
    if let Some(status) = pagination.filters.get("status") {
        entries.retain(|entry| entry.status == *status);
    }
    if let Some(search) = &pagination.search {
        let needle = search.to_lowercase();
        entries.retain(|entry| {
            entry.to.to_lowercase().contains(&needle)
                || entry.subject.to_lowercase().contains(&needle)
        });
    }
    let sort = query.allowed_sort(&["created_at", "to", "subject", "status"], "created_at");
    match sort.as_str() {
        "to" => entries.sort_by(|a, b| a.to.cmp(&b.to)),
        "subject" => entries.sort_by(|a, b| a.subject.cmp(&b.subject)),
        "status" => entries.sort_by(|a, b| a.status.cmp(&b.status)),
        _ => entries.sort_by(|a, b| a.created_at.cmp(&b.created_at)),
    }
    if pagination.order == "desc" {
        entries.reverse();
    }
    let total = entries.len() as u64;
    let start = ((pagination.page - 1) * pagination.per_page) as usize;
    let page_items: Vec<MailLogEntry> = entries
        .into_iter()
        .skip(start)
        .take(pagination.per_page as usize)
        .collect();
    api_json::success_response(
        PageResult::new(page_items, total, &pagination).to_json(),
        200,
    )
}

pub(crate) async fn list_connectors(
    state: web::Data<Arc<MailState>>,
    req: HttpRequest,
) -> HttpResponse {
    let _tenant = require_tenant(&req)?;
    let _scope = caller_company(&_tenant, &req)?;
    let configured = state.settings_snapshot().is_some();
    let local = sendmail_binary();
    api_json::success_response(
        json!([
            {
                "id": CONNECTOR_SMTP,
                "name": "SMTP",
                "description": "Deliver through the configured SMTP server (STARTTLS or implicit TLS).",
                "configured": configured,
                "available": true
            },
            {
                "id": CONNECTOR_LOCAL,
                "name": "Local sendmail",
                "description": "Deliver through the local sendmail binary (sendmail -t -i).",
                "configured": true,
                "available": local.is_some(),
                "binary": local
            },
            {
                "id": CONNECTOR_LOG,
                "name": "Log-only (debug)",
                "description": "Record the message in the send log without delivering it. Debug use only.",
                "configured": true,
                "available": true
            }
        ]),
        200,
    )
}
