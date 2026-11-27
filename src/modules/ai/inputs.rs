//! ai module - Request input shapes for completions, drafts and policies.

use serde::Deserialize;
use uuid::Uuid;

use super::constants::DEFAULT_RATE_LIMIT_PER_MINUTE;
use super::types::AiProviderKind;
use super::types::SiteContextInput;

// Request bodies.

#[derive(Debug, Deserialize)]
pub struct CreateProviderConnectionInput {
    pub name: String,
    pub kind: AiProviderKind,
    #[serde(default)]
    pub base_url: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
    /// Write-only: accepted once, encrypted, never returned.
    #[serde(default)]
    pub api_key: Option<String>,
    #[serde(default)]
    pub enabled: Option<bool>,
    #[serde(default = "default_rate_limit")]
    pub rate_limit_per_minute: Option<u32>,
    /// Admin-approved opt-in: the base URL may point at a private
    /// or loopback host. Defaults to false (public hosts only).
    #[serde(default)]
    pub allow_private_host: Option<bool>,
    /// Admin-approved opt-in: the base URL may use plain http.
    /// Defaults to false (https only).
    #[serde(default)]
    pub allow_http: Option<bool>,
}

pub(crate) fn default_rate_limit() -> Option<u32> {
    Some(DEFAULT_RATE_LIMIT_PER_MINUTE)
}

#[derive(Debug, Deserialize)]
pub struct UpdateProviderConnectionInput {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub base_url: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
    /// `null` clears the key, a string replaces it, absent keeps it.
    #[serde(default)]
    pub api_key: Option<Option<String>>,
    #[serde(default)]
    pub enabled: Option<bool>,
    #[serde(default)]
    pub rate_limit_per_minute: Option<u32>,
    #[serde(default)]
    pub allow_private_host: Option<bool>,
    #[serde(default)]
    pub allow_http: Option<bool>,
}

#[derive(Debug, Deserialize)]
pub struct CompleteInput {
    pub prompt: String,
    #[serde(default)]
    pub provider_id: Option<Uuid>,
    #[serde(default)]
    pub system_prompt: Option<String>,
    #[serde(default)]
    pub max_tokens: Option<u32>,
    #[serde(default)]
    pub temperature: Option<f32>,
    #[serde(default)]
    pub use_case: Option<String>,
    #[serde(default)]
    pub site: Option<SiteContextInput>,
    #[serde(default)]
    pub grounding_query: Option<String>,
    /// When true, the call runs on the caller's personal API key
    /// instead of the provider connection's key.
    #[serde(default)]
    pub use_personal_key: bool,
}

#[derive(Debug, Deserialize)]
pub struct SupportDraftInput {
    pub prompt: String,
    #[serde(default)]
    pub ticket_id: Option<String>,
    #[serde(default)]
    pub conversation_id: Option<String>,
    /// Caller-supplied thread text, used when no hook answers the
    /// ticket / chat context requests.
    #[serde(default)]
    pub context_text: Option<String>,
    #[serde(default)]
    pub provider_id: Option<Uuid>,
    #[serde(default)]
    pub max_tokens: Option<u32>,
    #[serde(default)]
    pub temperature: Option<f32>,
    #[serde(default)]
    pub site: Option<SiteContextInput>,
    /// When true, the call runs on the caller's personal API key.
    #[serde(default)]
    pub use_personal_key: bool,
}

#[derive(Debug, Deserialize)]
pub struct ContentDraftInput {
    pub prompt: String,
    /// `post` or `page`.
    #[serde(default = "default_content_type")]
    pub content_type: String,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub provider_id: Option<Uuid>,
    #[serde(default)]
    pub max_tokens: Option<u32>,
    #[serde(default)]
    pub temperature: Option<f32>,
    #[serde(default)]
    pub site: Option<SiteContextInput>,
    /// When true, the call runs on the caller's personal API key.
    #[serde(default)]
    pub use_personal_key: bool,
}

pub(crate) fn default_content_type() -> String {
    "post".to_string()
}

#[derive(Debug, Deserialize)]
pub struct EmailDraftInput {
    pub prompt: String,
    #[serde(default)]
    pub audience: Option<String>,
    #[serde(default)]
    pub list_id: Option<String>,
    #[serde(default)]
    pub provider_id: Option<Uuid>,
    #[serde(default)]
    pub max_tokens: Option<u32>,
    #[serde(default)]
    pub temperature: Option<f32>,
    #[serde(default)]
    pub site: Option<SiteContextInput>,
    /// When true, the call runs on the caller's personal API key.
    #[serde(default)]
    pub use_personal_key: bool,
}

#[derive(Debug, Deserialize)]
pub struct AnswerInput {
    pub question: String,
    #[serde(default)]
    pub provider_id: Option<Uuid>,
    #[serde(default)]
    pub max_tokens: Option<u32>,
    #[serde(default)]
    pub temperature: Option<f32>,
    #[serde(default)]
    pub site: Option<SiteContextInput>,
    /// When true, the call runs on the caller's personal API key.
    #[serde(default)]
    pub use_personal_key: bool,
}

// Access allowlist request bodies.

#[derive(Debug, Deserialize)]
pub struct CreateCompanyPolicyInput {
    pub company_id: Uuid,
    #[serde(default)]
    pub allowed_models: Option<Vec<String>>,
    #[serde(default)]
    pub allowed_features: Option<Vec<String>>,
    #[serde(default)]
    pub monthly_token_cap: Option<u64>,
    #[serde(default)]
    pub monthly_cost_cap_usd: Option<f64>,
    #[serde(default)]
    pub enabled: Option<bool>,
    /// Layered-gate switch for the company (`enabled AND
    /// ai_enabled` is the effective company gate). Defaults to on.
    #[serde(default)]
    pub ai_enabled: Option<bool>,
    /// Canonical feature names switched off for the company.
    #[serde(default)]
    pub disabled_features: Option<Vec<String>>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateCompanyPolicyInput {
    #[serde(default)]
    pub allowed_models: Option<Option<Vec<String>>>,
    #[serde(default)]
    pub allowed_features: Option<Option<Vec<String>>>,
    #[serde(default)]
    pub monthly_token_cap: Option<Option<u64>>,
    #[serde(default)]
    pub monthly_cost_cap_usd: Option<Option<f64>>,
    #[serde(default)]
    pub enabled: Option<bool>,
    #[serde(default)]
    pub ai_enabled: Option<bool>,
    #[serde(default)]
    pub disabled_features: Option<Option<Vec<String>>>,
}

#[derive(Debug, Deserialize)]
pub struct CreateUserPolicyInput {
    pub user_id: Uuid,
    pub company_id: Uuid,
    #[serde(default)]
    pub allowed_models: Option<Vec<String>>,
    #[serde(default)]
    pub allowed_features: Option<Vec<String>>,
    #[serde(default)]
    pub monthly_token_cap: Option<u64>,
    #[serde(default)]
    pub monthly_cost_cap_usd: Option<f64>,
    #[serde(default)]
    pub enabled: Option<bool>,
    /// Layered-gate switch for the user (`enabled AND ai_enabled`
    /// is the effective user gate). Defaults to on.
    #[serde(default)]
    pub ai_enabled: Option<bool>,
    /// Canonical feature names switched off for the user.
    #[serde(default)]
    pub disabled_features: Option<Vec<String>>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateUserPolicyInput {
    #[serde(default)]
    pub allowed_models: Option<Option<Vec<String>>>,
    #[serde(default)]
    pub allowed_features: Option<Option<Vec<String>>>,
    #[serde(default)]
    pub monthly_token_cap: Option<Option<u64>>,
    #[serde(default)]
    pub monthly_cost_cap_usd: Option<Option<f64>>,
    #[serde(default)]
    pub enabled: Option<bool>,
    #[serde(default)]
    pub ai_enabled: Option<bool>,
    #[serde(default)]
    pub disabled_features: Option<Option<Vec<String>>>,
}

#[derive(Debug, Deserialize)]
pub struct UpsertPersonalKeyInput {
    pub provider_kind: AiProviderKind,
    #[serde(default)]
    pub base_url: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
    /// Write-only: accepted once, encrypted, never returned.
    pub api_key: String,
    #[serde(default)]
    pub monthly_token_cap: Option<u64>,
    #[serde(default)]
    pub monthly_cost_cap_usd: Option<f64>,
    #[serde(default)]
    pub enabled: Option<bool>,
}

#[derive(Debug, Deserialize)]
pub struct CreatePersonalMcpInput {
    pub name: String,
    pub endpoint_url: String,
    /// Write-only auth token for the MCP server, if any.
    #[serde(default)]
    pub auth_token: Option<String>,
    #[serde(default)]
    pub enabled: Option<bool>,
}

#[derive(Debug, Deserialize)]
pub struct UpdatePersonalMcpInput {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub endpoint_url: Option<String>,
    /// `null` clears the token, a string replaces it, absent keeps it.
    #[serde(default)]
    pub auth_token: Option<Option<String>>,
    #[serde(default)]
    pub enabled: Option<bool>,
}
