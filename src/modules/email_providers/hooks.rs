//! email_providers - hooks.

use super::{
    adapters::*, credentials::*, events::*, handlers::*, http::*, module::*, provider_kinds::*,
    registry::*, store::*, worker::*,
};
use chrono::Utc;
use reqwest::header::{HeaderMap, HeaderValue};
use serde_json::{Value, json};
use std::collections::HashMap;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::HookContext;
use uuid::Uuid;

// ---------------------------------------------------------------------------
// Hook handlers.
// ---------------------------------------------------------------------------
pub(crate) fn on_email_send_requested(ctx: &mut HookContext) {
    let data = ctx.data.clone();
    let to = data
        .get("to")
        .and_then(|value| value.as_str())
        .unwrap_or("")
        .to_string();
    if let Err(error) = validate_email_address(&to) {
        ctx.add_result(json!({ "error": error.message }));
        return;
    }
    let connection =
        match resolve_connection(data.get("connection_id").and_then(|value| value.as_str())) {
            Ok(connection) => connection,
            Err(error) => {
                ctx.add_result(json!({ "error": error.message }));
                return;
            }
        };
    if !connection.enabled {
        ctx.add_result(json!({
            "error": format!("Email provider connection \"{}\" is disabled", connection.name)
        }));
        return;
    }
    let subject = data
        .get("subject")
        .and_then(|value| value.as_str())
        .unwrap_or("")
        .to_string();
    let template_id = data
        .get("template_id")
        .and_then(|value| value.as_str())
        .map(str::to_string);
    if subject.trim().is_empty() && template_id.is_none() {
        ctx.add_result(json!({
            "error": "email.send_requested requires a subject or a template_id"
        }));
        return;
    }
    let variables: HashMap<String, Value> = data
        .get("variables")
        .and_then(|value| serde_json::from_value(value.clone()).ok())
        .unwrap_or_default();
    let metadata: HashMap<String, String> = data
        .get("metadata")
        .and_then(|value| serde_json::from_value(value.clone()).ok())
        .unwrap_or_default();
    let message = EmailMessage {
        to: to.clone(),
        from: data
            .get("from")
            .and_then(|value| value.as_str())
            .map(str::to_string),
        from_name: data
            .get("from_name")
            .and_then(|value| value.as_str())
            .map(str::to_string),
        subject: subject.clone(),
        body_text: data
            .get("body_text")
            .and_then(|value| value.as_str())
            .map(str::to_string),
        body_html: data
            .get("body_html")
            .and_then(|value| value.as_str())
            .map(str::to_string),
        reply_to: data
            .get("reply_to")
            .and_then(|value| value.as_str())
            .map(str::to_string),
        template_id: template_id.clone(),
        variables,
        metadata,
        // Deliverability flags (task 354): a hook caller can mark a
        // send as bulk or opt out of deliverability headers; the
        // defaults keep transactional sends header-free.
        bulk: data
            .get("bulk")
            .and_then(|value| value.as_bool())
            .unwrap_or(false),
        include_unsubscribe: data
            .get("include_unsubscribe")
            .and_then(|value| value.as_bool())
            .unwrap_or(false),
        deliverability_opt_out: data
            .get("deliverability_opt_out")
            .and_then(|value| value.as_bool())
            .unwrap_or(false),
        test_mode: data
            .get("test_mode")
            .and_then(|value| value.as_bool())
            .unwrap_or(connection.test_mode),
    };
    let job_id = Uuid::now_v7().to_string();
    append_log(SendLogEntry {
        id: job_id.clone(),
        connection_id: connection.id.to_string(),
        provider: connection.kind.as_str().to_string(),
        to: to.clone(),
        subject,
        status: "queued".to_string(),
        provider_message_id: None,
        test_mode: message.test_mode,
        error: None,
        created_at: Utc::now().to_rfc3339(),
    });
    spawn_send_worker(QueuedSend {
        job_id: job_id.clone(),
        connection_id: connection.id,
        message,
    });
    ctx.add_result(json!({
        "queued": true,
        "job_id": job_id,
        "connection_id": connection.id.to_string(),
        "provider": connection.kind.as_str(),
    }));
}

pub(crate) fn on_webhook_verify(ctx: &mut HookContext) {
    let data = ctx.data.clone();
    let provider = data
        .get("provider")
        .and_then(|value| value.as_str())
        .unwrap_or("")
        .to_string();
    let Some(kind) = EmailProviderKind::from_str(&provider) else {
        ctx.add_result(json!({ "error": format!("Unknown email provider kind \"{provider}\"") }));
        return;
    };
    let connection = match data
        .get("connection_id")
        .and_then(|value| value.as_str())
        .map(|raw| Uuid::parse_str(raw))
    {
        Some(Ok(id)) => match get_connection(&id) {
            Some(connection) => connection,
            None => {
                ctx.add_result(
                    json!({ "error": format!("Email provider connection not found: {id}") }),
                );
                return;
            }
        },
        Some(Err(_)) => {
            ctx.add_result(json!({ "error": "Invalid connection id" }));
            return;
        }
        None => match first_enabled_connection(kind) {
            Some(connection) => connection,
            None => {
                ctx.add_result(json!({
                    "error": format!("No enabled connection exists for provider \"{provider}\"")
                }));
                return;
            }
        },
    };
    if connection.kind != kind {
        ctx.add_result(json!({
            "error": format!("Connection \"{}\" is a {} connection, not a {provider} connection", connection.name, connection.kind.as_str())
        }));
        return;
    }
    let headers_object = data.get("headers").cloned().unwrap_or(Value::Null);
    let mut headers = HeaderMap::new();
    if let Value::Object(map) = headers_object {
        for (name, value) in map {
            if let (Ok(header_name), Some(header_value)) = (
                name.parse::<reqwest::header::HeaderName>(),
                value
                    .as_str()
                    .and_then(|text| HeaderValue::from_str(text).ok()),
            ) {
                headers.insert(header_name, header_value);
            }
        }
    }
    let raw_body = data
        .get("body")
        .and_then(|value| value.as_str())
        .unwrap_or("")
        .as_bytes()
        .to_vec();
    // `webhook_verify` is async, but hook handlers are synchronous and
    // may run on an Actix worker thread that is already inside a
    // runtime, where `block_on` would panic. Run the verification on
    // a dedicated thread with its own runtime and wait for the result
    // over a channel instead.
    let result = (|| -> UnifiedResult<bool> {
        let adapter = email_provider_registry().build(&connection)?;
        let headers = headers.clone();
        let raw_body = raw_body.clone();
        let (tx, rx) = std::sync::mpsc::channel::<UnifiedResult<bool>>();
        std::thread::spawn(move || {
            let outcome = (|| -> UnifiedResult<bool> {
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .map_err(|error| {
                        UnifiedError::internal(format!(
                            "Cannot start webhook verification runtime: {error}"
                        ))
                    })?;
                runtime.block_on(adapter.webhook_verify(&headers, &raw_body))
            })();
            let _ = tx.send(outcome);
        });
        rx.recv()
            .map_err(|_| UnifiedError::internal("Webhook verification worker failed"))?
    })();
    match result {
        Ok(valid) => ctx.add_result(json!({ "valid": valid })),
        Err(error) => ctx.add_result(json!({ "error": error.message })),
    }
}
