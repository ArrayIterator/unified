//! email_providers - registry.

use super::{
    adapters::*, credentials::*, events::*, handlers::*, hooks::*, http::*, module::*,
    provider_kinds::*, store::*, worker::*,
};
use async_trait::async_trait;
use reqwest::header::HeaderMap;
use serde_json::Value;
use std::collections::HashMap;
use std::sync::{Arc, OnceLock, RwLock};
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::HookBus;
use unified_core::provider_registry::valid_provider_id;

// ---------------------------------------------------------------------------
// Provider registry: the plug-in point for adapters.
// ---------------------------------------------------------------------------
pub type EmailProviderFactory =
    Arc<dyn Fn(&EmailConnection) -> UnifiedResult<Arc<dyn EmailProvider>> + Send + Sync>;

pub struct EmailProviderRegistry {
    pub(crate) factories: RwLock<HashMap<EmailProviderKind, EmailProviderFactory>>,
    /// Factories keyed by registry definition id (C12, task 269):
    /// the extension point for a provider an admin registered
    /// through the provider registry, which has no enum variant.
    /// The factory receives the connection and decides how to build
    /// its adapter; the connection's `kind` is the carrier kind the
    /// admin stored it under.
    pub(crate) factories_by_id: RwLock<HashMap<String, EmailProviderFactory>>,
}

impl EmailProviderRegistry {
    pub(crate) fn new() -> Self {
        Self {
            factories: RwLock::new(HashMap::new()),
            factories_by_id: RwLock::new(HashMap::new()),
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

    /// Register (or replace) the factory for a registry definition id
    /// (C12, task 269). This is how a provider added through the
    /// provider registry — no code change, no enum variant — gets an
    /// adapter: register its definition, then register its factory
    /// under the same id, once at startup or from an extension.
    pub fn register_factory_by_id(
        &self,
        id: &str,
        factory: EmailProviderFactory,
    ) -> UnifiedResult<()> {
        if !valid_provider_id(id) {
            return Err(UnifiedError::bad_request(format!(
                "Invalid email provider id \"{id}\" (expected lowercase letters, digits, '-' or '_')"
            )));
        }
        self.factories_by_id
            .write()
            .expect("email provider registry lock poisoned")
            .insert(id.to_string(), factory);
        Ok(())
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

    /// Build an adapter through a registry definition id (C12, task
    /// 269). The id must have a factory registered with
    /// `register_factory_by_id`; otherwise this is an honest error
    /// naming the id, never a silent fallback to another provider.
    pub fn build_by_id(
        &self,
        id: &str,
        connection: &EmailConnection,
    ) -> UnifiedResult<Arc<dyn EmailProvider>> {
        let factories = self
            .factories_by_id
            .read()
            .expect("email provider registry lock poisoned");
        let factory = factories.get(id).ok_or_else(|| {
            UnifiedError::internal(format!(
                "No email provider adapter registered for id \"{id}\""
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

    /// Every registered provider id: the built-in kinds plus any
    /// definition ids with a factory, sorted. New providers appear
    /// here with no code change.
    pub fn registered_ids(&self) -> Vec<String> {
        let mut ids: Vec<String> = self
            .registered_kinds()
            .iter()
            .map(|kind| kind.as_str().to_string())
            .collect();
        let by_id = self
            .factories_by_id
            .read()
            .expect("email provider registry lock poisoned");
        ids.extend(by_id.keys().cloned());
        ids.sort();
        ids.dedup();
        ids
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
        registry.register_factory(
            EmailProviderKind::Gmail,
            Arc::new(|connection| {
                Ok(Arc::new(OAuthSmtpAdapter::new(connection)?) as Arc<dyn EmailProvider>)
            }),
        );
        registry.register_factory(
            EmailProviderKind::Yahoo,
            Arc::new(|connection| {
                Ok(Arc::new(OAuthSmtpAdapter::new(connection)?) as Arc<dyn EmailProvider>)
            }),
        );
        registry.register_factory(
            EmailProviderKind::Outlook,
            Arc::new(|connection| {
                Ok(Arc::new(OAuthSmtpAdapter::new(connection)?) as Arc<dyn EmailProvider>)
            }),
        );
        registry.register_factory(
            EmailProviderKind::Sendmail,
            Arc::new(|connection| {
                Ok(Arc::new(SendmailAdapter::new(connection)?) as Arc<dyn EmailProvider>)
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
    async fn webhook_verify(&self, headers: &HeaderMap, raw_body: &[u8]) -> UnifiedResult<bool> {
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

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use uuid::Uuid;

    fn test_connection() -> EmailConnection {
        EmailConnection {
            id: Uuid::new_v4(),
            name: "Test".to_string(),
            kind: EmailProviderKind::Smtp,
            domain: None,
            region: None,
            sender_email: None,
            sender_name: None,
            webhook_url: None,
            smtp_host: None,
            smtp_port: None,
            smtp_tls: None,
            smtp_username: None,
            credentials: EmailCredentials::default(),
            test_mode: false,
            enabled: true,
            last_tested_at: None,
            last_status: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }

    #[test]
    fn build_by_id_dispatches_to_a_registered_definition_factory() {
        // A provider added through the provider registry only: no enum
        // variant, no code change. The factory is invoked by id, an
        // unknown id is an honest error naming the id, and a bad id is
        // rejected at registration time.
        let registry = EmailProviderRegistry::new();
        let connection = test_connection();
        registry
            .register_factory_by_id(
                "acme_mail",
                Arc::new(|_| Err(UnifiedError::internal("factory invoked for acme_mail"))),
            )
            .unwrap();
        assert!(registry.registered_ids().contains(&"acme_mail".to_string()));
        let err = registry.build_by_id("acme_mail", &connection).unwrap_err();
        assert!(err.to_string().contains("factory invoked for acme_mail"));
        let missing = registry.build_by_id("no_such", &connection).unwrap_err();
        assert!(missing.to_string().contains("no_such"));
        assert!(
            registry
                .register_factory_by_id(
                    "Bad Id",
                    Arc::new(|_| Err(UnifiedError::internal("unreachable")))
                )
                .is_err()
        );
    }
}
