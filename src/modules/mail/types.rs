//! Mail - event names, settings and message types.
//!
//! Part of the `mail` module split; see `src/modules/mail.rs`.

use chrono::Utc;
use serde::Deserialize;
use serde::Serialize;
use serde_json::Value;
use std::collections::HashMap;

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
pub(crate) struct SmtpSettingsInput {
    pub(crate) host: String,
    pub(crate) port: Option<u16>,
    pub(crate) tls: Option<bool>,
    pub(crate) username: Option<String>,
    pub(crate) password: Option<String>,
    pub(crate) sender_name: Option<String>,
    pub(crate) sender_email: String,
    pub(crate) connector: Option<String>,
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
pub(crate) struct QueuedMail {
    pub(crate) job_id: String,
    pub(crate) to: String,
    pub(crate) subject: String,
    pub(crate) body_text: Option<String>,
    pub(crate) body_html: Option<String>,
    pub(crate) template_id: Option<String>,
    pub(crate) variables: HashMap<String, Value>,
}

/// A message ready to be handed to a delivery connector.
#[derive(Debug, Clone)]
pub(crate) struct OutgoingMessage {
    pub(crate) to: String,
    pub(crate) subject: String,
    pub(crate) body_text: Option<String>,
    pub(crate) body_html: Option<String>,
    pub(crate) template_id: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn smtp_settings_defaults() {
        let settings = SmtpSettings::default();
        assert_eq!(settings.port, 587);
        assert!(settings.tls);
        assert_eq!(settings.connector, "smtp");
        assert!(settings.host.is_empty());
    }
}
