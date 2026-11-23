//! email_providers - adapters.

use super::{
    credentials::*, crypto::*, deliverability::*, events::*, handlers::*, hooks::*, http::*,
    module::*, oauth::*, provider_kinds::*, registry::*, store::*, worker::*,
};
use async_trait::async_trait;
use base64::Engine;
use chrono::{DateTime, Utc};
use hmac::{Hmac, Mac};
use lettre::message::header::{ContentType, HeaderName, HeaderValue};
use lettre::message::{Mailbox, Message as LettreMessage, MultiPart};
use lettre::transport::smtp::authentication::{Credentials as SmtpCredentials, Mechanism};
use lettre::transport::smtp::client::{Tls, TlsParameters};
use lettre::{AsyncSmtpTransport, AsyncTransport, Tokio1Executor};
use reqwest::header::{AUTHORIZATION, CONTENT_TYPE, HeaderMap};
use serde_json::{Value, json};
use sha1::Sha1;
use std::collections::HashMap;
use std::sync::Mutex;
use std::time::Duration;
use unified_core::error::{UnifiedError, UnifiedResult};
use url::Url;

// ---------------------------------------------------------------------------
// Adapters.
// ---------------------------------------------------------------------------
/// Shared base: every HTTP adapter holds its connection and client.
macro_rules! http_adapter {
    ($name:ident) => {
        pub struct $name {
            connection: EmailConnection,
            client: reqwest::Client,
        }

        impl $name {
            pub(crate) fn new(connection: &EmailConnection) -> UnifiedResult<Self> {
                Ok(Self {
                    connection: connection.clone(),
                    client: build_client()?,
                })
            }
        }
    };
}

// --- Mailchimp Transactional (Mandrill) ------------------------------------

http_adapter!(MailchimpAdapter);

#[async_trait]
impl EmailProvider for MailchimpAdapter {
    fn kind(&self) -> EmailProviderKind {
        EmailProviderKind::Mailchimp
    }

    fn connection(&self) -> &EmailConnection {
        &self.connection
    }

    async fn test_connection(&self) -> UnifiedResult<ConnectionTestResult> {
        let started = std::time::Instant::now();
        let api_key = require_api_key(&self.connection)?;
        let url = Url::parse("https://mandrillapp.com/api/1.0/users/ping.json")
            .map_err(|_| UnifiedError::internal("Invalid Mandrill API URL"))?;
        check_allowlist(&url, self.kind())?;
        let body = send_text(
            &self.client,
            self.client
                .post(url)
                .header(CONTENT_TYPE, "application/json")
                .json(&json!({ "key": api_key })),
            "mailchimp",
        )
        .await?;
        let reachable = body.trim().contains("PONG");
        Ok(ConnectionTestResult {
            kind: self.kind().as_str().to_string(),
            reachable,
            latency_ms: started.elapsed().as_millis() as u64,
            account: None,
            detail: if reachable {
                "Mandrill answered the ping with PONG".to_string()
            } else {
                format!("Mandrill ping returned an unexpected body: {body}")
            },
        })
    }

    async fn send(&self, message: &EmailMessage) -> UnifiedResult<SendResult> {
        validate_email_address(&message.to)?;
        if message.test_mode && !self.kind().has_native_sandbox() {
            return Ok(simulated_result("mailchimp"));
        }
        let api_key = require_api_key(&self.connection)?;
        let (from, from_name) = message.sender(&self.connection);
        if from.is_empty() {
            return Err(UnifiedError::bad_request(
                "A sender email is required (set sender_email on the connection or from on the message)",
            ));
        }
        let mut mandrill_message = json!({
            "html": message.body_html,
            "text": message.body_text,
            "subject": message.subject,
            "from_email": from,
            "from_name": from_name,
            "to": [{ "email": message.to, "type": "to" }],
            "track_opens": true,
            "track_clicks": true,
            "metadata": message.metadata,
        });
        // Deliverability headers (task 354): Mandrill takes custom
        // headers in the message "headers" object; the
        // deliverability values win on collision.
        let deliverability = deliverability_headers_object(message, &self.connection);
        let mut headers = Value::Null;
        if let Some(reply_to) = &message.reply_to {
            headers = merge_headers_object(&headers, &json!({ "Reply-To": reply_to }));
        }
        headers = merge_headers_object(&headers, &deliverability);
        if headers.as_object().is_some_and(|object| !object.is_empty()) {
            mandrill_message["headers"] = headers;
        }
        let url = Url::parse("https://mandrillapp.com/api/1.0/messages/send.json")
            .map_err(|_| UnifiedError::internal("Invalid Mandrill API URL"))?;
        check_allowlist(&url, self.kind())?;
        let body = send_json(
            &self.client,
            self.client
                .post(url)
                .header(CONTENT_TYPE, "application/json")
                .json(&json!({ "key": api_key, "message": mandrill_message })),
            "mailchimp",
        )
        .await?;
        let first = body.as_array().and_then(|items| items.first()).cloned();
        let provider_message_id = first
            .as_ref()
            .and_then(|item| item.get("_id"))
            .and_then(|value| value.as_str())
            .map(str::to_string);
        let status = first
            .as_ref()
            .and_then(|item| item.get("status"))
            .and_then(|value| value.as_str())
            .unwrap_or("");
        if !matches!(status, "sent" | "queued" | "scheduled") {
            let reason = first
                .as_ref()
                .and_then(|item| item.get("reject_reason"))
                .and_then(|value| value.as_str())
                .unwrap_or("rejected");
            return Err(UnifiedError::new(
                format!("Mandrill rejected the message: {reason}"),
                502,
            ));
        }
        Ok(SendResult {
            provider_message_id,
            accepted: true,
            test_mode: message.test_mode,
            detail: format!("Mandrill accepted the message (status {status})"),
        })
    }

    async fn webhook_verify(&self, headers: &HeaderMap, raw_body: &[u8]) -> UnifiedResult<bool> {
        let secret = self
            .connection
            .credentials
            .webhook_secret
            .clone()
            .filter(|value| !value.trim().is_empty())
            .ok_or_else(|| {
                UnifiedError::unprocessable(
                    "Mandrill webhook verification needs webhook_secret (the webhook key) on the connection",
                )
            })?;
        let webhook_url = self.connection.webhook_url.clone().filter(|value| !value.trim().is_empty()).ok_or_else(|| {
            UnifiedError::unprocessable(
                "Mandrill webhook verification needs webhook_url (the exact public webhook URL) on the connection",
            )
        })?;
        let signature = headers
            .get("x-mandrill-signature")
            .and_then(|value| value.to_str().ok())
            .unwrap_or("");
        // Mandrill signs the URL plus every POST parameter, sorted by
        // key, each appended as key + value.
        let mut signed = webhook_url;
        let mut pairs: Vec<(String, String)> = url::form_urlencoded::parse(raw_body)
            .map(|(key, value)| (key.into_owned(), value.into_owned()))
            .collect();
        pairs.sort_by(|a, b| a.0.cmp(&b.0));
        for (key, value) in pairs {
            signed.push_str(&key);
            signed.push_str(&value);
        }
        let mut mac = Hmac::<Sha1>::new_from_slice(secret.as_bytes())
            .map_err(|_| UnifiedError::internal("Invalid Mandrill webhook key"))?;
        mac.update(signed.as_bytes());
        let expected =
            base64::engine::general_purpose::STANDARD.encode(mac.finalize().into_bytes());
        Ok(signatures_equal(signature, &expected))
    }
}

// --- Brevo -----------------------------------------------------------------

http_adapter!(BrevoAdapter);

#[async_trait]
impl EmailProvider for BrevoAdapter {
    fn kind(&self) -> EmailProviderKind {
        EmailProviderKind::Brevo
    }

    fn connection(&self) -> &EmailConnection {
        &self.connection
    }

    async fn test_connection(&self) -> UnifiedResult<ConnectionTestResult> {
        let started = std::time::Instant::now();
        let api_key = require_api_key(&self.connection)?;
        let url = Url::parse("https://api.brevo.com/v3/account")
            .map_err(|_| UnifiedError::internal("Invalid Brevo API URL"))?;
        check_allowlist(&url, self.kind())?;
        let body = send_json(
            &self.client,
            self.client.get(url).header("api-key", api_key),
            "brevo",
        )
        .await?;
        Ok(ConnectionTestResult {
            kind: self.kind().as_str().to_string(),
            reachable: true,
            latency_ms: started.elapsed().as_millis() as u64,
            account: body
                .get("email")
                .and_then(|v| v.as_str())
                .map(str::to_string),
            detail: "Brevo account endpoint answered".to_string(),
        })
    }

    async fn send(&self, message: &EmailMessage) -> UnifiedResult<SendResult> {
        validate_email_address(&message.to)?;
        if message.test_mode && !self.kind().has_native_sandbox() {
            return Ok(simulated_result("brevo"));
        }
        let api_key = require_api_key(&self.connection)?;
        let (from, from_name) = message.sender(&self.connection);
        if from.is_empty() {
            return Err(UnifiedError::bad_request(
                "A sender email is required (set sender_email on the connection or from on the message)",
            ));
        }
        let mut payload = json!({
            "sender": { "name": from_name, "email": from },
            "to": [{ "email": message.to }],
            "subject": message.subject,
            "htmlContent": message.body_html,
            "textContent": message.body_text,
            "tags": message.metadata.keys().cloned().collect::<Vec<_>>(),
        });
        if let Some(reply_to) = &message.reply_to {
            payload["replyTo"] = json!({ "email": reply_to });
        }
        // Deliverability headers (task 354): Brevo takes custom
        // headers in the payload "headers" object; the
        // deliverability values win on collision with the message
        // metadata.
        let deliverability = deliverability_headers_object(message, &self.connection);
        let headers = merge_headers_object(
            &serde_json::to_value(&message.metadata).unwrap_or(Value::Null),
            &deliverability,
        );
        if headers.as_object().is_some_and(|object| !object.is_empty()) {
            payload["headers"] = headers;
        }
        let url = Url::parse("https://api.brevo.com/v3/smtp/email")
            .map_err(|_| UnifiedError::internal("Invalid Brevo API URL"))?;
        check_allowlist(&url, self.kind())?;
        let body = send_json(
            &self.client,
            self.client
                .post(url)
                .header("api-key", api_key)
                .header(CONTENT_TYPE, "application/json")
                .json(&payload),
            "brevo",
        )
        .await?;
        Ok(SendResult {
            provider_message_id: body
                .get("messageId")
                .and_then(|value| value.as_str())
                .map(str::to_string),
            accepted: true,
            test_mode: message.test_mode,
            detail: "Brevo accepted the message".to_string(),
        })
    }
}

// --- SendGrid ---------------------------------------------------------------

http_adapter!(SendGridAdapter);

#[async_trait]
impl EmailProvider for SendGridAdapter {
    fn kind(&self) -> EmailProviderKind {
        EmailProviderKind::SendGrid
    }

    fn connection(&self) -> &EmailConnection {
        &self.connection
    }

    async fn test_connection(&self) -> UnifiedResult<ConnectionTestResult> {
        let started = std::time::Instant::now();
        let api_key = require_api_key(&self.connection)?;
        let url = Url::parse("https://api.sendgrid.com/v3/scopes")
            .map_err(|_| UnifiedError::internal("Invalid SendGrid API URL"))?;
        check_allowlist(&url, self.kind())?;
        let body = send_json(
            &self.client,
            self.client
                .get(url)
                .header(AUTHORIZATION, format!("Bearer {api_key}")),
            "sendgrid",
        )
        .await?;
        let scopes = body
            .get("scopes")
            .and_then(|value| value.as_array())
            .map(|items| items.len())
            .unwrap_or(0);
        Ok(ConnectionTestResult {
            kind: self.kind().as_str().to_string(),
            reachable: true,
            latency_ms: started.elapsed().as_millis() as u64,
            account: None,
            detail: format!("SendGrid answered with {scopes} scopes for the API key"),
        })
    }

    async fn send(&self, message: &EmailMessage) -> UnifiedResult<SendResult> {
        validate_email_address(&message.to)?;
        let api_key = require_api_key(&self.connection)?;
        let (from, from_name) = message.sender(&self.connection);
        if from.is_empty() {
            return Err(UnifiedError::bad_request(
                "A sender email is required (set sender_email on the connection or from on the message)",
            ));
        }
        let mut content = Vec::new();
        if let Some(text) = &message.body_text {
            content.push(json!({ "type": "text/plain", "value": text }));
        }
        if let Some(html) = &message.body_html {
            content.push(json!({ "type": "text/html", "value": html }));
        }
        if content.is_empty() {
            return Err(UnifiedError::bad_request("Provide body_text or body_html"));
        }
        let mut payload = json!({
            "personalizations": [{ "to": [{ "email": message.to }], "subject": message.subject }],
            "from": { "email": from, "name": from_name },
            "subject": message.subject,
            "content": content,
            "custom_args": message.metadata,
        });
        if let Some(reply_to) = &message.reply_to {
            payload["reply_to"] = json!({ "email": reply_to });
        }
        // Deliverability headers (task 354): SendGrid takes global
        // custom headers in the top-level "headers" object.
        let deliverability = deliverability_headers_object(message, &self.connection);
        if deliverability.as_object().is_some_and(|object| !object.is_empty()) {
            payload["headers"] = deliverability;
        }
        if message.test_mode {
            payload["mail_settings"] = json!({ "sandbox_mode": { "enable": true } });
        }
        let url = Url::parse("https://api.sendgrid.com/v3/mail/send")
            .map_err(|_| UnifiedError::internal("Invalid SendGrid API URL"))?;
        check_allowlist(&url, self.kind())?;
        let response = self
            .client
            .post(url)
            .header(AUTHORIZATION, format!("Bearer {api_key}"))
            .header(CONTENT_TYPE, "application/json")
            .json(&payload)
            .send()
            .await
            .map_err(|error| {
                UnifiedError::internal(format!("Provider \"sendgrid\" request failed: {error}"))
            })?;
        let status = response.status();
        if !status.is_success() {
            return Err(http_status_error(
                status,
                "sendgrid",
                parse_retry_after(&response),
            ));
        }
        let message_id = response
            .headers()
            .get("x-message-id")
            .and_then(|value| value.to_str().ok())
            .map(str::to_string);
        Ok(SendResult {
            provider_message_id: message_id,
            accepted: true,
            test_mode: message.test_mode,
            detail: if message.test_mode {
                "SendGrid sandbox accepted the message (not delivered)".to_string()
            } else {
                "SendGrid accepted the message".to_string()
            },
        })
    }
}

// --- Mailgun ----------------------------------------------------------------

pub struct MailgunAdapter {
    pub(crate) connection: EmailConnection,
    pub(crate) client: reqwest::Client,
}

impl MailgunAdapter {
    pub(crate) fn new(connection: &EmailConnection) -> UnifiedResult<Self> {
        Ok(Self {
            connection: connection.clone(),
            client: build_client()?,
        })
    }

    pub(crate) fn base_url(&self) -> UnifiedResult<Url> {
        let host = if self.connection.region.as_deref() == Some("eu") {
            "https://api.eu.mailgun.net"
        } else {
            "https://api.mailgun.net"
        };
        Url::parse(host).map_err(|_| UnifiedError::internal("Invalid Mailgun API URL"))
    }

    pub(crate) fn domain(&self) -> UnifiedResult<String> {
        self.connection
            .domain
            .clone()
            .filter(|value| !value.trim().is_empty())
            .ok_or_else(|| {
                UnifiedError::bad_request(format!(
                    "Connection \"{}\" has no domain configured",
                    self.connection.name
                ))
            })
    }
}

#[async_trait]
impl EmailProvider for MailgunAdapter {
    fn kind(&self) -> EmailProviderKind {
        EmailProviderKind::Mailgun
    }

    fn connection(&self) -> &EmailConnection {
        &self.connection
    }

    async fn test_connection(&self) -> UnifiedResult<ConnectionTestResult> {
        let started = std::time::Instant::now();
        let api_key = require_api_key(&self.connection)?;
        let domain = self.domain()?;
        let url = self
            .base_url()?
            .join(&format!("v3/domains/{domain}"))
            .map_err(|_| UnifiedError::internal("Invalid Mailgun API URL"))?;
        check_allowlist(&url, self.kind())?;
        let body = send_json(
            &self.client,
            self.client.get(url).basic_auth("api", Some(api_key)),
            "mailgun",
        )
        .await?;
        let state = body
            .get("domain")
            .and_then(|value| value.get("state"))
            .and_then(|value| value.as_str())
            .unwrap_or("unknown");
        Ok(ConnectionTestResult {
            kind: self.kind().as_str().to_string(),
            reachable: true,
            latency_ms: started.elapsed().as_millis() as u64,
            account: Some(domain),
            detail: format!("Mailgun domain state: {state}"),
        })
    }

    async fn send(&self, message: &EmailMessage) -> UnifiedResult<SendResult> {
        validate_email_address(&message.to)?;
        if message.test_mode && !self.kind().has_native_sandbox() {
            return Ok(simulated_result("mailgun"));
        }
        let api_key = require_api_key(&self.connection)?;
        let domain = self.domain()?;
        let (from, from_name) = message.sender(&self.connection);
        if from.is_empty() {
            return Err(UnifiedError::bad_request(
                "A sender email is required (set sender_email on the connection or from on the message)",
            ));
        }
        let from_header = match from_name.filter(|name| !name.trim().is_empty()) {
            Some(name) => format!("{name} <{from}>"),
            None => from.clone(),
        };
        let mut form: Vec<(String, String)> = vec![
            ("from".to_string(), from_header),
            ("to".to_string(), message.to.clone()),
            ("subject".to_string(), message.subject.clone()),
        ];
        if let Some(text) = &message.body_text {
            form.push(("text".to_string(), text.clone()));
        }
        if let Some(html) = &message.body_html {
            form.push(("html".to_string(), html.clone()));
        }
        if let Some(reply_to) = &message.reply_to {
            form.push(("h:Reply-To".to_string(), reply_to.clone()));
        }
        // Deliverability headers (task 354): Mailgun takes custom
        // headers as h:Name form fields.
        for (name, value) in deliverability_headers(message, &self.connection, false) {
            form.push((format!("h:{name}"), value));
        }
        for (key, value) in &message.metadata {
            form.push((format!("v:{key}"), value.clone()));
        }
        if message.test_mode {
            form.push(("o:testmode".to_string(), "yes".to_string()));
        }
        let url = self
            .base_url()?
            .join(&format!("v3/{domain}/messages"))
            .map_err(|_| UnifiedError::internal("Invalid Mailgun API URL"))?;
        check_allowlist(&url, self.kind())?;
        let body = send_json(
            &self.client,
            self.client
                .post(url)
                .basic_auth("api", Some(api_key))
                .form(&form),
            "mailgun",
        )
        .await?;
        Ok(SendResult {
            provider_message_id: body
                .get("id")
                .and_then(|value| value.as_str())
                .map(str::to_string),
            accepted: true,
            test_mode: message.test_mode,
            detail: if message.test_mode {
                "Mailgun test mode accepted the message (not delivered)".to_string()
            } else {
                "Mailgun accepted the message".to_string()
            },
        })
    }

    async fn webhook_verify(&self, _headers: &HeaderMap, raw_body: &[u8]) -> UnifiedResult<bool> {
        let secret = self
            .connection
            .credentials
            .webhook_secret
            .clone()
            .filter(|value| !value.trim().is_empty())
            .ok_or_else(|| {
                UnifiedError::unprocessable(
                    "Mailgun webhook verification needs webhook_secret (the webhook signing key) on the connection",
                )
            })?;
        let fields: HashMap<String, String> = url::form_urlencoded::parse(raw_body)
            .map(|(key, value)| (key.into_owned(), value.into_owned()))
            .collect();
        let (Some(timestamp), Some(token), Some(signature)) = (
            fields.get("timestamp"),
            fields.get("token"),
            fields.get("signature"),
        ) else {
            return Ok(false);
        };
        // Reject stale or non-numeric timestamps: a captured webhook
        // must not stay replayable forever (same 300 s window as the
        // Resend verifier). The freshness check runs before the
        // signature comparison so an old replay is refused even when
        // its signature is still valid.
        let Ok(sent_at) = timestamp.parse::<i64>() else {
            return Ok(false);
        };
        if (Utc::now().timestamp() - sent_at).abs() > 300 {
            return Ok(false);
        }
        let expected = hex_encode(&hmac_sha256(
            secret.as_bytes(),
            format!("{timestamp}{token}").as_bytes(),
        ));
        Ok(signatures_equal(signature, &expected))
    }
}

// --- Postmark ---------------------------------------------------------------

http_adapter!(PostmarkAdapter);

#[async_trait]
impl EmailProvider for PostmarkAdapter {
    fn kind(&self) -> EmailProviderKind {
        EmailProviderKind::Postmark
    }

    fn connection(&self) -> &EmailConnection {
        &self.connection
    }

    async fn test_connection(&self) -> UnifiedResult<ConnectionTestResult> {
        let started = std::time::Instant::now();
        let api_key = require_api_key(&self.connection)?;
        let url = Url::parse("https://api.postmarkapp.com/server")
            .map_err(|_| UnifiedError::internal("Invalid Postmark API URL"))?;
        check_allowlist(&url, self.kind())?;
        let body = send_json(
            &self.client,
            self.client
                .get(url)
                .header("X-Postmark-Server-Token", api_key)
                .header("Accept", "application/json"),
            "postmark",
        )
        .await?;
        Ok(ConnectionTestResult {
            kind: self.kind().as_str().to_string(),
            reachable: true,
            latency_ms: started.elapsed().as_millis() as u64,
            account: body
                .get("Name")
                .and_then(|v| v.as_str())
                .map(str::to_string),
            detail: "Postmark server endpoint answered".to_string(),
        })
    }

    async fn send(&self, message: &EmailMessage) -> UnifiedResult<SendResult> {
        validate_email_address(&message.to)?;
        if message.test_mode && !self.kind().has_native_sandbox() {
            return Ok(simulated_result("postmark"));
        }
        let api_key = require_api_key(&self.connection)?;
        let (from, from_name) = message.sender(&self.connection);
        if from.is_empty() {
            return Err(UnifiedError::bad_request(
                "A sender email is required (set sender_email on the connection or from on the message)",
            ));
        }
        let from_header = match from_name.filter(|name| !name.trim().is_empty()) {
            Some(name) => format!("{name} <{from}>"),
            None => from.clone(),
        };
        let mut payload = json!({
            "From": from_header,
            "To": message.to,
            "Subject": message.subject,
            "HtmlBody": message.body_html,
            "TextBody": message.body_text,
            "Metadata": message.metadata,
            "MessageStream": "outbound",
        });
        if let Some(reply_to) = &message.reply_to {
            payload["ReplyTo"] = json!(reply_to);
        }
        // Deliverability headers (task 354): Postmark takes custom
        // headers in the "Headers" array.
        let deliverability = deliverability_headers_array(message, &self.connection);
        if deliverability
            .as_array()
            .is_some_and(|items| !items.is_empty())
        {
            payload["Headers"] = deliverability;
        }
        let url = Url::parse("https://api.postmarkapp.com/email")
            .map_err(|_| UnifiedError::internal("Invalid Postmark API URL"))?;
        check_allowlist(&url, self.kind())?;
        let body = send_json(
            &self.client,
            self.client
                .post(url)
                .header("X-Postmark-Server-Token", api_key)
                .header("Accept", "application/json")
                .header(CONTENT_TYPE, "application/json")
                .json(&payload),
            "postmark",
        )
        .await?;
        if body.get("ErrorCode").and_then(|v| v.as_i64()).unwrap_or(0) != 0 {
            let reason = body
                .get("Message")
                .and_then(|v| v.as_str())
                .unwrap_or("Postmark rejected the message");
            return Err(UnifiedError::new(
                format!("Postmark rejected the message: {reason}"),
                502,
            ));
        }
        Ok(SendResult {
            provider_message_id: body
                .get("MessageID")
                .and_then(|value| value.as_str())
                .map(str::to_string),
            accepted: true,
            test_mode: message.test_mode,
            detail: "Postmark accepted the message".to_string(),
        })
    }
}

// --- Amazon SES (SigV4) -----------------------------------------------------

pub struct SesAdapter {
    pub(crate) connection: EmailConnection,
    pub(crate) client: reqwest::Client,
}

impl SesAdapter {
    pub(crate) fn new(connection: &EmailConnection) -> UnifiedResult<Self> {
        Ok(Self {
            connection: connection.clone(),
            client: build_client()?,
        })
    }

    pub(crate) fn region(&self) -> UnifiedResult<String> {
        self.connection
            .region
            .clone()
            .filter(|value| !value.trim().is_empty())
            .ok_or_else(|| {
                UnifiedError::bad_request(format!(
                    "Connection \"{}\" has no region configured",
                    self.connection.name
                ))
            })
    }

    pub(crate) fn endpoint(&self) -> UnifiedResult<Url> {
        let region = self.region()?;
        let url = Url::parse(&format!("https://email.{region}.amazonaws.com/"))
            .map_err(|_| UnifiedError::internal("Invalid SES endpoint URL"))?;
        if !url.host_str().unwrap_or("").ends_with(".amazonaws.com") {
            return Err(UnifiedError::bad_request(
                "Refusing SES request to a non-Amazon host",
            ));
        }
        Ok(url)
    }

    /// Send one signed SES Query API request and return the raw XML body.
    pub(crate) async fn ses_call(&self, params: &[(String, String)]) -> UnifiedResult<String> {
        let access_key_id = self
            .connection
            .credentials
            .access_key_id
            .clone()
            .filter(|value| !value.trim().is_empty())
            .ok_or_else(|| {
                UnifiedError::bad_request(format!(
                    "Connection \"{}\" has no access_key_id configured",
                    self.connection.name
                ))
            })?;
        let secret_access_key = self
            .connection
            .credentials
            .secret_access_key
            .clone()
            .filter(|value| !value.trim().is_empty())
            .ok_or_else(|| {
                UnifiedError::bad_request(format!(
                    "Connection \"{}\" has no secret_access_key configured",
                    self.connection.name
                ))
            })?;
        let region = self.region()?;
        let url = self.endpoint()?;
        // The serializer is not `Send`, so it must be dropped before
        // the request below is awaited: build the body in a block.
        let body = {
            let mut serializer = url::form_urlencoded::Serializer::new(String::new());
            for (key, value) in params {
                serializer.append_pair(key, value);
            }
            serializer.finish()
        };
        let now = Utc::now();
        let amz_date = now.format("%Y%m%dT%H%M%SZ").to_string();
        let host = url.host_str().unwrap_or("").to_string();
        let content_type = "application/x-www-form-urlencoded".to_string();
        let (authorization, _) = sigv4_authorization(
            &SigV4Params {
                access_key_id,
                secret_access_key,
                region,
                service: "ses".to_string(),
            },
            "POST",
            &url,
            &[
                ("content-type", content_type.clone()),
                ("host", host),
                ("x-amz-date", amz_date.clone()),
            ],
            body.as_bytes(),
            now,
        );
        send_text(
            &self.client,
            self.client
                .post(url)
                .header(CONTENT_TYPE, content_type)
                .header("X-Amz-Date", amz_date)
                .header(AUTHORIZATION, authorization)
                .body(body),
            "ses",
        )
        .await
    }
}

pub(crate) fn xml_tag(body: &str, tag: &str) -> Option<String> {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let start = body.find(&open)? + open.len();
    let end = body[start..].find(&close)? + start;
    Some(body[start..end].to_string())
}

#[async_trait]
impl EmailProvider for SesAdapter {
    fn kind(&self) -> EmailProviderKind {
        EmailProviderKind::Ses
    }

    fn connection(&self) -> &EmailConnection {
        &self.connection
    }

    async fn test_connection(&self) -> UnifiedResult<ConnectionTestResult> {
        let started = std::time::Instant::now();
        let body = self
            .ses_call(&[
                ("Action".to_string(), "GetSendQuota".to_string()),
                ("Version".to_string(), "2010-12-01".to_string()),
            ])
            .await?;
        let reachable = body.contains("GetSendQuotaResult");
        Ok(ConnectionTestResult {
            kind: self.kind().as_str().to_string(),
            reachable,
            latency_ms: started.elapsed().as_millis() as u64,
            account: xml_tag(&body, "Max24HourSend"),
            detail: if reachable {
                "SES GetSendQuota answered".to_string()
            } else {
                "SES answered, but the response did not contain a send quota".to_string()
            },
        })
    }

    async fn send(&self, message: &EmailMessage) -> UnifiedResult<SendResult> {
        validate_email_address(&message.to)?;
        if message.test_mode && !self.kind().has_native_sandbox() {
            return Ok(simulated_result("ses"));
        }
        let (from, from_name) = message.sender(&self.connection);
        if from.is_empty() {
            return Err(UnifiedError::bad_request(
                "A sender email is required (set sender_email on the connection or from on the message)",
            ));
        }
        // No deliverability headers: the SES SendEmail API has no
        // custom-header support (task 354) — the SES SMTP interface
        // is the alternative when headers are required.
        let source = match from_name.filter(|name| !name.trim().is_empty()) {
            Some(name) => format!("{name} <{from}>"),
            None => from.clone(),
        };
        let mut params: Vec<(String, String)> = vec![
            ("Action".to_string(), "SendEmail".to_string()),
            ("Version".to_string(), "2010-12-01".to_string()),
            ("Source".to_string(), source),
            (
                "Destination.ToAddresses.member.1".to_string(),
                message.to.clone(),
            ),
            ("Message.Subject.Data".to_string(), message.subject.clone()),
            ("Message.Subject.Charset".to_string(), "UTF-8".to_string()),
        ];
        if let Some(text) = &message.body_text {
            params.push(("Message.Body.Text.Data".to_string(), text.clone()));
            params.push(("Message.Body.Text.Charset".to_string(), "UTF-8".to_string()));
        }
        if let Some(html) = &message.body_html {
            params.push(("Message.Body.Html.Data".to_string(), html.clone()));
            params.push(("Message.Body.Html.Charset".to_string(), "UTF-8".to_string()));
        }
        if let Some(reply_to) = &message.reply_to {
            params.push(("ReplyToAddresses.member.1".to_string(), reply_to.clone()));
        }
        for (index, (key, value)) in message.metadata.iter().enumerate() {
            params.push((format!("Tags.member.{}.Name", index + 1), key.clone()));
            params.push((format!("Tags.member.{}.Value", index + 1), value.clone()));
        }
        let body = self.ses_call(&params).await?;
        Ok(SendResult {
            provider_message_id: xml_tag(&body, "MessageId"),
            accepted: true,
            test_mode: message.test_mode,
            detail: "SES accepted the message".to_string(),
        })
    }
}

// --- Resend ------------------------------------------------------------------

http_adapter!(ResendAdapter);

#[async_trait]
impl EmailProvider for ResendAdapter {
    fn kind(&self) -> EmailProviderKind {
        EmailProviderKind::Resend
    }

    fn connection(&self) -> &EmailConnection {
        &self.connection
    }

    async fn test_connection(&self) -> UnifiedResult<ConnectionTestResult> {
        let started = std::time::Instant::now();
        let api_key = require_api_key(&self.connection)?;
        let url = Url::parse("https://api.resend.com/domains")
            .map_err(|_| UnifiedError::internal("Invalid Resend API URL"))?;
        check_allowlist(&url, self.kind())?;
        let body = send_json(
            &self.client,
            self.client
                .get(url)
                .header(AUTHORIZATION, format!("Bearer {api_key}")),
            "resend",
        )
        .await?;
        let domains = body
            .get("data")
            .and_then(|value| value.as_array())
            .map(|items| items.len())
            .unwrap_or(0);
        Ok(ConnectionTestResult {
            kind: self.kind().as_str().to_string(),
            reachable: true,
            latency_ms: started.elapsed().as_millis() as u64,
            account: None,
            detail: format!("Resend answered with {domains} verified domain(s)"),
        })
    }

    async fn send(&self, message: &EmailMessage) -> UnifiedResult<SendResult> {
        validate_email_address(&message.to)?;
        if message.test_mode && !self.kind().has_native_sandbox() {
            return Ok(simulated_result("resend"));
        }
        let api_key = require_api_key(&self.connection)?;
        let (from, from_name) = message.sender(&self.connection);
        if from.is_empty() {
            return Err(UnifiedError::bad_request(
                "A sender email is required (set sender_email on the connection or from on the message)",
            ));
        }
        let from_header = match from_name.filter(|name| !name.trim().is_empty()) {
            Some(name) => format!("{name} <{from}>"),
            None => from.clone(),
        };
        let mut payload = json!({
            "from": from_header,
            "to": [message.to],
            "subject": message.subject,
            "html": message.body_html,
            "text": message.body_text,
            "tags": message.metadata.iter().map(|(name, value)| json!({ "name": name, "value": value })).collect::<Vec<_>>(),
        });
        if let Some(reply_to) = &message.reply_to {
            payload["reply_to"] = json!(reply_to);
        }
        // Deliverability headers (task 354): Resend takes custom
        // headers in the top-level "headers" object.
        let deliverability = deliverability_headers_object(message, &self.connection);
        if deliverability.as_object().is_some_and(|object| !object.is_empty()) {
            payload["headers"] = deliverability;
        }
        let url = Url::parse("https://api.resend.com/emails")
            .map_err(|_| UnifiedError::internal("Invalid Resend API URL"))?;
        check_allowlist(&url, self.kind())?;
        let body = send_json(
            &self.client,
            self.client
                .post(url)
                .header(AUTHORIZATION, format!("Bearer {api_key}"))
                .header(CONTENT_TYPE, "application/json")
                .json(&payload),
            "resend",
        )
        .await?;
        Ok(SendResult {
            provider_message_id: body
                .get("id")
                .and_then(|value| value.as_str())
                .map(str::to_string),
            accepted: true,
            test_mode: message.test_mode,
            detail: "Resend accepted the message".to_string(),
        })
    }

    async fn webhook_verify(&self, headers: &HeaderMap, raw_body: &[u8]) -> UnifiedResult<bool> {
        // Svix-signed webhook: headers svix-id, svix-timestamp,
        // svix-signature ("v1,<base64 hmac>"). The signing secret is
        // base64-encoded and may carry a "whsec_" prefix.
        let secret = self
            .connection
            .credentials
            .webhook_secret
            .clone()
            .filter(|value| !value.trim().is_empty())
            .ok_or_else(|| {
                UnifiedError::unprocessable(
                    "Resend webhook verification needs webhook_secret (the Svix signing secret) on the connection",
                )
            })?;
        let secret = secret.strip_prefix("whsec_").unwrap_or(&secret);
        let key = base64::engine::general_purpose::STANDARD
            .decode(secret)
            .map_err(|_| {
                UnifiedError::unprocessable(
                    "Resend webhook_secret is not valid base64 (Svix secrets are base64-encoded)",
                )
            })?;
        let id = headers
            .get("svix-id")
            .and_then(|value| value.to_str().ok())
            .unwrap_or("");
        let timestamp = headers
            .get("svix-timestamp")
            .and_then(|value| value.to_str().ok())
            .unwrap_or("");
        let signatures = headers
            .get("svix-signature")
            .and_then(|value| value.to_str().ok())
            .unwrap_or("");
        if id.is_empty() || timestamp.is_empty() || signatures.is_empty() {
            return Ok(false);
        }
        // Reject stale webhooks (older than 5 minutes either way).
        if let Ok(sent_at) = timestamp.parse::<i64>() {
            let now = Utc::now().timestamp();
            if (now - sent_at).abs() > 300 {
                return Ok(false);
            }
        } else {
            return Ok(false);
        }
        let signed = format!("{id}.{timestamp}.{}", String::from_utf8_lossy(raw_body));
        let expected =
            base64::engine::general_purpose::STANDARD.encode(hmac_sha256(&key, signed.as_bytes()));
        for candidate in signatures.split_whitespace() {
            if let Some(value) = candidate.strip_prefix("v1,") {
                if signatures_equal(value, &expected) {
                    return Ok(true);
                }
            }
        }
        Ok(false)
    }
}

// --- SparkPost ---------------------------------------------------------------

pub struct SparkPostAdapter {
    pub(crate) connection: EmailConnection,
    pub(crate) client: reqwest::Client,
}

impl SparkPostAdapter {
    pub(crate) fn new(connection: &EmailConnection) -> UnifiedResult<Self> {
        Ok(Self {
            connection: connection.clone(),
            client: build_client()?,
        })
    }

    pub(crate) fn base_url(&self) -> UnifiedResult<Url> {
        let host = if self.connection.region.as_deref() == Some("eu") {
            "https://api.eu.sparkpost.com"
        } else {
            "https://api.sparkpost.com"
        };
        Url::parse(host).map_err(|_| UnifiedError::internal("Invalid SparkPost API URL"))
    }
}

#[async_trait]
impl EmailProvider for SparkPostAdapter {
    fn kind(&self) -> EmailProviderKind {
        EmailProviderKind::SparkPost
    }

    fn connection(&self) -> &EmailConnection {
        &self.connection
    }

    async fn test_connection(&self) -> UnifiedResult<ConnectionTestResult> {
        let started = std::time::Instant::now();
        let api_key = require_api_key(&self.connection)?;
        let url = self
            .base_url()?
            .join("api/v1/account")
            .map_err(|_| UnifiedError::internal("Invalid SparkPost API URL"))?;
        check_allowlist(&url, self.kind())?;
        let body = send_json(
            &self.client,
            self.client.get(url).header(AUTHORIZATION, api_key),
            "sparkpost",
        )
        .await?;
        let status = body
            .get("results")
            .and_then(|value| value.get("status"))
            .and_then(|value| value.as_str())
            .unwrap_or("unknown");
        Ok(ConnectionTestResult {
            kind: self.kind().as_str().to_string(),
            reachable: true,
            latency_ms: started.elapsed().as_millis() as u64,
            account: None,
            detail: format!("SparkPost account status: {status}"),
        })
    }

    async fn send(&self, message: &EmailMessage) -> UnifiedResult<SendResult> {
        validate_email_address(&message.to)?;
        if message.test_mode && !self.kind().has_native_sandbox() {
            return Ok(simulated_result("sparkpost"));
        }
        let api_key = require_api_key(&self.connection)?;
        let (from, from_name) = message.sender(&self.connection);
        if from.is_empty() {
            return Err(UnifiedError::bad_request(
                "A sender email is required (set sender_email on the connection or from on the message)",
            ));
        }
        let mut content = json!({
            "from": { "name": from_name, "email": from },
            "subject": message.subject,
            "html": message.body_html,
            "text": message.body_text,
        });
        if let Some(reply_to) = &message.reply_to {
            content["reply_to"] = json!(reply_to);
        }
        // Deliverability headers (task 354): SparkPost takes custom
        // headers in the content "headers" object.
        let deliverability = deliverability_headers_object(message, &self.connection);
        if deliverability.as_object().is_some_and(|object| !object.is_empty()) {
            content["headers"] = deliverability;
        }
        let payload = json!({
            "options": { "sandbox": message.test_mode },
            "content": content,
            "recipients": [{ "address": { "email": message.to } }],
            "metadata": message.metadata,
        });
        let url = self
            .base_url()?
            .join("api/v1/transmissions")
            .map_err(|_| UnifiedError::internal("Invalid SparkPost API URL"))?;
        check_allowlist(&url, self.kind())?;
        let body = send_json(
            &self.client,
            self.client
                .post(url)
                .header(AUTHORIZATION, api_key)
                .header(CONTENT_TYPE, "application/json")
                .json(&payload),
            "sparkpost",
        )
        .await?;
        Ok(SendResult {
            provider_message_id: body
                .get("results")
                .and_then(|value| value.get("id"))
                .and_then(|value| value.as_str())
                .map(str::to_string),
            accepted: true,
            test_mode: message.test_mode,
            detail: if message.test_mode {
                "SparkPost sandbox accepted the message (not delivered)".to_string()
            } else {
                "SparkPost accepted the message".to_string()
            },
        })
    }
}

// --- SMTP (delegates to the Mail Manager) ------------------------------------
//
// Delivery goes through the `mail.send_requested` hook owned by the
// Mail Manager, which owns the SMTP transport (lettre), the SMTP
// settings, and the send log. This adapter never opens an SMTP
// connection itself.

// ---------------------------------------------------------------------------
// Shared SMTP-family helpers (SMTP, OAuth, sendmail).
// ---------------------------------------------------------------------------

/// Build the lettre message for an SMTP-family send, mirroring
/// `mail/service.rs::build_message`: multipart alternative when
/// both bodies exist, plain text or HTML alone otherwise.
/// `default_from` is the last-resort sender (`unified@localhost`
/// for local sendmail); without one, a missing sender is an honest
/// 400 instead of a silently wrong From. Addresses are parsed with
/// lettre's own parser, which also rejects header-injecting
/// newlines.
pub(crate) fn build_lettre_message(
    connection: &EmailConnection,
    message: &EmailMessage,
    default_from: Option<&str>,
) -> UnifiedResult<LettreMessage> {
    let (from, from_name) = message.sender(connection);
    let from = if from.trim().is_empty() {
        default_from.unwrap_or("").to_string()
    } else {
        from
    };
    if from.trim().is_empty() {
        return Err(UnifiedError::bad_request(
            "A sender email is required (set sender_email on the connection or from on the message)",
        ));
    }
    let from_address: lettre::Address = from
        .trim()
        .parse()
        .map_err(|_| UnifiedError::bad_request(format!("Invalid sender email address: {from}")))?;
    let from_mailbox = Mailbox::new(
        from_name.filter(|name| !name.trim().is_empty()),
        from_address,
    );
    let to = message.to.trim().parse::<Mailbox>().map_err(|_| {
        UnifiedError::bad_request(format!("Invalid recipient email address: {}", message.to))
    })?;
    let mut builder = LettreMessage::builder()
        .from(from_mailbox)
        .to(to)
        .subject(message.subject.as_str());
    if let Some(reply_to) = message
        .reply_to
        .as_deref()
        .filter(|value| !value.trim().is_empty())
    {
        let reply_mailbox = reply_to.parse::<Mailbox>().map_err(|_| {
            UnifiedError::bad_request(format!("Invalid reply-to email address: {reply_to}"))
        })?;
        builder = builder.reply_to(reply_mailbox);
    }
    // Deliverability headers (task 354, `deliverability.rs`):
    // List-Unsubscribe on bulk or opted-in transactional sends,
    // Precedence / X-Auto-Response-Suppress / Feedback-ID on bulk.
    // Computed once here so SMTP, OAuth, and sendmail all emit
    // them; the header names are static, so `raw_header` takes
    // them directly.
    for (name, value) in deliverability_headers(message, connection, true) {
        builder = builder.raw_header(HeaderValue::new(
            HeaderName::new_from_ascii_str(name),
            value,
        ));
    }
    match (&message.body_text, &message.body_html) {
        (Some(text), Some(html)) => builder
            .multipart(MultiPart::alternative_plain_html(
                text.clone(),
                html.clone(),
            ))
            .map_err(|error| UnifiedError::bad_request(format!("Cannot build message: {error}"))),
        (Some(text), None) => builder
            .body(text.clone())
            .map_err(|error| UnifiedError::bad_request(format!("Cannot build message: {error}"))),
        (None, Some(html)) => builder
            .header(ContentType::TEXT_HTML)
            .body(html.clone())
            .map_err(|error| UnifiedError::bad_request(format!("Cannot build message: {error}"))),
        (None, None) => Err(UnifiedError::bad_request(
            "Provide body_text or body_html (or a template_id that renders one)",
        )),
    }
}

/// Build an async SMTP transport for explicit server settings.
/// `tls_mode`: `"starttls"` (default, port 587), `"implicit"`
/// (port 465), `"none"` (plaintext, local relays only). STARTTLS is
/// REQUIRED, never opportunistic: a server that cannot upgrade
/// fails the connection instead of silently sending plaintext.
/// Credentials are only attached when a username is configured.
pub(crate) fn smtp_transport(
    host: &str,
    port: u16,
    tls_mode: &str,
    username: Option<&str>,
    password: Option<String>,
) -> UnifiedResult<AsyncSmtpTransport<Tokio1Executor>> {
    let tls_parameters = |host: &str| {
        TlsParameters::new(host.to_string())
            .map_err(|error| UnifiedError::bad_request(format!("Invalid TLS parameters: {error}")))
    };
    let tls = match tls_mode {
        "implicit" => Tls::Wrapper(tls_parameters(host)?),
        "none" => Tls::None,
        _ => Tls::Required(tls_parameters(host)?),
    };
    let mut builder = AsyncSmtpTransport::<Tokio1Executor>::builder_dangerous(host)
        .port(port)
        .tls(tls)
        .timeout(Some(Duration::from_secs(DEFAULT_TIMEOUT_SECS)));
    if let Some(username) = username.filter(|value| !value.trim().is_empty()) {
        builder = builder.credentials(SmtpCredentials::new(
            username.to_string(),
            password.unwrap_or_default(),
        ));
    }
    Ok(builder.build())
}

// --- Generic SMTP (direct, per-connection server settings) -----------

pub struct SmtpAdapter {
    pub(crate) connection: EmailConnection,
}

impl SmtpAdapter {
    pub(crate) fn new(connection: &EmailConnection) -> UnifiedResult<Self> {
        // Validate the server settings up front (host, TLS
        // parameters, decryptable password) so a broken connection
        // is rejected at build time, not at the first send.
        let _ = Self::transport(connection)?;
        Ok(Self {
            connection: connection.clone(),
        })
    }

    /// Resolve the SMTP server: the connection's own settings win,
    /// then the catalog preset for the kind (task 354), then the
    /// adapter defaults (587, STARTTLS). A preset kind with no
    /// preset host and no connection host is an honest 400.
    fn resolve_server(
        connection: &EmailConnection,
    ) -> UnifiedResult<(String, u16, String)> {
        let preset = connection
            .kind
            .definition()
            .and_then(|def| def.smtp_preset());
        let host = connection
            .smtp_host
            .clone()
            .filter(|value| !value.trim().is_empty())
            .or_else(|| preset.as_ref().map(|(host, _, _)| host.clone()))
            .ok_or_else(|| {
                UnifiedError::bad_request(format!(
                    "Connection \"{}\" has no smtp_host configured and provider \"{}\" has no preset relay",
                    connection.name,
                    connection.kind.as_str()
                ))
            })?;
        let port = connection
            .smtp_port
            .or_else(|| preset.as_ref().map(|(_, port, _)| *port))
            .unwrap_or(587);
        let tls_mode = connection
            .smtp_tls
            .clone()
            .filter(|value| !value.trim().is_empty())
            .or_else(|| preset.as_ref().map(|(_, _, tls)| tls.clone()))
            .unwrap_or_else(|| "starttls".to_string());
        Ok((host, port, tls_mode))
    }

    fn transport(
        connection: &EmailConnection,
    ) -> UnifiedResult<AsyncSmtpTransport<Tokio1Executor>> {
        let (host, port, tls_mode) = Self::resolve_server(connection)?;
        let password = decrypt_optional(&connection.credentials.password)?;
        smtp_transport(
            &host,
            port,
            &tls_mode,
            connection.smtp_username.as_deref(),
            password,
        )
    }
}

#[async_trait]
impl EmailProvider for SmtpAdapter {
    fn kind(&self) -> EmailProviderKind {
        // The preset kind, not hard-coded `Smtp`: logs, capabilities,
        // and the allow-list read the preset's definition.
        self.connection.kind
    }

    fn connection(&self) -> &EmailConnection {
        &self.connection
    }

    /// A real check now: the transport connects and issues NOOP.
    /// A refused or unreachable server is `reachable: false` with
    /// the transport error as the detail — never a 500, because a
    /// failed connection test is a result, not a handler error.
    async fn test_connection(&self) -> UnifiedResult<ConnectionTestResult> {
        let started = std::time::Instant::now();
        let (host, port, _) = match Self::resolve_server(&self.connection) {
            Ok(server) => server,
            Err(error) => {
                return Ok(ConnectionTestResult {
                    kind: self.kind().as_str().to_string(),
                    reachable: false,
                    latency_ms: started.elapsed().as_millis() as u64,
                    account: None,
                    detail: error.message,
                });
            }
        };
        let transport = match Self::transport(&self.connection) {
            Ok(transport) => transport,
            Err(error) => {
                return Ok(ConnectionTestResult {
                    kind: self.kind().as_str().to_string(),
                    reachable: false,
                    latency_ms: started.elapsed().as_millis() as u64,
                    account: None,
                    detail: error.message,
                });
            }
        };
        match transport.test_connection().await {
            Ok(true) => Ok(ConnectionTestResult {
                kind: self.kind().as_str().to_string(),
                reachable: true,
                latency_ms: started.elapsed().as_millis() as u64,
                account: None,
                detail: format!("SMTP {host}:{port} answered NOOP"),
            }),
            Ok(false) => Ok(ConnectionTestResult {
                kind: self.kind().as_str().to_string(),
                reachable: false,
                latency_ms: started.elapsed().as_millis() as u64,
                account: None,
                detail: format!("SMTP {host}:{port} connected but did not answer NOOP"),
            }),
            Err(error) => Ok(ConnectionTestResult {
                kind: self.kind().as_str().to_string(),
                reachable: false,
                latency_ms: started.elapsed().as_millis() as u64,
                account: None,
                detail: format!("SMTP connection to {host}:{port} failed: {error}"),
            }),
        }
    }

    async fn send(&self, message: &EmailMessage) -> UnifiedResult<SendResult> {
        validate_email_address(&message.to)?;
        if message.test_mode {
            return Ok(simulated_result(self.connection.kind.as_str()));
        }
        let transport = Self::transport(&self.connection)?;
        let lettre_message = build_lettre_message(&self.connection, message, None)?;
        transport
            .send(lettre_message)
            .await
            .map_err(|error| UnifiedError::new(format!("SMTP delivery failed: {error}"), 502))?;
        let (host, port, _) = Self::resolve_server(&self.connection)?;
        Ok(SendResult {
            provider_message_id: None,
            accepted: true,
            test_mode: false,
            detail: format!("Accepted by SMTP {host}:{port}"),
        })
    }

    async fn webhook_verify(&self, _headers: &HeaderMap, _raw_body: &[u8]) -> UnifiedResult<bool> {
        Err(unsupported(self.kind(), "webhook verification", "webhooks"))
    }
}

// --- OAuth account providers (Gmail / Yahoo / Outlook) ---------------

/// The tokens one OAuth connection currently holds. The adapter
/// refreshes the access token on first use when it is expired and
/// persists the rotated tokens back to the stored connection.
#[derive(Debug, Clone, Default)]
struct OAuthTokens {
    access_token: Option<String>,
    refresh_token: Option<String>,
    expires_at: Option<DateTime<Utc>>,
}

/// One adapter for all three OAuth account providers: the only
/// difference is the static config (endpoints, scopes, SMTP host).
/// Delivery is SMTP + XOAUTH2 on port 587 with required STARTTLS;
/// the access token is the password and the sender address is the
/// XOAUTH2 identity.
pub struct OAuthSmtpAdapter {
    connection: EmailConnection,
    config: &'static OAuthProviderConfig,
    tokens: Mutex<OAuthTokens>,
}

impl OAuthSmtpAdapter {
    pub(crate) fn new(connection: &EmailConnection) -> UnifiedResult<Self> {
        let config = connection.kind.oauth_config().ok_or_else(|| {
            UnifiedError::internal(format!(
                "No OAuth configuration for provider kind \"{}\"",
                connection.kind.as_str()
            ))
        })?;
        let access_token = decrypt_optional(&connection.credentials.access_token)?;
        let refresh_token = decrypt_optional(&connection.credentials.refresh_token)?;
        Ok(Self {
            connection: connection.clone(),
            config,
            tokens: Mutex::new(OAuthTokens {
                access_token,
                refresh_token,
                expires_at: connection.credentials.access_token_expires_at,
            }),
        })
    }

    /// A fresh access token: the stored one when it is still valid,
    /// otherwise a refresh from the stored refresh token (persisted
    /// back, rotated refresh token included). Errors honestly when
    /// the account was never linked — that is a setup error, not a
    /// send failure.
    async fn ensure_fresh_token(&self) -> UnifiedResult<String> {
        {
            let tokens = self.tokens.lock().expect("oauth tokens lock poisoned");
            if let Some(access_token) = &tokens.access_token {
                if !access_token_expired(tokens.expires_at) {
                    return Ok(access_token.clone());
                }
            }
        }
        let (client_id, client_secret, refresh_token) = {
            let tokens = self.tokens.lock().expect("oauth tokens lock poisoned");
            let refresh_token = tokens.refresh_token.clone().ok_or_else(|| {
                UnifiedError::unprocessable(format!(
                    "Connection \"{}\" is not linked to a {} account yet; link it from the OAuth authorize endpoint",
                    self.connection.name,
                    self.connection.kind.as_str()
                ))
            })?;
            let client_id = self
                .connection
                .credentials
                .client_id
                .clone()
                .unwrap_or_default();
            let client_secret =
                decrypt_optional(&self.connection.credentials.client_secret)?.unwrap_or_default();
            (client_id, client_secret, refresh_token)
        };
        let refreshed = refresh_access_token(
            self.connection.kind,
            &client_id,
            &client_secret,
            &refresh_token,
        )
        .await?;
        let new_access = refreshed.access_token.clone();
        let new_refresh = refreshed.refresh_token.clone();
        let new_expires = refreshed.expires_at();
        {
            let mut tokens = self.tokens.lock().expect("oauth tokens lock poisoned");
            tokens.access_token = Some(new_access.clone());
            if let Some(rotated) = &new_refresh {
                tokens.refresh_token = Some(rotated.clone());
            }
            tokens.expires_at = Some(new_expires);
        }
        self.persist_tokens(&new_access, new_refresh.as_deref(), Some(new_expires))?;
        emit_event(
            EVENT_EMAIL_PROVIDER_OAUTH_REFRESHED,
            json!({
                "connection_id": self.connection.id.to_string(),
                "kind": self.connection.kind.as_str(),
                "expires_at": new_expires.to_rfc3339(),
            }),
        );
        Ok(new_access)
    }

    /// Write refreshed tokens back to the stored connection,
    /// encrypted. A missing stored connection (deleted mid-flight)
    /// is not an error: the in-memory tokens are already updated.
    fn persist_tokens(
        &self,
        access_token: &str,
        refresh_token: Option<&str>,
        expires_at: Option<DateTime<Utc>>,
    ) -> UnifiedResult<()> {
        let encrypted_access = encrypt_secret(access_token)?;
        let encrypted_refresh = match refresh_token {
            Some(token) => Some(encrypt_secret(token)?),
            None => None,
        };
        if let Some(mut stored) = connections_store().get_mut(&self.connection.id) {
            stored.credentials.access_token = Some(encrypted_access);
            if let Some(encrypted) = encrypted_refresh {
                stored.credentials.refresh_token = Some(encrypted);
            }
            stored.credentials.access_token_expires_at = expires_at;
            stored.updated_at = Utc::now();
        }
        Ok(())
    }

    fn transport(
        &self,
        access_token: &str,
        sender_email: &str,
    ) -> UnifiedResult<AsyncSmtpTransport<Tokio1Executor>> {
        let parameters =
            TlsParameters::new(self.config.smtp_host.to_string()).map_err(|error| {
                UnifiedError::bad_request(format!("Invalid TLS parameters: {error}"))
            })?;
        Ok(
            AsyncSmtpTransport::<Tokio1Executor>::builder_dangerous(self.config.smtp_host)
                .port(self.config.smtp_port)
                .tls(Tls::Required(parameters))
                .credentials(SmtpCredentials::new(
                    sender_email.to_string(),
                    access_token.to_string(),
                ))
                .authentication(vec![Mechanism::Xoauth2])
                .timeout(Some(Duration::from_secs(DEFAULT_TIMEOUT_SECS)))
                .build(),
        )
    }
}

#[async_trait]
impl EmailProvider for OAuthSmtpAdapter {
    fn kind(&self) -> EmailProviderKind {
        self.connection.kind
    }

    fn connection(&self) -> &EmailConnection {
        &self.connection
    }

    async fn test_connection(&self) -> UnifiedResult<ConnectionTestResult> {
        let started = std::time::Instant::now();
        let from = self.connection.sender_email.clone().unwrap_or_default();
        let account = if from.is_empty() {
            None
        } else {
            Some(from.clone())
        };
        let token = match self.ensure_fresh_token().await {
            Ok(token) => token,
            Err(error) => {
                return Ok(ConnectionTestResult {
                    kind: self.kind().as_str().to_string(),
                    reachable: false,
                    latency_ms: started.elapsed().as_millis() as u64,
                    account,
                    detail: error.message,
                });
            }
        };
        let transport = match self.transport(&token, &from) {
            Ok(transport) => transport,
            Err(error) => {
                return Ok(ConnectionTestResult {
                    kind: self.kind().as_str().to_string(),
                    reachable: false,
                    latency_ms: started.elapsed().as_millis() as u64,
                    account,
                    detail: error.message,
                });
            }
        };
        match transport.test_connection().await {
            Ok(true) => Ok(ConnectionTestResult {
                kind: self.kind().as_str().to_string(),
                reachable: true,
                latency_ms: started.elapsed().as_millis() as u64,
                account,
                detail: format!(
                    "{} SMTP {}:{} answered NOOP",
                    self.connection.kind.as_str(),
                    self.config.smtp_host,
                    self.config.smtp_port
                ),
            }),
            Ok(false) => Ok(ConnectionTestResult {
                kind: self.kind().as_str().to_string(),
                reachable: false,
                latency_ms: started.elapsed().as_millis() as u64,
                account,
                detail: format!(
                    "{} SMTP connected but did not answer NOOP",
                    self.connection.kind.as_str()
                ),
            }),
            Err(error) => Ok(ConnectionTestResult {
                kind: self.kind().as_str().to_string(),
                reachable: false,
                latency_ms: started.elapsed().as_millis() as u64,
                account,
                detail: format!(
                    "{} SMTP connection failed: {error}",
                    self.connection.kind.as_str()
                ),
            }),
        }
    }

    async fn send(&self, message: &EmailMessage) -> UnifiedResult<SendResult> {
        validate_email_address(&message.to)?;
        if message.test_mode {
            return Ok(simulated_result(self.connection.kind.as_str()));
        }
        let (from, _) = message.sender(&self.connection);
        if from.trim().is_empty() {
            return Err(UnifiedError::bad_request(
                "A sender email is required (set sender_email on the connection or from on the message)",
            ));
        }
        let token = self.ensure_fresh_token().await?;
        let transport = self.transport(&token, &from)?;
        let lettre_message = build_lettre_message(&self.connection, message, None)?;
        transport.send(lettre_message).await.map_err(|error| {
            UnifiedError::new(
                format!("{} delivery failed: {error}", self.connection.kind.as_str()),
                502,
            )
        })?;
        Ok(SendResult {
            provider_message_id: None,
            accepted: true,
            test_mode: false,
            detail: format!("Accepted by {} SMTP", self.connection.kind.as_str()),
        })
    }

    async fn webhook_verify(&self, _headers: &HeaderMap, _raw_body: &[u8]) -> UnifiedResult<bool> {
        Err(unsupported(self.kind(), "webhook verification", "webhooks"))
    }
}

// --- Local sendmail ------------------------------------------------------

pub struct SendmailAdapter {
    pub(crate) connection: EmailConnection,
}

impl SendmailAdapter {
    pub(crate) fn new(connection: &EmailConnection) -> UnifiedResult<Self> {
        Ok(Self {
            connection: connection.clone(),
        })
    }

    /// Locate the local sendmail binary (same probe as the mail
    /// module: `/usr/sbin/sendmail`, then `/usr/bin/sendmail`).
    pub(crate) fn sendmail_binary() -> Option<&'static str> {
        for candidate in ["/usr/sbin/sendmail", "/usr/bin/sendmail"] {
            if std::path::Path::new(candidate).exists() {
                return Some(candidate);
            }
        }
        None
    }
}

#[async_trait]
impl EmailProvider for SendmailAdapter {
    fn kind(&self) -> EmailProviderKind {
        EmailProviderKind::Sendmail
    }

    fn connection(&self) -> &EmailConnection {
        &self.connection
    }

    /// The connection test for sendmail is the binary probe: no
    /// credentials exist to verify, so presence of the binary is
    /// the honest signal. A missing binary is `reachable: false`,
    /// never a 500.
    async fn test_connection(&self) -> UnifiedResult<ConnectionTestResult> {
        let started = std::time::Instant::now();
        match Self::sendmail_binary() {
            Some(binary) => Ok(ConnectionTestResult {
                kind: self.kind().as_str().to_string(),
                reachable: true,
                latency_ms: started.elapsed().as_millis() as u64,
                account: None,
                detail: format!("Sendmail binary found at {binary}"),
            }),
            None => Ok(ConnectionTestResult {
                kind: self.kind().as_str().to_string(),
                reachable: false,
                latency_ms: started.elapsed().as_millis() as u64,
                account: None,
                detail: "No sendmail binary found at /usr/sbin/sendmail or /usr/bin/sendmail; install a local MTA (for example postfix) to use this provider".to_string(),
            }),
        }
    }

    async fn send(&self, message: &EmailMessage) -> UnifiedResult<SendResult> {
        validate_email_address(&message.to)?;
        if message.test_mode {
            return Ok(simulated_result("sendmail"));
        }
        let binary = Self::sendmail_binary().ok_or_else(|| {
            UnifiedError::new(
                "No sendmail binary found at /usr/sbin/sendmail or /usr/bin/sendmail",
                502,
            )
        })?;
        let lettre_message =
            build_lettre_message(&self.connection, message, Some("unified@localhost"))?;
        // Spawning the binary directly (std::process::Command) instead
        // of lettre's SendmailTransport: that transport needs the
        // `sendmail-transport` feature, which pulls the async-std
        // tree into the lock for one trivial spawn. The direct spawn
        // is the same behaviour — `-t` reads recipients from the
        // message headers, `-i` keeps dots in the body literal —
        // with no new dependency. Blocking process I/O runs off the
        // async worker in spawn_blocking, mirroring the mail module.
        let command = binary.to_string();
        let formatted = lettre_message.formatted();
        let sent = tokio::task::spawn_blocking(move || {
            use std::io::Write;
            let mut child = std::process::Command::new(&command)
                .arg("-t")
                .arg("-i")
                .stdin(std::process::Stdio::piped())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::piped())
                .spawn()
                .map_err(|error| format!("Failed to spawn {command}: {error}"))?;
            if let Some(mut stdin) = child.stdin.take() {
                stdin
                    .write_all(&formatted)
                    .map_err(|error| format!("Failed to write the message to {command}: {error}"))?;
            }
            let output = child
                .wait_with_output()
                .map_err(|error| format!("Failed to wait for {command}: {error}"))?;
            if output.status.success() {
                Ok(())
            } else {
                Err(format!(
                    "{command} exited with {}: {}",
                    output.status,
                    String::from_utf8_lossy(&output.stderr).trim()
                ))
            }
        })
        .await
        .map_err(|error| {
            UnifiedError::internal(format!("Sendmail delivery worker failed: {error}"))
        })?;
        sent.map_err(|error| UnifiedError::new(format!("Sendmail delivery failed: {error}"), 502))?;
        Ok(SendResult {
            provider_message_id: None,
            accepted: true,
            test_mode: false,
            detail: format!("Accepted by local sendmail ({binary})"),
        })
    }

    async fn webhook_verify(&self, _headers: &HeaderMap, _raw_body: &[u8]) -> UnifiedResult<bool> {
        Err(unsupported(self.kind(), "webhook verification", "webhooks"))
    }
}

// --- Generic HTTP API providers (task 354) ---------------------------
//
// One adapter for the providers whose only send path is a simple
// JSON HTTP API (Plunk, Courier, Loops, OneSignal): the endpoint,
// auth scheme, payload template, and response id path all come
// from the provider definition's `http_api` object, so a new
// provider of this shape needs data, not code.
//
// Template placeholders (in string values):
//   {to} {from} {from_name} {subject} {text} {html}
//   {body} (HTML when present, else text)
//   {reply_to} {template_id} {app_id} (the OneSignal App ID, from
//   the connection's domain field)
//   {variables} (whole value only — replaced by the variables object)
//   {deliverability_headers} (whole value only — the deliverability
//   headers object; dropped when empty)
// A placeholder whose value is missing drops the key; an unknown
// placeholder is an honest 400. A template that mentions
// {template_id} requires one on the message (Loops); a message
// that carries a template_id for a provider whose template has no
// {template_id} is refused — template sends are not silently
// downgraded to free-form sends.
//
// Deliverability headers: only Plunk's send API documents a
// `headers` object, so only its template carries
// {deliverability_headers}; Courier, Loops, and OneSignal have no
// documented custom-header field on their send endpoints. The
// generic adapter has no webhook override: webhook verification
// falls back to the shared secret-token check (the provider's
// native scheme is not implemented, as the webhook notes say).

http_adapter!(GenericHttpAdapter);

/// Render a generic HTTP payload template against one message.
/// Returns `None` for a node that must be dropped (a missing
/// optional placeholder); the caller drops the key (objects) or
/// the item (arrays).
fn render_http_template_value(
    template: &Value,
    message: &EmailMessage,
    connection: &EmailConnection,
) -> UnifiedResult<Option<Value>> {
    match template {
        Value::Object(object) => {
            let mut rendered = serde_json::Map::new();
            for (key, value) in object {
                if let Some(value) =
                    render_http_template_value(value, message, connection)?
                {
                    rendered.insert(key.clone(), value);
                }
            }
            Ok(Some(Value::Object(rendered)))
        }
        Value::Array(items) => {
            let mut rendered = Vec::new();
            for item in items {
                if let Some(value) =
                    render_http_template_value(item, message, connection)?
                {
                    rendered.push(value);
                }
            }
            Ok(Some(Value::Array(rendered)))
        }
        Value::String(text) => render_http_template_string(text, message, connection),
        other => Ok(Some(other.clone())),
    }
}

fn render_http_template_string(
    text: &str,
    message: &EmailMessage,
    connection: &EmailConnection,
) -> UnifiedResult<Option<Value>> {
    // Whole-value object placeholders: {variables} and
    // {deliverability_headers}.
    if text == "{variables}" {
        return Ok(Some(Value::Object(
            message
                .variables
                .iter()
                .map(|(key, value)| (key.clone(), value.clone()))
                .collect(),
        )));
    }
    if text == "{deliverability_headers}" {
        let headers = deliverability_headers_object(message, connection);
        if headers.as_object().is_some_and(|object| object.is_empty()) {
            return Ok(None);
        }
        return Ok(Some(headers));
    }
    let (from, from_name) = message.sender(connection);
    let body = message
        .body_html
        .clone()
        .or_else(|| message.body_text.clone());
    let mut rendered = text.to_string();
    let mut missing = false;
    let mut start = 0;
    while let Some(open) = rendered[start..].find('{') {
        let open = start + open;
        let Some(close) = rendered[open..].find('}') else {
            return Err(UnifiedError::bad_request(format!(
                "Unmatched '{{' in the http_api payload template for provider \"{}\"",
                connection.kind.as_str()
            )));
        };
        let close = open + close;
        let placeholder = &rendered[open + 1..close];
        let value: Option<String> = match placeholder {
            "to" => Some(message.to.clone()),
            "from" => Some(from.clone()),
            "from_name" => from_name.clone(),
            "subject" => Some(message.subject.clone()),
            "text" => message.body_text.clone(),
            "html" => message.body_html.clone(),
            "body" => body.clone(),
            "reply_to" => message.reply_to.clone(),
            "template_id" => message.template_id.clone(),
            "app_id" => connection.domain.clone(),
            _ => {
                return Err(UnifiedError::bad_request(format!(
                    "Unknown placeholder {{{placeholder}}} in the http_api payload template for provider \"{}\"",
                    connection.kind.as_str()
                )));
            }
        };
        let Some(value) = value.filter(|value| !value.trim().is_empty()) else {
            missing = true;
            break;
        };
        rendered.replace_range(open..=close, &value);
        start = open + value.len();
    }
    if missing {
        return Ok(None);
    }
    Ok(Some(Value::String(rendered)))
}

/// Extract a provider message id from a JSON response by dotted
/// path (`"id"`, `"requestId"`, `"data.id"`). `None` when the path
/// is absent or the value is not a string.
fn extract_id_path(body: &Value, path: &str) -> Option<String> {
    let mut current = body;
    for segment in path.split('.') {
        current = current.get(segment)?;
    }
    current.as_str().map(str::to_string)
}

#[async_trait]
impl EmailProvider for GenericHttpAdapter {
    fn kind(&self) -> EmailProviderKind {
        self.connection.kind
    }

    fn connection(&self) -> &EmailConnection {
        &self.connection
    }

    /// No dedicated ping endpoint exists for these APIs, so the
    /// test is a GET on the API base with the stored credentials:
    /// any HTTP response proves reachability (the status is the
    /// detail); only a transport failure is unreachable.
    async fn test_connection(&self) -> UnifiedResult<ConnectionTestResult> {
        let started = std::time::Instant::now();
        let api_key = require_api_key(&self.connection)?;
        let def = self.kind().definition().ok_or_else(|| {
            UnifiedError::internal(format!(
                "No provider definition registered for kind \"{}\"",
                self.kind().as_str()
            ))
        })?;
        let http_api = def.http_api.as_ref().ok_or_else(|| {
            UnifiedError::internal(format!(
                "Provider \"{}\" has no http_api configuration",
                self.kind().as_str()
            ))
        })?;
        let base = def.effective_base_url(false).ok_or_else(|| {
            UnifiedError::internal(format!(
                "Provider \"{}\" has no base URL",
                self.kind().as_str()
            ))
        })?;
        let url = Url::parse(base)
            .map_err(|_| UnifiedError::internal("Invalid provider API base URL"))?;
        check_allowlist(&url, self.kind())?;
        let (header, value) = generic_http_auth(http_api, &api_key)?;
        match self.client.get(url).header(header, value).send().await {
            Ok(response) => Ok(ConnectionTestResult {
                kind: self.kind().as_str().to_string(),
                reachable: true,
                latency_ms: started.elapsed().as_millis() as u64,
                account: None,
                detail: format!(
                    "{} answered HTTP {}",
                    self.kind().as_str(),
                    response.status()
                ),
            }),
            Err(error) => Ok(ConnectionTestResult {
                kind: self.kind().as_str().to_string(),
                reachable: false,
                latency_ms: started.elapsed().as_millis() as u64,
                account: None,
                detail: format!("{} request failed: {error}", self.kind().as_str()),
            }),
        }
    }

    async fn send(&self, message: &EmailMessage) -> UnifiedResult<SendResult> {
        validate_email_address(&message.to)?;
        if message.test_mode && !self.kind().has_native_sandbox() {
            return Ok(simulated_result(self.kind().as_str()));
        }
        let api_key = require_api_key(&self.connection)?;
        let (from, _) = message.sender(&self.connection);
        if from.is_empty() {
            return Err(UnifiedError::bad_request(
                "A sender email is required (set sender_email on the connection or from on the message)",
            ));
        }
        let def = self.kind().definition().ok_or_else(|| {
            UnifiedError::internal(format!(
                "No provider definition registered for kind \"{}\"",
                self.kind().as_str()
            ))
        })?;
        let http_api = def.http_api.as_ref().ok_or_else(|| {
            UnifiedError::internal(format!(
                "Provider \"{}\" has no http_api configuration",
                self.kind().as_str()
            ))
        })?;
        let template = http_api.get("payload").ok_or_else(|| {
            UnifiedError::internal(format!(
                "Provider \"{}\" has no http_api payload template",
                self.kind().as_str()
            ))
        })?;
        let template_text = template.to_string();
        if template_text.contains("{template_id}") && message.template_id.is_none() {
            return Err(UnifiedError::bad_request(format!(
                "Provider \"{}\" requires a template_id (create a published template in the provider dashboard and pass its id)",
                self.kind().as_str()
            )));
        }
        if message.template_id.is_some() && !template_text.contains("{template_id}") {
            return Err(UnifiedError::bad_request(format!(
                "Provider \"{}\" does not support template sends; provide a subject and body_text/body_html instead",
                self.kind().as_str()
            )));
        }
        let endpoint = http_api
            .get("endpoint")
            .and_then(Value::as_str)
            .ok_or_else(|| {
                UnifiedError::internal(format!(
                    "Provider \"{}\" has no http_api endpoint",
                    self.kind().as_str()
                ))
            })?;
        let url = Url::parse(endpoint)
            .map_err(|_| UnifiedError::internal("Invalid provider API endpoint"))?;
        check_allowlist(&url, self.kind())?;
        let payload = render_http_template_value(template, message, &self.connection)?
            .unwrap_or(Value::Null);
        let (header, value) = generic_http_auth(http_api, &api_key)?;
        let body = send_json(
            &self.client,
            self.client
                .post(url)
                .header(header, value)
                .header(CONTENT_TYPE, "application/json")
                .json(&payload),
            self.kind().as_str(),
        )
        .await?;
        Ok(SendResult {
            provider_message_id: http_api
                .get("id_path")
                .and_then(Value::as_str)
                .and_then(|path| extract_id_path(&body, path)),
            accepted: true,
            test_mode: message.test_mode,
            detail: format!("{} accepted the message", self.kind().as_str()),
        })
    }
}

/// The Authorization header for a generic HTTP provider, from the
/// definition's `http_api.auth`: `"bearer"` sends
/// `Bearer <api_key>`; `"basic-key"` sends `Basic <api_key>` (the
/// raw key, not a base64 user:password pair — OneSignal's REST API
/// key works this way).
fn generic_http_auth(
    http_api: &Value,
    api_key: &str,
) -> UnifiedResult<(reqwest::header::HeaderName, String)> {
    let auth = http_api.get("auth");
    let scheme = auth
        .and_then(|value| value.get("scheme"))
        .and_then(Value::as_str)
        .unwrap_or("bearer");
    let header_name = auth
        .and_then(|value| value.get("header"))
        .and_then(Value::as_str)
        .unwrap_or("Authorization");
    let header = header_name
        .parse::<reqwest::header::HeaderName>()
        .map_err(|_| {
            UnifiedError::internal(format!(
                "Invalid http_api auth header \"{header_name}\""
            ))
        })?;
    let value = match scheme {
        "basic-key" => format!("Basic {api_key}"),
        "bearer" => format!("Bearer {api_key}"),
        other => {
            return Err(UnifiedError::internal(format!(
                "Unsupported http_api auth scheme \"{other}\""
            )));
        }
    };
    Ok((header, value))
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    fn mailgun_connection() -> EmailConnection {
        let now = Utc::now();
        EmailConnection {
            id: Uuid::now_v7(),
            name: "test-mailgun".to_string(),
            kind: EmailProviderKind::Mailgun,
            domain: Some("example.com".to_string()),
            region: None,
            sender_email: None,
            sender_name: None,
            webhook_url: None,
            unsubscribe_url: None,
            smtp_host: None,
            smtp_port: None,
            smtp_tls: None,
            smtp_username: None,
            credentials: EmailCredentials {
                api_key: Some("key-test".to_string()),
                access_key_id: None,
                secret_access_key: None,
                webhook_secret: Some("webhook-secret".to_string()),
                password: None,
                client_id: None,
                client_secret: None,
                refresh_token: None,
                access_token: None,
                access_token_expires_at: None,
            },
            test_mode: false,
            enabled: true,
            last_tested_at: None,
            last_status: None,
            created_at: now,
            updated_at: now,
        }
    }

    fn smtp_connection() -> EmailConnection {
        let now = Utc::now();
        EmailConnection {
            id: Uuid::now_v7(),
            name: "test-smtp".to_string(),
            kind: EmailProviderKind::Smtp,
            domain: None,
            region: None,
            sender_email: Some("news@example.com".to_string()),
            sender_name: Some("Example News".to_string()),
            webhook_url: None,
            unsubscribe_url: None,
            smtp_host: Some("smtp.example.com".to_string()),
            smtp_port: Some(587),
            smtp_tls: Some("starttls".to_string()),
            smtp_username: None,
            credentials: EmailCredentials::default(),
            test_mode: false,
            enabled: true,
            last_tested_at: None,
            last_status: None,
            created_at: now,
            updated_at: now,
        }
    }

    fn oauth_connection(kind: EmailProviderKind) -> EmailConnection {
        let now = Utc::now();
        EmailConnection {
            id: Uuid::now_v7(),
            name: format!("test-{}", kind.as_str()),
            kind,
            domain: None,
            region: None,
            sender_email: Some("user@example.com".to_string()),
            sender_name: None,
            webhook_url: None,
            unsubscribe_url: None,
            smtp_host: None,
            smtp_port: None,
            smtp_tls: None,
            smtp_username: None,
            credentials: EmailCredentials {
                api_key: None,
                access_key_id: None,
                secret_access_key: None,
                webhook_secret: None,
                password: None,
                client_id: Some("client-id-1".to_string()),
                client_secret: None,
                refresh_token: None,
                access_token: None,
                access_token_expires_at: None,
            },
            test_mode: false,
            enabled: true,
            last_tested_at: None,
            last_status: None,
            created_at: now,
            updated_at: now,
        }
    }

    fn test_message() -> EmailMessage {
        EmailMessage {
            to: "reader@example.com".to_string(),
            from: None,
            from_name: None,
            subject: "Test".to_string(),
            body_text: Some("Hello".to_string()),
            body_html: Some("<p>Hello</p>".to_string()),
            reply_to: None,
            template_id: None,
            variables: HashMap::new(),
            metadata: HashMap::new(),
            bulk: false,
            include_unsubscribe: false,
            deliverability_opt_out: false,
            test_mode: false,
        }
    }

    fn mailgun_body(timestamp: &str, token: &str) -> Vec<u8> {
        let signature = hex_encode(&hmac_sha256(
            b"webhook-secret",
            format!("{timestamp}{token}").as_bytes(),
        ));
        format!("timestamp={timestamp}&token={token}&signature={signature}").into_bytes()
    }

    #[tokio::test]
    async fn mailgun_webhook_verify_rejects_stale_and_non_numeric_timestamps() {
        let adapter = MailgunAdapter::new(&mailgun_connection()).unwrap();
        let headers = HeaderMap::new();
        let now = Utc::now().timestamp();
        // A fresh webhook with a valid signature verifies.
        let fresh = mailgun_body(&now.to_string(), "token-fresh");
        assert!(adapter.webhook_verify(&headers, &fresh).await.unwrap());
        // The same signature one hour later is a replay: refused even
        // though the HMAC still matches.
        let stale = mailgun_body(&(now - 3600).to_string(), "token-stale");
        assert!(!adapter.webhook_verify(&headers, &stale).await.unwrap());
        // A non-numeric timestamp is refused outright.
        let garbled = mailgun_body("not-a-timestamp", "token-garbled");
        assert!(!adapter.webhook_verify(&headers, &garbled).await.unwrap());
    }

    #[test]
    fn build_lettre_message_covers_body_shapes() {
        let connection = smtp_connection();
        // Both bodies: multipart alternative builds.
        assert!(build_lettre_message(&connection, &test_message(), None).is_ok());
        // Text only and HTML only both build.
        let mut text_only = test_message();
        text_only.body_html = None;
        assert!(build_lettre_message(&connection, &text_only, None).is_ok());
        let mut html_only = test_message();
        html_only.body_text = None;
        assert!(build_lettre_message(&connection, &html_only, None).is_ok());
        // No body at all is an honest error.
        let mut empty = test_message();
        empty.body_text = None;
        empty.body_html = None;
        assert!(build_lettre_message(&connection, &empty, None).is_err());
        // No sender anywhere is an honest error unless a default is given.
        let mut no_sender = smtp_connection();
        no_sender.sender_email = None;
        let mut message = test_message();
        message.from = None;
        assert!(build_lettre_message(&no_sender, &message, None).is_err());
        assert!(build_lettre_message(&no_sender, &message, Some("unified@localhost")).is_ok());
        // An invalid recipient is rejected by the parser.
        let mut bad_to = test_message();
        bad_to.to = "not an address".to_string();
        assert!(build_lettre_message(&connection, &bad_to, None).is_err());
    }

    #[test]
    fn smtp_adapter_rejects_a_connection_without_host() {
        let mut connection = smtp_connection();
        connection.smtp_host = None;
        let err = SmtpAdapter::new(&connection).unwrap_err();
        assert!(err.to_string().contains("smtp_host"));
    }

    #[test]
    fn smtp_adapter_accepts_valid_server_settings() {
        // Building the adapter only validates settings (no network):
        // an unreachable host still builds; the connection test is
        // what reports it.
        assert!(SmtpAdapter::new(&smtp_connection()).is_ok());
        let mut implicit = smtp_connection();
        implicit.smtp_port = Some(465);
        implicit.smtp_tls = Some("implicit".to_string());
        assert!(SmtpAdapter::new(&implicit).is_ok());
        let mut plain = smtp_connection();
        plain.smtp_tls = Some("none".to_string());
        assert!(SmtpAdapter::new(&plain).is_ok());
    }

    #[tokio::test]
    async fn smtp_test_connection_reports_an_unreachable_host() {
        // No SMTP server listens on smtp.example.com:587 from the
        // test runner, so the test must report unreachable — never
        // a 500, never a hang (the transport timeout applies).
        let adapter = SmtpAdapter::new(&smtp_connection()).unwrap();
        let result = adapter.test_connection().await.unwrap();
        assert!(!result.reachable);
        assert!(result.detail.contains("smtp.example.com"));
    }

    #[tokio::test]
    async fn oauth_adapter_requires_a_linked_account() {
        // No tokens stored: the refresh path must refuse with a
        // setup error naming the missing link, without touching
        // the network.
        for kind in [
            EmailProviderKind::Gmail,
            EmailProviderKind::Yahoo,
            EmailProviderKind::Outlook,
        ] {
            let adapter = OAuthSmtpAdapter::new(&oauth_connection(kind)).unwrap();
            let err = adapter.ensure_fresh_token().await.unwrap_err();
            assert!(
                err.to_string().contains("not linked"),
                "kind {}: {}",
                kind.as_str(),
                err
            );
            let result = adapter.test_connection().await.unwrap();
            assert!(!result.reachable);
            assert!(result.detail.contains("not linked"));
        }
    }

    #[tokio::test]
    async fn sendmail_test_connection_matches_binary_presence() {
        // Deterministic on any machine: the result must agree with
        // the probe, and the detail must name the binary (or its
        // absence) either way.
        let adapter = SendmailAdapter::new(&smtp_connection()).unwrap();
        let result = adapter.test_connection().await.unwrap();
        assert_eq!(
            result.reachable,
            SendmailAdapter::sendmail_binary().is_some()
        );
        if result.reachable {
            assert!(result.detail.contains("Sendmail binary found"));
        } else {
            assert!(result.detail.contains("No sendmail binary"));
        }
    }

    #[tokio::test]
    async fn sendmail_send_without_a_binary_fails_closed() {
        // When no binary exists, send must fail with a 502 naming
        // the missing binary — never a panic, never a silent drop.
        if SendmailAdapter::sendmail_binary().is_some() {
            return;
        }
        let mut connection = smtp_connection();
        connection.kind = EmailProviderKind::Sendmail;
        let adapter = SendmailAdapter::new(&connection).unwrap();
        let err = adapter.send(&test_message()).await.unwrap_err();
        assert!(err.to_string().contains("No sendmail binary"));
    }

    // --- Task 354: preset resolution and generic HTTP rendering ---

    #[test]
    fn smtp_preset_resolution_prefers_connection_then_preset_then_default() {
        // A Zoho connection with no host of its own resolves to the
        // catalog preset (465, implicit SSL).
        let mut connection = smtp_connection();
        connection.kind = EmailProviderKind::Zoho;
        connection.smtp_host = None;
        connection.smtp_port = None;
        connection.smtp_tls = None;
        let (host, port, tls) = SmtpAdapter::resolve_server(&connection).unwrap();
        assert_eq!(host, "smtp.zoho.com");
        assert_eq!(port, 465);
        assert_eq!(tls, "implicit");
        // The connection's own host wins over the preset.
        connection.smtp_host = Some("relay.example.com".to_string());
        connection.smtp_port = Some(2525);
        let (host, port, tls) = SmtpAdapter::resolve_server(&connection).unwrap();
        assert_eq!(host, "relay.example.com");
        assert_eq!(port, 2525);
        assert_eq!(tls, "implicit");
        // A kind with neither a host nor a preset fails loudly.
        let mut bare = smtp_connection();
        bare.kind = EmailProviderKind::Smtp;
        bare.smtp_host = None;
        bare.smtp_port = None;
        bare.smtp_tls = None;
        let err = SmtpAdapter::resolve_server(&bare).unwrap_err();
        assert!(err.to_string().contains("no smtp_host configured"));
    }

    #[test]
    fn generic_http_template_renders_plunk_payload() {
        let def = EmailProviderKind::Plunk
            .definition()
            .expect("plunk definition");
        let template = def
            .http_api
            .as_ref()
            .expect("plunk http_api")
            .get("payload")
            .expect("plunk payload template")
            .clone();
        let connection = {
            let mut connection = smtp_connection();
            connection.kind = EmailProviderKind::Plunk;
            connection
        };
        // Transactional: no deliverability headers, so the whole
        // "headers" key is dropped from the payload.
        let rendered = render_http_template_value(&template, &test_message(), &connection)
            .unwrap()
            .unwrap();
        assert_eq!(rendered["to"], "reader@example.com");
        assert_eq!(rendered["subject"], "Test");
        assert_eq!(rendered["body"], "<p>Hello</p>");
        assert_eq!(rendered["from"], "news@example.com");
        assert_eq!(rendered["name"], "Example News");
        assert!(rendered.get("headers").is_none());
        // Bulk: the headers object is emitted and carries the
        // unsubscribe header.
        let mut bulk = test_message();
        bulk.bulk = true;
        let rendered = render_http_template_value(&template, &bulk, &connection)
            .unwrap()
            .unwrap();
        let headers = rendered.get("headers").expect("bulk headers");
        assert!(headers
            .get("List-Unsubscribe")
            .is_some_and(|value| value.as_str().is_some_and(|text| text
                .contains("mailto:news@example.com"))));
        assert_eq!(headers["Precedence"], "bulk");
    }

    #[test]
    fn generic_http_template_drops_missing_keys_and_rejects_unknown_placeholders() {
        let connection = smtp_connection();
        let template = json!({
            "to": "{to}",
            "subject": "{subject}",
            "reply_to": "{reply_to}",
            "template_id": "{template_id}",
            "variables": "{variables}",
        });
        let mut message = test_message();
        message.variables.insert("name".to_string(), json!("Reader"));
        let rendered =
            render_http_template_value(&template, &message, &connection)
                .unwrap()
                .unwrap();
        assert_eq!(rendered["to"], "reader@example.com");
        // Missing optional placeholders drop their keys.
        assert!(rendered.get("reply_to").is_none());
        assert!(rendered.get("template_id").is_none());
        // The whole-value {variables} becomes the variables object.
        assert_eq!(rendered["variables"]["name"], "Reader");
        // An unknown placeholder is an honest 400, not a passthrough.
        let bad = json!({ "to": "{unknown_field}" });
        let err = render_http_template_value(&bad, &message, &connection).unwrap_err();
        assert!(err.to_string().contains("Unknown placeholder {unknown_field}"));
        // An unmatched brace is an honest 400 too.
        let bad = json!({ "to": "reader@example.com {to" });
        let err = render_http_template_value(&bad, &message, &connection).unwrap_err();
        assert!(err.to_string().contains("Unmatched '{'"));
    }

    #[test]
    fn generic_http_auth_uses_bearer_or_raw_basic_key() {
        let bearer = json!({ "scheme": "bearer", "header": "Authorization" });
        let (header, value) = generic_http_auth(&bearer, "sk_test").unwrap();
        assert_eq!(header.as_str(), "authorization");
        assert_eq!(value, "Bearer sk_test");
        // OneSignal: the REST API key is sent raw after "Basic",
        // not as a base64 user:password pair.
        let basic = json!({ "scheme": "basic-key", "header": "Authorization" });
        let (_, value) = generic_http_auth(&basic, "os_rest_key").unwrap();
        assert_eq!(value, "Basic os_rest_key");
        let bad = json!({ "scheme": "digest" });
        assert!(generic_http_auth(&bad, "key").is_err());
    }

    #[test]
    fn generic_http_id_path_extraction_reads_nested_paths() {
        let body = json!({ "id": "one-1", "requestId": "req-2", "data": { "id": "nested-3" } });
        assert_eq!(extract_id_path(&body, "id").as_deref(), Some("one-1"));
        assert_eq!(
            extract_id_path(&body, "requestId").as_deref(),
            Some("req-2")
        );
        assert_eq!(
            extract_id_path(&body, "data.id").as_deref(),
            Some("nested-3")
        );
        assert!(extract_id_path(&body, "missing").is_none());
    }
}
