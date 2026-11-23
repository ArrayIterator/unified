//! Mail - message building and delivery connectors.
//!
//! Part of the `mail` module split; see `src/modules/mail.rs`.

use unified_core::error::UnifiedError;
use unified_core::error::UnifiedResult;
use base64::Engine;
use lettre::Address;
use lettre::SmtpTransport;
use lettre::Transport;
use lettre::message::Mailbox;
use lettre::message::Message;
use lettre::message::MultiPart;
use lettre::message::header::ContentType;
use lettre::transport::smtp::authentication::Credentials;
use lettre::transport::smtp::client::Tls;
use lettre::transport::smtp::client::TlsParameters;
use serde_json::json;
use std::sync::Arc;

use super::state::MailState;
use super::types::CONNECTOR_LOCAL;
use super::types::CONNECTOR_LOG;
use super::types::EVENT_MAIL_FAILED;
use super::types::EVENT_MAIL_SENT;
use super::types::EVENT_TEMPLATE_RENDER;
use super::types::OutgoingMessage;
use super::types::QueuedMail;
use super::types::SmtpSettings;

/// Parse and validate a single email address (bare address or
/// `Name <address>` form) using lettre's own parser.
pub(crate) fn parse_mailbox(input: &str) -> UnifiedResult<Mailbox> {
    input
        .trim()
        .parse::<Mailbox>()
        .map_err(|_| UnifiedError::bad_request(format!("Invalid email address: {input}")))
}

/// Reject CR or LF in a value that is interpolated into an RFC 822
/// header. A newline there would let the caller inject arbitrary
/// headers (header injection) into the local-sendmail message.
pub(crate) fn validate_header_value(field: &str, value: &str) -> UnifiedResult<()> {
    if value.contains('\r') || value.contains('\n') {
        return Err(UnifiedError::bad_request(format!(
            "{field} must not contain CR or LF characters"
        )));
    }
    Ok(())
}

/// Validate every value the local-sendmail path interpolates raw into
/// RFC 822 headers, plus the recipient address. Called by
/// `deliver_sync` for every connector so no delivery path can send a
/// message carrying a header-injecting value.
pub(crate) fn validate_outgoing(
    settings: &SmtpSettings,
    msg: &OutgoingMessage,
) -> UnifiedResult<()> {
    validate_header_value("to", &msg.to)?;
    parse_mailbox(&msg.to)?;
    validate_header_value("subject", &msg.subject)?;
    validate_header_value("sender_name", &settings.sender_name)?;
    settings.sender_email.parse::<Address>().map_err(|_| {
        UnifiedError::bad_request("Configured sender_email is not a valid email address")
    })?;
    Ok(())
}

/// Build the lettre message for an outgoing mail.
fn build_message(settings: &SmtpSettings, msg: &OutgoingMessage) -> UnifiedResult<Message> {
    let from_address: Address = settings.sender_email.parse().map_err(|_| {
        UnifiedError::bad_request("Configured sender_email is not a valid email address")
    })?;
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
            .multipart(MultiPart::alternative_plain_html(
                text.clone(),
                html.clone(),
            ))
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
        let parameters = TlsParameters::new(settings.host.clone()).map_err(|error| {
            UnifiedError::bad_request(format!("Invalid TLS parameters: {error}"))
        })?;
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
pub(crate) fn sendmail_binary() -> Option<&'static str> {
    for candidate in ["/usr/sbin/sendmail", "/usr/bin/sendmail"] {
        if std::path::Path::new(candidate).exists() {
            return Some(candidate);
        }
    }
    None
}

/// Maximum length of one RFC 2047 encoded-word, including delimiters.
const MAX_ENCODED_WORD_LEN: usize = 75;

/// Overhead of one `=?UTF-8?B?...?=` word: the 10-character prefix
/// `=?UTF-8?B?` plus the 2-character `?=` suffix.
const ENCODED_WORD_OVERHEAD: usize = 12;

/// Encode one chunk of text as a single RFC 2047 encoded-word.
fn encode_word(chunk: &str) -> String {
    let encoded = base64::engine::general_purpose::STANDARD.encode(chunk.as_bytes());
    format!("=?UTF-8?B?{encoded}?=")
}

/// Encode a header value as RFC 2047 encoded-word(s) when it is not ASCII.
///
/// A single encoded-word must be at most 75 characters (RFC 2047 §2),
/// so a long value is split into several words, each encoding at most
/// 45 bytes (60 base64 characters, a 72-character word). Splitting
/// happens on character boundaries so every word decodes to valid UTF-8.
fn encode_header(value: &str) -> String {
    if value.is_ascii() {
        return value.to_string();
    }
    // A single word fits when its base64 payload keeps the whole word
    // within the 75-character limit.
    let single_len = ENCODED_WORD_OVERHEAD + value.len().div_ceil(3) * 4;
    if single_len <= MAX_ENCODED_WORD_LEN {
        return encode_word(value);
    }
    const MAX_CHUNK_BYTES: usize = 45;
    let mut words = Vec::new();
    let mut chunk = String::new();
    let mut chunk_len = 0;
    for ch in value.chars() {
        if chunk_len + ch.len_utf8() > MAX_CHUNK_BYTES && !chunk.is_empty() {
            words.push(encode_word(&chunk));
            chunk.clear();
            chunk_len = 0;
        }
        chunk.push(ch);
        chunk_len += ch.len_utf8();
    }
    if !chunk.is_empty() {
        words.push(encode_word(&chunk));
    }
    words.join(" ")
}

/// Build the RFC 822 text of a message for the local sendmail connector.
///
/// The recipient, subject, sender name and sender address are
/// interpolated raw into headers, so they are validated first: a CR
/// or LF in any of them would inject arbitrary headers into the
/// message handed to `sendmail -t -i`.
fn build_rfc822(settings: &SmtpSettings, msg: &OutgoingMessage) -> UnifiedResult<String> {
    validate_outgoing(settings, msg)?;
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
    Ok(format!(
        "From: {from}\r\nTo: {}\r\nSubject: {}\r\nMIME-Version: 1.0\r\nContent-Type: {content_type}\r\n\r\n{body}",
        msg.to,
        encode_header(&msg.subject),
    ))
}

/// Deliver through the local sendmail binary (`sendmail -t -i`).
fn deliver_via_sendmail(settings: &SmtpSettings, msg: &OutgoingMessage) -> Result<(), String> {
    let binary = sendmail_binary().ok_or_else(|| {
        "No sendmail binary found at /usr/sbin/sendmail or /usr/bin/sendmail".to_string()
    })?;
    let text = build_rfc822(settings, msg).map_err(|error| error.message)?;
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
pub(crate) fn deliver_sync(settings: &SmtpSettings, msg: &OutgoingMessage) -> UnifiedResult<()> {
    // Validate header values up front: the local-sendmail path
    // interpolates them raw into RFC 822 headers.
    validate_outgoing(settings, msg)?;
    match settings.connector.as_str() {
        CONNECTOR_LOG => {
            log::info!(
                "Mail (log-only connector) to {} subject {:?} not delivered",
                msg.to,
                msg.subject
            );
            Ok(())
        }
        CONNECTOR_LOCAL => deliver_via_sendmail(settings, msg).map_err(|error| {
            UnifiedError::new(format!("Local sendmail delivery failed: {error}"), 502)
        }),
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
pub(crate) fn deliver_queued(state: &Arc<MailState>, job: QueuedMail) {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mailbox_parsing() {
        assert!(parse_mailbox("user@example.com").is_ok());
        assert!(parse_mailbox("Jane Doe <jane@example.com>").is_ok());
        assert!(parse_mailbox("not-an-email").is_err());
        assert!(parse_mailbox("").is_err());
    }

    #[test]
    fn message_building() {
        let settings = SmtpSettings {
            sender_email: "sender@example.com".to_string(),
            ..Default::default()
        };
        let text_only = OutgoingMessage {
            to: "user@example.com".to_string(),
            subject: "Hi".to_string(),
            body_text: Some("Hello".to_string()),
            body_html: None,
            template_id: None,
        };
        assert!(build_message(&settings, &text_only).is_ok());
        let html_only = OutgoingMessage {
            to: "user@example.com".to_string(),
            subject: "Hi".to_string(),
            body_text: None,
            body_html: Some("<p>Hello</p>".to_string()),
            template_id: None,
        };
        assert!(build_message(&settings, &html_only).is_ok());
        let no_body = OutgoingMessage {
            to: "user@example.com".to_string(),
            subject: "Hi".to_string(),
            body_text: None,
            body_html: None,
            template_id: None,
        };
        assert!(build_message(&settings, &no_body).is_err());
        let bad_sender = SmtpSettings {
            sender_email: "not-an-email".to_string(),
            ..Default::default()
        };
        assert!(build_message(&bad_sender, &text_only).is_err());
    }

    #[test]
    fn header_encoding_folds_long_subjects() {
        let long_subject = "\u{e9}".repeat(100); // 200 bytes of non-ASCII text
        let encoded = encode_header(&long_subject);
        for word in encoded.split(' ') {
            assert!(word.len() <= 75, "encoded-word too long: {word}");
            assert!(word.starts_with("=?UTF-8?B?"));
            assert!(word.ends_with("?="));
        }
        // Every word decodes to valid UTF-8 and together they round-trip.
        let decoded: String = encoded
            .split(' ')
            .map(|word| {
                let payload = word
                    .strip_prefix("=?UTF-8?B?")
                    .expect("encoded-word prefix")
                    .strip_suffix("?=")
                    .expect("encoded-word suffix");
                let bytes = base64::engine::general_purpose::STANDARD
                    .decode(payload)
                    .expect("valid base64 payload");
                String::from_utf8(bytes).expect("valid UTF-8 chunk")
            })
            .collect();
        assert_eq!(decoded, long_subject);
        // A short non-ASCII subject stays a single word.
        assert_eq!(encode_header("caf\u{e9}").split(' ').count(), 1);
    }

    #[test]
    fn header_value_validation_rejects_crlf() {
        assert!(validate_header_value("subject", "Hello").is_ok());
        assert!(validate_header_value("subject", "Hello\r\nBcc: victim@example.com").is_err());
        assert!(validate_header_value("subject", "Hello\nBcc: victim@example.com").is_err());
        assert!(validate_header_value("subject", "Hello\rBcc: victim@example.com").is_err());
        assert!(validate_header_value("sender_name", "Jane").is_ok());
        assert!(validate_header_value("sender_name", "Jane\r\nX-Injected: 1").is_err());
    }

    fn test_settings() -> SmtpSettings {
        SmtpSettings {
            sender_email: "sender@example.com".to_string(),
            ..Default::default()
        }
    }

    fn test_message() -> OutgoingMessage {
        OutgoingMessage {
            to: "user@example.com".to_string(),
            subject: "Hi".to_string(),
            body_text: Some("Hello".to_string()),
            body_html: None,
            template_id: None,
        }
    }

    #[test]
    fn outgoing_validation_rejects_injection() {
        let settings = test_settings();
        let base = test_message();
        assert!(validate_outgoing(&settings, &base).is_ok());
        let bad_to = OutgoingMessage {
            to: "user@example.com\r\nBcc: victim@example.com".to_string(),
            ..base.clone()
        };
        assert!(validate_outgoing(&settings, &bad_to).is_err());
        let bad_subject = OutgoingMessage {
            subject: "Hi\r\nBcc: victim@example.com".to_string(),
            ..base.clone()
        };
        assert!(validate_outgoing(&settings, &bad_subject).is_err());
        let bad_sender = SmtpSettings {
            sender_name: "Mallory\r\nBcc: victim@example.com".to_string(),
            ..settings.clone()
        };
        assert!(validate_outgoing(&bad_sender, &base).is_err());
        let bad_sender_email = SmtpSettings {
            sender_email: "sender@example.com\r\nBcc: victim@example.com".to_string(),
            ..settings.clone()
        };
        assert!(validate_outgoing(&bad_sender_email, &base).is_err());
    }

    #[test]
    fn rfc822_build_rejects_injection() {
        let settings = test_settings();
        assert!(build_rfc822(&settings, &test_message()).is_ok());
        let bad_subject = OutgoingMessage {
            subject: "Hi\r\nX-Injected: 1".to_string(),
            ..test_message()
        };
        assert!(build_rfc822(&settings, &bad_subject).is_err());
        let bad_sender = SmtpSettings {
            sender_name: "Mallory\nX-Injected: 1".to_string(),
            ..settings.clone()
        };
        assert!(build_rfc822(&bad_sender, &test_message()).is_err());
    }

    #[test]
    fn deliver_sync_rejects_crlf_before_delivery() {
        // The log-only connector would otherwise accept anything, so
        // validation must run before the connector is reached.
        let settings = SmtpSettings {
            connector: CONNECTOR_LOG.to_string(),
            ..test_settings()
        };
        let bad = OutgoingMessage {
            subject: "Hi\r\nBcc: victim@example.com".to_string(),
            ..test_message()
        };
        let error = deliver_sync(&settings, &bad).expect_err("CRLF subject is rejected");
        assert_eq!(error.code, 400);
    }
}
