//! email_providers - provider kinds.

use serde::{Deserialize, Serialize};
use super::{events::*, credentials::*, store::*, http::*, registry::*, adapters::*, worker::*, hooks::*, handlers::*, module::*};



#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
// ---------------------------------------------------------------------------
// Provider kinds and capabilities.
// ---------------------------------------------------------------------------
/// Every email provider this module can talk to.
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


pub(crate) fn settings_fields(kind: EmailProviderKind) -> Vec<SettingsFieldDef> {
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
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capabilities_for_marks_sandbox_support() {
        assert!(!capabilities_for(EmailProviderKind::Mailchimp).sandbox);
        assert!(capabilities_for(EmailProviderKind::SendGrid).sandbox);
        assert!(capabilities_for(EmailProviderKind::Mailgun).sandbox);
        assert!(!capabilities_for(EmailProviderKind::Ses).sandbox);
    }

    #[test]
    fn capabilities_for_reflects_provider_limits() {
        let postmark = capabilities_for(EmailProviderKind::Postmark);
        assert!(postmark.transactional);
        assert!(!postmark.bulk);
        assert!(postmark.templates);
        let smtp = capabilities_for(EmailProviderKind::Smtp);
        assert!(smtp.transactional);
        assert!(!smtp.webhooks);
        assert!(!smtp.tracking);
        let ses = capabilities_for(EmailProviderKind::Ses);
        assert!(!ses.templates);
        assert!(ses.bulk);
    }
}
