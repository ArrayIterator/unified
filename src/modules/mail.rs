// Mail Manager module.
//
// Core mail support for the whole application: SMTP settings, sending,
// a send log, and three delivery connectors (SMTP, local sendmail, and a
// log-only debug connector). Other modules never send mail directly;
// they emit the `mail.send_requested` hook event and this module takes
// responsibility for delivery.
//
// Routes (all under `/api/v1/mail`):
//   GET    /settings              - public SMTP settings (password never returned)
//   PUT    /settings              - store SMTP settings (password is write-only)
//   POST   /send                  - send one message (optionally rendered from a template)
//   POST   /test                  - send a test message, report the real SMTP result
//   GET    /log                   - paginated send log (filter by status, search)
//   GET    /connectors            - available delivery connectors
//
// Hook contracts:
//   Listens to `mail.send_requested` (from Billing, Tickets, Auth,
//   Notifications, Email Templates). The message is queued in memory and
//   the `scheduler.enqueue` event is emitted so the Scheduler module can
//   own execution of the job. When no hook bus is available (for example
//   the module was registered without `init`), the message is delivered
//   directly in a worker thread instead and the outcome is logged.
//   The Scheduler (or any other module) can trigger delivery of a queued
//   job by emitting `mail.deliver` with `{ "job_id": ... }`.
//   Emits `mail.sent` / `mail.failed` after every delivery attempt.
//   When a `template_id` is supplied, the template is rendered through
//   the `template.render_requested` event owned by the Email Templates
//   module; when no handler answers, sending fails with 503.

use crate::core::context::AppContext;
use crate::core::error::{UnifiedError, UnifiedResult};
use crate::core::hooks::HookBus;
use crate::core::json as api_json;
use crate::core::modules::{Module, ModuleRegistry, RouteInfo};
use crate::core::pagination::{PageResult, PaginationParams};
use crate::modules::ai;
use actix_web::{web, HttpRequest, HttpResponse};
use base64::Engine;
use chrono::Utc;
use lettre::message::header::ContentType;
use lettre::message::{Mailbox, Message, MultiPart};
use lettre::transport::smtp::authentication::Credentials;
use lettre::transport::smtp::client::{Tls, TlsParameters};
use lettre::{Address, SmtpTransport, Transport};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, RwLock};
use uuid::Uuid;

pub const EVENT_MAIL_SENT: &str = "mail.sent";
pub const EVENT_MAIL_FAILED: &str = "mail.failed";
pub const EVENT_MAIL_SEND_REQUESTED: &str = "mail.send_requested";
pub const EVENT_MAIL_DELIVER: &str = "mail.deliver";
pub const EVENT_SCHEDULER_ENQUEUE: &str = "scheduler.enqueue";
pub const EVENT_TEMPLATE_RENDER: &str = "template.render_requested";

/// Delivery connector ids.
pub const CONNECTOR_SMTP: &str = "smtp";
pub const CONNECTOR_LOCAL: &str = "local";
pub const CONNECTOR_LOG: &str = "log";

/// SMTP settings as stored. The password is write-only: it is accepted on
/// input but never serialized into any response.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SmtpSettings {
    pub host: String,
    pub port: u16,
    pub tls: bool,
    pub username: String,
    #[serde(skip_serializing)]
    pub password: String,
    pub sender_name: String,
    pub sender_email: String,
    /// Delivery connector: `smtp` (default), `local` (sendmail binary),
    /// or `log` (debug only, records the message without delivering it).
    pub connector: String,
    pub updated_at: String,
}

impl Default for SmtpSettings {
    fn default() -> Self {
        Self {
            host: String::new(),
            port: 587,
            tls: true,
            username: String::new(),
            password: String::new(),
            sender_name: String::new(),
            sender_email: String::new(),
            connector: CONNECTOR_SMTP.to_string(),
            updated_at: Utc::now().to_rfc3339(),
        }
    }
}

#[derive(Debug, Deserialize)]
struct SmtpSettingsInput {
    host: String,
    port: Option<u16>,
    tls: Option<bool>,
    username: Option<String>,
    password: Option<String>,
    sender_name: Option<String>,
    sender_email: String,
    connector: Option<String>,
}

/// One entry of the send log. `status` is `queued`, `sent`, or `failed`.
#[derive(Debug, Clone, Serialize)]
pub struct MailLogEntry {
    pub id: String,
    pub to: String,
    pub subject: String,
    pub status: String,
    pub connector: String,
    pub template_id: Option<String>,
    pub error: Option<String>,
    pub created_at: String,
}

/// A message waiting for delivery (queued through the hook bus).
#[derive(Debug, Clone)]
struct QueuedMail {
    job_id: String,
    to: String,
    subject: String,
    body_text: Option<String>,
    body_html: Option<String>,
    template_id: Option<String>,
    variables: HashMap<String, Value>,
}

/// A message ready to be handed to a delivery connector.
#[derive(Debug, Clone)]
struct OutgoingMessage {
    to: String,
    subject: String,
    body_text: Option<String>,
    body_html: Option<String>,
    template_id: Option<String>,
}

pub struct MailState {
    settings: RwLock<Option<SmtpSettings>>,
    log: RwLock<Vec<MailLogEntry>>,
    queue: RwLock<VecDeque<QueuedMail>>,
    hooks: RwLock<Option<Arc<HookBus>>>,
    debug: AtomicBool,
}

impl MailState {
    fn new() -> Self {
        Self {
            settings: RwLock::new(None),
            log: RwLock::new(Vec::new()),
            queue: RwLock::new(VecDeque::new()),
            hooks: RwLock::new(None),
            debug: AtomicBool::new(false),
        }
    }

    fn set_hooks(&self, hooks: Arc<HookBus>) {
        *self.hooks.write().expect("mail hooks lock poisoned") = Some(hooks);
    }

    fn hooks(&self) -> Option<Arc<HookBus>> {
        self.hooks.read().expect("mail hooks lock poisoned").clone()
    }

    fn is_debug(&self) -> bool {
        self.debug.load(Ordering::SeqCst)
    }

    fn settings_snapshot(&self) -> Option<SmtpSettings> {
        self.settings
            .read()
            .expect("mail settings lock poisoned")
            .clone()
    }

    fn append_log(&self, entry: MailLogEntry) {
        self.log
            .write()
            .expect("mail log lock poisoned")
            .push(entry);
    }

    fn update_log_status(&self, id: &str, status: &str, error: Option<String>) {
        let mut log = self.log.write().expect("mail log lock poisoned");
        if let Some(entry) = log.iter_mut().find(|entry| entry.id == id) {
            entry.status = status.to_string();
            entry.error = error;
        }
    }

    fn enqueue(&self, job: QueuedMail) {
        self.queue
            .write()
            .expect("mail queue lock poisoned")
            .push_back(job);
    }

    fn take_queued(&self, job_id: &str) -> Option<QueuedMail> {
        let mut queue = self.queue.write().expect("mail queue lock poisoned");
        let position = queue.iter().position(|job| job.job_id == job_id)?;
        queue.remove(position)
    }
}

/// Parse and validate a single email address (bare address or
/// `Name <address>` form) using lettre's own parser.
fn parse_mailbox(input: &str) -> UnifiedResult<Mailbox> {
    input
        .trim()
        .parse::<Mailbox>()
        .map_err(|_| UnifiedError::bad_request(format!("Invalid email address: {input}")))
}

/// Build the lettre message for an outgoing mail.
fn build_message(settings: &SmtpSettings, msg: &OutgoingMessage) -> UnifiedResult<Message> {
    let from_address: Address = settings
        .sender_email
        .parse()
        .map_err(|_| UnifiedError::bad_request("Configured sender_email is not a valid email address"))?;
    let from = Mailbox::new(
        if settings.sender_name.trim().is_empty() {
            None
        } else {
            Some(settings.sender_name.clone())
        },
        from_address,
    );
    let to = parse_mailbox(&msg.to)?;
    match (&msg.body_text, &msg.body_html) {
        (Some(text), Some(html)) => Message::builder()
            .from(from)
            .to(to)
            .subject(msg.subject.as_str())
            .multipart(MultiPart::alternative_plain_html(text.clone(), html.clone()))
            .map_err(|error| UnifiedError::bad_request(format!("Cannot build message: {error}"))),
        (Some(text), None) => Message::builder()
            .from(from)
            .to(to)
            .subject(msg.subject.as_str())
            .body(text.clone())
            .map_err(|error| UnifiedError::bad_request(format!("Cannot build message: {error}"))),
        (None, Some(html)) => Message::builder()
            .from(from)
            .to(to)
            .subject(msg.subject.as_str())
            .header(ContentType::TEXT_HTML)
            .body(html.clone())
            .map_err(|error| UnifiedError::bad_request(format!("Cannot build message: {error}"))),
        (None, None) => Err(UnifiedError::bad_request(
            "Provide body_text or body_html (or a template_id that renders one)",
        )),
    }
}

/// Build a blocking SMTP transport from the stored settings.
/// Port 465 uses implicit TLS, any other TLS port uses STARTTLS.
fn build_smtp_transport(settings: &SmtpSettings) -> UnifiedResult<SmtpTransport> {
    let mut builder = SmtpTransport::builder_dangerous(settings.host.as_str());
    builder = builder.port(settings.port);
    if !settings.username.trim().is_empty() {
        builder = builder.credentials(Credentials::new(
            settings.username.clone(),
            settings.password.clone(),
        ));
    }
    let tls = if settings.tls {
        let parameters = TlsParameters::new(settings.host.as_str())
            .map_err(|error| UnifiedError::bad_request(format!("Invalid TLS parameters: {error}")))?;
        if settings.port == 465 {
            Tls::Wrapper(parameters)
        } else {
            Tls::Required(parameters)
        }
    } else {
        Tls::None
    };
    Ok(builder.tls(tls).build())
}

/// Locate a sendmail binary for the local connector.
fn sendmail_binary() -> Option<&'static str> {
    for candidate in ["/usr/sbin/sendmail", "/usr/bin/sendmail"] {
        if std::path::Path::new(candidate).exists() {
            return Some(candidate);
        }
    }
    None
}

/// Encode a header value as an RFC 2047 encoded-word when it is not ASCII.
fn encode_header(value: &str) -> String {
    if value.is_ascii() {
        value.to_string()
    } else {
        let encoded = base64::engine::general_purpose::STANDARD.encode(value.as_bytes());
        format!("=?UTF-8?B?{encoded}?=")
    }
}

/// Build the RFC 822 text of a message for the local sendmail connector.
fn build_rfc822(settings: &SmtpSettings, msg: &OutgoingMessage) -> String {
    let from = if settings.sender_name.trim().is_empty() {
        settings.sender_email.clone()
    } else {
        format!("{} <{}>", settings.sender_name, settings.sender_email)
    };
    let (content_type, body) = match (&msg.body_text, &msg.body_html) {
        (_, Some(html)) => ("text/html; charset=UTF-8", html.clone()),
        (Some(text), None) => ("text/plain; charset=UTF-8", text.clone()),
        (None, None) => ("text/plain; charset=UTF-8", String::new()),
    };
    format!(
        "From: {from}\r\nTo: {}\r\nSubject: {}\r\nMIME-Version: 1.0\r\nContent-Type: {content_type}\r\n\r\n{body}",
        msg.to,
        encode_header(&msg.subject),
    )
}

/// Deliver through the local sendmail binary (`sendmail -t -i`).
fn deliver_via_sendmail(settings: &SmtpSettings, msg: &OutgoingMessage) -> Result<(), String> {
    let binary = sendmail_binary()
        .ok_or_else(|| "No sendmail binary found at /usr/sbin/sendmail or /usr/bin/sendmail".to_string())?;
    let text = build_rfc822(settings, msg);
    let mut child = std::process::Command::new(binary)
        .args(["-t", "-i"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|error| format!("Cannot start sendmail ({binary}): {error}"))?;
    if let Some(mut stdin) = child.stdin.take() {
        use std::io::Write;
        stdin
            .write_all(text.as_bytes())
            .map_err(|error| format!("Cannot write message to sendmail: {error}"))?;
    }
    let output = child
        .wait_with_output()
        .map_err(|error| format!("Cannot wait for sendmail: {error}"))?;
    if output.status.success() {
        Ok(())
    } else {
        Err(format!(
            "sendmail exited with status {}: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        ))
    }
}

/// Deliver one message through the configured connector. This is blocking
/// I/O by design: HTTP handlers run it inside `spawn_blocking`, and the
/// hook worker thread calls it directly.
fn deliver_sync(settings: &SmtpSettings, msg: &OutgoingMessage) -> UnifiedResult<()> {
    match settings.connector.as_str() {
        CONNECTOR_LOG => {
            log::info!(
                "Mail (log-only connector) to {} subject {:?} not delivered",
                msg.to,
                msg.subject
            );
            Ok(())
        }
        CONNECTOR_LOCAL => deliver_via_sendmail(settings, msg)
            .map_err(|error| UnifiedError::new(format!("Local sendmail delivery failed: {error}"), 502)),
        _ => {
            let message = build_message(settings, msg)?;
            let transport = build_smtp_transport(settings)?;
            transport
                .send(&message)
                .map_err(|error| UnifiedError::new(format!("SMTP send failed: {error}"), 502))?;
            Ok(())
        }
    }
}

/// Deliver a queued job: render its template when needed, send it, and
/// update the log entry plus the `mail.sent` / `mail.failed` events.
fn deliver_queued(state: &Arc<MailState>, job: QueuedMail) {
    let log_id = job.job_id.clone();
    let mark_failed = |state: &Arc<MailState>, reason: String| {
        state.update_log_status(&log_id, "failed", Some(reason.clone()));
        if let Some(hooks) = state.hooks() {
            hooks.emit(
                EVENT_MAIL_FAILED,
                json!({ "job_id": log_id.clone(), "to": job.to.clone(), "error": reason }),
            );
        }
    };

    let mut msg = OutgoingMessage {
        to: job.to.clone(),
        subject: job.subject.clone(),
        body_text: job.body_text.clone(),
        body_html: job.body_html.clone(),
        template_id: job.template_id.clone(),
    };
    if let Some(template_id) = &job.template_id {
        match state.hooks() {
            Some(hooks) => {
                let rendered = hooks.emit(
                    EVENT_TEMPLATE_RENDER,
                    json!({ "template_id": template_id, "variables": job.variables }),
                );
                match rendered.results.first() {
                    Some(result) if result.get("error").is_none() => {
                        if let Some(html) = result.get("rendered_html").and_then(|v| v.as_str()) {
                            msg.body_html = Some(html.to_string());
                        }
                        if msg.subject.is_empty() {
                            if let Some(subject) = result.get("subject").and_then(|v| v.as_str()) {
                                msg.subject = subject.to_string();
                            }
                        }
                    }
                    Some(result) => {
                        let reason = result
                            .get("error")
                            .and_then(|v| v.as_str())
                            .unwrap_or("template render failed")
                            .to_string();
                        mark_failed(state, format!("Template render failed: {reason}"));
                        return;
                    }
                    None => {
                        mark_failed(
                            state,
                            "Email template service did not answer the template.render_requested event"
                                .to_string(),
                        );
                        return;
                    }
                }
            }
            None => {
                mark_failed(
                    state,
                    "Cannot render template without the hook bus".to_string(),
                );
                return;
            }
        }
    }

    let settings = match state.settings_snapshot() {
        Some(settings) => settings,
        None => {
            mark_failed(state, "SMTP settings are not configured".to_string());
            return;
        }
    };
    match deliver_sync(&settings, &msg) {
        Ok(()) => {
            state.update_log_status(&log_id, "sent", None);
            if let Some(hooks) = state.hooks() {
                hooks.emit(
                    EVENT_MAIL_SENT,
                    json!({ "job_id": log_id, "to": msg.to, "subject": msg.subject }),
                );
            }
        }
        Err(error) => mark_failed(state, error.message.clone()),
    }
}

// ---------------------------------------------------------------------------
// HTTP handlers
// ---------------------------------------------------------------------------

async fn get_settings(state: web::Data<Arc<MailState>>) -> HttpResponse {
    match state.settings_snapshot() {
        Some(settings) => api_json::success_response(
            json!({ "configured": true, "settings": settings }),
            200,
        ),
        None => api_json::success_response(json!({ "configured": false, "settings": null }), 200),
    }
}

async fn update_settings(
    state: web::Data<Arc<MailState>>,
    body: web::Json<SmtpSettingsInput>,
) -> HttpResponse {
    let debug = state.is_debug();
    let input = body.into_inner();
    if input.host.trim().is_empty() {
        return api_json::error_response(
            &UnifiedError::bad_request("SMTP host must not be empty"),
            debug,
        );
    }
    let port = input.port.unwrap_or(if input.tls.unwrap_or(true) { 587 } else { 25 });
    if port == 0 {
        return api_json::error_response(
            &UnifiedError::bad_request("SMTP port must be between 1 and 65535"),
            debug,
        );
    }
    if let Err(error) = input.sender_email.parse::<Address>() {
        return api_json::error_response(
            &UnifiedError::bad_request(format!("sender_email is not a valid email address: {error}")),
            debug,
        );
    }
    let connector = input.connector.clone().unwrap_or_else(|| CONNECTOR_SMTP.to_string());
    if !matches!(connector.as_str(), CONNECTOR_SMTP | CONNECTOR_LOCAL | CONNECTOR_LOG) {
        return api_json::error_response(
            &UnifiedError::bad_request(format!(
                "connector must be one of: {CONNECTOR_SMTP}, {CONNECTOR_LOCAL}, {CONNECTOR_LOG}"
            )),
            debug,
        );
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

async fn send_mail(
    state: web::Data<Arc<MailState>>,
    body: web::Json<SendMailInput>,
) -> HttpResponse {
    let state: Arc<MailState> = state.into_inner();
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
                            &UnifiedError::new(
                                format!("Template render failed: {reason}"),
                                404,
                            ),
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
                    &UnifiedError::new(
                        "Email template service is unavailable (no hook bus)",
                        503,
                    ),
                    debug,
                );
            }
        }
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

async fn test_mail(
    state: web::Data<Arc<MailState>>,
    body: web::Json<TestMailInput>,
) -> HttpResponse {
    let state: Arc<MailState> = state.into_inner();
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
        body_text: Some(
            "This is a test message sent from the Unified Mail Manager.".to_string(),
        ),
        body_html: Some(
            "<p>This is a test message sent from the Unified Mail Manager.</p>".to_string(),
        ),
        template_id: None,
    };
    let outcome = tokio::task::spawn_blocking(move || deliver_sync(&settings, &msg)).await;
    let (sent, error) = match outcome {
        Ok(Ok(())) => (true, None),
        Ok(Err(error)) => (false, Some(error.message)),
        Err(join_error) => (false, Some(format!("Mail delivery task failed: {join_error}"))),
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

async fn list_log(
    state: web::Data<Arc<MailState>>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let pagination = query.normalized();
    let mut entries = state
        .log
        .read()
        .expect("mail log lock poisoned")
        .clone();
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
    api_json::success_response(PageResult::new(page_items, total, &pagination).to_json(), 200)
}

async fn list_connectors(state: web::Data<Arc<MailState>>) -> HttpResponse {
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

// ---------------------------------------------------------------------------
// AI capabilities (AI Capabilities Contract, see `crate::modules::ai`).
//
// Declared here, registered once in `init`, grounded through the
// `mail.ai.draft_requested` hook, and run through the AI module's
// gated runner. Drafts are returned inline and are never sent by
// this module: sending stays on the existing human endpoints.
// ---------------------------------------------------------------------------

/// AI capabilities this module exposes through the AI module's
/// contract. The capability is non-PII: inputs are redacted before
/// they reach the model.
pub const AI_CAPABILITIES: &[ai::AiCapability] = &[
    ai::AiCapability {
        id: "draft",
        module_id: "mail",
        name: "Draft email",
        description: "Draft an email from a subject and bullet points (a string, or a JSON array of points). The output is returned inline; it is never sent by this module.",
        input_fields: &["subject", "bullets"],
        allows_pii: false,
        use_case: "email",
    },
];

/// Run one registered mail AI capability:
/// `POST /api/v1/mail/ai/{capability}` with body
/// `{"input": {...}}`. Requires `ai:use`; delegates to the AI
/// module's gated runner.
async fn ai_capability(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<ai::CapabilityCallInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let capability = path.into_inner();
    let caller = match ai::caller(&req) {
        Ok(caller) => caller,
        Err(error) => return api_json::error_response(&error, debug),
    };
    if let Err(error) = ai::require_permission(&ctx, &caller.role, ai::PERM_AI_USE) {
        return api_json::error_response(&error, debug);
    }
    let company_id = ai::resolve_capability_company(&ctx, &req, &caller);
    match ai::run_ai_capability(
        &ctx,
        &caller,
        company_id,
        "mail",
        &capability,
        body.into_inner().input,
    )
    .await
    {
        Ok(data) => api_json::success_response(data, 200),
        Err(error) => api_json::error_response(&error, debug),
    }
}

// ---------------------------------------------------------------------------
// Module
// ---------------------------------------------------------------------------

pub struct MailModule {
    state: Arc<MailState>,
}

impl MailModule {
    pub fn new() -> Self {
        Self {
            state: Arc::new(MailState::new()),
        }
    }
}

pub fn register(registry: &ModuleRegistry) {
    registry.register(Arc::new(MailModule::new()));
}

impl Module for MailModule {
    fn id(&self) -> &'static str {
        "mail"
    }

    fn name(&self) -> &'static str {
        "Mail Manager"
    }

    fn description(&self) -> &'static str {
        "SMTP settings, mail sending, send log, and delivery connectors (SMTP, local sendmail, log-only debug)."
    }

    fn dependencies(&self) -> Vec<&'static str> {
        vec!["scheduler"]
    }

    fn init(&self, ctx: &AppContext) -> UnifiedResult<()> {
        self.state.debug.store(ctx.is_debug(), Ordering::SeqCst);
        self.state.set_hooks(ctx.hooks.clone());
        ai::register_ai_capabilities("mail", AI_CAPABILITIES);
        Ok(())
    }

    fn register_hooks(&self, bus: &HookBus) {
        // Incoming send requests from Billing, Tickets, Auth, Notifications
        // and Email Templates.
        let state = self.state.clone();
        bus.register(
            EVENT_MAIL_SEND_REQUESTED,
            "mail",
            Arc::new(move |ctx| {
                let payload = ctx.data.clone();
                let to = payload
                    .get("to")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let subject = payload
                    .get("subject")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let body_text = payload
                    .get("body_text")
                    .and_then(|v| v.as_str())
                    .map(|v| v.to_string());
                let body_html = payload
                    .get("body_html")
                    .and_then(|v| v.as_str())
                    .map(|v| v.to_string());
                let template_id = payload
                    .get("template_id")
                    .and_then(|v| v.as_str())
                    .map(|v| v.to_string());
                let variables: HashMap<String, Value> = payload
                    .get("variables")
                    .and_then(|v| serde_json::from_value(v.clone()).ok())
                    .unwrap_or_default();
                if to.trim().is_empty()
                    || (subject.trim().is_empty() && template_id.is_none())
                {
                    ctx.add_result(json!({
                        "error": "mail.send_requested requires 'to' and a subject or template_id"
                    }));
                    return;
                }
                let job_id = Uuid::now_v7().to_string();
                let job = QueuedMail {
                    job_id: job_id.clone(),
                    to: to.clone(),
                    subject: subject.clone(),
                    body_text,
                    body_html,
                    template_id: template_id.clone(),
                    variables,
                };
                let connector = state
                    .settings_snapshot()
                    .map(|settings| settings.connector)
                    .unwrap_or_else(|| CONNECTOR_SMTP.to_string());
                state.append_log(MailLogEntry {
                    id: job_id.clone(),
                    to: to.clone(),
                    subject: subject.clone(),
                    status: "queued".to_string(),
                    connector,
                    template_id,
                    error: None,
                    created_at: Utc::now().to_rfc3339(),
                });
                state.enqueue(job.clone());

                if let Some(hooks) = state.hooks() {
                    // Hand execution to the Scheduler module; delivery is
                    // the scheduler's responsibility from here. The
                    // scheduler calls the job back through `mail.deliver`.
                    hooks.emit(
                        EVENT_SCHEDULER_ENQUEUE,
                        json!({
                            "job_id": job_id,
                            "queue": "mail",
                            "event": EVENT_MAIL_SEND_REQUESTED,
                            "payload": payload,
                        }),
                    );
                    ctx.add_result(json!({
                        "queued": true,
                        "job_id": job_id,
                        "via": "scheduler"
                    }));
                } else {
                    // No hook bus: deliver directly in a worker thread and
                    // log the real outcome.
                    let worker_state = state.clone();
                    std::thread::spawn(move || deliver_queued(&worker_state, job));
                    ctx.add_result(json!({
                        "queued": true,
                        "job_id": job_id,
                        "via": "direct"
                    }));
                }
            }),
        );

        // Delivery callback used by the Scheduler module (or any other
        // module) to execute a queued job.
        let state = self.state.clone();
        bus.register(
            EVENT_MAIL_DELIVER,
            "mail",
            Arc::new(move |ctx| {
                let job_id = ctx
                    .get("job_id")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                match state.take_queued(&job_id) {
                    Some(job) => {
                        let worker_state = state.clone();
                        std::thread::spawn(move || deliver_queued(&worker_state, job));
                        ctx.add_result(json!({ "delivering": true, "job_id": job_id }));
                    }
                    None => {
                        ctx.add_result(json!({
                            "error": format!("Queued mail job not found: {job_id}")
                        }));
                    }
                }
            }),
        );

        // Ground the `draft` AI capability. This module holds no
        // groundable store: the subject and bullet points come from
        // the caller, and the send log holds recipient addresses, so
        // it is never offered as grounding. The empty result keeps
        // the hook contract explicit and `grounded` false. Tenant
        // scoping is vacuous here: no tenant data is read at all.
        bus.register(
            "mail.ai.draft_requested",
            "mail",
            Arc::new(move |ctx| {
                ctx.add_result(json!({}));
            }),
        );
    }

    fn route_info(&self) -> Vec<RouteInfo> {
        vec![
            RouteInfo { method: "GET", path: "/api/v1/mail/settings", owner: "mail", description: "Get settings" },
            RouteInfo { method: "PUT", path: "/api/v1/mail/settings", owner: "mail", description: "Update settings" },
            RouteInfo { method: "POST", path: "/api/v1/mail/send", owner: "mail", description: "Send mail" },
            RouteInfo { method: "POST", path: "/api/v1/mail/test", owner: "mail", description: "Test mail" },
            RouteInfo { method: "GET", path: "/api/v1/mail/log", owner: "mail", description: "List log" },
            RouteInfo { method: "GET", path: "/api/v1/mail/connectors", owner: "mail", description: "List connectors" },
            RouteInfo { method: "POST", path: "/api/v1/mail/ai/{capability}", owner: "mail", description: "Run one registered mail AI capability (draft)" },
        ]
    }

    fn configure_routes(&self, cfg: &mut actix_web::web::ServiceConfig) {
        let state = self.state.clone();
        cfg.service(
            web::scope("/api/v1/mail")
                .app_data(web::Data::new(state))
                .route("/settings", web::get().to(get_settings))
                .route("/settings", web::put().to(update_settings))
                .route("/send", web::post().to(send_mail))
                .route("/test", web::post().to(test_mail))
                .route("/log", web::get().to(list_log))
                .route("/connectors", web::get().to(list_connectors))
                .route("/ai/{capability}", web::post().to(ai_capability)),
        );
    }
}
