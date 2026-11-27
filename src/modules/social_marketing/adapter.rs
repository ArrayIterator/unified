//! Provider adapter interface: publishing goes through
//! one of two adapters, chosen by the network's kind - the
//! generic HTTP adapter (POST JSON to the configured API
//! base plus the configured publish path, with the
//! configured auth) or the log-only test provider (records
//! a publish without any network call).
//!
//! No real network is named or special-cased: the HTTP
//! adapter posts a fixed JSON shape (`account`, `type`,
//! `text`, `media_ids`, `link`, `hashtags`, plus the
//! network's configured `request_fields`) to whatever
//! endpoint the tenant configured. Live publishing depends
//! entirely on a configured HTTP network with real
//! credentials; this module seeds no network, no account,
//! and no credential, and the log-only network never
//! touches the network. Provider error text is truncated
//! before it is stored, and a credential is never logged
//! or included in a stored error.

use super::types::{AuthType, ProviderKind, SocialAccount, SocialNetwork};
use serde_json::{Value, json};
use std::time::Duration;
use unified_core::error::{UnifiedError, UnifiedResult};
use uuid::Uuid;

/// One publish attempt: the post content and the target
/// account's handle.
#[derive(Debug, Clone)]
pub struct SocialPublishRequest {
    pub account_handle: String,
    pub post_type: String,
    pub text: String,
    pub media_ids: Vec<String>,
    pub link: Option<String>,
    pub hashtags: Vec<String>,
}

/// Outcome of one adapter publish attempt.
#[derive(Debug, Clone)]
pub struct SocialPublishOutcome {
    /// True when the network accepted the post.
    pub accepted: bool,
    /// The network's post id, when the network returned one.
    pub provider_post_id: Option<String>,
    /// Failure text (network error or transport error),
    /// truncated to 500 characters, never a credential.
    pub error: Option<String>,
}

/// The provider adapter interface: one `publish` per
/// target account. Implementations must never log or
/// return a credential.
#[async_trait::async_trait]
pub trait SocialProviderAdapter: Send + Sync {
    async fn publish(
        &self,
        network: &SocialNetwork,
        account: &SocialAccount,
        credential: Option<&str>,
        request: &SocialPublishRequest,
    ) -> SocialPublishOutcome;
}

/// The log-only test provider: records the publish as
/// accepted with a synthetic provider post id and makes no
/// network call. Use it to exercise posts end to end
/// without publishing anything anywhere.
pub struct LogOnlyAdapter;

#[async_trait::async_trait]
impl SocialProviderAdapter for LogOnlyAdapter {
    async fn publish(
        &self,
        _network: &SocialNetwork,
        _account: &SocialAccount,
        _credential: Option<&str>,
        _request: &SocialPublishRequest,
    ) -> SocialPublishOutcome {
        SocialPublishOutcome {
            accepted: true,
            provider_post_id: Some(format!("log-only-{}", Uuid::new_v4())),
            error: None,
        }
    }
}

/// The generic HTTP adapter: POSTs JSON to the network's
/// configured API base plus its publish path. Auth is
/// applied from the network's auth type and the account's
/// decrypted credential: `bearer` sends
/// `Authorization: Bearer <credential>`, `api_key_header`
/// sends the credential in the configured header (default
/// `X-Api-Key`), `basic` sends HTTP basic auth with the
/// account's stored username and the credential as the
/// password, and `none` sends no auth header. A 2xx response
/// is an acceptance; anything else is a failure whose stored
/// error is the status plus the first 500 characters of the
/// response body. The provider post id is read from the
/// response JSON's `post_id`, `message_id`, `id`, or `sid`
/// field when present.
pub struct HttpAdapter {
    client: reqwest::Client,
}

impl HttpAdapter {
    pub fn new() -> UnifiedResult<Self> {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(30))
            .build()
            .map_err(|error| {
                UnifiedError::internal(format!("Cannot build the social HTTP client: {error}"))
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
impl SocialProviderAdapter for HttpAdapter {
    async fn publish(
        &self,
        network: &SocialNetwork,
        _account: &SocialAccount,
        credential: Option<&str>,
        request: &SocialPublishRequest,
    ) -> SocialPublishOutcome {
        let Some(api_base) = network.api_base_url.as_deref() else {
            return SocialPublishOutcome {
                accepted: false,
                provider_post_id: None,
                error: Some("Network has no api_base_url".to_string()),
            };
        };
        let endpoint = format!("{}{}", api_base.trim_end_matches('/'), network.publish_path);
        let mut body = json!({
            "account": request.account_handle,
            "type": request.post_type,
            "text": request.text,
            "media_ids": request.media_ids,
            "link": request.link,
            "hashtags": request.hashtags,
        });
        if let Some(fields) = network.request_fields.as_object() {
            let Some(object) = body.as_object_mut() else {
                return SocialPublishOutcome {
                    accepted: false,
                    provider_post_id: None,
                    error: Some("Cannot build the provider request body".to_string()),
                };
            };
            for (key, value) in fields {
                object.insert(key.clone(), value.clone());
            }
        }
        let mut call = self.client.post(endpoint).json(&body);
        match network.auth_type {
            AuthType::None => {}
            AuthType::Bearer => {
                let Some(credential) = credential else {
                    return SocialPublishOutcome {
                        accepted: false,
                        provider_post_id: None,
                        error: Some(
                            "Network needs a credential for bearer auth; none is stored"
                                .to_string(),
                        ),
                    };
                };
                call = call.bearer_auth(credential);
            }
            AuthType::ApiKeyHeader => {
                let Some(credential) = credential else {
                    return SocialPublishOutcome {
                        accepted: false,
                        provider_post_id: None,
                        error: Some(
                            "Network needs a credential for api-key auth; none is stored"
                                .to_string(),
                        ),
                    };
                };
                let header = network.auth_header.as_deref().unwrap_or("X-Api-Key");
                call = call.header(header, credential);
            }
            AuthType::Basic => {
                let Some(credential) = credential else {
                    return SocialPublishOutcome {
                        accepted: false,
                        provider_post_id: None,
                        error: Some(
                            "Network needs a credential for basic auth; none is stored".to_string(),
                        ),
                    };
                };
                let username = _account.auth_username.as_deref().unwrap_or("");
                call = call.basic_auth(username, Some(credential));
            }
        }
        let response = match call.send().await {
            Ok(response) => response,
            Err(error) => {
                return SocialPublishOutcome {
                    accepted: false,
                    provider_post_id: None,
                    error: Some(truncate_error(&format!("Provider request failed: {error}"))),
                };
            }
        };
        let status = response.status();
        let text = response.text().await.unwrap_or_default();
        if !status.is_success() {
            return SocialPublishOutcome {
                accepted: false,
                provider_post_id: None,
                error: Some(truncate_error(&format!(
                    "Provider returned HTTP {}: {}",
                    status.as_u16(),
                    text
                ))),
            };
        }
        let provider_post_id = serde_json::from_str::<Value>(&text).ok().and_then(|value| {
            value
                .get("post_id")
                .or_else(|| value.get("message_id"))
                .or_else(|| value.get("id"))
                .or_else(|| value.get("sid"))
                .and_then(Value::as_str)
                .map(str::to_string)
        });
        SocialPublishOutcome {
            accepted: true,
            provider_post_id,
            error: None,
        }
    }
}

/// Truncate a stored error to 500 characters so a verbose
/// provider response cannot flood the post row.
pub(crate) fn truncate_error(text: &str) -> String {
    if text.len() <= 500 {
        text.to_string()
    } else {
        format!("{}...", &text[..500])
    }
}

/// Dispatch one publish through the adapter for its
/// network's kind. Returns the outcome; the caller records
/// it on the post row.
pub async fn dispatch(
    network: &SocialNetwork,
    account: &SocialAccount,
    credential: Option<&str>,
    request: &SocialPublishRequest,
) -> SocialPublishOutcome {
    match network.kind {
        ProviderKind::LogOnly => {
            LogOnlyAdapter
                .publish(network, account, credential, request)
                .await
        }
        ProviderKind::Http => {
            HttpAdapter::default()
                .publish(network, account, credential, request)
                .await
        }
    }
}
