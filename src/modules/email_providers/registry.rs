//! email_providers - registry.

use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::{HookBus};
use async_trait::async_trait;
use reqwest::header::{HeaderMap};
use serde_json::{Value};
use std::collections::HashMap;
use std::sync::{Arc, OnceLock, RwLock};
use super::{events::*, provider_kinds::*, credentials::*, store::*, http::*, adapters::*, worker::*, hooks::*, handlers::*, module::*};



// ---------------------------------------------------------------------------
// Provider registry: the plug-in point for adapters.
// ---------------------------------------------------------------------------
pub type EmailProviderFactory = Arc<
    dyn Fn(&EmailConnection) -> UnifiedResult<Arc<dyn EmailProvider>> + Send + Sync,
>;


pub struct EmailProviderRegistry {
    pub(crate) factories: RwLock<HashMap<EmailProviderKind, EmailProviderFactory>>,
}


impl EmailProviderRegistry {
    pub(crate) fn new() -> Self {
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


pub(crate) static REGISTRY: OnceLock<EmailProviderRegistry> = OnceLock::new();


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
pub(crate) static EMAIL_HOOKS: OnceLock<RwLock<Option<Arc<HookBus>>>> = OnceLock::new();


pub(crate) fn email_hooks_store() -> &'static RwLock<Option<Arc<HookBus>>> {
    EMAIL_HOOKS.get_or_init(|| RwLock::new(None))
}


pub(crate) fn email_hooks() -> Option<Arc<HookBus>> {
    email_hooks_store()
        .read()
        .expect("email hooks lock poisoned")
        .clone()
}


pub(crate) fn emit_event(event: &str, data: Value) {
    if let Some(hooks) = email_hooks() {
        hooks.emit(event, data);
    }
}



#[async_trait]
// ---------------------------------------------------------------------------
// Common adapter interface.
// ---------------------------------------------------------------------------
/// The interface every email provider adapter implements. Only `send`
/// and `test_connection` are required; `send_batch` defaults to
/// sending one by one, and `webhook_verify` defaults to the shared
/// secret-token check (or an honest "not supported" error when the
/// provider has no verifiable signature and no secret is configured).
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


pub(crate) fn unsupported(kind: EmailProviderKind, action: &str, capability: &str) -> UnifiedError {
    UnifiedError::bad_request(format!(
        "Provider \"{}\" does not support {action} (capability {capability} is false)",
        kind.as_str()
    ))
}


pub(crate) fn require_api_key(connection: &EmailConnection) -> UnifiedResult<String> {
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
