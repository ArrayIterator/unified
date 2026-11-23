//! email_providers - provider kinds.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::{Arc, OnceLock};
use unified_core::provider_registry::{
    ProviderDefinition, leak_str, leak_str_slice, provider_registry,
};
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

    /// Human-readable display name, from the provider registry.
    pub fn display_name(&self) -> &'static str {
        attrs(self).display_name
    }

    /// Hosts this provider's API is allowed to talk to, from the
    /// provider registry. A request to any other host is refused
    /// before it is sent.
    pub fn allowed_hosts(&self) -> &'static [&'static str] {
        attrs(self).allowed_hosts
    }

    /// Whether the API host is fixed (allow-listed) or user-supplied.
    /// Exactly the providers with a non-empty host list.
    pub fn has_fixed_hosts(&self) -> bool {
        !self.allowed_hosts().is_empty()
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
        capabilities_for(*self).sandbox
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

    /// The registry definition for this kind, if one is registered.
    /// Every built-in kind has one; `None` only when the registry
    /// was built without seeds (tests).
    pub fn definition(&self) -> Option<Arc<ProviderDefinition>> {
        provider_registry().get("email", self.as_str())
    }
}

/// The enabled provider catalog, built-ins plus any provider an admin
/// registered, sorted by id. New providers appear here with no code
/// change; the enum-based `all()` lists built-ins only.
pub fn catalog() -> Vec<Arc<ProviderDefinition>> {
    provider_registry().list_family_enabled("email")
}

// ---------------------------------------------------------------------------
// Registry-backed attributes (C12, task 269).
// ---------------------------------------------------------------------------
/// The `&'static` view of one provider's registry definition; see the
/// same bridge in the payments module. Capabilities and settings
/// fields are owned types and read the definition directly, so only
/// the display name and host list need the leak cache.
struct EmailAttrs {
    display_name: &'static str,
    allowed_hosts: &'static [&'static str],
}

fn attrs_from_definition(def: &ProviderDefinition) -> EmailAttrs {
    EmailAttrs {
        display_name: leak_str(&def.display_name),
        allowed_hosts: leak_str_slice(&def.allowed_hosts),
    }
}

static ATTRS: OnceLock<HashMap<EmailProviderKind, EmailAttrs>> = OnceLock::new();

fn attrs(kind: &EmailProviderKind) -> &'static EmailAttrs {
    let map = ATTRS.get_or_init(|| {
        let mut out = HashMap::new();
        for def in provider_registry().list_family("email") {
            if let Some(parsed) = EmailProviderKind::from_str(&def.id) {
                out.insert(parsed, attrs_from_definition(&def));
            }
        }
        out
    });
    // Every enum variant has a seed definition; the expect documents
    // that invariant (a registry built without seeds breaks it).
    map.get(kind)
        .expect("every EmailProviderKind has a registry definition")
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


/// Capabilities per kind, from the provider registry (C12, task
/// 269). A missing definition reads as "supports nothing" (fail
/// closed) instead of falling back to another provider.
pub fn capabilities_for(kind: EmailProviderKind) -> EmailCapabilities {
    let def = match kind.definition() {
        Some(def) => def,
        None => {
            return EmailCapabilities {
                transactional: false,
                bulk: false,
                templates: false,
                webhooks: false,
                tracking: false,
                sandbox: false,
            };
        }
    };
    EmailCapabilities {
        transactional: def.capability("transactional"),
        bulk: def.capability("bulk"),
        templates: def.capability("templates"),
        webhooks: def.capability("webhooks"),
        tracking: def.capability("tracking"),
        sandbox: def.capability("sandbox"),
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


/// Settings-page field definitions per provider kind, from the
/// provider registry (C12, task 269). The seed holds the full
/// expanded lists, including the common sender / webhook / test-mode
/// fields. A missing definition yields no fields.
pub(crate) fn settings_fields(kind: EmailProviderKind) -> Vec<SettingsFieldDef> {
    let Some(def) = kind.definition() else {
        return Vec::new();
    };
    def.credentials_shape
        .iter()
        .map(|shape| SettingsFieldDef {
            key: shape.name.clone(),
            label: shape.label.clone(),
            field_type: shape.field_type.clone(),
            required: shape.required,
            secret: shape.secret,
            placeholder: shape.placeholder.clone(),
            help: if shape.help.is_empty() {
                shape.description.clone()
            } else {
                shape.help.clone()
            },
        })
        .collect()
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

    #[test]
    fn builtin_attributes_come_from_registry_data() {
        assert_eq!(
            EmailProviderKind::Mailchimp.display_name(),
            "Mailchimp Transactional (Mandrill)"
        );
        assert_eq!(
            EmailProviderKind::Mailgun.allowed_hosts(),
            &["api.mailgun.net", "api.eu.mailgun.net"]
        );
        assert!(EmailProviderKind::SendGrid.has_fixed_hosts());
        assert!(!EmailProviderKind::Ses.has_fixed_hosts());
        assert!(!EmailProviderKind::Smtp.has_fixed_hosts());
        assert!(EmailProviderKind::SendGrid.has_native_sandbox());
        assert!(!EmailProviderKind::Ses.has_native_sandbox());
        let fields = settings_fields(EmailProviderKind::Mailgun);
        assert_eq!(fields[0].key, "api_key");
        assert!(fields[0].required && fields[0].secret);
        assert!(fields.iter().any(|field| field.key == "domain" && field.required));
        assert!(fields.iter().any(|field| field.key == "sender_email"));
        assert!(fields.iter().any(|field| field.key == "test_mode"));
    }

    #[test]
    fn add_one_email_provider_without_code_change() {
        // Register a brand-new provider through the registry only, then
        // prove the catalog, the definition lookup, and every attribute
        // resolve from that data. No enum variant exists for it and no
        // match arm was added. (Building an adapter for it needs the
        // string-keyed factory in `registry.rs`; the connection model
        // still keys on the enum — see INTEGRATION_NOTES_C12.)
        let mut def = unified_core::provider_registry::ProviderDefinition::new(
            "email",
            "acme_mail",
            "Acme Mail",
            "smtp",
        );
        def.allowed_hosts = vec!["api.acme.test".to_string()];
        def.capabilities = serde_json::json!({"transactional": true, "bulk": true});
        provider_registry().register(def).unwrap();
        assert!(catalog().iter().any(|entry| entry.id == "acme_mail"));
        let found = provider_registry().get("email", "acme_mail").unwrap();
        assert_eq!(found.display_name, "Acme Mail");
        assert!(found.capability("transactional"));
        assert!(found.capability("bulk"));
        assert!(!found.capability("templates"));
        assert!(found.host_allowed("api.acme.test"));
    }
}
