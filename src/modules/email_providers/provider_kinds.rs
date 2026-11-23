//! email_providers - provider kinds.

use super::{
    adapters::*, credentials::*, events::*, handlers::*, hooks::*, http::*, module::*, registry::*,
    store::*, worker::*,
};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::{Arc, OnceLock};
use unified_core::provider_registry::{
    ProviderDefinition, leak_str, leak_str_slice, provider_registry,
};

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
    Gmail,
    Yahoo,
    Outlook,
    Sendmail,
    Zoho,
    ZohoEu,
    Proton,
    Yandex,
    Fastmail,
    Icloud,
    Gmx,
    MailCom,
    Aol,
    QqMail,
    Mail163,
    Mail126,
    Sina,
    MailRu,
    Posteo,
    Migadu,
    Mailfence,
    Startmail,
    Runbox,
    Kolabnow,
    Infomaniak,
    Ovh,
    Gandi,
    Namecheap,
    Dreamhost,
    Mxroute,
    SenderNet,
    Sendpulse,
    Mailjet,
    Mailtrap,
    ElasticEmail,
    Smtp2go,
    Mailersend,
    Zeptomail,
    Sendlayer,
    Socketlabs,
    SmtpCom,
    CampaignMonitor,
    Plunk,
    Courier,
    Loops,
    Onesignal,
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
            Self::Gmail => "gmail",
            Self::Yahoo => "yahoo",
            Self::Outlook => "outlook",
            Self::Sendmail => "sendmail",
            Self::Zoho => "zoho",
            Self::ZohoEu => "zoho-eu",
            Self::Proton => "proton",
            Self::Yandex => "yandex",
            Self::Fastmail => "fastmail",
            Self::Icloud => "icloud",
            Self::Gmx => "gmx",
            Self::MailCom => "mail-com",
            Self::Aol => "aol",
            Self::QqMail => "qq-mail",
            Self::Mail163 => "mail-163",
            Self::Mail126 => "mail-126",
            Self::Sina => "sina",
            Self::MailRu => "mail-ru",
            Self::Posteo => "posteo",
            Self::Migadu => "migadu",
            Self::Mailfence => "mailfence",
            Self::Startmail => "startmail",
            Self::Runbox => "runbox",
            Self::Kolabnow => "kolabnow",
            Self::Infomaniak => "infomaniak",
            Self::Ovh => "ovh",
            Self::Gandi => "gandi",
            Self::Namecheap => "namecheap",
            Self::Dreamhost => "dreamhost",
            Self::Mxroute => "mxroute",
            Self::SenderNet => "sender-net",
            Self::Sendpulse => "sendpulse",
            Self::Mailjet => "mailjet",
            Self::Mailtrap => "mailtrap",
            Self::ElasticEmail => "elastic-email",
            Self::Smtp2go => "smtp2go",
            Self::Mailersend => "mailersend",
            Self::Zeptomail => "zeptomail",
            Self::Sendlayer => "sendlayer",
            Self::Socketlabs => "socketlabs",
            Self::SmtpCom => "smtp-com",
            Self::CampaignMonitor => "campaign-monitor",
            Self::Plunk => "plunk",
            Self::Courier => "courier",
            Self::Loops => "loops",
            Self::Onesignal => "onesignal",
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
            "gmail" => Some(Self::Gmail),
            "yahoo" => Some(Self::Yahoo),
            "outlook" => Some(Self::Outlook),
            "sendmail" => Some(Self::Sendmail),
            "zoho" => Some(Self::Zoho),
            "zoho-eu" => Some(Self::ZohoEu),
            "proton" => Some(Self::Proton),
            "yandex" => Some(Self::Yandex),
            "fastmail" => Some(Self::Fastmail),
            "icloud" => Some(Self::Icloud),
            "gmx" => Some(Self::Gmx),
            "mail-com" => Some(Self::MailCom),
            "aol" => Some(Self::Aol),
            "qq-mail" => Some(Self::QqMail),
            "mail-163" => Some(Self::Mail163),
            "mail-126" => Some(Self::Mail126),
            "sina" => Some(Self::Sina),
            "mail-ru" => Some(Self::MailRu),
            "posteo" => Some(Self::Posteo),
            "migadu" => Some(Self::Migadu),
            "mailfence" => Some(Self::Mailfence),
            "startmail" => Some(Self::Startmail),
            "runbox" => Some(Self::Runbox),
            "kolabnow" => Some(Self::Kolabnow),
            "infomaniak" => Some(Self::Infomaniak),
            "ovh" => Some(Self::Ovh),
            "gandi" => Some(Self::Gandi),
            "namecheap" => Some(Self::Namecheap),
            "dreamhost" => Some(Self::Dreamhost),
            "mxroute" => Some(Self::Mxroute),
            "sender-net" => Some(Self::SenderNet),
            "sendpulse" => Some(Self::Sendpulse),
            "mailjet" => Some(Self::Mailjet),
            "mailtrap" => Some(Self::Mailtrap),
            "elastic-email" => Some(Self::ElasticEmail),
            "smtp2go" => Some(Self::Smtp2go),
            "mailersend" => Some(Self::Mailersend),
            "zeptomail" => Some(Self::Zeptomail),
            "sendlayer" => Some(Self::Sendlayer),
            "socketlabs" => Some(Self::Socketlabs),
            "smtp-com" => Some(Self::SmtpCom),
            "campaign-monitor" => Some(Self::CampaignMonitor),
            "plunk" => Some(Self::Plunk),
            "courier" => Some(Self::Courier),
            "loops" => Some(Self::Loops),
            "onesignal" => Some(Self::Onesignal),
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
            Self::Gmail,
            Self::Yahoo,
            Self::Outlook,
            Self::Sendmail,
            Self::Zoho,
            Self::ZohoEu,
            Self::Proton,
            Self::Yandex,
            Self::Fastmail,
            Self::Icloud,
            Self::Gmx,
            Self::MailCom,
            Self::Aol,
            Self::QqMail,
            Self::Mail163,
            Self::Mail126,
            Self::Sina,
            Self::MailRu,
            Self::Posteo,
            Self::Migadu,
            Self::Mailfence,
            Self::Startmail,
            Self::Runbox,
            Self::Kolabnow,
            Self::Infomaniak,
            Self::Ovh,
            Self::Gandi,
            Self::Namecheap,
            Self::Dreamhost,
            Self::Mxroute,
            Self::SenderNet,
            Self::Sendpulse,
            Self::Mailjet,
            Self::Mailtrap,
            Self::ElasticEmail,
            Self::Smtp2go,
            Self::Mailersend,
            Self::Zeptomail,
            Self::Sendlayer,
            Self::Socketlabs,
            Self::SmtpCom,
            Self::CampaignMonitor,
            Self::Plunk,
            Self::Courier,
            Self::Loops,
            Self::Onesignal,
        ]
    }

    /// Logo reference for the catalog and settings UI: the stem from
    /// the provider registry (`"email-gmail"`), falling back to
    /// `email-<id>` when a definition carries no logo. The SVG lives
    /// in the module's `logos/` sprite as
    /// `provider-logo-email-gmail`; the catalog returns only this
    /// stem, never a URL.
    pub fn logo(&self) -> String {
        match self.definition().and_then(|def| def.logo.clone()) {
            Some(logo) if !logo.trim().is_empty() => logo,
            _ => format!("email-{}", self.as_str()),
        }
    }

    /// True for the account providers that authenticate with OAuth2
    /// (Gmail, Yahoo, Outlook).
    pub fn is_oauth(&self) -> bool {
        super::oauth::is_oauth_kind(*self)
    }

    /// The OAuth2 configuration for the account providers, `None`
    /// for every other kind.
    pub fn oauth_config(&self) -> Option<&'static super::oauth::OAuthProviderConfig> {
        super::oauth::config_for(*self)
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
            Self::Mailchimp => {
                "Sending domains must be verified in Mandrill (Settings > Sending Domains); add SPF and DKIM records for the domain before sending bulk mail."
            }
            Self::Brevo => {
                "Verify the sender domain in Brevo (Senders & IP > Domains) and add the SPF and DKIM records Brevo shows; unverified senders are limited."
            }
            Self::SendGrid => {
                "Authenticate the sender domain in SendGrid (Settings > Sender Authentication) with SPF and DKIM; single sender verification is enough for low volume."
            }
            Self::Mailgun => {
                "Add and verify the domain in Mailgun (Sending > Domains) with the SPF and DKIM TXT records; sandbox domains only deliver to authorized recipients."
            }
            Self::Postmark => {
                "Verify the sender signature (Sender Signatures) or the whole domain with DKIM in Postmark; every From address must be verified."
            }
            Self::Ses => {
                "Verify the sender identity (email address or domain) in SES; new accounts are in the SES sandbox and can only send to verified addresses until production access is granted."
            }
            Self::Resend => {
                "Verify the sending domain in Resend (Domains) with SPF and DKIM; unverified accounts can only send to the account owner address."
            }
            Self::SparkPost => {
                "Verify the sending domain in SparkPost (Account > Sending Domains) with SPF and DKIM records."
            }
            Self::Smtp => {
                "Delivery goes directly to the SMTP host on this connection; the From domain should have SPF and DKIM records for that host."
            }
            Self::Gmail => {
                "The sender address must be the linked Google account; Gmail rejects mail sent from any other address over XOAUTH2."
            }
            Self::Yahoo => {
                "The sender address must be the linked Yahoo account; Yahoo rejects mail sent from any other address over XOAUTH2."
            }
            Self::Outlook => {
                "The sender address must be the linked Microsoft account; Outlook rejects mail sent from any other address over XOAUTH2."
            }
            Self::Zoho | Self::ZohoEu | Self::Proton | Self::Yandex | Self::Fastmail | Self::Icloud | Self::Gmx | Self::MailCom | Self::Aol | Self::QqMail | Self::Mail163 | Self::Mail126 | Self::Sina | Self::MailRu | Self::Posteo | Self::Migadu | Self::Mailfence | Self::Startmail | Self::Runbox | Self::Kolabnow | Self::Infomaniak | Self::Ovh | Self::Gandi | Self::Namecheap | Self::Dreamhost | Self::Mxroute | Self::SenderNet | Self::Sendpulse | Self::Mailjet | Self::Mailtrap | Self::ElasticEmail | Self::Smtp2go | Self::Mailersend | Self::Zeptomail | Self::Sendlayer | Self::Socketlabs | Self::SmtpCom | Self::CampaignMonitor => {
                "Delivery goes through this provider's SMTP relay (preset host and port from the catalog); the From domain should have SPF and DKIM records for that relay."
            }
            Self::Plunk => {
                "Verify the sending domain in Plunk (Settings > Sending Domains) and add the SPF and DKIM records Plunk shows."
            }
            Self::Courier => {
                "Verify the sending domain in Courier (Settings > Sending Domains) with SPF and DKIM records."
            }
            Self::Loops => {
                "Verify the sending domain in Loops (Settings > Sending Domain) with SPF and DKIM records."
            }
            Self::Onesignal => {
                "Verify the sending domain in OneSignal (Settings > Email) with SPF and DKIM records."
            }
            Self::Sendmail => {
                "Local delivery only: the From domain needs no DNS records, but receiving servers may reject mail from an unconfigured host."
            }
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
            Self::Mailchimp => {
                "Mandrill webhooks are verified with HMAC-SHA1 over the webhook URL plus the sorted POST parameters, using the webhook key. Set webhook_url to the exact public webhook URL."
            }
            Self::Brevo => {
                "Brevo sends no signature; set a webhook_secret on the connection and Brevo must send it back as the X-Webhook-Secret header (configure it in the Brevo webhook URL as a custom header)."
            }
            Self::SendGrid => {
                "SendGrid Event Webhook signatures use ECDSA verification, which this module does not implement; set a webhook_secret on the connection and require it as the X-Webhook-Secret header instead."
            }
            Self::Mailgun => {
                "Mailgun webhooks are verified with HMAC-SHA256 over timestamp + token, using the webhook signing key (webhook_secret)."
            }
            Self::Postmark => {
                "Postmark sends no signature; set a webhook_secret on the connection and require it as the X-Webhook-Secret header (Postmark supports basic auth on the webhook URL as an alternative)."
            }
            Self::Ses => {
                "SES delivers events through SNS, whose signatures need RSA certificate verification, which this module does not implement; set a webhook_secret on the connection and require it as the X-Webhook-Secret header instead."
            }
            Self::Resend => {
                "Resend webhooks are Svix-signed and verified with HMAC-SHA256 over the svix id, timestamp and body, using the webhook signing secret (webhook_secret)."
            }
            Self::SparkPost => {
                "SparkPost sends no signature; set a webhook_secret on the connection and require it as the X-Webhook-Secret header."
            }
            Self::Smtp => {
                "SMTP has no webhooks; bounce and complaint handling is not available for this provider."
            }
            Self::Gmail => {
                "Gmail has no webhooks; bounce and complaint handling is not available for this provider."
            }
            Self::Yahoo => {
                "Yahoo has no webhooks; bounce and complaint handling is not available for this provider."
            }
            Self::Outlook => {
                "Outlook has no webhooks; bounce and complaint handling is not available for this provider."
            }
            Self::Zoho | Self::ZohoEu | Self::Proton | Self::Yandex | Self::Fastmail | Self::Icloud | Self::Gmx | Self::MailCom | Self::Aol | Self::QqMail | Self::Mail163 | Self::Mail126 | Self::Sina | Self::MailRu | Self::Posteo | Self::Migadu | Self::Mailfence | Self::Startmail | Self::Runbox | Self::Kolabnow | Self::Infomaniak | Self::Ovh | Self::Gandi | Self::Namecheap | Self::Dreamhost | Self::Mxroute | Self::SenderNet | Self::Sendpulse | Self::Mailjet | Self::Mailtrap | Self::ElasticEmail | Self::Smtp2go | Self::Mailersend | Self::Zeptomail | Self::Sendlayer | Self::Socketlabs | Self::SmtpCom | Self::CampaignMonitor => {
                "SMTP has no webhooks; bounce and complaint handling is not available for this provider."
            }
            Self::Plunk | Self::Courier | Self::Loops | Self::Onesignal => {
                "Webhook verification for this provider is not implemented; set a webhook_secret on the connection and require it as the X-Webhook-Secret header instead."
            }
            Self::Sendmail => {
                "Local sendmail has no webhooks; bounces go to the sender mailbox on the server."
            }
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
    match kind.definition() {
        Some(def) => capabilities_for_definition(&def),
        None => EmailCapabilities {
            transactional: false,
            bulk: false,
            templates: false,
            webhooks: false,
            tracking: false,
            sandbox: false,
        },
    }
}

/// Capabilities from a registry definition directly (task 354):
/// the catalog entry for a hook-provided definition has no enum
/// kind, so it reads its capabilities here.
pub fn capabilities_for_definition(def: &ProviderDefinition) -> EmailCapabilities {
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
#[derive(Debug, Clone, Serialize, Deserialize)]
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
    settings_fields_for_definition(&def)
}

/// Settings-page field definitions from a registry definition
/// directly (task 354): the catalog entry for a hook-provided
/// definition has no enum kind, so it reads its fields here.
pub(crate) fn settings_fields_for_definition(
    def: &ProviderDefinition,
) -> Vec<SettingsFieldDef> {
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
        assert!(
            fields
                .iter()
                .any(|field| field.key == "domain" && field.required)
        );
        assert!(fields.iter().any(|field| field.key == "sender_email"));
        assert!(fields.iter().any(|field| field.key == "test_mode"));
    }

    #[test]
    fn new_kinds_have_capabilities_logos_and_fields() {
        assert_eq!(EmailProviderKind::all().len(), 55);
        // OAuth kinds: transactional only, no webhooks, no sandbox.
        for kind in [
            EmailProviderKind::Gmail,
            EmailProviderKind::Yahoo,
            EmailProviderKind::Outlook,
        ] {
            let caps = capabilities_for(kind);
            assert!(caps.transactional);
            assert!(
                !caps.bulk && !caps.templates && !caps.webhooks && !caps.tracking && !caps.sandbox
            );
            assert!(kind.is_oauth());
            assert!(kind.oauth_config().is_some());
            assert_eq!(kind.logo(), format!("email-{}", kind.as_str()));
        }
        // Sendmail: no credentials, no OAuth, bulk-capable local delivery.
        let sendmail = capabilities_for(EmailProviderKind::Sendmail);
        assert!(sendmail.transactional && sendmail.bulk);
        assert!(!sendmail.webhooks);
        assert!(!EmailProviderKind::Sendmail.is_oauth());
        assert_eq!(EmailProviderKind::Sendmail.logo(), "email-sendmail");
        // SMTP shape now carries the full connection settings.
        let smtp_fields = settings_fields(EmailProviderKind::Smtp);
        for key in [
            "host",
            "port",
            "tls_mode",
            "username",
            "password",
            "sender_email",
        ] {
            assert!(
                smtp_fields.iter().any(|field| field.key == key),
                "smtp field {key} missing"
            );
        }
        assert!(
            smtp_fields
                .iter()
                .any(|field| field.key == "password" && field.secret)
        );
        // OAuth shapes carry the app credentials and the sender address.
        let gmail_fields = settings_fields(EmailProviderKind::Gmail);
        assert!(
            gmail_fields
                .iter()
                .any(|field| field.key == "client_id" && field.required)
        );
        assert!(
            gmail_fields
                .iter()
                .any(|field| field.key == "client_secret" && field.required && field.secret)
        );
        assert!(
            gmail_fields
                .iter()
                .any(|field| field.key == "sender_email" && field.required)
        );
        // Sendmail shape needs no secret at all.
        let sendmail_fields = settings_fields(EmailProviderKind::Sendmail);
        assert!(!sendmail_fields.iter().any(|field| field.secret));
        assert!(
            sendmail_fields
                .iter()
                .any(|field| field.key == "sender_email")
        );
    }

    #[test]
    fn task354_preset_and_generic_kinds() {
        // SMTP presets: transactional, no webhooks, logo stem from
        // the registry, fields carry username/password/sender_email,
        // never OAuth.
        for kind in [
            EmailProviderKind::Zoho,
            EmailProviderKind::Proton,
            EmailProviderKind::Mailjet,
            EmailProviderKind::Mailtrap,
            EmailProviderKind::CampaignMonitor,
        ] {
            let caps = capabilities_for(kind);
            assert!(caps.transactional, "kind {}", kind.as_str());
            assert!(!caps.webhooks);
            assert!(!kind.is_oauth());
            assert_eq!(kind.logo(), format!("email-{}", kind.as_str()));
            let fields = settings_fields(kind);
            assert!(
                fields.iter().any(|field| field.key == "username" && field.required),
                "kind {} has no username field",
                kind.as_str()
            );
            assert!(
                fields
                    .iter()
                    .any(|field| field.key == "password" && field.required && field.secret),
                "kind {} has no secret password field",
                kind.as_str()
            );
            assert!(fields.iter().any(|field| field.key == "sender_email"));
        }
        // Mailbox presets are not bulk; relay presets are.
        assert!(!capabilities_for(EmailProviderKind::Zoho).bulk);
        assert!(capabilities_for(EmailProviderKind::Mailjet).bulk);
        assert!(capabilities_for(EmailProviderKind::Mailtrap).sandbox);
        // Generic HTTP kinds: API key field, no OAuth, logo stem.
        for kind in [
            EmailProviderKind::Plunk,
            EmailProviderKind::Courier,
            EmailProviderKind::Loops,
            EmailProviderKind::Onesignal,
        ] {
            assert!(capabilities_for(kind).transactional);
            assert!(!kind.is_oauth());
            let fields = settings_fields(kind);
            assert!(
                fields
                    .iter()
                    .any(|field| field.key == "api_key" && field.required && field.secret),
                "kind {} has no secret api_key field",
                kind.as_str()
            );
        }
        // Loops is template-based; OneSignal needs its App ID.
        assert!(capabilities_for(EmailProviderKind::Loops).templates);
        assert!(
            settings_fields(EmailProviderKind::Onesignal)
                .iter()
                .any(|field| field.key == "domain" && field.required)
        );
        // Every kind resolves its definition from the registry.
        for kind in EmailProviderKind::all() {
            assert!(
                kind.definition().is_some(),
                "kind {} has no definition",
                kind.as_str()
            );
        }
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
