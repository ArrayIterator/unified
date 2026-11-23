//! ai module - Provider, draft, usage, policy, key and context types.

use unified_core::hooks::HookContext;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::time::Instant;
use uuid::Uuid;

// ---------------------------------------------------------------------------
// Provider kinds.
// ---------------------------------------------------------------------------

/// Every AI provider this module can talk to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum AiProviderKind {
    #[serde(rename = "anthropic")]
    Anthropic,
    #[serde(rename = "openai")]
    OpenAi,
    #[serde(rename = "openai_compatible")]
    OpenAiCompatible,
    #[serde(rename = "local")]
    Local,
}

impl AiProviderKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Anthropic => "anthropic",
            Self::OpenAi => "openai",
            Self::OpenAiCompatible => "openai_compatible",
            Self::Local => "local",
        }
    }

    pub fn from_str(value: &str) -> Option<Self> {
        match value {
            "anthropic" => Some(Self::Anthropic),
            "openai" => Some(Self::OpenAi),
            "openai_compatible" => Some(Self::OpenAiCompatible),
            "local" => Some(Self::Local),
            _ => None,
        }
    }

    pub fn all() -> &'static [AiProviderKind] {
        &[
            Self::Anthropic,
            Self::OpenAi,
            Self::OpenAiCompatible,
            Self::Local,
        ]
    }

    pub fn display_name(&self) -> &'static str {
        match self {
            Self::Anthropic => "Anthropic (Claude)",
            Self::OpenAi => "OpenAI (GPT)",
            Self::OpenAiCompatible => "OpenAI-compatible endpoint",
            Self::Local => "Local / bring-your-own endpoint",
        }
    }

    /// Fixed API base URL, when the provider has one. Compatible and
    /// local providers have none: the admin supplies the base URL.
    pub fn default_base_url(&self) -> Option<&'static str> {
        match self {
            Self::Anthropic => Some("https://api.anthropic.com"),
            Self::OpenAi => Some("https://api.openai.com/v1"),
            Self::OpenAiCompatible | Self::Local => None,
        }
    }

    /// Default model when the admin does not pick one. Compatible and
    /// local providers have none: the model is required there.
    pub fn default_model(&self) -> Option<&'static str> {
        match self {
            Self::Anthropic => Some("claude-sonnet-4-20250514"),
            Self::OpenAi => Some("gpt-4o-mini"),
            Self::OpenAiCompatible | Self::Local => None,
        }
    }

    /// Hosts this provider's API is allowed to talk to. A request to
    /// any other host is refused before it is sent.
    pub fn allowed_hosts(&self) -> &'static [&'static str] {
        match self {
            Self::Anthropic => &["api.anthropic.com"],
            Self::OpenAi => &["api.openai.com"],
            Self::OpenAiCompatible | Self::Local => &[],
        }
    }

    /// Whether the provider needs an API key. A local endpoint may
    /// run without one.
    pub fn key_required(&self) -> bool {
        !matches!(self, Self::Local)
    }
}

// ---------------------------------------------------------------------------
// Provider connections (stored; credentials write-only).
// ---------------------------------------------------------------------------

/// One stored provider connection. `api_key_encrypted` holds the
/// ciphertext from `encrypt_api_key`; the plaintext key is never
/// stored anywhere in this module.
#[derive(Debug, Clone)]
pub struct AiProviderConnection {
    pub id: Uuid,
    pub name: String,
    pub kind: AiProviderKind,
    /// Effective base URL (the kind default when the admin left it empty).
    pub base_url: String,
    pub model: String,
    pub api_key_encrypted: Option<String>,
    /// Fingerprint prefix of the stored key, or `None` when no key is set.
    pub key_fingerprint: Option<String>,
    pub enabled: bool,
    /// Calls allowed per minute for this key (rate limit bucket).
    pub rate_limit_per_minute: u32,
    /// Admin-approved opt-in: the base URL may point at a private
    /// or loopback host (for example a self-hosted model on the
    /// internal network). `false` by default: non-first-party
    /// endpoints must resolve to public addresses.
    pub allow_private_host: bool,
    /// Admin-approved opt-in: the base URL may use plain http
    /// instead of https. `false` by default: https is required.
    pub allow_http: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Masked read view of a connection: credentials are never returned.
#[derive(Debug, Clone, Serialize)]
pub struct AiProviderConnectionView {
    pub id: Uuid,
    pub name: String,
    pub kind: String,
    pub kind_display: String,
    pub base_url: String,
    pub model: String,
    pub key_configured: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub key_fingerprint: Option<String>,
    pub enabled: bool,
    pub rate_limit_per_minute: u32,
    pub allow_private_host: bool,
    pub allow_http: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl AiProviderConnection {
    pub(crate) fn view(&self) -> AiProviderConnectionView {
        AiProviderConnectionView {
            id: self.id,
            name: self.name.clone(),
            kind: self.kind.as_str().to_string(),
            kind_display: self.kind.display_name().to_string(),
            base_url: self.base_url.clone(),
            model: self.model.clone(),
            key_configured: self.api_key_encrypted.is_some(),
            key_fingerprint: self.key_fingerprint.clone(),
            enabled: self.enabled,
            rate_limit_per_minute: self.rate_limit_per_minute,
            allow_private_host: self.allow_private_host,
            allow_http: self.allow_http,
            created_at: self.created_at,
            updated_at: self.updated_at,
        }
    }
}

/// One catalog entry: a provider kind an admin may configure.
#[derive(Debug, Clone, Serialize)]
pub struct AiProviderCatalogEntry {
    pub kind: String,
    pub display_name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default_base_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default_model: Option<String>,
    pub key_required: bool,
}

// ---------------------------------------------------------------------------
// Drafts.
// ---------------------------------------------------------------------------

/// What a draft is for. The kind decides which owning module may
/// deliver it after a human approves it; this module delivers none.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DraftKind {
    SupportReply,
    ChatReply,
    Content,
    Email,
    KbAnswer,
    Generic,
}

impl DraftKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::SupportReply => "support_reply",
            Self::ChatReply => "chat_reply",
            Self::Content => "content",
            Self::Email => "email",
            Self::KbAnswer => "kb_answer",
            Self::Generic => "generic",
        }
    }

    pub fn from_str(value: &str) -> Option<Self> {
        match value {
            "support_reply" => Some(Self::SupportReply),
            "chat_reply" => Some(Self::ChatReply),
            "content" => Some(Self::Content),
            "email" => Some(Self::Email),
            "kb_answer" => Some(Self::KbAnswer),
            "generic" => Some(Self::Generic),
            _ => None,
        }
    }
}

/// Review state of a draft. `Draft` is the only state a generation
/// ever produces; `Approved` / `Rejected` are set only by the human
/// review endpoints.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DraftStatus {
    Draft,
    Approved,
    Rejected,
}

impl DraftStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Draft => "draft",
            Self::Approved => "approved",
            Self::Rejected => "rejected",
        }
    }
}

/// One knowledgebase article used as grounding for a draft or answer.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GroundingSource {
    pub article_id: String,
    pub title: String,
    pub url: String,
    pub score: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub excerpt: Option<String>,
}

/// An AI-generated draft. `ai_generated` is always true and is part
/// of the serialized payload, so no consumer can mistake the text
/// for human-written content. There is no publish/send field and no
/// publish/send transition: approval only records the human review.
#[derive(Debug, Clone, Serialize)]
pub struct AiDraft {
    pub id: Uuid,
    pub kind: DraftKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub subject: Option<String>,
    pub body: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prompt: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provider_id: Option<Uuid>,
    pub provider_kind: String,
    pub model: String,
    pub actor: String,
    pub status: DraftStatus,
    /// Always true: this text was generated by an AI provider.
    pub ai_generated: bool,
    /// Owning object type: `ticket`, `chat_conversation`, `post`,
    /// `page`, `newsletter`, or `None` for a free-form draft.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target_id: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sources: Vec<GroundingSource>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub usage_id: Option<Uuid>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reviewed_by: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reviewed_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

// ---------------------------------------------------------------------------
// Usage tracking.
// ---------------------------------------------------------------------------

/// One provider call, recorded whether it succeeded or failed, so
/// cost and rate-limit questions have an audit trail.
#[derive(Debug, Clone, Serialize)]
pub struct AiUsageRecord {
    pub id: Uuid,
    pub provider_id: Uuid,
    pub provider_kind: String,
    pub actor: String,
    /// `complete`, `support_reply`, `chat_reply`, `content`, `email`,
    /// `kb_answer`, or `test`.
    pub use_case: String,
    pub model: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prompt_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub completion_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub total_tokens: Option<u32>,
    /// Estimated cost in USD from the provider's reported token
    /// counts and the price table in `estimate_cost_usd`; `None`
    /// when the model has no known price.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub estimated_cost_usd: Option<f64>,
    pub latency_ms: u64,
    /// `success` or `error`.
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_code: Option<u16>,
    /// Owning user, parsed from the `X-User-Id` header. `None` for
    /// records written before allowlists existed, and for calls whose
    /// actor id is not a UUID.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user_id: Option<Uuid>,
    /// Owning company, resolved from the verified caller's
    /// server-resolved company or the `user.company_requested`
    /// hook. `None` for legacy records and for callers with no
    /// company.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub company_id: Option<Uuid>,
    /// Non-leaking reason code when the allowlist gate denied the
    /// call (`ai_not_allowed_for_company`, ...). `None` for calls
    /// that reached a provider.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub denial_reason: Option<String>,
    /// True when the call ran on the caller's personal API key
    /// instead of the provider connection's key.
    pub used_personal_key: bool,
    pub created_at: DateTime<Utc>,
}

/// Aggregated usage over the current filter set.
#[derive(Debug, Clone, Serialize)]
pub struct AiUsageSummary {
    pub calls: u64,
    pub successful_calls: u64,
    pub failed_calls: u64,
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub total_tokens: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub estimated_cost_usd: Option<f64>,
}

// ---------------------------------------------------------------------------
// Access allowlists (company + user policies), personal keys, personal
// MCP servers.
//
// The in-memory DashMap stores are authoritative; the 000036
// migration persists the same rows. A policy list of `None` means
// "unrestricted" for that dimension; `Some(vec![])` means "allow
// nothing". A cap of `None` means "no cap"; a user cap of `None` or
// `0` means "inherit the company cap" (see `effective_user_cap`).
// ---------------------------------------------------------------------------

/// One company's AI access policy. Keyed by `company_id` in the
/// state store (one policy per company).
#[derive(Debug, Clone, Serialize)]
pub struct AiCompanyPolicy {
    pub id: Uuid,
    pub company_id: Uuid,
    /// `None` = any model; `Some(list)` = only these models.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub allowed_models: Option<Vec<String>>,
    /// Use-case names from `KNOWN_USE_CASES`. `None` = any feature.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub allowed_features: Option<Vec<String>>,
    /// Monthly token budget for the whole company. `None` = no cap.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub monthly_token_cap: Option<u64>,
    /// Monthly cost budget (USD) for the whole company. `None` = no
    /// cap. Stored as NUMERIC(19,4) in the database.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub monthly_cost_cap_usd: Option<f64>,
    pub enabled: bool,
    /// Layered-gate switch for the company, separate from the
    /// allowlist master switch `enabled`: the effective company
    /// gate is `enabled AND ai_enabled`. A company admin turns AI
    /// off for the whole company here without touching the
    /// allowlist lists and caps.
    pub ai_enabled: bool,
    /// Canonical feature (use-case) names switched off for this
    /// company by the layered gate. `None` = no feature is switched
    /// off here (features follow the allowlist `allowed_features`
    /// instead). Checked after the allowlist feature list.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub disabled_features: Option<Vec<String>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// One user's AI access policy, set by an admin of the user's
/// company (or by a platform admin). Keyed by `user_id` in the
/// state store (one policy per user).
#[derive(Debug, Clone, Serialize)]
pub struct AiUserPolicy {
    pub id: Uuid,
    pub user_id: Uuid,
    pub company_id: Uuid,
    /// `None` = any model; `Some(list)` = only these models.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub allowed_models: Option<Vec<String>>,
    /// Use-case names from `KNOWN_USE_CASES`. `None` = any feature.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub allowed_features: Option<Vec<String>>,
    /// Monthly token budget for this user. `None` or `0` = inherit
    /// the company cap.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub monthly_token_cap: Option<u64>,
    /// Monthly cost budget (USD) for this user. `None` or `0` =
    /// inherit the company cap.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub monthly_cost_cap_usd: Option<f64>,
    pub enabled: bool,
    /// Layered-gate switch for the user, separate from the
    /// allowlist master switch `enabled`: the effective user gate
    /// is `enabled AND ai_enabled`. A disabled user is denied for
    /// every feature; a feature switched off below denies only
    /// that feature.
    pub ai_enabled: bool,
    /// Canonical feature (use-case) names switched off for this
    /// user by the layered gate. `None` = no feature is switched
    /// off here. Checked after the allowlist feature list.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub disabled_features: Option<Vec<String>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Global AI settings: the platform kill-switch plus the global
/// per-feature switches (layer (d) of the layered gate). One
/// process-wide value, held in `AiState.global_settings` and read
/// fresh on every gate evaluation; persisted in the
/// `ai_global_settings` table (migration 000063), keyed by setting
/// name (`ai.global_enabled`, `ai.features.<name>.enabled`).
#[derive(Debug, Clone, Serialize)]
pub struct AiGlobalSettings {
    /// Master kill-switch. `false` denies every AI call
    /// immediately, regardless of the other layers.
    pub global_enabled: bool,
    /// Explicit opt-in to legacy open mode: when `true` AND no
    /// company or user policy exists anywhere, calls from any
    /// caller holding `ai:use` are allowed without a policy. The
    /// default is `false` (default-deny): with no policies at all,
    /// calls are denied until an admin creates a policy or
    /// explicitly turns this on. Once any policy exists,
    /// enforcement is strict regardless of this flag.
    pub open_mode: bool,
    /// Canonical feature (use-case) names switched off globally.
    /// Empty means no feature is switched off globally.
    pub disabled_features: Vec<String>,
    pub updated_at: DateTime<Utc>,
}

impl Default for AiGlobalSettings {
    fn default() -> Self {
        Self {
            global_enabled: true,
            open_mode: false,
            disabled_features: Vec::new(),
            updated_at: Utc::now(),
        }
    }
}

/// The outcome of one layered-gate evaluation, in evaluation order:
/// settings kill-switch -> product -> company allowlist + gate ->
/// user allowlist + gate -> feature toggles. Every layer must allow;
/// the first denying layer decides. The decision is computed fresh
/// on every call and never cached across calls.
#[derive(Debug, Clone, Serialize)]
pub struct AiGateDecision {
    pub allowed: bool,
    /// Which layer denied the call: `settings`, `product`,
    /// `company`, `user`, or `feature`. `None` when allowed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub denied_layer: Option<String>,
    /// The requested feature, when the call was denied. `None`
    /// when allowed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub denied_feature: Option<String>,
    /// Non-leaking reason code for the denial (the same code the
    /// completion path returns in the `reason` error metadata).
    /// `None` when allowed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason_code: Option<String>,
    /// Per-layer outcome, in evaluation order. Admin-facing only:
    /// the effective-gate endpoint is admin-only, and the
    /// completion deny path returns only the reason code.
    pub layers: Vec<AiGateLayerOutcome>,
}

/// One layer's outcome inside an `AiGateDecision`.
#[derive(Debug, Clone, Serialize)]
pub struct AiGateLayerOutcome {
    pub layer: String,
    pub allowed: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

/// One user's own API key (bring-your-own-key). The plaintext key is
/// never stored: only the ciphertext from `encrypt_api_key` and a
/// fingerprint prefix. The key is scoped to its owning user and is
/// usable only for that user's own AI calls.
#[derive(Debug, Clone)]
pub struct AiPersonalKey {
    pub id: Uuid,
    pub user_id: Uuid,
    pub provider_kind: AiProviderKind,
    /// Base URL for a compatible / local endpoint. `None` means the
    /// kind default (Anthropic / OpenAI). Used only by the key test;
    /// live calls use the provider connection's base URL.
    pub base_url: Option<String>,
    /// Model this key is for. `None` = any model of this kind.
    pub model: Option<String>,
    pub api_key_encrypted: String,
    /// Fingerprint prefix of the stored key, for the metadata view.
    pub key_fingerprint: String,
    /// Monthly token budget for calls on this key. `None` or `0` =
    /// inherit the caller's effective user cap.
    pub monthly_token_cap: Option<u64>,
    /// Monthly cost budget (USD) for calls on this key. `None` or
    /// `0` = inherit the caller's effective user cap.
    pub monthly_cost_cap_usd: Option<f64>,
    pub enabled: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Metadata-only read view of a personal key: the key itself is
/// never returned.
#[derive(Debug, Clone, Serialize)]
pub struct AiPersonalKeyView {
    pub id: Uuid,
    pub user_id: Uuid,
    pub provider_kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    pub key_configured: bool,
    pub key_fingerprint: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub monthly_token_cap: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub monthly_cost_cap_usd: Option<f64>,
    pub enabled: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl AiPersonalKey {
    pub(crate) fn view(&self) -> AiPersonalKeyView {
        AiPersonalKeyView {
            id: self.id,
            user_id: self.user_id,
            provider_kind: self.provider_kind.as_str().to_string(),
            base_url: self.base_url.clone(),
            model: self.model.clone(),
            key_configured: true,
            key_fingerprint: self.key_fingerprint.clone(),
            monthly_token_cap: self.monthly_token_cap,
            monthly_cost_cap_usd: self.monthly_cost_cap_usd,
            enabled: self.enabled,
            created_at: self.created_at,
            updated_at: self.updated_at,
        }
    }
}

/// One user's own MCP server. The auth token is write-only and
/// encrypted like an API key. The server is never shared with other
/// users: every lookup filters on `user_id`, and the server is
/// offered only as a grounding source for its owner's AI calls.
#[derive(Debug, Clone)]
pub struct AiUserMcpServer {
    pub id: Uuid,
    pub user_id: Uuid,
    pub name: String,
    /// Validated at write time: https only, public host only (see
    /// `validate_mcp_endpoint_url`).
    pub endpoint_url: String,
    pub auth_token_encrypted: Option<String>,
    /// Fingerprint prefix of the stored token, or `None`.
    pub token_fingerprint: Option<String>,
    pub enabled: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Metadata-only read view of a personal MCP server: the auth token
/// is never returned.
#[derive(Debug, Clone, Serialize)]
pub struct AiUserMcpServerView {
    pub id: Uuid,
    pub user_id: Uuid,
    pub name: String,
    pub endpoint_url: String,
    pub token_configured: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub token_fingerprint: Option<String>,
    pub enabled: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl AiUserMcpServer {
    pub(crate) fn view(&self) -> AiUserMcpServerView {
        AiUserMcpServerView {
            id: self.id,
            user_id: self.user_id,
            name: self.name.clone(),
            endpoint_url: self.endpoint_url.clone(),
            token_configured: self.auth_token_encrypted.is_some(),
            token_fingerprint: self.token_fingerprint.clone(),
            enabled: self.enabled,
            created_at: self.created_at,
            updated_at: self.updated_at,
        }
    }
}

/// Per-key rate-limit bucket: calls in the current minute window.
#[derive(Debug, Clone)]
pub(crate) struct RateBucket {
    pub(crate) window_start: Instant,
    pub(crate) count: u32,
}

/// One pre-call cap reservation: the estimated tokens and cost of
/// a call that passed the cap check but has not recorded actual
/// usage yet. Cap checks count live reservations alongside
/// recorded monthly usage, so concurrent calls cannot each pass
/// the check before any of them records usage (check-then-act).
/// The reservation is removed once the call's actual usage is
/// recorded; a reservation older than `CAP_RESERVATION_TTL_SECS`
/// is stale and ignored by cap checks.
#[derive(Debug, Clone)]
pub(crate) struct CapReservation {
    pub(crate) id: Uuid,
    pub(crate) user_id: Option<Uuid>,
    pub(crate) company_id: Option<Uuid>,
    pub(crate) tokens: u64,
    pub(crate) cost_usd: f64,
    pub(crate) created_at: DateTime<Utc>,
}

// ---------------------------------------------------------------------------
// Shared context.
//
// THE merge point with the public server module (`mcp.rs`, built in
// parallel): `assemble_shared_context` is the single function that
// builds the site context both the AI features and the public server
// consume. It grounds on the knowledgebase module's real search path
// (`knowledgebase.search`, published articles only) and merges site
// facts contributed through `site.context_requested`. This module
// answers `site.context_requested` itself (registered in
// `register_hooks`), so a caller that emits that event gets the same
// context. The public server module should either call
// `assemble_shared_context` directly (same process, same source) or
// emit `site.context_requested` and use the results - never build a
// second context assembler. `mcp.rs` did not exist yet when this file
// was written; the merge is the function name and the event name
// above, both public.
// ---------------------------------------------------------------------------

/// Caller-supplied site facts. Explicit caller values win over
/// hook-contributed ones when both are present.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct SiteContextInput {
    #[serde(default)]
    pub site_name: Option<String>,
    #[serde(default)]
    pub site_url: Option<String>,
    #[serde(default)]
    pub locale: Option<String>,
}

impl SiteContextInput {
    pub(crate) fn from_hook(ctx: &HookContext) -> Self {
        Self {
            site_name: ctx
                .get("site_name")
                .and_then(|value| value.as_str())
                .map(str::to_string),
            site_url: ctx
                .get("site_url")
                .and_then(|value| value.as_str())
                .map(str::to_string),
            locale: ctx
                .get("locale")
                .and_then(|value| value.as_str())
                .map(str::to_string),
        }
    }
}

/// The shared site context: site facts plus grounding articles.
#[derive(Debug, Clone, Serialize)]
pub struct SharedContext {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub site_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub site_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub locale: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub grounding_articles: Vec<GroundingSource>,
    /// Extra site facts contributed through `site.context_requested`
    /// (pages, products, summaries, ...), as received.
    #[serde(default, skip_serializing_if = "serde_json::Map::is_empty")]
    pub extra: serde_json::Map<String, Value>,
    pub assembled_at: DateTime<Utc>,
}
