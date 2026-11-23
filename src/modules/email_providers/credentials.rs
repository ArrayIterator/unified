//! email_providers - credentials.

use unified_core::error::{UnifiedError, UnifiedResult};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;
use super::{events::*, provider_kinds::*, store::*, http::*, registry::*, adapters::*, worker::*, hooks::*, handlers::*, module::*};



#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
// ---------------------------------------------------------------------------
// Credentials and connections.
// ---------------------------------------------------------------------------
/// Credentials for one connection. Every field is optional at the type
/// level; `validate_connection` enforces the per-kind required set.
/// Values are write-only: they are never serialized into a view, never
/// logged, and never included in hook payloads.
pub struct EmailCredentials {
    #[serde(default)]
    pub api_key: Option<String>,
    #[serde(default)]
    pub access_key_id: Option<String>,
    #[serde(default)]
    pub secret_access_key: Option<String>,
    #[serde(default)]
    pub webhook_secret: Option<String>,
}


impl EmailCredentials {
    pub(crate) fn configured_fields(&self) -> Vec<&'static str> {
        let mut fields = Vec::new();
        let present = |value: &Option<String>| {
            value.as_deref().map(|v| !v.trim().is_empty()).unwrap_or(false)
        };
        if present(&self.api_key) {
            fields.push("api_key");
        }
        if present(&self.access_key_id) {
            fields.push("access_key_id");
        }
        if present(&self.secret_access_key) {
            fields.push("secret_access_key");
        }
        if present(&self.webhook_secret) {
            fields.push("webhook_secret");
        }
        fields
    }
}


/// One stored email provider connection. Kept in memory (same pattern
/// as the cloud provider connections); durable persistence is a
/// follow-up for the integrator (see the migration sketch in the
/// coordinator report).
#[derive(Debug, Clone)]
pub struct EmailConnection {
    pub id: Uuid,
    pub name: String,
    pub kind: EmailProviderKind,
    pub domain: Option<String>,
    pub region: Option<String>,
    pub sender_email: Option<String>,
    pub sender_name: Option<String>,
    /// Exact public webhook URL (needed to verify Mandrill signatures).
    pub webhook_url: Option<String>,
    pub credentials: EmailCredentials,
    pub test_mode: bool,
    pub enabled: bool,
    pub last_tested_at: Option<DateTime<Utc>>,
    pub last_status: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}


/// Masked read view of a connection: credentials are never returned.
#[derive(Debug, Clone, Serialize)]
pub struct EmailConnectionView {
    pub id: Uuid,
    pub name: String,
    pub kind: String,
    pub kind_display: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub domain: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub region: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sender_email: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sender_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub webhook_url: Option<String>,
    pub configured_credentials: Vec<String>,
    pub capabilities: EmailCapabilities,
    pub test_mode: bool,
    pub enabled: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_tested_at: Option<DateTime<Utc>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_status: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}


impl EmailConnection {
    pub(crate) fn view(&self) -> EmailConnectionView {
        EmailConnectionView {
            id: self.id,
            name: self.name.clone(),
            kind: self.kind.as_str().to_string(),
            kind_display: self.kind.display_name().to_string(),
            domain: self.domain.clone(),
            region: self.region.clone(),
            sender_email: self.sender_email.clone(),
            sender_name: self.sender_name.clone(),
            webhook_url: self.webhook_url.clone(),
            configured_credentials: self
                .credentials
                .configured_fields()
                .into_iter()
                .map(str::to_string)
                .collect(),
            capabilities: capabilities_for(self.kind),
            test_mode: self.test_mode,
            enabled: self.enabled,
            last_tested_at: self.last_tested_at,
            last_status: self.last_status.clone(),
            created_at: self.created_at,
            updated_at: self.updated_at,
        }
    }
}


#[derive(Debug, Clone, Deserialize)]
pub struct CreateConnectionRequest {
    pub name: String,
    pub kind: String,
    #[serde(default)]
    pub domain: Option<String>,
    #[serde(default)]
    pub region: Option<String>,
    #[serde(default)]
    pub sender_email: Option<String>,
    #[serde(default)]
    pub sender_name: Option<String>,
    #[serde(default)]
    pub webhook_url: Option<String>,
    #[serde(default)]
    pub api_key: Option<String>,
    #[serde(default)]
    pub access_key_id: Option<String>,
    #[serde(default)]
    pub secret_access_key: Option<String>,
    #[serde(default)]
    pub webhook_secret: Option<String>,
    #[serde(default)]
    pub test_mode: Option<bool>,
    #[serde(default)]
    pub enabled: Option<bool>,
}


#[derive(Debug, Clone, Deserialize)]
pub struct UpdateConnectionRequest {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub domain: Option<String>,
    #[serde(default)]
    pub region: Option<String>,
    #[serde(default)]
    pub sender_email: Option<String>,
    #[serde(default)]
    pub sender_name: Option<String>,
    #[serde(default)]
    pub webhook_url: Option<String>,
    #[serde(default)]
    pub api_key: Option<String>,
    #[serde(default)]
    pub access_key_id: Option<String>,
    #[serde(default)]
    pub secret_access_key: Option<String>,
    #[serde(default)]
    pub webhook_secret: Option<String>,
    #[serde(default)]
    pub test_mode: Option<bool>,
    #[serde(default)]
    pub enabled: Option<bool>,
}


pub(crate) fn validate_connection(input: &CreateConnectionRequest) -> UnifiedResult<EmailProviderKind> {
    let kind = EmailProviderKind::from_str(&input.kind).ok_or_else(|| {
        UnifiedError::bad_request(format!("Unknown email provider kind \"{}\"", input.kind))
    })?;
    if input.name.trim().is_empty() {
        return Err(UnifiedError::bad_request("Connection name is required"));
    }
    let has = |value: &Option<String>| {
        value.as_deref().map(|v| !v.trim().is_empty()).unwrap_or(false)
    };
    let missing = |field: &str| {
        UnifiedError::bad_request(format!(
            "Provider \"{}\" requires credential field \"{field}\"",
            kind.as_str()
        ))
    };
    match kind {
        EmailProviderKind::Mailchimp
        | EmailProviderKind::Brevo
        | EmailProviderKind::SendGrid
        | EmailProviderKind::Postmark
        | EmailProviderKind::Resend
        | EmailProviderKind::SparkPost => {
            if !has(&input.api_key) {
                return Err(missing("api_key"));
            }
        }
        EmailProviderKind::Mailgun => {
            if !has(&input.api_key) {
                return Err(missing("api_key"));
            }
            if !has(&input.domain) {
                return Err(UnifiedError::bad_request(
                    "Provider \"mailgun\" requires domain (the verified sending domain)",
                ));
            }
        }
        EmailProviderKind::Ses => {
            if !has(&input.access_key_id) {
                return Err(missing("access_key_id"));
            }
            if !has(&input.secret_access_key) {
                return Err(missing("secret_access_key"));
            }
            if !has(&input.region) {
                return Err(UnifiedError::bad_request(
                    "Provider \"ses\" requires region",
                ));
            }
        }
        EmailProviderKind::Smtp => {}
    }
    if let Some(email) = &input.sender_email {
        if !email.trim().is_empty() && !email.contains('@') {
            return Err(UnifiedError::bad_request(
                "sender_email is not a valid email address",
            ));
        }
    }
    if let Some(region) = &input.region {
        if kind == EmailProviderKind::Mailgun
            && !region.trim().is_empty()
            && !matches!(region.trim(), "us" | "eu")
        {
            return Err(UnifiedError::bad_request(
                "Mailgun region must be \"us\" or \"eu\"",
            ));
        }
        if kind == EmailProviderKind::SparkPost
            && !region.trim().is_empty()
            && !matches!(region.trim(), "us" | "eu")
        {
            return Err(UnifiedError::bad_request(
                "SparkPost region must be \"us\" or \"eu\"",
            ));
        }
    }
    Ok(kind)
}
