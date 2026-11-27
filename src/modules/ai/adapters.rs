//! ai module - Anthropic and OpenAI-compatible provider adapters.

use unified_core::error::{UnifiedError, UnifiedResult};
use async_trait::async_trait;
use reqwest::header::{AUTHORIZATION, CONTENT_TYPE, HeaderValue};
use serde_json::{Value, json};
use std::time::Instant;
use uuid::Uuid;

use super::handlers_drafts::complete;
use super::helpers::ok;
use super::providers::AiProviderAdapter;
use super::providers::CompletionCall;
use super::providers::ProviderEndpointOptions;
use super::providers::ProviderTestResult;
use super::providers::RawCompletion;
use super::providers::build_client;
use super::providers::send_json;
use super::providers::validate_base_url_with_options;
use super::types::AiProviderConnection;
use super::types::AiProviderKind;

// Adapters.

/// Anthropic Messages API adapter (`POST {base}/v1/messages`).
pub struct AnthropicAdapter {
    pub(crate) kind: AiProviderKind,
    pub(crate) base_url: String,
    pub(crate) model: String,
    pub(crate) api_key: Option<String>,
    pub(crate) provider_id: Uuid,
    pub(crate) client: reqwest::Client,
}

impl AnthropicAdapter {
    pub(crate) fn new(
        connection: &AiProviderConnection,
        api_key: Option<String>,
    ) -> UnifiedResult<Self> {
        // Re-validate the stored base URL (with the connection's
        // own opt-ins) at call time, not only at write time: a
        // connection stored before the SSRF rules, or built
        // synthetically for a personal-key test, must not reach an
        // unvalidated host with the API key.
        validate_base_url_with_options(
            &connection.base_url,
            connection.kind,
            ProviderEndpointOptions::from_connection(connection),
        )?;
        Ok(Self {
            kind: connection.kind,
            base_url: connection.base_url.clone(),
            model: connection.model.clone(),
            api_key,
            provider_id: connection.id,
            client: build_client()?,
        })
    }
}

#[async_trait]
impl AiProviderAdapter for AnthropicAdapter {
    fn kind(&self) -> AiProviderKind {
        self.kind
    }

    fn model(&self) -> &str {
        &self.model
    }

    async fn complete(&self, call: &CompletionCall) -> UnifiedResult<RawCompletion> {
        let key = self.api_key.clone().ok_or_else(|| {
            UnifiedError::bad_request("Provider \"anthropic\" requires an API key")
        })?;
        let key_header = HeaderValue::from_str(&key).map_err(|_| {
            UnifiedError::internal("Stored provider key is not a valid HTTP header value")
        })?;
        let url = format!("{}/v1/messages", self.base_url.trim_end_matches('/'));
        let mut body = json!({
            "model": self.model,
            "max_tokens": call.max_tokens,
            "messages": [{ "role": "user", "content": call.prompt }],
        });
        if let Some(system) = &call.system_prompt {
            body["system"] = Value::String(system.clone());
        }
        if let Some(temperature) = call.temperature {
            body["temperature"] = Value::from(temperature);
        }
        let response = send_json(
            &self.client,
            self.client
                .post(&url)
                .header("x-api-key", key_header)
                .header("anthropic-version", "2023-06-01")
                .header(CONTENT_TYPE, "application/json")
                .json(&body),
            "anthropic",
        )
        .await?;
        let text = response
            .get("content")
            .and_then(|content| content.as_array())
            .and_then(|blocks| blocks.first())
            .and_then(|block| block.get("text"))
            .and_then(|value| value.as_str())
            .ok_or_else(|| {
                UnifiedError::internal("Provider \"anthropic\" returned a response without text")
            })?
            .to_string();
        let prompt_tokens = response
            .get("usage")
            .and_then(|usage| usage.get("input_tokens"))
            .and_then(|value| value.as_u64())
            .map(|value| value as u32);
        let completion_tokens = response
            .get("usage")
            .and_then(|usage| usage.get("output_tokens"))
            .and_then(|value| value.as_u64())
            .map(|value| value as u32);
        Ok(RawCompletion {
            text,
            prompt_tokens,
            completion_tokens,
            model: self.model.clone(),
        })
    }

    async fn test_connection(&self) -> UnifiedResult<ProviderTestResult> {
        let key = self.api_key.clone().ok_or_else(|| {
            UnifiedError::bad_request("Provider \"anthropic\" requires an API key")
        })?;
        let key_header = HeaderValue::from_str(&key).map_err(|_| {
            UnifiedError::internal("Stored provider key is not a valid HTTP header value")
        })?;
        let started = Instant::now();
        let url = format!("{}/v1/models", self.base_url.trim_end_matches('/'));
        let response = send_json(
            &self.client,
            self.client
                .get(&url)
                .header("x-api-key", key_header)
                .header("anthropic-version", "2023-06-01"),
            "anthropic",
        )
        .await?;
        let model_count = response
            .get("data")
            .and_then(|value| value.as_array())
            .map(|items| items.len());
        Ok(ProviderTestResult {
            provider_id: self.provider_id,
            kind: self.kind.as_str().to_string(),
            reachable: true,
            latency_ms: started.elapsed().as_millis() as u64,
            model_count,
            detail: "Connection test succeeded".to_string(),
        })
    }
}

/// OpenAI Chat Completions adapter (`POST {base}/chat/completions`),
/// shared by OpenAI, OpenAI-compatible, and local endpoints: the wire
/// format is identical, only the base URL and the key differ.
pub struct OpenAiCompatibleAdapter {
    pub(crate) kind: AiProviderKind,
    pub(crate) base_url: String,
    pub(crate) model: String,
    pub(crate) api_key: Option<String>,
    pub(crate) provider_id: Uuid,
    pub(crate) client: reqwest::Client,
}

impl OpenAiCompatibleAdapter {
    pub(crate) fn new(
        connection: &AiProviderConnection,
        api_key: Option<String>,
    ) -> UnifiedResult<Self> {
        // Re-validate the stored base URL (with the connection's
        // own opt-ins) at call time; see `AnthropicAdapter::new`.
        validate_base_url_with_options(
            &connection.base_url,
            connection.kind,
            ProviderEndpointOptions::from_connection(connection),
        )?;
        Ok(Self {
            kind: connection.kind,
            base_url: connection.base_url.clone(),
            model: connection.model.clone(),
            api_key,
            provider_id: connection.id,
            client: build_client()?,
        })
    }

    pub(crate) fn models_url(&self) -> String {
        format!("{}/models", self.base_url.trim_end_matches('/'))
    }

    pub(crate) fn completions_url(&self) -> String {
        format!("{}/chat/completions", self.base_url.trim_end_matches('/'))
    }

    pub(crate) fn auth_header(&self) -> Option<HeaderValue> {
        self.api_key
            .as_ref()
            .and_then(|key| HeaderValue::from_str(&format!("Bearer {key}")).ok())
    }
}

#[async_trait]
impl AiProviderAdapter for OpenAiCompatibleAdapter {
    fn kind(&self) -> AiProviderKind {
        self.kind
    }

    fn model(&self) -> &str {
        &self.model
    }

    async fn complete(&self, call: &CompletionCall) -> UnifiedResult<RawCompletion> {
        if self.kind.key_required() && self.api_key.as_deref().is_none_or(|key| key.is_empty()) {
            return Err(UnifiedError::bad_request(format!(
                "Provider \"{}\" requires an API key",
                self.kind.as_str()
            )));
        }
        let mut messages: Vec<Value> = Vec::new();
        if let Some(system) = &call.system_prompt {
            messages.push(json!({ "role": "system", "content": system }));
        }
        messages.push(json!({ "role": "user", "content": call.prompt }));
        let mut body = json!({
            "model": self.model,
            "messages": messages,
            "max_tokens": call.max_tokens,
        });
        if let Some(temperature) = call.temperature {
            body["temperature"] = Value::from(temperature);
        }
        let mut request = self
            .client
            .post(self.completions_url())
            .header(CONTENT_TYPE, "application/json")
            .json(&body);
        if let Some(auth) = self.auth_header() {
            request = request.header(AUTHORIZATION, auth);
        }
        let response = send_json(&self.client, request, self.kind.as_str()).await?;
        let text = response
            .get("choices")
            .and_then(|choices| choices.as_array())
            .and_then(|choices| choices.first())
            .and_then(|choice| choice.get("message"))
            .and_then(|message| message.get("content"))
            .and_then(|value| value.as_str())
            .ok_or_else(|| {
                UnifiedError::internal(format!(
                    "Provider \"{}\" returned a response without text",
                    self.kind.as_str()
                ))
            })?
            .to_string();
        let prompt_tokens = response
            .get("usage")
            .and_then(|usage| usage.get("prompt_tokens"))
            .and_then(|value| value.as_u64())
            .map(|value| value as u32);
        let completion_tokens = response
            .get("usage")
            .and_then(|usage| usage.get("completion_tokens"))
            .and_then(|value| value.as_u64())
            .map(|value| value as u32);
        Ok(RawCompletion {
            text,
            prompt_tokens,
            completion_tokens,
            model: self.model.clone(),
        })
    }

    async fn test_connection(&self) -> UnifiedResult<ProviderTestResult> {
        if self.kind.key_required() && self.api_key.as_deref().is_none_or(|key| key.is_empty()) {
            return Err(UnifiedError::bad_request(format!(
                "Provider \"{}\" requires an API key",
                self.kind.as_str()
            )));
        }
        let started = Instant::now();
        let mut request = self.client.get(self.models_url());
        if let Some(auth) = self.auth_header() {
            request = request.header(AUTHORIZATION, auth);
        }
        let response = send_json(&self.client, request, self.kind.as_str()).await?;
        let model_count = response
            .get("data")
            .and_then(|value| value.as_array())
            .map(|items| items.len());
        Ok(ProviderTestResult {
            provider_id: self.provider_id,
            kind: self.kind.as_str().to_string(),
            reachable: true,
            latency_ms: started.elapsed().as_millis() as u64,
            model_count,
            detail: "Connection test succeeded".to_string(),
        })
    }
}
