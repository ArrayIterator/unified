// Email provider registry.
//
// One hookable registry plus a common provider trait, mirroring the
// provider-registry pattern of the cloud providers module and the
// payment-gateway trait: every email provider is a typed HTTP client
// built from one stored connection, with capability flags, a real
// connection test, sandbox / test mode, and 429 handling that surfaces
// the provider's `Retry-After` delay in `error.metadata`.
//
// Providers: Mailchimp Transactional (Mandrill), Brevo, SendGrid,
// Mailgun, Postmark, Amazon SES (SigV4), Resend, SparkPost, and
// generic SMTP (which delegates to the Mail Manager's SMTP transport
// through the `mail.send_requested` hook; this module never
// re-implements SMTP delivery).
//
// Provider selection is PER send: every send call takes an explicit
// provider connection id. A global default connection may exist as a
// fallback, but no send is ever forced through it.
//
// Credentials live only on the connection record, are never logged,
// and are never returned by any API response: read views expose only
// which credential fields are configured. Provider API hosts are
// allow-listed per adapter.
//
// Routes (all under `/api/v1/email-providers`):
//   GET    /catalog                  - provider kinds, capabilities, settings fields
//   GET    /connections              - paginated connections (filter kind, enabled, search)
//   POST   /connections              - create a connection
//   GET    /connections/{id}         - one connection (masked view)
//   PUT    /connections/{id}         - update a connection (secrets are write-only)
//   DELETE /connections/{id}         - delete a connection
//   POST   /connections/{id}/test    - connection test against the real provider API
//   POST   /connections/{id}/test-send - send a real test message through the connection
//   GET    /default                  - the global default connection id (fallback only)
//   PUT    /default                  - set the global default connection id
//   POST   /send                     - send one message through an explicit connection
//   POST   /send-batch               - send one message to many recipients
//   GET    /log                      - paginated send log (filter status, provider, search)
//
// Hook contracts:
//   Answers `email.send_requested` with `{ connection_id?, to, subject,
//   body_text?, body_html?, template_id?, variables?, metadata?, from? }`
//   by queueing the message and adding a result `{ queued, job_id,
//   connection_id }` (or `{ error }`); delivery runs in a worker and
//   emits `email.sent` / `email.failed`.
//   Answers `email_provider.webhook_verify` with `{ provider,
//   connection_id?, headers, body }` by adding a result `{ valid }`
//   (or `{ error }`) using the adapter's signature check.
//   Emits `email_provider.connected`, `email_provider.connection.created`,
//   `email_provider.connection.deleted`, `email.sent`, `email.failed`.

use crate::core::context::AppContext;
use crate::core::error::{UnifiedError, UnifiedResult};
use crate::core::hooks::{HookBus, HookContext};
use crate::core::json as api_json;
use crate::core::modules::{Module, ModuleRegistry, RouteInfo};
use crate::core::pagination::{PageResult, PaginationParams};
use actix_web::{web, HttpResponse};
use async_trait::async_trait;
use base64::Engine;
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use hmac::{Hmac, Mac};
use reqwest::header::{HeaderMap, HeaderValue, AUTHORIZATION, CONTENT_TYPE, RETRY_AFTER};
use reqwest::StatusCode;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha1::Sha1;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock, RwLock};
use std::time::Duration;
use url::Url;
use uuid::Uuid;

// ---------------------------------------------------------------------------
// Events.
// ---------------------------------------------------------------------------

pub const EVENT_EMAIL_SEND_REQUESTED: &str = "email.send_requested";
pub const EVENT_EMAIL_SENT: &str = "email.sent";
pub const EVENT_EMAIL_FAILED: &str = "email.failed";
pub const EVENT_EMAIL_PROVIDER_CONNECTED: &str = "email_provider.connected";
pub const EVENT_EMAIL_PROVIDER_CONNECTION_CREATED: &str = "email_provider.connection.created";
pub const EVENT_EMAIL_PROVIDER_CONNECTION_DELETED: &str = "email_provider.connection.deleted";
pub const EVENT_EMAIL_PROVIDER_WEBHOOK_VERIFY: &str = "email_provider.webhook_verify";
pub const EVENT_MAIL_SEND_REQUESTED: &str = "mail.send_requested";
pub const EVENT_TEMPLATE_RENDER_REQUESTED: &str = "template.render_requested";

// ---------------------------------------------------------------------------
// Limits.
// ---------------------------------------------------------------------------

const DEFAULT_TIMEOUT_SECS: u64 = 30;
const DEFAULT_CONNECT_TIMEOUT_SECS: u64 = 10;
const MAX_RETRY_AFTER: Duration = Duration::from_secs(120);
const MAX_BATCH_RECIPIENTS: usize = 1000;
const USER_AGENT: &str = concat!("unified/", env!("CARGO_PKG_VERSION"), " email-providers");

// ---------------------------------------------------------------------------
// Provider kinds and capabilities.
// ---------------------------------------------------------------------------

/// Every email provider this module can talk to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EmailProviderKind {
    Mailchimp,
    Brevo,
    SendGrid,
    Mailgun,
    Postmark,
    Ses,
    Resend,
    SparkPost,
    Smtp,
}

impl EmailProviderKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Mailchimp => "mailchimp",
            Self::Brevo => "brevo",
            Self::SendGrid => "sendgrid",
            Self::Mailgun => "mailgun",
            Self::Postmark => "postmark",
            Self::Ses => "ses",
            Self::Resend => "resend",
            Self::SparkPost => "sparkpost",
            Self::Smtp => "smtp",
        }
    }

    pub fn from_str(value: &str) -> Option<Self> {
        match value {
            "mailchimp" => Some(Self::Mailchimp),
            "brevo" => Some(Self::Brevo),
            "sendgrid" => Some(Self::SendGrid),
            "mailgun" => Some(Self::Mailgun),
            "postmark" => Some(Self::Postmark),
            "ses" => Some(Self::Ses),
            "resend" => Some(Self::Resend),
            "sparkpost" => Some(Self::SparkPost),
            "smtp" => Some(Self::Smtp),
            _ => None,
        }
    }

    pub fn all() -> &'static [EmailProviderKind] {
        &[
            Self::Mailchimp,
            Self::Brevo,
            Self::SendGrid,
            Self::Mailgun,
            Self::Postmark,
            Self::Ses,
            Self::Resend,
            Self::SparkPost,
            Self::Smtp,
        ]
    }

    pub fn display_name(&self) -> &'static str {
        match self {
            Self::Mailchimp => "Mailchimp Transactional (Mandrill)",
            Self::Brevo => "Brevo (Sendinblue)",
            Self::SendGrid => "SendGrid",
            Self::Mailgun => "Mailgun",
            Self::Postmark => "Postmark",
            Self::Ses => "Amazon SES",
            Self::Resend => "Resend",
            Self::SparkPost => "SparkPost",
            Self::Smtp => "SMTP (via Mail Manager)",
        }
    }

    /// Hosts this provider's API is allowed to talk to. A request to
    /// any other host is refused before it is sent.
    pub fn allowed_hosts(&self) -> &'static [&'static str] {
        match self {
            Self::Mailchimp => &["mandrillapp.com"],
            Self::Brevo => &["api.brevo.com"],
            Self::SendGrid => &["api.sendgrid.com"],
            Self::Mailgun => &["api.mailgun.net", "api.eu.mailgun.net"],
            Self::Postmark => &["api.postmarkapp.com"],
            Self::Ses => &[],
            // SES endpoints are per-region (`email.<region>.amazonaws.com`);
            // validated by `validate_ses_endpoint`.
            Self::Resend => &["api.resend.com"],
            Self::SparkPost => &["api.sparkpost.com", "api.eu.sparkpost.com"],
            Self::Smtp => &[],
        }
    }

    pub fn has_fixed_hosts(&self) -> bool {
        !matches!(self, Self::Ses | Self::Smtp)
    }

    /// Notes on sender-domain validation for the settings page.
    pub fn domain_notes(&self) -> &'static str {
        match self {
            Self::Mailchimp => "Sending domains must be verified in Mandrill (Settings > Sending Domains); add SPF and DKIM records for the domain before sending bulk mail.",
            Self::Brevo => "Verify the sender domain in Brevo (Senders & IP > Domains) and add the SPF and DKIM records Brevo shows; unverified senders are limited.",
            Self::SendGrid => "Authenticate the sender domain in SendGrid (Settings > Sender Authentication) with SPF and DKIM; single sender verification is enough for low volume.",
            Self::Mailgun => "Add and verify the domain in Mailgun (Sending > Domains) with the SPF and DKIM TXT records; sandbox domains only deliver to authorized recipients.",
            Self::Postmark => "Verify the sender signature (Sender Signatures) or the whole domain with DKIM in Postmark; every From address must be verified.",
            Self::Ses => "Verify the sender identity (email address or domain) in SES; new accounts are in the SES sandbox and can only send to verified addresses until production access is granted.",
            Self::Resend => "Verify the sending domain in Resend (Domains) with SPF and DKIM; unverified accounts can only send to the account owner address.",
            Self::SparkPost => "Verify the sending domain in SparkPost (Account > Sending Domains) with SPF and DKIM records.",
            Self::Smtp => "Delivery uses the Mail Manager SMTP settings; the From domain should have SPF and DKIM records for the SMTP host.",
        }
    }

    /// Whether the provider offers a native sandbox that accepts mail
    /// without delivering it. When a connection has test mode on and
    /// the provider has no native sandbox, the adapter records a
    /// simulated accepted delivery (clearly flagged in the log and in
    /// the result) instead of calling the provider.
    pub fn has_native_sandbox(&self) -> bool {
        matches!(self, Self::SendGrid | Self::Mailgun)
    }

    /// Webhook signature support, stated honestly per provider.
    pub fn webhook_notes(&self) -> &'static str {
        match self {
            Self::Mailchimp => "Mandrill webhooks are verified with HMAC-SHA1 over the webhook URL plus the sorted POST parameters, using the webhook key. Set webhook_url to the exact public webhook URL.",
            Self::Brevo => "Brevo sends no signature; set a webhook_secret on the connection and Brevo must send it back as the X-Webhook-Secret header (configure it in the Brevo webhook URL as a custom header).",
            Self::SendGrid => "SendGrid Event Webhook signatures use ECDSA verification, which this module does not implement; set a webhook_secret on the connection and require it as the X-Webhook-Secret header instead.",
            Self::Mailgun => "Mailgun webhooks are verified with HMAC-SHA256 over timestamp + token, using the webhook signing key (webhook_secret).",
            Self::Postmark => "Postmark sends no signature; set a webhook_secret on the connection and require it as the X-Webhook-Secret header (Postmark supports basic auth on the webhook URL as an alternative).",
            Self::Ses => "SES delivers events through SNS, whose signatures need RSA certificate verification, which this module does not implement; set a webhook_secret on the connection and require it as the X-Webhook-Secret header instead.",
            Self::Resend => "Resend webhooks are Svix-signed and verified with HMAC-SHA256 over the svix id, timestamp and body, using the webhook signing secret (webhook_secret).",
            Self::SparkPost => "SparkPost sends no signature; set a webhook_secret on the connection and require it as the X-Webhook-Secret header.",
            Self::Smtp => "SMTP has no webhooks; bounce and complaint handling is not available for this provider.",
        }
    }
}

/// What an adapter can do. Callers must check these flags before
/// relying on a feature: an unsupported call fails with a 400 naming
/// the missing capability.
#[derive(Debug, Clone, Copy, Serialize)]
pub struct EmailCapabilities {
    pub transactional: bool,
    pub bulk: bool,
    pub templates: bool,
    pub webhooks: bool,
    pub tracking: bool,
    pub sandbox: bool,
}

pub fn capabilities_for(kind: EmailProviderKind) -> EmailCapabilities {
    match kind {
        EmailProviderKind::Mailchimp => EmailCapabilities {
            transactional: true,
            bulk: true,
            templates: true,
            webhooks: true,
            tracking: true,
            sandbox: false,
        },
        EmailProviderKind::Brevo => EmailCapabilities {
            transactional: true,
            bulk: true,
            templates: true,
            webhooks: true,
            tracking: true,
            sandbox: false,
        },
        EmailProviderKind::SendGrid => EmailCapabilities {
            transactional: true,
            bulk: true,
            templates: true,
            webhooks: true,
            tracking: true,
            sandbox: true,
        },
        EmailProviderKind::Mailgun => EmailCapabilities {
            transactional: true,
            bulk: true,
            templates: true,
            webhooks: true,
            tracking: true,
            sandbox: true,
        },
        EmailProviderKind::Postmark => EmailCapabilities {
            transactional: true,
            bulk: false,
            templates: true,
            webhooks: true,
            tracking: true,
            sandbox: false,
        },
        EmailProviderKind::Ses => EmailCapabilities {
            transactional: true,
            bulk: true,
            templates: false,
            webhooks: true,
            tracking: true,
            sandbox: false,
        },
        EmailProviderKind::Resend => EmailCapabilities {
            transactional: true,
            bulk: true,
            templates: false,
            webhooks: true,
            tracking: true,
            sandbox: false,
        },
        EmailProviderKind::SparkPost => EmailCapabilities {
            transactional: true,
            bulk: true,
            templates: true,
            webhooks: true,
            tracking: true,
            sandbox: false,
        },
        EmailProviderKind::Smtp => EmailCapabilities {
            transactional: true,
            bulk: true,
            templates: false,
            webhooks: false,
            tracking: false,
            sandbox: false,
        },
    }
}

/// Result of a connection test.
#[derive(Debug, Clone, Serialize)]
pub struct ConnectionTestResult {
    pub kind: String,
    pub reachable: bool,
    pub latency_ms: u64,
    pub account: Option<String>,
    pub detail: String,
}

/// One field of the per-provider settings page.
#[derive(Debug, Clone, Serialize)]
pub struct SettingsFieldDef {
    pub key: String,
    pub label: String,
    #[serde(rename = "type")]
    pub field_type: String,
    pub required: bool,
    pub secret: bool,
    pub placeholder: String,
    pub help: String,
}

fn settings_fields(kind: EmailProviderKind) -> Vec<SettingsFieldDef> {
    let field = |key: &str,
                 label: &str,
                 field_type: &str,
                 required: bool,
                 secret: bool,
                 placeholder: &str,
                 help: &str| {
        SettingsFieldDef {
            key: key.to_string(),
            label: label.to_string(),
            field_type: field_type.to_string(),
            required,
            secret,
            placeholder: placeholder.to_string(),
            help: help.to_string(),
        }
    };
    let sender_email = field(
        "sender_email",
        "Default sender email",
        "email",
        false,
        false,
        "news@example.com",
        "From address used when a send does not supply its own. Must be verified with the provider.",
    );
    let sender_name = field(
        "sender_name",
        "Default sender name",
        "text",
        false,
        false,
        "Example News",
        "Display name shown next to the sender email.",
    );
    let webhook_secret = field(
        "webhook_secret",
        "Webhook secret / signing key",
        "password",
        false,
        true,
        "",
        "Secret used to verify incoming webhooks from this provider (see the webhook notes for the kind).",
    );
    let test_mode = field(
        "test_mode",
        "Test mode (sandbox)",
        "boolean",
        false,
        false,
        "",
        "When on, mail is accepted by the provider sandbox where one exists, otherwise recorded as a simulated delivery and never sent.",
    );
    let mut fields = match kind {
        EmailProviderKind::Mailchimp => vec![
            field("api_key", "Mandrill API key", "password", true, true, "", "API key from the Mandrill dashboard (Settings > API Keys)."),
            field("webhook_url", "Public webhook URL", "url", false, false, "https://example.com/api/v1/newsletter/webhooks/mailchimp", "Exact public URL of the webhook receiver; Mandrill signs webhooks with this URL."),
        ],
        EmailProviderKind::Brevo => vec![field(
            "api_key",
            "Brevo API key",
            "password",
            true,
            true,
            "",
            "API key from Brevo (Settings > SMTP & API > API Keys), version 3.",
        )],
        EmailProviderKind::SendGrid => vec![field(
            "api_key",
            "SendGrid API key",
            "password",
            true,
            true,
            "",
            "API key from SendGrid (Settings > API Keys) with Mail Send permission.",
        )],
        EmailProviderKind::Mailgun => vec![
            field("api_key", "Mailgun API key", "password", true, true, "", "Private API key from Mailgun (Settings > API Keys)."),
            field("domain", "Mailgun domain", "text", true, false, "mg.example.com", "Verified sending domain in Mailgun."),
            field("region", "Region", "select", false, false, "us", "us (api.mailgun.net) or eu (api.eu.mailgun.net)."),
        ],
        EmailProviderKind::Postmark => vec![field(
            "api_key",
            "Postmark server token",
            "password",
            true,
            true,
            "",
            "Server API token from Postmark (Servers > API Tokens).",
        )],
        EmailProviderKind::Ses => vec![
            field("access_key_id", "AWS access key id", "text", true, false, "", "IAM access key with ses:SendEmail permission."),
            field("secret_access_key", "AWS secret access key", "password", true, true, "", "IAM secret access key."),
            field("region", "AWS region", "text", true, false, "us-east-1", "SES region, for example us-east-1 or eu-west-1."),
        ],
        EmailProviderKind::Resend => vec![field(
            "api_key",
            "Resend API key",
            "password",
            true,
            true,
            "",
            "API key from Resend (API Keys).",
        )],
        EmailProviderKind::SparkPost => vec![
            field("api_key", "SparkPost API key", "password", true, true, "", "API key from SparkPost (Account > API Keys) with Transmissions: Read/Write."),
            field("region", "Region", "select", false, false, "us", "us (api.sparkpost.com) or eu (api.eu.sparkpost.com)."),
        ],
        EmailProviderKind::Smtp => vec![field(
            "sender_email",
            "Sender email override",
            "email",
            false,
            false,
            "news@example.com",
            "Optional From override for the Mail Manager SMTP settings; empty means the SMTP settings apply.",
        )],
    };
    if kind != EmailProviderKind::Smtp {
        fields.push(sender_email);
        fields.push(sender_name);
    }
    fields.push(webhook_secret);
    fields.push(test_mode);
    fields
}

// ---------------------------------------------------------------------------
// Credentials and connections.
// ---------------------------------------------------------------------------

/// Credentials for one connection. Every field is optional at the type
/// level; `validate_connection` enforces the per-kind required set.
/// Values are write-only: they are never serialized into a view, never
/// logged, and never included in hook payloads.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
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
    fn configured_fields(&self) -> Vec<&'static str> {
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
    fn view(&self) -> EmailConnectionView {
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

fn validate_connection(input: &CreateConnectionRequest) -> UnifiedResult<EmailProviderKind> {
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

// ---------------------------------------------------------------------------
// In-memory stores (same pattern as the cloud provider connections).
// ---------------------------------------------------------------------------

static CONNECTIONS: OnceLock<DashMap<Uuid, EmailConnection>> = OnceLock::new();

fn connections_store() -> &'static DashMap<Uuid, EmailConnection> {
    CONNECTIONS.get_or_init(DashMap::new)
}

/// The global default connection id (fallback only; every send takes
/// an explicit connection id first).
static DEFAULT_CONNECTION: OnceLock<RwLock<Option<Uuid>>> = OnceLock::new();

fn default_connection_store() -> &'static RwLock<Option<Uuid>> {
    DEFAULT_CONNECTION.get_or_init(|| RwLock::new(None))
}

pub fn get_connection(id: &Uuid) -> Option<EmailConnection> {
    connections_store().get(id).map(|entry| entry.value().clone())
}

pub fn default_connection() -> Option<EmailConnection> {
    let id = *default_connection_store()
        .read()
        .expect("default connection lock poisoned")?;
    get_connection(&id)
}

/// Resolve the connection for a send: an explicit id wins, otherwise
/// the global default. Fails honestly when neither resolves.
pub fn resolve_connection(connection_id: Option<&str>) -> UnifiedResult<EmailConnection> {
    if let Some(raw) = connection_id {
        let id = Uuid::parse_str(raw).map_err(|_| {
            UnifiedError::bad_request(format!("Invalid connection id \"{raw}\""))
        })?;
        return get_connection(&id).ok_or_else(|| {
            UnifiedError::not_found(format!("Email provider connection not found: {raw}"))
        });
    }
    default_connection().ok_or_else(|| {
        UnifiedError::bad_request(
            "No email provider connection_id was supplied and no default connection is set",
        )
    })
}

fn first_enabled_connection(kind: EmailProviderKind) -> Option<EmailConnection> {
    connections_store()
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|connection| connection.kind == kind && connection.enabled)
        .min_by_key(|connection| connection.created_at)
}

// ---------------------------------------------------------------------------
// Messages, results, and the send log.
// ---------------------------------------------------------------------------

/// One outgoing email. `metadata` carries caller context (for example
/// `campaign_id` / `subscriber_id`); adapters map it to the provider's
/// custom-args / tags / metadata field where the provider supports one,
/// so webhooks can be matched back to the campaign.
#[derive(Debug, Clone)]
pub struct EmailMessage {
    pub to: String,
    pub from: Option<String>,
    pub from_name: Option<String>,
    pub subject: String,
    pub body_text: Option<String>,
    pub body_html: Option<String>,
    pub reply_to: Option<String>,
    pub template_id: Option<String>,
    pub variables: HashMap<String, Value>,
    pub metadata: HashMap<String, String>,
    pub test_mode: bool,
}

impl EmailMessage {
    fn sender(&self, connection: &EmailConnection) -> (String, Option<String>) {
        let from = self
            .from
            .clone()
            .or_else(|| connection.sender_email.clone())
            .unwrap_or_default();
        let name = self
            .from_name
            .clone()
            .or_else(|| connection.sender_name.clone());
        (from, name)
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct SendResult {
    pub provider_message_id: Option<String>,
    pub accepted: bool,
    /// True when the delivery was simulated (test mode on a provider
    /// without a native sandbox) or accepted by a provider sandbox.
    pub test_mode: bool,
    pub detail: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct SendLogEntry {
    pub id: String,
    pub connection_id: String,
    pub provider: String,
    pub to: String,
    pub subject: String,
    pub status: String,
    pub provider_message_id: Option<String>,
    pub test_mode: bool,
    pub error: Option<String>,
    pub created_at: String,
}

static SEND_LOG: OnceLock<RwLock<Vec<SendLogEntry>>> = OnceLock::new();

fn send_log_store() -> &'static RwLock<Vec<SendLogEntry>> {
    SEND_LOG.get_or_init(|| RwLock::new(Vec::new()))
}

fn append_log(entry: SendLogEntry) {
    send_log_store()
        .write()
        .expect("send log lock poisoned")
        .push(entry);
}

fn update_log(id: &str, status: &str, provider_message_id: Option<String>, error: Option<String>) {
    let mut log = send_log_store().write().expect("send log lock poisoned");
    if let Some(entry) = log.iter_mut().find(|entry| entry.id == id) {
        entry.status = status.to_string();
        entry.provider_message_id = provider_message_id;
        entry.error = error;
    }
}

/// A queued send job (created by the `email.send_requested` hook).
#[derive(Debug, Clone)]
struct QueuedSend {
    job_id: String,
    connection_id: Uuid,
    message: EmailMessage,
}

// ---------------------------------------------------------------------------
// Shared HTTP helpers (mirroring the cloud providers module).
// ---------------------------------------------------------------------------

fn hex_encode(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn sha256_hex(data: &[u8]) -> String {
    hex_encode(&Sha256::digest(data))
}

type HmacSha256 = Hmac<Sha256>;

fn hmac_sha256(key: &[u8], message: &[u8]) -> Vec<u8> {
    let mut mac = HmacSha256::new_from_slice(key).expect("HMAC key length is always valid");
    mac.update(message);
    mac.finalize().into_bytes().to_vec()
}

/// Constant-time-ish comparison for webhook signatures (avoids leaking
/// the match position through early exit).
fn signatures_equal(a: &str, b: &str) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.bytes().zip(b.bytes()).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

fn build_client() -> UnifiedResult<reqwest::Client> {
    reqwest::Client::builder()
        .user_agent(USER_AGENT)
        .timeout(Duration::from_secs(DEFAULT_TIMEOUT_SECS))
        .connect_timeout(Duration::from_secs(DEFAULT_CONNECT_TIMEOUT_SECS))
        .build()
        .map_err(|error| UnifiedError::internal(format!("Cannot build HTTP client: {error}")))
}

fn check_allowlist(url: &Url, kind: EmailProviderKind) -> UnifiedResult<()> {
    if !kind.has_fixed_hosts() {
        return Ok(());
    }
    let host = url.host_str().unwrap_or("");
    if !kind.allowed_hosts().contains(&host) {
        return Err(UnifiedError::bad_request(format!(
            "Refusing request to non-allow-listed host \"{host}\" for provider \"{}\"",
            kind.as_str()
        )));
    }
    Ok(())
}

fn parse_retry_after(response: &reqwest::Response) -> Option<Duration> {
    let value = response.headers().get(RETRY_AFTER)?.to_str().ok()?;
    value
        .trim()
        .parse::<u64>()
        .ok()
        .map(|secs| Duration::from_secs(secs).min(MAX_RETRY_AFTER))
}

/// Map a provider HTTP response to an error. A 429 keeps its status
/// and carries the `Retry-After` delay in metadata; 401/403/404 map to
/// the matching `UnifiedError`; other failures stay generic so provider
/// response bodies (which may contain account data) are never leaked
/// into the client-visible message.
fn http_status_error(
    status: StatusCode,
    provider: &str,
    retry_after: Option<Duration>,
) -> UnifiedError {
    let message = format!("Provider \"{provider}\" request failed with HTTP {status}");
    match status {
        StatusCode::TOO_MANY_REQUESTS => {
            let mut error = UnifiedError::new(message, 429)
                .with_metadata("provider", Value::String(provider.to_string()));
            if let Some(delay) = retry_after {
                error = error.with_metadata("retry_after_secs", Value::from(delay.as_secs()));
            }
            error
        }
        StatusCode::UNAUTHORIZED => UnifiedError::unauthorized(format!(
            "Provider \"{provider}\" rejected the credentials (HTTP 401)"
        )),
        StatusCode::FORBIDDEN => UnifiedError::forbidden(format!(
            "Provider \"{provider}\" refused the request (HTTP 403): the credentials lack permission for this operation"
        )),
        StatusCode::NOT_FOUND => {
            UnifiedError::not_found(format!("Provider \"{provider}\" resource not found (HTTP 404)"))
        }
        _ if status.is_client_error() => {
            UnifiedError::bad_request(message).with_metadata("provider", Value::String(provider.to_string()))
        }
        _ => UnifiedError::internal(message).with_metadata("provider", Value::String(provider.to_string())),
    }
}

async fn send_json(
    client: &reqwest::Client,
    request: reqwest::RequestBuilder,
    provider: &str,
) -> UnifiedResult<Value> {
    let response = request.send().await.map_err(|error| {
        UnifiedError::internal(format!("Provider \"{provider}\" request failed: {error}"))
    })?;
    let status = response.status();
    if !status.is_success() {
        return Err(http_status_error(status, provider, parse_retry_after(&response)));
    }
    response.json::<Value>().await.map_err(|error| {
        UnifiedError::internal(format!(
            "Provider \"{provider}\" returned an unreadable response: {error}"
        ))
    })
}

async fn send_text(
    client: &reqwest::Client,
    request: reqwest::RequestBuilder,
    provider: &str,
) -> UnifiedResult<String> {
    let response = request.send().await.map_err(|error| {
        UnifiedError::internal(format!("Provider \"{provider}\" request failed: {error}"))
    })?;
    let status = response.status();
    if !status.is_success() {
        return Err(http_status_error(status, provider, parse_retry_after(&response)));
    }
    response.text().await.map_err(|error| {
        UnifiedError::internal(format!(
            "Provider \"{provider}\" returned an unreadable response: {error}"
        ))
    })
}

/// Simulated accepted delivery for test mode on a provider without a
/// native sandbox. The result is always flagged `test_mode: true` so
/// it can never be mistaken for a real delivery.
fn simulated_result(provider: &str) -> SendResult {
    SendResult {
        provider_message_id: Some(format!("test-mode-{}", Uuid::now_v7())),
        accepted: true,
        test_mode: true,
        detail: format!(
            "Test mode: provider \"{provider}\" has no native sandbox, so the message was recorded as a simulated delivery and was not sent"
        ),
    }
}

fn validate_email_address(input: &str) -> UnifiedResult<()> {
    let address = input.trim();
    if address.is_empty() || !address.contains('@') {
        return Err(UnifiedError::bad_request(format!(
            "Invalid email address: {input}"
        )));
    }
    let mut parts = address.split('@');
    let (Some(local), Some(domain), None) = (parts.next(), parts.next(), parts.next()) else {
        return Err(UnifiedError::bad_request(format!(
            "Invalid email address: {input}"
        )));
    };
    if local.is_empty() || domain.is_empty() || !domain.contains('.') {
        return Err(UnifiedError::bad_request(format!(
            "Invalid email address: {input}"
        )));
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// AWS SigV4 signing (SES), following the same scheme as the cloud
// providers module: canonical request, credential scope, and the
// four-step HMAC key derivation. Only `host`, `x-amz-date`, and
// `content-type` are signed, and the payload hash is the SHA-256 of
// the request body.
// ---------------------------------------------------------------------------

struct SigV4Params {
    access_key_id: String,
    secret_access_key: String,
    region: String,
    service: String,
}

fn sigv4_authorization(
    params: &SigV4Params,
    method: &str,
    url: &Url,
    headers_to_sign: &[(&str, String)],
    payload: &[u8],
    now: DateTime<Utc>,
) -> (String, String) {
    let amz_date = now.format("%Y%m%dT%H%M%SZ").to_string();
    let date_stamp = now.format("%Y%m%d").to_string();
    let mut signed: Vec<(&str, String)> = headers_to_sign.to_vec();
    signed.sort_by(|a, b| a.0.cmp(b.0));
    let signed_headers: Vec<&str> = signed.iter().map(|(name, _)| *name).collect();
    let canonical_headers: String = signed
        .iter()
        .map(|(name, value)| format!("{name}:{}\n", value.trim()))
        .collect();
    let canonical_query = url.query().unwrap_or("");
    let canonical_request = format!(
        "{method}\n{}\n{canonical_query}\n{canonical_headers}{}\n{}",
        url.path(),
        signed_headers.join(";"),
        sha256_hex(payload),
    );
    let scope = format!("{date_stamp}/{}/{}/aws4_request", params.region, params.service);
    let string_to_sign = format!(
        "AWS4-HMAC-SHA256\n{amz_date}\n{scope}\n{}",
        sha256_hex(canonical_request.as_bytes()),
    );
    let k_date = hmac_sha256(
        format!("AWS4{}", params.secret_access_key).as_bytes(),
        date_stamp.as_bytes(),
    );
    let k_region = hmac_sha256(&k_date, params.region.as_bytes());
    let k_service = hmac_sha256(&k_region, params.service.as_bytes());
    let k_signing = hmac_sha256(&k_service, b"aws4_request");
    let signature = hex_encode(&hmac_sha256(&k_signing, string_to_sign.as_bytes()));
    let authorization = format!(
        "AWS4-HMAC-SHA256 Credential={}/{scope}, SignedHeaders={}, Signature={signature}",
        params.access_key_id,
        signed_headers.join(";"),
    );
    (authorization, amz_date)
}

// ---------------------------------------------------------------------------
// Provider registry: the plug-in point for adapters.
// ---------------------------------------------------------------------------

pub type EmailProviderFactory = Arc<
    dyn Fn(&EmailConnection) -> UnifiedResult<Arc<dyn EmailProvider>> + Send + Sync,
>;

pub struct EmailProviderRegistry {
    factories: RwLock<HashMap<EmailProviderKind, EmailProviderFactory>>,
}

impl EmailProviderRegistry {
    fn new() -> Self {
        Self {
            factories: RwLock::new(HashMap::new()),
        }
    }

    /// Register (or replace) the factory for a kind. This is how a new
    /// provider plugs in: call it once at startup, or from an extension,
    /// instead of editing this module.
    pub fn register_factory(&self, kind: EmailProviderKind, factory: EmailProviderFactory) {
        self.factories
            .write()
            .expect("email provider registry lock poisoned")
            .insert(kind, factory);
    }

    pub fn build(&self, connection: &EmailConnection) -> UnifiedResult<Arc<dyn EmailProvider>> {
        let factories = self
            .factories
            .read()
            .expect("email provider registry lock poisoned");
        let factory = factories.get(&connection.kind).ok_or_else(|| {
            UnifiedError::internal(format!(
                "No email provider adapter registered for kind \"{}\"",
                connection.kind.as_str()
            ))
        })?;
        factory(connection)
    }

    pub fn registered_kinds(&self) -> Vec<EmailProviderKind> {
        let factories = self
            .factories
            .read()
            .expect("email provider registry lock poisoned");
        let mut kinds: Vec<EmailProviderKind> = factories.keys().copied().collect();
        kinds.sort_by_key(|kind| kind.as_str());
        kinds
    }
}

static REGISTRY: OnceLock<EmailProviderRegistry> = OnceLock::new();

/// The shared registry, with the built-in adapters registered on
/// first use.
pub fn email_provider_registry() -> &'static EmailProviderRegistry {
    REGISTRY.get_or_init(|| {
        let registry = EmailProviderRegistry::new();
        registry.register_factory(
            EmailProviderKind::Mailchimp,
            Arc::new(|connection| {
                Ok(Arc::new(MailchimpAdapter::new(connection)?) as Arc<dyn EmailProvider>)
            }),
        );
        registry.register_factory(
            EmailProviderKind::Brevo,
            Arc::new(|connection| {
                Ok(Arc::new(BrevoAdapter::new(connection)?) as Arc<dyn EmailProvider>)
            }),
        );
        registry.register_factory(
            EmailProviderKind::SendGrid,
            Arc::new(|connection| {
                Ok(Arc::new(SendGridAdapter::new(connection)?) as Arc<dyn EmailProvider>)
            }),
        );
        registry.register_factory(
            EmailProviderKind::Mailgun,
            Arc::new(|connection| {
                Ok(Arc::new(MailgunAdapter::new(connection)?) as Arc<dyn EmailProvider>)
            }),
        );
        registry.register_factory(
            EmailProviderKind::Postmark,
            Arc::new(|connection| {
                Ok(Arc::new(PostmarkAdapter::new(connection)?) as Arc<dyn EmailProvider>)
            }),
        );
        registry.register_factory(
            EmailProviderKind::Ses,
            Arc::new(|connection| {
                Ok(Arc::new(SesAdapter::new(connection)?) as Arc<dyn EmailProvider>)
            }),
        );
        registry.register_factory(
            EmailProviderKind::Resend,
            Arc::new(|connection| {
                Ok(Arc::new(ResendAdapter::new(connection)?) as Arc<dyn EmailProvider>)
            }),
        );
        registry.register_factory(
            EmailProviderKind::SparkPost,
            Arc::new(|connection| {
                Ok(Arc::new(SparkPostAdapter::new(connection)?) as Arc<dyn EmailProvider>)
            }),
        );
        registry.register_factory(
            EmailProviderKind::Smtp,
            Arc::new(|connection| {
                Ok(Arc::new(SmtpAdapter::new(connection)?) as Arc<dyn EmailProvider>)
            }),
        );
        registry
    })
}

/// The hook bus, shared with the adapters that delegate through hooks
/// (SMTP). Set once by the module `init`; adapters that need it read it
/// from here so factories stay synchronous.
static EMAIL_HOOKS: OnceLock<RwLock<Option<Arc<HookBus>>>> = OnceLock::new();

fn email_hooks_store() -> &'static RwLock<Option<Arc<HookBus>>> {
    EMAIL_HOOKS.get_or_init(|| RwLock::new(None))
}

fn email_hooks() -> Option<Arc<HookBus>> {
    email_hooks_store()
        .read()
        .expect("email hooks lock poisoned")
        .clone()
}

fn emit_event(event: &str, data: Value) {
    if let Some(hooks) = email_hooks() {
        hooks.emit(event, data);
    }
}

// ---------------------------------------------------------------------------
// Common adapter interface.
// ---------------------------------------------------------------------------

/// The interface every email provider adapter implements. Only `send`
/// and `test_connection` are required; `send_batch` defaults to
/// sending one by one, and `webhook_verify` defaults to the shared
/// secret-token check (or an honest "not supported" error when the
/// provider has no verifiable signature and no secret is configured).
#[async_trait]
pub trait EmailProvider: Send + Sync {
    fn kind(&self) -> EmailProviderKind;

    fn capabilities(&self) -> EmailCapabilities {
        capabilities_for(self.kind())
    }

    /// Verify the credentials against the real provider API.
    async fn test_connection(&self) -> UnifiedResult<ConnectionTestResult>;

    /// Send one message.
    async fn send(&self, message: &EmailMessage) -> UnifiedResult<SendResult>;

    /// Send one message to many recipients. The default implementation
    /// sends one by one and reports the first failure; adapters with a
    /// native bulk endpoint override it.
    async fn send_batch(
        &self,
        message: &EmailMessage,
        recipients: &[String],
    ) -> UnifiedResult<Vec<SendResult>> {
        let mut results = Vec::with_capacity(recipients.len());
        for recipient in recipients {
            let mut single = message.clone();
            single.to = recipient.clone();
            results.push(self.send(&single).await?);
        }
        Ok(results)
    }

    /// Verify a webhook's signature before any processing. Returns
    /// `Ok(true)` when the signature is valid, `Ok(false)` when the
    /// provider supplies a signature that does not match, and an
    /// error when verification cannot be performed at all (missing
    /// secret, unsupported provider, or a missing webhook URL). A
    /// `false` or an error both mean the webhook must be rejected.
    async fn webhook_verify(
        &self,
        headers: &HeaderMap,
        raw_body: &[u8],
    ) -> UnifiedResult<bool> {
        self.webhook_verify_secret_token(headers, raw_body)
    }

    /// Shared fallback: when the connection has a webhook secret, the
    /// `X-Webhook-Secret` header must match it. Providers with a
    /// native signature scheme override `webhook_verify` instead.
    fn webhook_verify_secret_token(
        &self,
        headers: &HeaderMap,
        _raw_body: &[u8],
    ) -> UnifiedResult<bool> {
        let secret = self.connection().credentials.webhook_secret.clone();
        let Some(secret) = secret.filter(|value| !value.trim().is_empty()) else {
            return Err(UnifiedError::unprocessable(format!(
                "Provider \"{}\" has no verifiable webhook signature and no webhook_secret is configured on the connection; set webhook_secret to verify webhooks for this provider",
                self.kind().as_str()
            )));
        };
        let presented = headers
            .get("x-webhook-secret")
            .and_then(|value| value.to_str().ok())
            .unwrap_or("");
        Ok(signatures_equal(presented, &secret))
    }

    fn connection(&self) -> &EmailConnection;
}

fn unsupported(kind: EmailProviderKind, action: &str, capability: &str) -> UnifiedError {
    UnifiedError::bad_request(format!(
        "Provider \"{}\" does not support {action} (capability {capability} is false)",
        kind.as_str()
    ))
}

fn require_api_key(connection: &EmailConnection) -> UnifiedResult<String> {
    connection
        .credentials
        .api_key
        .clone()
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| {
            UnifiedError::bad_request(format!(
                "Connection \"{}\" has no api_key configured",
                connection.name
            ))
        })
}

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
            fn new(connection: &EmailConnection) -> UnifiedResult<Self> {
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
            provider_message_id: first
                .and_then(|item| item.get("_id"))
                .and_then(|value| value.as_str())
                .map(str::to_string),
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
    connection: EmailConnection,
    client: reqwest::Client,
}

impl MailgunAdapter {
    fn new(connection: &EmailConnection) -> UnifiedResult<Self> {
        Ok(Self {
            connection: connection.clone(),
            client: build_client()?,
        })
    }

    fn base_url(&self) -> UnifiedResult<Url> {
        let host = if self.connection.region.as_deref() == Some("eu") {
            "https://api.eu.mailgun.net"
        } else {
            "https://api.mailgun.net"
        };
        Url::parse(host).map_err(|_| UnifiedError::internal("Invalid Mailgun API URL"))
    }

    fn domain(&self) -> UnifiedResult<String> {
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
    connection: EmailConnection,
    client: reqwest::Client,
}

impl SesAdapter {
    fn new(connection: &EmailConnection) -> UnifiedResult<Self> {
        Ok(Self {
            connection: connection.clone(),
            client: build_client()?,
        })
    }

    fn region(&self) -> UnifiedResult<String> {
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

    fn endpoint(&self) -> UnifiedResult<Url> {
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
    async fn ses_call(&self, params: &[(String, String)]) -> UnifiedResult<String> {
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
        let mut serializer = url::form_urlencoded::Serializer::new(String::new());
        for (key, value) in params {
            serializer.append_pair(key, value);
        }
        let body = serializer.finish();
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

fn xml_tag(body: &str, tag: &str) -> Option<String> {
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
    connection: EmailConnection,
    client: reqwest::Client,
}

impl SparkPostAdapter {
    fn new(connection: &EmailConnection) -> UnifiedResult<Self> {
        Ok(Self {
            connection: connection.clone(),
            client: build_client()?,
        })
    }

    fn base_url(&self) -> UnifiedResult<Url> {
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
    connection: EmailConnection,
}

impl SmtpAdapter {
    fn new(connection: &EmailConnection) -> UnifiedResult<Self> {
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

// ---------------------------------------------------------------------------
// Delivery worker (used by the `email.send_requested` hook).
// ---------------------------------------------------------------------------

/// Render a template through the Email Templates module, when the
/// message references one. Returns the rendered HTML and subject.
fn render_template(
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

async fn run_send_job(job: QueuedSend) {
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
                update_log(
                    &job.job_id,
                    "failed",
                    None,
                    Some(error.message.clone()),
                );
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
fn spawn_send_worker(job: QueuedSend) {
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

// ---------------------------------------------------------------------------
// Hook handlers.
// ---------------------------------------------------------------------------

fn on_email_send_requested(ctx: &mut HookContext) {
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
    let connection = match resolve_connection(
        data.get("connection_id").and_then(|value| value.as_str()),
    ) {
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

fn on_webhook_verify(ctx: &mut HookContext) {
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
                ctx.add_result(json!({ "error": format!("Email provider connection not found: {id}") }));
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
                value.as_str().and_then(|text| HeaderValue::from_str(text).ok()),
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
        rx.recv().map_err(|_| {
            UnifiedError::internal("Webhook verification worker failed")
        })?
    })();
    match result {
        Ok(valid) => ctx.add_result(json!({ "valid": valid })),
        Err(error) => ctx.add_result(json!({ "error": error.message })),
    }
}

// ---------------------------------------------------------------------------
// HTTP handlers.
// ---------------------------------------------------------------------------

pub struct EmailProviderState {
    hooks: RwLock<Option<Arc<HookBus>>>,
    debug: AtomicBool,
}

impl EmailProviderState {
    fn new() -> Self {
        Self {
            hooks: RwLock::new(None),
            debug: AtomicBool::new(false),
        }
    }

    fn set_hooks(&self, hooks: Arc<HookBus>) {
        *self.hooks.write().expect("email hooks lock poisoned") = Some(hooks.clone());
        *email_hooks_store()
            .write()
            .expect("email hooks lock poisoned") = Some(hooks);
    }

    fn is_debug(&self) -> bool {
        self.debug.load(Ordering::SeqCst)
    }

    fn emit(&self, event: &str, data: Value) {
        if let Some(hooks) = self
            .hooks
            .read()
            .expect("email hooks lock poisoned")
            .clone()
        {
            hooks.emit(event, data);
        }
    }
}

async fn list_catalog() -> HttpResponse {
    let catalog: Vec<Value> = EmailProviderKind::all()
        .iter()
        .map(|kind| {
            json!({
                "kind": kind.as_str(),
                "name": kind.display_name(),
                "capabilities": capabilities_for(*kind),
                "settings_fields": settings_fields(*kind),
                "domain_notes": kind.domain_notes(),
                "webhook_notes": kind.webhook_notes(),
            })
        })
        .collect();
    api_json::success_response(json!({ "providers": catalog }), 200)
}

fn paginate_connections(
    mut items: Vec<EmailConnectionView>,
    query: &PaginationParams,
) -> HttpResponse {
    let pagination = query.normalized();
    let total = items.len() as u64;
    let start = ((pagination.page - 1) * pagination.per_page) as usize;
    let page_items: Vec<EmailConnectionView> = items
        .drain(..)
        .skip(start)
        .take(pagination.per_page as usize)
        .collect();
    api_json::success_response(PageResult::new(page_items, total, &pagination).to_json(), 200)
}

async fn list_connections(query: web::Query<PaginationParams>) -> HttpResponse {
    let params = query.into_inner();
    let mut items: Vec<EmailConnection> = connections_store()
        .iter()
        .map(|entry| entry.value().clone())
        .collect();
    if let Some(kind) = params.filters.get("kind") {
        items.retain(|connection| connection.kind.as_str() == kind);
    }
    if let Some(enabled) = params.filters.get("enabled") {
        let wanted = enabled == "true" || enabled == "1";
        items.retain(|connection| connection.enabled == wanted);
    }
    if let Some(search) = &params.search {
        let needle = search.to_lowercase();
        items.retain(|connection| connection.name.to_lowercase().contains(&needle));
    }
    let sort = params.allowed_sort(&["name", "kind", "created_at"], "created_at");
    match sort.as_str() {
        "name" => items.sort_by(|a, b| a.name.cmp(&b.name)),
        "kind" => items.sort_by(|a, b| a.kind.as_str().cmp(b.kind.as_str())),
        _ => items.sort_by(|a, b| a.created_at.cmp(&b.created_at)),
    }
    if params.normalized().order == "desc" {
        items.reverse();
    }
    let views: Vec<EmailConnectionView> = items.into_iter().map(|c| c.view()).collect();
    paginate_connections(views, &params)
}

async fn create_connection(
    state: web::Data<Arc<EmailProviderState>>,
    body: web::Json<CreateConnectionRequest>,
) -> HttpResponse {
    let debug = state.is_debug();
    let input = body.into_inner();
    let kind = match validate_connection(&input) {
        Ok(kind) => kind,
        Err(error) => return api_json::error_response(&error, debug),
    };
    let now = Utc::now();
    let connection = EmailConnection {
        id: Uuid::now_v7(),
        name: input.name.trim().to_string(),
        kind,
        domain: input.domain.filter(|value| !value.trim().is_empty()),
        region: input.region.filter(|value| !value.trim().is_empty()),
        sender_email: input.sender_email.filter(|value| !value.trim().is_empty()),
        sender_name: input.sender_name.filter(|value| !value.trim().is_empty()),
        webhook_url: input.webhook_url.filter(|value| !value.trim().is_empty()),
        credentials: EmailCredentials {
            api_key: input.api_key.filter(|value| !value.trim().is_empty()),
            access_key_id: input.access_key_id.filter(|value| !value.trim().is_empty()),
            secret_access_key: input.secret_access_key.filter(|value| !value.trim().is_empty()),
            webhook_secret: input.webhook_secret.filter(|value| !value.trim().is_empty()),
        },
        test_mode: input.test_mode.unwrap_or(false),
        enabled: input.enabled.unwrap_or(true),
        last_tested_at: None,
        last_status: None,
        created_at: now,
        updated_at: now,
    };
    connections_store().insert(connection.id, connection.clone());
    state.emit(
        EVENT_EMAIL_PROVIDER_CONNECTION_CREATED,
        json!({
            "connection_id": connection.id.to_string(),
            "kind": connection.kind.as_str(),
            "name": connection.name,
        }),
    );
    api_json::success_response(connection.view(), 201)
}

fn parse_connection_id(path: &str) -> UnifiedResult<Uuid> {
    Uuid::parse_str(path).map_err(|_| UnifiedError::bad_request(format!("Invalid connection id \"{path}\"")))
}

async fn get_connection_handler(
    state: web::Data<Arc<EmailProviderState>>,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = state.is_debug();
    let id = match parse_connection_id(&path.into_inner()) {
        Ok(id) => id,
        Err(error) => return api_json::error_response(&error, debug),
    };
    match get_connection(&id) {
        Some(connection) => api_json::success_response(connection.view(), 200),
        None => api_json::error_response(
            &UnifiedError::not_found("Email provider connection not found"),
            debug,
        ),
    }
}

async fn update_connection(
    state: web::Data<Arc<EmailProviderState>>,
    path: web::Path<String>,
    body: web::Json<UpdateConnectionRequest>,
) -> HttpResponse {
    let debug = state.is_debug();
    let id = match parse_connection_id(&path.into_inner()) {
        Ok(id) => id,
        Err(error) => return api_json::error_response(&error, debug),
    };
    let input = body.into_inner();
    let mut connections = connections_store();
    let Some(mut connection) = connections.get_mut(&id) else {
        return api_json::error_response(
            &UnifiedError::not_found("Email provider connection not found"),
            debug,
        );
    };
    // Re-validate the merged record so a partial update cannot drop a
    // credential the provider kind requires.
    let merged = CreateConnectionRequest {
        name: input.name.clone().unwrap_or_else(|| connection.name.clone()),
        kind: connection.kind.as_str().to_string(),
        domain: input.domain.clone().or_else(|| connection.domain.clone()),
        region: input.region.clone().or_else(|| connection.region.clone()),
        sender_email: input.sender_email.clone().or_else(|| connection.sender_email.clone()),
        sender_name: input.sender_name.clone().or_else(|| connection.sender_name.clone()),
        webhook_url: input.webhook_url.clone().or_else(|| connection.webhook_url.clone()),
        api_key: input.api_key.clone().or_else(|| connection.credentials.api_key.clone()),
        access_key_id: input
            .access_key_id
            .clone()
            .or_else(|| connection.credentials.access_key_id.clone()),
        secret_access_key: input
            .secret_access_key
            .clone()
            .or_else(|| connection.credentials.secret_access_key.clone()),
        webhook_secret: input
            .webhook_secret
            .clone()
            .or_else(|| connection.credentials.webhook_secret.clone()),
        test_mode: input.test_mode.or(Some(connection.test_mode)),
        enabled: input.enabled.or(Some(connection.enabled)),
    };
    if let Err(error) = validate_connection(&merged) {
        return api_json::error_response(&error, debug);
    }
    if let Some(name) = input.name {
        connection.name = name.trim().to_string();
    }
    if let Some(domain) = input.domain {
        connection.domain = if domain.trim().is_empty() { None } else { Some(domain) };
    }
    if let Some(region) = input.region {
        connection.region = if region.trim().is_empty() { None } else { Some(region) };
    }
    if let Some(sender_email) = input.sender_email {
        connection.sender_email = if sender_email.trim().is_empty() { None } else { Some(sender_email) };
    }
    if let Some(sender_name) = input.sender_name {
        connection.sender_name = if sender_name.trim().is_empty() { None } else { Some(sender_name) };
    }
    if let Some(webhook_url) = input.webhook_url {
        connection.webhook_url = if webhook_url.trim().is_empty() { None } else { Some(webhook_url) };
    }
    // Secrets are write-only: an omitted field keeps the stored value,
    // an empty string clears it.
    if let Some(api_key) = input.api_key {
        connection.credentials.api_key = if api_key.is_empty() { None } else { Some(api_key) };
    }
    if let Some(access_key_id) = input.access_key_id {
        connection.credentials.access_key_id = if access_key_id.is_empty() { None } else { Some(access_key_id) };
    }
    if let Some(secret_access_key) = input.secret_access_key {
        connection.credentials.secret_access_key =
            if secret_access_key.is_empty() { None } else { Some(secret_access_key) };
    }
    if let Some(webhook_secret) = input.webhook_secret {
        connection.credentials.webhook_secret =
            if webhook_secret.is_empty() { None } else { Some(webhook_secret) };
    }
    if let Some(test_mode) = input.test_mode {
        connection.test_mode = test_mode;
    }
    if let Some(enabled) = input.enabled {
        connection.enabled = enabled;
    }
    connection.updated_at = Utc::now();
    let view = connection.view();
    drop(connections);
    api_json::success_response(view, 200)
}

async fn delete_connection(
    state: web::Data<Arc<EmailProviderState>>,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = state.is_debug();
    let id = match parse_connection_id(&path.into_inner()) {
        Ok(id) => id,
        Err(error) => return api_json::error_response(&error, debug),
    };
    match connections_store().remove(&id) {
        Some((_, connection)) => {
            {
                let mut default = default_connection_store()
                    .write()
                    .expect("default connection lock poisoned");
                if *default == Some(id) {
                    *default = None;
                }
            }
            state.emit(
                EVENT_EMAIL_PROVIDER_CONNECTION_DELETED,
                json!({
                    "connection_id": id.to_string(),
                    "kind": connection.kind.as_str(),
                    "name": connection.name,
                }),
            );
            api_json::success_response(json!({ "deleted": true, "id": id.to_string() }), 200)
        }
        None => api_json::error_response(
            &UnifiedError::not_found("Email provider connection not found"),
            debug,
        ),
    }
}

async fn test_connection_handler(
    state: web::Data<Arc<EmailProviderState>>,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = state.is_debug();
    let id = match parse_connection_id(&path.into_inner()) {
        Ok(id) => id,
        Err(error) => return api_json::error_response(&error, debug),
    };
    let Some(connection) = get_connection(&id) else {
        return api_json::error_response(
            &UnifiedError::not_found("Email provider connection not found"),
            debug,
        );
    };
    let adapter = match email_provider_registry().build(&connection) {
        Ok(adapter) => adapter,
        Err(error) => return api_json::error_response(&error, debug),
    };
    match adapter.test_connection().await {
        Ok(result) => {
            if let Some(mut stored) = connections_store().get_mut(&id) {
                stored.last_tested_at = Some(Utc::now());
                stored.last_status = Some(if result.reachable {
                    "ok".to_string()
                } else {
                    "failed".to_string()
                });
            }
            state.emit(
                EVENT_EMAIL_PROVIDER_CONNECTED,
                json!({
                    "connection_id": id.to_string(),
                    "kind": connection.kind.as_str(),
                    "reachable": result.reachable,
                    "latency_ms": result.latency_ms,
                }),
            );
            api_json::success_response(result, 200)
        }
        Err(error) => {
            if let Some(mut stored) = connections_store().get_mut(&id) {
                stored.last_tested_at = Some(Utc::now());
                stored.last_status = Some("failed".to_string());
            }
            api_json::error_response(&error, debug)
        }
    }
}

#[derive(Debug, Deserialize)]
struct SendInput {
    connection_id: Option<String>,
    to: String,
    from: Option<String>,
    from_name: Option<String>,
    subject: Option<String>,
    body_text: Option<String>,
    body_html: Option<String>,
    reply_to: Option<String>,
    template_id: Option<String>,
    variables: Option<HashMap<String, Value>>,
    metadata: Option<HashMap<String, String>>,
    test_mode: Option<bool>,
}

fn send_input_to_message(input: &SendInput, connection: &EmailConnection) -> UnifiedResult<EmailMessage> {
    validate_email_address(&input.to)?;
    let subject = input.subject.clone().unwrap_or_default();
    if subject.trim().is_empty() && input.template_id.is_none() {
        return Err(UnifiedError::bad_request(
            "Provide a subject or a template_id that renders one",
        ));
    }
    if input.body_text.is_none() && input.body_html.is_none() && input.template_id.is_none() {
        return Err(UnifiedError::bad_request(
            "Provide body_text or body_html (or a template_id)",
        ));
    }
    Ok(EmailMessage {
        to: input.to.clone(),
        from: input.from.clone(),
        from_name: input.from_name.clone(),
        subject,
        body_text: input.body_text.clone(),
        body_html: input.body_html.clone(),
        reply_to: input.reply_to.clone(),
        template_id: input.template_id.clone(),
        variables: input.variables.clone().unwrap_or_default(),
        metadata: input.metadata.clone().unwrap_or_default(),
        test_mode: input.test_mode.unwrap_or(connection.test_mode),
    })
}

async fn send_mail_handler(
    state: web::Data<Arc<EmailProviderState>>,
    body: web::Json<SendInput>,
) -> HttpResponse {
    let debug = state.is_debug();
    let input = body.into_inner();
    let connection = match resolve_connection(input.connection_id.as_deref()) {
        Ok(connection) => connection,
        Err(error) => return api_json::error_response(&error, debug),
    };
    if !connection.enabled {
        return api_json::error_response(
            &UnifiedError::bad_request(format!(
                "Email provider connection \"{}\" is disabled",
                connection.name
            )),
            debug,
        );
    }
    let mut message = match send_input_to_message(&input, &connection) {
        Ok(message) => message,
        Err(error) => return api_json::error_response(&error, debug),
    };
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
            Err(error) => return api_json::error_response(&error, debug),
        }
    }
    let adapter = match email_provider_registry().build(&connection) {
        Ok(adapter) => adapter,
        Err(error) => return api_json::error_response(&error, debug),
    };
    let job_id = Uuid::now_v7().to_string();
    append_log(SendLogEntry {
        id: job_id.clone(),
        connection_id: connection.id.to_string(),
        provider: connection.kind.as_str().to_string(),
        to: message.to.clone(),
        subject: message.subject.clone(),
        status: "queued".to_string(),
        provider_message_id: None,
        test_mode: message.test_mode,
        error: None,
        created_at: Utc::now().to_rfc3339(),
    });
    match adapter.send(&message).await {
        Ok(result) => {
            update_log(
                &job_id,
                if result.test_mode { "test" } else { "sent" },
                result.provider_message_id.clone(),
                None,
            );
            state.emit(
                EVENT_EMAIL_SENT,
                json!({
                    "job_id": job_id,
                    "to": message.to,
                    "connection_id": connection.id.to_string(),
                    "provider": connection.kind.as_str(),
                    "provider_message_id": result.provider_message_id,
                    "test_mode": result.test_mode,
                }),
            );
            api_json::success_response(
                json!({ "job_id": job_id, "connection_id": connection.id.to_string(), "result": result }),
                200,
            )
        }
        Err(error) => {
            update_log(&job_id, "failed", None, Some(error.message.clone()));
            state.emit(
                EVENT_EMAIL_FAILED,
                json!({
                    "job_id": job_id,
                    "to": message.to,
                    "connection_id": connection.id.to_string(),
                    "provider": connection.kind.as_str(),
                    "error": error.message,
                }),
            );
            api_json::error_response(&error, debug)
        }
    }
}

#[derive(Debug, Deserialize)]
struct SendBatchInput {
    connection_id: Option<String>,
    recipients: Vec<String>,
    from: Option<String>,
    from_name: Option<String>,
    subject: Option<String>,
    body_text: Option<String>,
    body_html: Option<String>,
    reply_to: Option<String>,
    template_id: Option<String>,
    variables: Option<HashMap<String, Value>>,
    metadata: Option<HashMap<String, String>>,
    test_mode: Option<bool>,
}

async fn send_batch_handler(
    state: web::Data<Arc<EmailProviderState>>,
    body: web::Json<SendBatchInput>,
) -> HttpResponse {
    let debug = state.is_debug();
    let input = body.into_inner();
    if input.recipients.is_empty() {
        return api_json::error_response(
            &UnifiedError::bad_request("recipients must not be empty"),
            debug,
        );
    }
    if input.recipients.len() > MAX_BATCH_RECIPIENTS {
        return api_json::error_response(
            &UnifiedError::bad_request(format!(
                "recipients must not exceed {MAX_BATCH_RECIPIENTS} per request"
            )),
            debug,
        );
    }
    let connection = match resolve_connection(input.connection_id.as_deref()) {
        Ok(connection) => connection,
        Err(error) => return api_json::error_response(&error, debug),
    };
    if !connection.enabled {
        return api_json::error_response(
            &UnifiedError::bad_request(format!(
                "Email provider connection \"{}\" is disabled",
                connection.name
            )),
            debug,
        );
    }
    let base = SendInput {
        connection_id: input.connection_id.clone(),
        to: input.recipients[0].clone(),
        from: input.from.clone(),
        from_name: input.from_name.clone(),
        subject: input.subject.clone(),
        body_text: input.body_text.clone(),
        body_html: input.body_html.clone(),
        reply_to: input.reply_to.clone(),
        template_id: input.template_id.clone(),
        variables: input.variables.clone(),
        metadata: input.metadata.clone(),
        test_mode: input.test_mode,
    };
    let mut message = match send_input_to_message(&base, &connection) {
        Ok(message) => message,
        Err(error) => return api_json::error_response(&error, debug),
    };
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
            Err(error) => return api_json::error_response(&error, debug),
        }
    }
    let adapter = match email_provider_registry().build(&connection) {
        Ok(adapter) => adapter,
        Err(error) => return api_json::error_response(&error, debug),
    };
    let mut results = Vec::with_capacity(input.recipients.len());
    let mut failures = 0u64;
    for recipient in &input.recipients {
        if let Err(error) = validate_email_address(recipient) {
            results.push(json!({ "to": recipient, "accepted": false, "error": error.message }));
            failures += 1;
            continue;
        }
        let mut single = message.clone();
        single.to = recipient.clone();
        match adapter.send(&single).await {
            Ok(result) => {
                append_log(SendLogEntry {
                    id: Uuid::now_v7().to_string(),
                    connection_id: connection.id.to_string(),
                    provider: connection.kind.as_str().to_string(),
                    to: recipient.clone(),
                    subject: single.subject.clone(),
                    status: if result.test_mode { "test" } else { "sent" }.to_string(),
                    provider_message_id: result.provider_message_id.clone(),
                    test_mode: result.test_mode,
                    error: None,
                    created_at: Utc::now().to_rfc3339(),
                });
                results.push(json!({ "to": recipient, "accepted": true, "result": result }));
            }
            Err(error) => {
                failures += 1;
                append_log(SendLogEntry {
                    id: Uuid::now_v7().to_string(),
                    connection_id: connection.id.to_string(),
                    provider: connection.kind.as_str().to_string(),
                    to: recipient.clone(),
                    subject: single.subject.clone(),
                    status: "failed".to_string(),
                    provider_message_id: None,
                    test_mode: single.test_mode,
                    error: Some(error.message.clone()),
                    created_at: Utc::now().to_rfc3339(),
                });
                results.push(json!({ "to": recipient, "accepted": false, "error": error.message }));
            }
        }
    }
    let sent = (input.recipients.len() as u64) - failures;
    api_json::success_response(
        json!({
            "connection_id": connection.id.to_string(),
            "provider": connection.kind.as_str(),
            "sent": sent,
            "failed": failures,
            "results": results,
        }),
        200,
    )
}

async fn list_send_log(query: web::Query<PaginationParams>) -> HttpResponse {
    let params = query.into_inner();
    let pagination = params.normalized();
    let mut entries = send_log_store()
        .read()
        .expect("send log lock poisoned")
        .clone();
    if let Some(status) = pagination.filters.get("status") {
        entries.retain(|entry| entry.status == *status);
    }
    if let Some(provider) = pagination.filters.get("provider") {
        entries.retain(|entry| entry.provider == *provider);
    }
    if let Some(connection_id) = pagination.filters.get("connection_id") {
        entries.retain(|entry| entry.connection_id == *connection_id);
    }
    if let Some(search) = &pagination.search {
        let needle = search.to_lowercase();
        entries.retain(|entry| {
            entry.to.to_lowercase().contains(&needle)
                || entry.subject.to_lowercase().contains(&needle)
        });
    }
    let sort = params.allowed_sort(&["created_at", "to", "subject", "status"], "created_at");
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
    let page_items: Vec<SendLogEntry> = entries
        .into_iter()
        .skip(start)
        .take(pagination.per_page as usize)
        .collect();
    api_json::success_response(PageResult::new(page_items, total, &pagination).to_json(), 200)
}

async fn get_default_connection() -> HttpResponse {
    match default_connection() {
        Some(connection) => api_json::success_response(
            json!({ "connection_id": connection.id.to_string(), "connection": connection.view() }),
            200,
        ),
        None => api_json::success_response(json!({ "connection_id": null, "connection": null }), 200),
    }
}

#[derive(Debug, Deserialize)]
struct SetDefaultInput {
    connection_id: Option<String>,
}

async fn set_default_connection(body: web::Json<SetDefaultInput>) -> HttpResponse {
    let input = body.into_inner();
    let Some(raw) = input.connection_id else {
        let mut default = default_connection_store()
            .write()
            .expect("default connection lock poisoned");
        *default = None;
        return api_json::success_response(json!({ "connection_id": null }), 200);
    };
    let id = match Uuid::parse_str(&raw) {
        Ok(id) => id,
        Err(_) => {
            return api_json::error_response(
                &UnifiedError::bad_request(format!("Invalid connection id \"{raw}\"")),
                false,
            )
        }
    };
    match get_connection(&id) {
        Some(connection) => {
            {
                let mut default = default_connection_store()
                    .write()
                    .expect("default connection lock poisoned");
                *default = Some(id);
            }
            api_json::success_response(
                json!({ "connection_id": id.to_string(), "connection": connection.view() }),
                200,
            )
        }
        None => api_json::error_response(
            &UnifiedError::not_found("Email provider connection not found"),
            false,
        ),
    }
}

// ---------------------------------------------------------------------------
// Module
// ---------------------------------------------------------------------------

pub struct EmailProvidersModule {
    state: Arc<EmailProviderState>,
}

impl EmailProvidersModule {
    pub fn new() -> Self {
        Self {
            state: Arc::new(EmailProviderState::new()),
        }
    }
}

pub fn register(registry: &ModuleRegistry) {
    registry.register(Arc::new(EmailProvidersModule::new()));
}

impl Module for EmailProvidersModule {
    fn id(&self) -> &'static str {
        "email_providers"
    }

    fn name(&self) -> &'static str {
        "Email Providers"
    }

    fn description(&self) -> &'static str {
        "Email provider registry: Mailchimp Transactional, Brevo, SendGrid, Mailgun, Postmark, Amazon SES, Resend, SparkPost, and SMTP (via the Mail Manager), with per-send provider selection, connection tests, and webhook signature verification."
    }

    fn dependencies(&self) -> Vec<&'static str> {
        vec!["mail", "email-templates"]
    }

    fn init(&self, ctx: &AppContext) -> UnifiedResult<()> {
        self.state.debug.store(ctx.is_debug(), Ordering::SeqCst);
        self.state.set_hooks(ctx.hooks.clone());
        Ok(())
    }

    fn register_hooks(&self, bus: &HookBus) {
        bus.register(
            EVENT_EMAIL_SEND_REQUESTED,
            "email_providers",
            Arc::new(move |ctx: &mut HookContext| on_email_send_requested(ctx)),
        );
        bus.register(
            EVENT_EMAIL_PROVIDER_WEBHOOK_VERIFY,
            "email_providers",
            Arc::new(move |ctx: &mut HookContext| on_webhook_verify(ctx)),
        );
    }

    fn route_info(&self) -> Vec<RouteInfo> {
        vec![
            RouteInfo { method: "GET", path: "/api/v1/email-providers/catalog", owner: "email_providers", description: "Provider catalog: kinds, capabilities, settings fields, domain and webhook notes." },
            RouteInfo { method: "GET", path: "/api/v1/email-providers/connections", owner: "email_providers", description: "Paginated provider connections (masked views)." },
            RouteInfo { method: "POST", path: "/api/v1/email-providers/connections", owner: "email_providers", description: "Create a provider connection." },
            RouteInfo { method: "GET", path: "/api/v1/email-providers/connections/{id}", owner: "email_providers", description: "One provider connection (masked view)." },
            RouteInfo { method: "PUT", path: "/api/v1/email-providers/connections/{id}", owner: "email_providers", description: "Update a provider connection." },
            RouteInfo { method: "DELETE", path: "/api/v1/email-providers/connections/{id}", owner: "email_providers", description: "Delete a provider connection." },
            RouteInfo { method: "POST", path: "/api/v1/email-providers/connections/{id}/test", owner: "email_providers", description: "Test a connection against the real provider API." },
            RouteInfo { method: "POST", path: "/api/v1/email-providers/connections/{id}/test-send", owner: "email_providers", description: "Send a real test message through the connection." },
            RouteInfo { method: "GET", path: "/api/v1/email-providers/default", owner: "email_providers", description: "The global default connection (fallback only)." },
            RouteInfo { method: "PUT", path: "/api/v1/email-providers/default", owner: "email_providers", description: "Set or clear the global default connection." },
            RouteInfo { method: "POST", path: "/api/v1/email-providers/send", owner: "email_providers", description: "Send one message through an explicit provider connection." },
            RouteInfo { method: "POST", path: "/api/v1/email-providers/send-batch", owner: "email_providers", description: "Send one message to many recipients through an explicit provider connection." },
            RouteInfo { method: "GET", path: "/api/v1/email-providers/log", owner: "email_providers", description: "Paginated send log." },
        ]
    }

    fn configure_routes(&self, cfg: &mut actix_web::web::ServiceConfig) {
        let state = self.state.clone();
        cfg.service(
            web::scope("/api/v1/email-providers")
                .app_data(web::Data::new(state))
                .route("/catalog", web::get().to(list_catalog))
                .route("/connections", web::get().to(list_connections))
                .route("/connections", web::post().to(create_connection))
                .route("/default", web::get().to(get_default_connection))
                .route("/default", web::put().to(set_default_connection))
                .route("/send", web::post().to(send_mail_handler))
                .route("/send-batch", web::post().to(send_batch_handler))
                .route("/log", web::get().to(list_log))
                .route("/connections/{id}", web::get().to(get_connection_handler))
                .route("/connections/{id}", web::put().to(update_connection))
                .route("/connections/{id}", web::delete().to(delete_connection))
                .route("/connections/{id}/test", web::post().to(test_connection_handler))
                .route("/connections/{id}/test-send", web::post().to(test_connection_send)),
        );
    }
}

#[derive(Debug, Deserialize)]
struct TestSendInput {
    to: String,
    subject: Option<String>,
}

async fn test_connection_send(
    state: web::Data<Arc<EmailProviderState>>,
    path: web::Path<String>,
    body: web::Json<TestSendInput>,
) -> HttpResponse {
    let debug = state.is_debug();
    let id = match parse_connection_id(&path.into_inner()) {
        Ok(id) => id,
        Err(error) => return api_json::error_response(&error, debug),
    };
    let input = body.into_inner();
    if let Err(error) = validate_email_address(&input.to) {
        return api_json::error_response(&error, debug);
    }
    let Some(connection) = get_connection(&id) else {
        return api_json::error_response(
            &UnifiedError::not_found("Email provider connection not found"),
            debug,
        );
    };
    let message = EmailMessage {
        to: input.to.clone(),
        from: None,
        from_name: None,
        subject: input
            .subject
            .unwrap_or_else(|| "Unified test message".to_string()),
        body_text: Some("This is a test message sent from the Unified Email Providers module.".to_string()),
        body_html: Some("<p>This is a test message sent from the Unified Email Providers module.</p>".to_string()),
        reply_to: None,
        template_id: None,
        variables: HashMap::new(),
        metadata: HashMap::new(),
        test_mode: connection.test_mode,
    };
    let adapter = match email_provider_registry().build(&connection) {
        Ok(adapter) => adapter,
        Err(error) => return api_json::error_response(&error, debug),
    };
    match adapter.send(&message).await {
        Ok(result) => {
            append_log(SendLogEntry {
                id: Uuid::now_v7().to_string(),
                connection_id: connection.id.to_string(),
                provider: connection.kind.as_str().to_string(),
                to: input.to,
                subject: message.subject,
                status: if result.test_mode { "test" } else { "sent" }.to_string(),
                provider_message_id: result.provider_message_id.clone(),
                test_mode: result.test_mode,
                error: None,
                created_at: Utc::now().to_rfc3339(),
            });
            api_json::success_response(
                json!({ "sent": result.accepted, "provider": connection.kind.as_str(), "result": result }),
                200,
            )
        }
        Err(error) => {
            append_log(SendLogEntry {
                id: Uuid::now_v7().to_string(),
                connection_id: connection.id.to_string(),
                provider: connection.kind.as_str().to_string(),
                to: input.to,
                subject: message.subject,
                status: "failed".to_string(),
                provider_message_id: None,
                test_mode: message.test_mode,
                error: Some(error.message.clone()),
                created_at: Utc::now().to_rfc3339(),
            });
            api_json::error_response(&error, debug)
        }
    }
}
