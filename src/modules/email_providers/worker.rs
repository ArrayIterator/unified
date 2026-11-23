//! email_providers - worker.

use super::{
    adapters::*, credentials::*, events::*, handlers::*, hooks::*, http::*, module::*,
    provider_kinds::*, registry::*, store::*,
};
use serde_json::{Value, json};
use std::collections::HashMap;
use unified_core::error::{UnifiedError, UnifiedResult};

// ---------------------------------------------------------------------------
// Delivery worker (used by the `email.send_requested` hook).
// ---------------------------------------------------------------------------
/// Render a template through the Email Templates module, when the
/// message references one. Returns the rendered HTML and subject.
pub(crate) fn render_template(
    template_id: &str,
    variables: &HashMap<String, Value>,
) -> UnifiedResult<(String, String)> {
    let Some(hooks) = email_hooks() else {
        return Err(UnifiedError::new(
            "Email template service is unavailable (no hook bus)",
            503,
        ));
    };
    let rendered = hooks.emit(
        EVENT_TEMPLATE_RENDER_REQUESTED,
        json!({ "template_id": template_id, "variables": variables }),
    );
    match rendered.results.first() {
        Some(result) if result.get("error").is_none() => {
            let html = result
                .get("rendered_html")
                .and_then(|value| value.as_str())
                .unwrap_or("")
                .to_string();
            let subject = result
                .get("subject")
                .and_then(|value| value.as_str())
                .unwrap_or("")
                .to_string();
            Ok((html, subject))
        }
        Some(result) => {
            let reason = result
                .get("error")
                .and_then(|value| value.as_str())
                .unwrap_or("template render failed");
            Err(UnifiedError::new(
                format!("Template render failed: {reason}"),
                404,
            ))
        }
        None => Err(UnifiedError::new(
            "Email template service did not answer the template.render_requested event",
            503,
        )),
    }
}

pub(crate) async fn run_send_job(job: QueuedSend) {
    let mut message = job.message.clone();
    if let Some(template_id) = &message.template_id {
        match render_template(template_id, &message.variables) {
            Ok((html, subject)) => {
                if message.body_html.is_none() {
                    message.body_html = Some(html);
                }
                if message.subject.trim().is_empty() {
                    message.subject = subject;
                }
            }
            Err(error) => {
                update_log(&job.job_id, "failed", None, Some(error.message.clone()));
                emit_event(
                    EVENT_EMAIL_FAILED,
                    json!({
                        "job_id": job.job_id,
                        "to": message.to,
                        "connection_id": job.connection_id.to_string(),
                        "error": error.message,
                    }),
                );
                return;
            }
        }
    }
    let connection = match get_connection(&job.connection_id) {
        Some(connection) => connection,
        None => {
            update_log(
                &job.job_id,
                "failed",
                None,
                Some("Email provider connection was deleted before delivery".to_string()),
            );
            return;
        }
    };
    // The connection may have been disabled after the send was
    // queued; re-check here, like the HTTP send handler does, and
    // never deliver through a disabled connection.
    if !connection.enabled {
        let reason = format!(
            "Email provider connection \"{}\" is disabled",
            connection.name
        );
        update_log(&job.job_id, "failed", None, Some(reason.clone()));
        emit_event(
            EVENT_EMAIL_FAILED,
            json!({
                "job_id": job.job_id,
                "to": message.to,
                "connection_id": job.connection_id.to_string(),
                "error": reason,
            }),
        );
        return;
    }
    let adapter = match email_provider_registry().build(&connection) {
        Ok(adapter) => adapter,
        Err(error) => {
            update_log(&job.job_id, "failed", None, Some(error.message.clone()));
            emit_event(
                EVENT_EMAIL_FAILED,
                json!({
                    "job_id": job.job_id,
                    "to": message.to,
                    "connection_id": job.connection_id.to_string(),
                    "error": error.message,
                }),
            );
            return;
        }
    };
    match adapter.send(&message).await {
        Ok(result) => {
            update_log(
                &job.job_id,
                if result.test_mode { "test" } else { "sent" },
                result.provider_message_id.clone(),
                None,
            );
            emit_event(
                EVENT_EMAIL_SENT,
                json!({
                    "job_id": job.job_id,
                    "to": message.to,
                    "connection_id": job.connection_id.to_string(),
                    "provider": connection.kind.as_str(),
                    "provider_message_id": result.provider_message_id,
                    "test_mode": result.test_mode,
                }),
            );
        }
        Err(error) => {
            update_log(&job.job_id, "failed", None, Some(error.message.clone()));
            emit_event(
                EVENT_EMAIL_FAILED,
                json!({
                    "job_id": job.job_id,
                    "to": message.to,
                    "connection_id": job.connection_id.to_string(),
                    "provider": connection.kind.as_str(),
                    "error": error.message,
                    "code": error.code,
                }),
            );
        }
    }
}

/// Start the background worker for one queued send. Hook handlers are
/// synchronous and may run outside a runtime, so spawn on the current
/// runtime when there is one and fall back to a dedicated thread with
/// its own runtime otherwise.
pub(crate) fn spawn_send_worker(job: QueuedSend) {
    if tokio::runtime::Handle::try_current().is_ok() {
        tokio::spawn(async move {
            run_send_job(job).await;
        });
    } else {
        std::thread::spawn(move || {
            let Ok(runtime) = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            else {
                return;
            };
            runtime.block_on(run_send_job(job));
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use uuid::Uuid;

    fn disabled_connection() -> EmailConnection {
        let now = Utc::now();
        EmailConnection {
            id: Uuid::now_v7(),
            name: "disabled-conn".to_string(),
            kind: EmailProviderKind::Smtp,
            domain: None,
            region: None,
            sender_email: Some("sender@example.com".to_string()),
            sender_name: None,
            webhook_url: None,
            smtp_host: None,
            smtp_port: None,
            smtp_tls: None,
            smtp_username: None,
            credentials: EmailCredentials::default(),
            test_mode: false,
            enabled: false,
            last_tested_at: None,
            last_status: None,
            created_at: now,
            updated_at: now,
        }
    }

    #[tokio::test]
    async fn run_send_job_aborts_when_the_connection_was_disabled_after_queueing() {
        let connection = disabled_connection();
        connections_store().insert(connection.id, connection.clone());
        let job_id = Uuid::now_v7().to_string();
        append_log(SendLogEntry {
            id: job_id.clone(),
            connection_id: connection.id.to_string(),
            provider: connection.kind.as_str().to_string(),
            to: "user@example.com".to_string(),
            subject: "queued subject".to_string(),
            status: "queued".to_string(),
            provider_message_id: None,
            test_mode: false,
            error: None,
            created_at: Utc::now().to_rfc3339(),
        });
        let message = EmailMessage {
            to: "user@example.com".to_string(),
            from: None,
            from_name: None,
            subject: "queued subject".to_string(),
            body_text: Some("hello".to_string()),
            body_html: None,
            reply_to: None,
            template_id: None,
            variables: HashMap::new(),
            metadata: HashMap::new(),
            test_mode: false,
        };
        run_send_job(QueuedSend {
            job_id: job_id.clone(),
            connection_id: connection.id,
            message,
        })
        .await;
        let entry = send_log_store()
            .read()
            .expect("send log lock poisoned")
            .iter()
            .find(|entry| entry.id == job_id)
            .expect("queued log entry")
            .clone();
        assert_eq!(entry.status.as_str(), "failed");
        assert!(entry.error.as_deref().unwrap_or("").contains("disabled"));
    }
}
