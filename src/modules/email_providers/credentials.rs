//! email_providers - credentials.

use super::{
    adapters::*, events::*, handlers::*, hooks::*, http::*, module::*, provider_kinds::*,
    registry::*, store::*, worker::*,
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use unified_core::error::{UnifiedError, UnifiedResult};
use uuid::Uuid;

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
// ---------------------------------------------------------------------------
// Credentials and connections.
// ---------------------------------------------------------------------------
/// Credentials for one connection. Every field is optional at the type
/// level; `validate_connection` enforces the per-kind required set.
/// Values are write-only: they are never serialized into a view, never
/// logged, and never included in hook payloads.
///
/// Two generations live side by side here. The pre-existing API
/// fields (`api_key`, `access_key_id`, `secret_access_key`,
/// `webhook_secret`) keep their original write-only treatment. The
/// secrets added by task 350 (`password`, `client_secret`,
/// `refresh_token`, `access_token`) are stored ENCRYPTED (see
/// `crypto.rs`, `v1.` format, key `app.secret_key`); adapters
/// decrypt them at use time and fail closed when no secret is
/// configured. `client_id` is not a secret and is stored as-is.
pub struct EmailCredentials {
    #[serde(default)]
    pub api_key: Option<String>,
    #[serde(default)]
    pub access_key_id: Option<String>,
    #[serde(default)]
    pub secret_access_key: Option<String>,
    #[serde(default)]
    pub webhook_secret: Option<String>,
    /// SMTP password for the generic SMTP kind (encrypted at rest).
    #[serde(default)]
    pub password: Option<String>,
    /// OAuth client id (Gmail / Yahoo / Outlook). Not a secret.
    #[serde(default)]
    pub client_id: Option<String>,
    /// OAuth client secret (encrypted at rest).
    #[serde(default)]
    pub client_secret: Option<String>,
    /// OAuth refresh token, set by the authorization callback
    /// (encrypted at rest). Its presence means the account is linked.
    #[serde(default)]
    pub refresh_token: Option<String>,
    /// OAuth access token, set by the callback or a refresh
    /// (encrypted at rest). Short-lived; the adapter refreshes it.
    #[serde(default)]
    pub access_token: Option<String>,
    /// When the stored access token expires (UTC). `None` means
    /// unknown, which the adapter treats as expired.
    #[serde(default)]
    pub access_token_expires_at: Option<DateTime<Utc>>,
}

impl EmailCredentials {
    pub(crate) fn configured_fields(&self) -> Vec<&'static str> {
        let mut fields = Vec::new();
        let present = |value: &Option<String>| {
            value
                .as_deref()
                .map(|v| !v.trim().is_empty())
                .unwrap_or(false)
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
        if present(&self.password) {
            fields.push("password");
        }
        if present(&self.client_id) {
            fields.push("client_id");
        }
        if present(&self.client_secret) {
            fields.push("client_secret");
        }
        if present(&self.refresh_token) {
            fields.push("refresh_token");
        }
        if present(&self.access_token) {
            fields.push("access_token");
        }
        fields
    }

    /// True when an OAuth account is linked: a refresh token is
    /// stored, so the adapter can obtain fresh access tokens. The
    /// token value itself is never exposed; see the connection view's
    /// `oauth_linked` flag.
    pub(crate) fn oauth_linked(&self) -> bool {
        self.refresh_token
            .as_deref()
            .map(|value| !value.trim().is_empty())
            .unwrap_or(false)
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
    /// Generic SMTP server (kind `smtp` only): host, port, TLS mode
    /// (`starttls` | `implicit` | `none`), and the auth username.
    /// The password lives in `credentials.password` (encrypted).
    pub smtp_host: Option<String>,
    pub smtp_port: Option<u16>,
    pub smtp_tls: Option<String>,
    pub smtp_username: Option<String>,
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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub smtp_host: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub smtp_port: Option<u16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub smtp_tls: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub smtp_username: Option<String>,
    pub configured_credentials: Vec<String>,
    pub capabilities: EmailCapabilities,
    /// True when an OAuth account is linked to this connection (a
    /// refresh token is stored). The token itself is never returned.
    pub oauth_linked: bool,
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
            smtp_host: self.smtp_host.clone(),
            smtp_port: self.smtp_port,
            smtp_tls: self.smtp_tls.clone(),
            smtp_username: self.smtp_username.clone(),
            configured_credentials: self
                .credentials
                .configured_fields()
                .into_iter()
                .map(str::to_string)
                .collect(),
            capabilities: capabilities_for(self.kind),
            oauth_linked: self.credentials.oauth_linked(),
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
    pub smtp_host: Option<String>,
    #[serde(default)]
    pub smtp_port: Option<u16>,
    #[serde(default)]
    pub smtp_tls: Option<String>,
    #[serde(default)]
    pub smtp_username: Option<String>,
    #[serde(default)]
    pub api_key: Option<String>,
    #[serde(default)]
    pub access_key_id: Option<String>,
    #[serde(default)]
    pub secret_access_key: Option<String>,
    #[serde(default)]
    pub webhook_secret: Option<String>,
    #[serde(default)]
    pub password: Option<String>,
    #[serde(default)]
    pub client_id: Option<String>,
    #[serde(default)]
    pub client_secret: Option<String>,
    #[serde(default)]
    pub refresh_token: Option<String>,
    #[serde(default)]
    pub access_token: Option<String>,
    #[serde(default)]
    pub access_token_expires_at: Option<DateTime<Utc>>,
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
    pub smtp_host: Option<String>,
    #[serde(default)]
    pub smtp_port: Option<u16>,
    #[serde(default)]
    pub smtp_tls: Option<String>,
    #[serde(default)]
    pub smtp_username: Option<String>,
    #[serde(default)]
    pub api_key: Option<String>,
    #[serde(default)]
    pub access_key_id: Option<String>,
    #[serde(default)]
    pub secret_access_key: Option<String>,
    #[serde(default)]
    pub webhook_secret: Option<String>,
    #[serde(default)]
    pub password: Option<String>,
    #[serde(default)]
    pub client_id: Option<String>,
    #[serde(default)]
    pub client_secret: Option<String>,
    #[serde(default)]
    pub refresh_token: Option<String>,
    #[serde(default)]
    pub access_token: Option<String>,
    #[serde(default)]
    pub access_token_expires_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub test_mode: Option<bool>,
    #[serde(default)]
    pub enabled: Option<bool>,
}

pub(crate) fn validate_connection(
    input: &CreateConnectionRequest,
) -> UnifiedResult<EmailProviderKind> {
    let kind = EmailProviderKind::from_str(&input.kind).ok_or_else(|| {
        UnifiedError::bad_request(format!("Unknown email provider kind \"{}\"", input.kind))
    })?;
    if input.name.trim().is_empty() {
        return Err(UnifiedError::bad_request("Connection name is required"));
    }
    let has = |value: &Option<String>| {
        value
            .as_deref()
            .map(|v| !v.trim().is_empty())
            .unwrap_or(false)
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
        // OAuth account providers: the app credentials must be on
        // the connection before the account can be linked (the
        // refresh token itself arrives later, from the callback).
        // The sender address is the XOAUTH2 identity, so it is
        // required too.
        EmailProviderKind::Gmail | EmailProviderKind::Yahoo | EmailProviderKind::Outlook => {
            if !has(&input.client_id) {
                return Err(missing("client_id"));
            }
            if !has(&input.client_secret) {
                return Err(missing("client_secret"));
            }
            if !has(&input.sender_email) {
                return Err(UnifiedError::bad_request(format!(
                    "Provider \"{}\" requires sender_email (the linked account address)",
                    kind.as_str()
                )));
            }
        }
        // Generic SMTP: the connection carries the whole server
        // configuration now (host, port, TLS mode, auth, from) —
        // the adapter delivers directly instead of delegating to
        // the Mail Manager. A port of 0 is never valid; the TLS
        // mode must be one of the three supported values.
        EmailProviderKind::Smtp => {
            if !has(&input.smtp_host) {
                return Err(UnifiedError::bad_request(
                    "Provider \"smtp\" requires smtp_host",
                ));
            }
            match input.smtp_port {
                None => {
                    return Err(UnifiedError::bad_request(
                        "Provider \"smtp\" requires smtp_port (587 for STARTTLS, 465 for implicit TLS)",
                    ));
                }
                Some(0) => {
                    return Err(UnifiedError::bad_request(
                        "smtp_port must be between 1 and 65535",
                    ));
                }
                Some(_) => {}
            }
            if let Some(tls) = input
                .smtp_tls
                .as_deref()
                .map(str::trim)
                .filter(|v| !v.is_empty())
            {
                if !matches!(tls, "starttls" | "implicit" | "none") {
                    return Err(UnifiedError::bad_request(
                        "smtp_tls must be \"starttls\", \"implicit\" or \"none\"",
                    ));
                }
            }
            if !has(&input.sender_email) {
                return Err(UnifiedError::bad_request(
                    "Provider \"smtp\" requires sender_email",
                ));
            }
        }
        // Local sendmail: no credentials at all. The sender address
        // is optional — the adapter falls back to unified@localhost.
        EmailProviderKind::Sendmail => {}
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

#[cfg(test)]
mod tests {
    use super::*;

    fn base_input(kind: &str) -> CreateConnectionRequest {
        CreateConnectionRequest {
            name: "test".to_string(),
            kind: kind.to_string(),
            domain: None,
            region: None,
            sender_email: None,
            sender_name: None,
            webhook_url: None,
            smtp_host: None,
            smtp_port: None,
            smtp_tls: None,
            smtp_username: None,
            api_key: None,
            access_key_id: None,
            secret_access_key: None,
            webhook_secret: None,
            password: None,
            client_id: None,
            client_secret: None,
            refresh_token: None,
            access_token: None,
            access_token_expires_at: None,
            test_mode: None,
            enabled: None,
        }
    }

    #[test]
    fn smtp_validation_requires_host_port_and_sender() {
        // Nothing configured: the first missing field is named.
        let err = validate_connection(&base_input("smtp")).unwrap_err();
        assert!(err.to_string().contains("smtp_host"));
        let mut input = base_input("smtp");
        input.smtp_host = Some("smtp.example.com".to_string());
        assert!(
            validate_connection(&input)
                .unwrap_err()
                .to_string()
                .contains("smtp_port")
        );
        input.smtp_port = Some(0);
        assert!(
            validate_connection(&input)
                .unwrap_err()
                .to_string()
                .contains("smtp_port")
        );
        input.smtp_port = Some(587);
        assert!(
            validate_connection(&input)
                .unwrap_err()
                .to_string()
                .contains("sender_email")
        );
        input.sender_email = Some("news@example.com".to_string());
        assert_eq!(
            validate_connection(&input).unwrap(),
            EmailProviderKind::Smtp
        );
        // An unknown TLS mode is refused; the three real ones pass.
        input.smtp_tls = Some("bogus".to_string());
        assert!(
            validate_connection(&input)
                .unwrap_err()
                .to_string()
                .contains("smtp_tls")
        );
        for mode in ["starttls", "implicit", "none"] {
            input.smtp_tls = Some(mode.to_string());
            assert!(validate_connection(&input).is_ok(), "tls mode {mode}");
        }
    }

    #[test]
    fn oauth_validation_requires_client_credentials_and_sender() {
        for kind in ["gmail", "yahoo", "outlook"] {
            let err = validate_connection(&base_input(kind)).unwrap_err();
            assert!(err.to_string().contains("client_id"), "kind {kind}");
            let mut input = base_input(kind);
            input.client_id = Some("client-id-1".to_string());
            assert!(
                validate_connection(&input)
                    .unwrap_err()
                    .to_string()
                    .contains("client_secret")
            );
            input.client_secret = Some("client-secret-1".to_string());
            assert!(
                validate_connection(&input)
                    .unwrap_err()
                    .to_string()
                    .contains("sender_email")
            );
            input.sender_email = Some("user@example.com".to_string());
            assert!(validate_connection(&input).is_ok(), "kind {kind}");
        }
    }

    #[test]
    fn sendmail_validation_needs_nothing() {
        assert_eq!(
            validate_connection(&base_input("sendmail")).unwrap(),
            EmailProviderKind::Sendmail
        );
        let mut input = base_input("sendmail");
        input.sender_email = Some("noreply@example.com".to_string());
        assert!(validate_connection(&input).is_ok());
    }

    #[test]
    fn configured_fields_lists_the_new_secrets() {
        let credentials = EmailCredentials {
            api_key: Some("key-1".to_string()),
            access_key_id: None,
            secret_access_key: None,
            webhook_secret: None,
            password: Some("v1.encrypted".to_string()),
            client_id: Some("client-id-1".to_string()),
            client_secret: Some("v1.encrypted".to_string()),
            refresh_token: Some("v1.encrypted".to_string()),
            access_token: Some("v1.encrypted".to_string()),
            access_token_expires_at: None,
        };
        let fields = credentials.configured_fields();
        for expected in [
            "api_key",
            "password",
            "client_id",
            "client_secret",
            "refresh_token",
            "access_token",
        ] {
            assert!(fields.contains(&expected), "field {expected} missing");
        }
        assert!(credentials.oauth_linked());
        assert!(!EmailCredentials::default().oauth_linked());
    }
}
