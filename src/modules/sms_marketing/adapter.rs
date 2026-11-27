//! Provider adapter interface: sending goes through one
//! of two adapters, chosen by the provider's kind - the
//! generic HTTP adapter (POST JSON to the configured
//! endpoint with the configured auth) or the log-only test
//! provider (records a send without any network call).
//!
//! No real provider is named or special-cased: the HTTP
//! adapter posts a fixed JSON shape (`to`, `from`,
//! `message`, plus the provider's configured
//! `request_fields`) to whatever endpoint the tenant
//! configured. Live delivery depends entirely on a
//! configured HTTP provider with real credentials; this
//! module seeds no provider and no credential, and the
//! log-only provider never touches the network. Provider
//! error text is truncated before it is stored, and a
//! credential is never logged or included in a stored error.

use super::types::{AuthType, ProviderKind, SmsProvider};
use serde_json::{Value, json};
use std::time::Duration;
use unified_core::error::{UnifiedError, UnifiedResult};
use uuid::Uuid;

/// One send attempt: the rendered body, the recipient, and
/// the sender id the provider should send from.
#[derive(Debug, Clone)]
pub struct SmsSendRequest {
    pub to: String,
    pub from: String,
    pub body: String,
}

/// Outcome of one adapter send attempt.
#[derive(Debug, Clone)]
pub struct SmsSendOutcome {
    /// True when the provider accepted the message.
    pub accepted: bool,
    /// The provider's message id, when the provider
    /// returned one.
    pub provider_message_id: Option<String>,
    /// Failure text (provider error or transport error),
    /// truncated to 500 characters, never a credential.
    pub error: Option<String>,
}

/// The provider adapter interface: one `send` per message.
/// Implementations must never log or return a credential.
#[async_trait::async_trait]
pub trait SmsProviderAdapter: Send + Sync {
    async fn send(
        &self,
        provider: &SmsProvider,
        credential: Option<&str>,
        request: &SmsSendRequest,
    ) -> SmsSendOutcome;
}

/// The log-only test provider: records the send as accepted
/// with a synthetic provider message id and makes no network
/// call. Use it to exercise campaigns end to end without
/// spending credit or contacting any gateway.
pub struct LogOnlyAdapter;

#[async_trait::async_trait]
impl SmsProviderAdapter for LogOnlyAdapter {
    async fn send(
        &self,
        _provider: &SmsProvider,
        _credential: Option<&str>,
        _request: &SmsSendRequest,
    ) -> SmsSendOutcome {
        SmsSendOutcome {
            accepted: true,
            provider_message_id: Some(format!("log-only-{}", Uuid::new_v4())),
            error: None,
        }
    }
}

/// The generic HTTP adapter: POSTs JSON to the provider's
/// configured endpoint. Auth is applied from the provider's
/// auth type and the decrypted credential: `bearer` sends
/// `Authorization: Bearer <credential>`, `api_key_header`
/// sends the credential in the configured header (default
/// `X-Api-Key`), `basic` sends HTTP basic auth with the
/// provider's stored username and the credential as the
/// password, and `none` sends no auth header. A 2xx response
/// is an acceptance; anything else is a failure whose stored
/// error is the status plus the first 500 characters of the
/// response body. The provider message id is read from the
/// response JSON's `message_id`, `id`, or `sid` field when
/// present.
pub struct HttpAdapter {
    client: reqwest::Client,
}

impl HttpAdapter {
    pub fn new() -> UnifiedResult<Self> {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(30))
            .build()
            .map_err(|error| {
                UnifiedError::internal(format!("Cannot build the SMS HTTP client: {error}"))
            })?;
        Ok(Self { client })
    }
}

impl Default for HttpAdapter {
    fn default() -> Self {
        Self::new().expect("reqwest client builds with a timeout")
    }
}

#[async_trait::async_trait]
impl SmsProviderAdapter for HttpAdapter {
    async fn send(
        &self,
        provider: &SmsProvider,
        credential: Option<&str>,
        request: &SmsSendRequest,
    ) -> SmsSendOutcome {
        let Some(endpoint) = provider.endpoint_url.as_deref() else {
            return SmsSendOutcome {
                accepted: false,
                provider_message_id: None,
                error: Some("Provider has no endpoint_url".to_string()),
            };
        };
        let mut body = json!({
            "to": request.to,
            "from": request.from,
            "message": request.body,
        });
        if let Some(fields) = provider.request_fields.as_object() {
            let Some(object) = body.as_object_mut() else {
                return SmsSendOutcome {
                    accepted: false,
                    provider_message_id: None,
                    error: Some("Cannot build the provider request body".to_string()),
                };
            };
            for (key, value) in fields {
                object.insert(key.clone(), value.clone());
            }
        }
        let mut call = self.client.post(endpoint).json(&body);
        match provider.auth_type {
            AuthType::None => {}
            AuthType::Bearer => {
                let Some(credential) = credential else {
                    return SmsSendOutcome {
                        accepted: false,
                        provider_message_id: None,
                        error: Some(
                            "Provider needs a credential for bearer auth; none is stored"
                                .to_string(),
                        ),
                    };
                };
                call = call.bearer_auth(credential);
            }
            AuthType::ApiKeyHeader => {
                let Some(credential) = credential else {
                    return SmsSendOutcome {
                        accepted: false,
                        provider_message_id: None,
                        error: Some(
                            "Provider needs a credential for api-key auth; none is stored"
                                .to_string(),
                        ),
                    };
                };
                let header = provider.auth_header.as_deref().unwrap_or("X-Api-Key");
                call = call.header(header, credential);
            }
            AuthType::Basic => {
                let Some(credential) = credential else {
                    return SmsSendOutcome {
                        accepted: false,
                        provider_message_id: None,
                        error: Some(
                            "Provider needs a credential for basic auth; none is stored"
                                .to_string(),
                        ),
                    };
                };
                let username = provider.auth_username.as_deref().unwrap_or("");
                call = call.basic_auth(username, Some(credential));
            }
        }
        let response = match call.send().await {
            Ok(response) => response,
            Err(error) => {
                return SmsSendOutcome {
                    accepted: false,
                    provider_message_id: None,
                    error: Some(truncate_error(&format!("Provider request failed: {error}"))),
                };
            }
        };
        let status = response.status();
        let text = response.text().await.unwrap_or_default();
        if !status.is_success() {
            return SmsSendOutcome {
                accepted: false,
                provider_message_id: None,
                error: Some(truncate_error(&format!(
                    "Provider returned HTTP {}: {}",
                    status.as_u16(),
                    text
                ))),
            };
        }
        let provider_message_id = serde_json::from_str::<Value>(&text).ok().and_then(|value| {
            value
                .get("message_id")
                .or_else(|| value.get("id"))
                .or_else(|| value.get("sid"))
                .and_then(Value::as_str)
                .map(str::to_string)
        });
        SmsSendOutcome {
            accepted: true,
            provider_message_id,
            error: None,
        }
    }
}

/// Truncate a stored error to 500 characters so a verbose
/// provider response cannot flood the message row.
pub(crate) fn truncate_error(text: &str) -> String {
    if text.len() <= 500 {
        text.to_string()
    } else {
        format!("{}...", &text[..500])
    }
}

/// Dispatch one message through the adapter for its
/// provider's kind. Returns the outcome; the caller records
/// it on the message row.
pub async fn dispatch(
    provider: &SmsProvider,
    credential: Option<&str>,
    request: &SmsSendRequest,
) -> SmsSendOutcome {
    match provider.kind {
        ProviderKind::LogOnly => LogOnlyAdapter.send(provider, credential, request).await,
        ProviderKind::Http => {
            HttpAdapter::default()
                .send(provider, credential, request)
                .await
        }
    }
}
