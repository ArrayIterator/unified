//! email_providers - adapters.

use unified_core::error::{UnifiedError, UnifiedResult};
use async_trait::async_trait;
use base64::Engine;
use chrono::{Utc};
use hmac::{Hmac, Mac};
use reqwest::header::{HeaderMap, AUTHORIZATION, CONTENT_TYPE};
use serde_json::{json, Value};
use sha1::Sha1;
use std::collections::HashMap;
use url::Url;
use super::{events::*, provider_kinds::*, credentials::*, store::*, http::*, registry::*, worker::*, hooks::*, handlers::*, module::*};



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
        if let Some(reply_to) = &message.reply_to {
            mandrill_message["headers"] = json!({ "Reply-To": reply_to });
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

    async fn webhook_verify(
        &self,
        headers: &HeaderMap,
        raw_body: &[u8],
    ) -> UnifiedResult<bool> {
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
        let expected = base64::engine::general_purpose::STANDARD.encode(mac.finalize().into_bytes());
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
            account: body.get("email").and_then(|v| v.as_str()).map(str::to_string),
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
        if !message.metadata.is_empty() {
            payload["headers"] = serde_json::to_value(&message.metadata).unwrap_or(Value::Null);
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
            return Err(UnifiedError::bad_request(
                "Provide body_text or body_html",
            ));
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

    async fn webhook_verify(
        &self,
        _headers: &HeaderMap,
        raw_body: &[u8],
    ) -> UnifiedResult<bool> {
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
            account: body.get("Name").and_then(|v| v.as_str()).map(str::to_string),
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
            return Err(UnifiedError::new(format!("Postmark rejected the message: {reason}"), 502));
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
        if !url
            .host_str()
            .unwrap_or("")
            .ends_with(".amazonaws.com")
        {
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
        let source = match from_name.filter(|name| !name.trim().is_empty()) {
            Some(name) => format!("{name} <{from}>"),
            None => from.clone(),
        };
        let mut params: Vec<(String, String)> = vec![
            ("Action".to_string(), "SendEmail".to_string()),
            ("Version".to_string(), "2010-12-01".to_string()),
            ("Source".to_string(), source),
            ("Destination.ToAddresses.member.1".to_string(), message.to.clone()),
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
            params.push((
                format!("Tags.member.{}.Name", index + 1),
                key.clone(),
            ));
            params.push((
                format!("Tags.member.{}.Value", index + 1),
                value.clone(),
            ));
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

    async fn webhook_verify(
        &self,
        headers: &HeaderMap,
        raw_body: &[u8],
    ) -> UnifiedResult<bool> {
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
        let signed = format!(
            "{id}.{timestamp}.{}",
            String::from_utf8_lossy(raw_body)
        );
        let expected = base64::engine::general_purpose::STANDARD
            .encode(hmac_sha256(&key, signed.as_bytes()));
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

pub struct SmtpAdapter {
    pub(crate) connection: EmailConnection,
}


impl SmtpAdapter {
    pub(crate) fn new(connection: &EmailConnection) -> UnifiedResult<Self> {
        Ok(Self {
            connection: connection.clone(),
        })
    }
}


#[async_trait]
impl EmailProvider for SmtpAdapter {
    fn kind(&self) -> EmailProviderKind {
        EmailProviderKind::Smtp
    }

    fn connection(&self) -> &EmailConnection {
        &self.connection
    }

    async fn test_connection(&self) -> UnifiedResult<ConnectionTestResult> {
        let started = std::time::Instant::now();
        let Some(hooks) = email_hooks() else {
            return Ok(ConnectionTestResult {
                kind: self.kind().as_str().to_string(),
                reachable: false,
                latency_ms: started.elapsed().as_millis() as u64,
                account: None,
                detail: "Mail Manager hook bus is not available".to_string(),
            });
        };
        let registered = hooks.has(EVENT_MAIL_SEND_REQUESTED);
        Ok(ConnectionTestResult {
            kind: self.kind().as_str().to_string(),
            reachable: registered,
            latency_ms: started.elapsed().as_millis() as u64,
            account: None,
            detail: if registered {
                "Mail Manager answers mail.send_requested; use test-send for a real delivery check".to_string()
            } else {
                "Mail Manager does not answer mail.send_requested (mail module is not registered)".to_string()
            },
        })
    }

    async fn send(&self, message: &EmailMessage) -> UnifiedResult<SendResult> {
        validate_email_address(&message.to)?;
        if message.test_mode {
            return Ok(simulated_result("smtp"));
        }
        let Some(hooks) = email_hooks() else {
            return Err(UnifiedError::new(
                "Mail Manager is unavailable (no hook bus)",
                503,
            ));
        };
        let (from, _) = message.sender(&self.connection);
        let answer = hooks.emit(
            EVENT_MAIL_SEND_REQUESTED,
            json!({
                "to": message.to,
                "subject": message.subject,
                "body_text": message.body_text,
                "body_html": message.body_html,
                "template_id": message.template_id,
                "variables": message.variables,
                "from": from,
            }),
        );
        match answer.results.first() {
            Some(result) if result.get("error").is_none() => Ok(SendResult {
                provider_message_id: result
                    .get("job_id")
                    .and_then(|value| value.as_str())
                    .map(str::to_string),
                accepted: true,
                test_mode: false,
                detail: "Mail Manager accepted the message for SMTP delivery".to_string(),
            }),
            Some(result) => {
                let reason = result
                    .get("error")
                    .and_then(|value| value.as_str())
                    .unwrap_or("mail service rejected the request");
                Err(UnifiedError::new(
                    format!("Mail Manager rejected the message: {reason}"),
                    502,
                ))
            }
            None => Err(UnifiedError::new(
                "Mail Manager did not answer the mail.send_requested event: no mail handler is registered",
                503,
            )),
        }
    }

    async fn webhook_verify(
        &self,
        _headers: &HeaderMap,
        _raw_body: &[u8],
    ) -> UnifiedResult<bool> {
        Err(unsupported(self.kind(), "webhook verification", "webhooks"))
    }
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
            credentials: EmailCredentials {
                api_key: Some("key-test".to_string()),
                access_key_id: None,
                secret_access_key: None,
                webhook_secret: Some("webhook-secret".to_string()),
            },
            test_mode: false,
            enabled: true,
            last_tested_at: None,
            last_status: None,
            created_at: now,
            updated_at: now,
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
}
