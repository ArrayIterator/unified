// AI module.
//
// Provider abstraction over Anthropic, OpenAI, any OpenAI-compatible
// endpoint, and a local / bring-your-own endpoint, with drafts that a
// human must approve before anything is sent or published.
//
// Routes live under `/api/v1/ai`:
//   - `GET  /providers` - provider catalog (kinds, defaults, no secrets).
//   - `GET/POST  /provider-connections` - masked list / create.
//   - `GET/PATCH/DELETE /provider-connections/{id}` - masked read / update / delete.
//   - `POST /provider-connections/{id}/test` - live connection test.
//   - `POST /complete` - one completion, stored as a draft.
//   - `POST /support-draft`, `/chat-draft` - reply drafts for tickets / chat.
//   - `POST /content-draft` - draft post / page text for the CMS and site builder.
//   - `POST /email-draft` - draft subject + body for email / newsletter.
//   - `POST /answer` - knowledgebase-grounded answer, stored as a draft.
//   - `GET  /drafts`, `GET /drafts/{id}` - list / read drafts.
//   - `POST /drafts/{id}/approve`, `/drafts/{id}/reject` - human review.
//   - `DELETE /drafts/{id}` - delete a draft.
//   - `GET  /usage`, `GET /usage/{id}`, `GET /usage/summary` - call usage.
//   - `GET  /context` - the shared site context (see below).
//
// Human approval is enforced in code, not by convention:
//   - Every generation stores an `AiDraft` with status `draft` and
//     `ai_generated: true`. Drafts are the only thing this module
//     produces.
//   - There is NO publish or send path in this module: no endpoint
//     calls a ticket reply, chat send, content publish, page publish,
//     or email/newsletter send endpoint, and no hook emitted here
//     asks another module to send or publish AI text. Approving a
//     draft only flips its status to `approved` and emits
//     `ai.draft.approved`; the owning module (tickets, chat, content,
//     site builder, newsletter) must pick the approved text up through
//     its own existing human endpoint to deliver it.
//   - Draft payloads always carry `ai_generated: true` and
//     `status: "draft"` until a human reviews them.
//
// Interconnection happens only through the hook bus:
//   - Capability checks ask the roles module through the `role.check`
//     hook (`ai:use`, `ai:approve`, `ai:manage_providers`,
//     `ai:read_usage`); a missing answer denies the action.
//   - Support drafts gather ticket / chat context through
//     `ticket.context_requested` and `chat.context_requested` and
//     merge whatever those modules return; the caller may also pass
//     `context_text` directly.
//   - Knowledgebase answers use the knowledgebase module's real
//     search path: the `knowledgebase.search` hook (the same hook
//     the support assistant uses; only published articles are
//     returned).
//   - Every call, draft, approval, and provider change emits an
//     event (`ai.call.completed`, `ai.call.failed`,
//     `ai.draft.created`, `ai.draft.approved`, `ai.draft.rejected`,
//     `ai.provider.created`, `ai.provider.deleted`) with the audit
//     log's payload shape (`actor_id`, `resource`, `id`), so the
//     audit module records them once the integrator adds those names
//     to `AUDITED_EVENTS` in `audit.rs`.
//   - The shared context is assembled by `assemble_shared_context`
//     (see the Shared Context section): it grounds on
//     `knowledgebase.search` and collects site facts from
//     `site.context_requested`. This module answers
//     `site.context_requested` itself, so the context source is one
//     function, shared with the public server module that is built
//     in parallel.
//
// Credentials are write-only: an API key is accepted once, encrypted
// with a key derived from `app.secret_key`, and stored only as
// ciphertext. No response, log line, hook payload, or usage record
// ever contains a key or a fragment of one; read views expose only
// `key_configured` and a short fingerprint prefix. The encryption is
// a versioned (`v1.`) HMAC keystream with an HMAC tag - not an AEAD
// cipher - because no AEAD crate is in `Cargo.toml` yet; when one is
// added, replace `encrypt_api_key` / `decrypt_api_key` and bump the
// version prefix so old values can be re-encrypted on next write.

use crate::core::context::AppContext;
use crate::core::error::{UnifiedError, UnifiedResult};
use crate::core::hooks::{HookBus, HookContext};
use crate::core::json;
use crate::core::modules::{Module, ModuleRegistry, RouteInfo};
use crate::core::pagination::{PageResult, PaginationParams};
use actix_web::{web, HttpRequest, HttpResponse};
use async_trait::async_trait;
use base64::Engine as _;
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use hmac::{Hmac, Mac};
use reqwest::header::{HeaderValue, AUTHORIZATION, CONTENT_TYPE, RETRY_AFTER};
use reqwest::StatusCode;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};
use url::Url;
use uuid::Uuid;

// ---------------------------------------------------------------------------
// Events.
// ---------------------------------------------------------------------------

/// Emitted after a successful provider call (audit payload shape).
pub const EVENT_AI_CALL_COMPLETED: &str = "ai.call.completed";
/// Emitted after a failed provider call (audit payload shape).
pub const EVENT_AI_CALL_FAILED: &str = "ai.call.failed";
/// Emitted when a draft is stored.
pub const EVENT_AI_DRAFT_CREATED: &str = "ai.draft.created";
/// Emitted when a human approves a draft. Delivery is NOT part of
/// this event: the owning module must send/publish through its own
/// human endpoint.
pub const EVENT_AI_DRAFT_APPROVED: &str = "ai.draft.approved";
/// Emitted when a human rejects a draft.
pub const EVENT_AI_DRAFT_REJECTED: &str = "ai.draft.rejected";
/// Emitted when a provider connection is created.
pub const EVENT_AI_PROVIDER_CREATED: &str = "ai.provider.created";
/// Emitted when a provider connection is deleted.
pub const EVENT_AI_PROVIDER_DELETED: &str = "ai.provider.deleted";
/// Shared-context request: answered by this module (and, once built,
/// by the public server module) with site facts. The requester of
/// this event gets the same context the AI features use.
pub const EVENT_SITE_CONTEXT_REQUESTED: &str = "site.context_requested";
/// Alias of the shared-context request under the AI namespace.
pub const EVENT_AI_CONTEXT_REQUESTED: &str = "ai.context_requested";
/// Knowledgebase search, answered by the knowledgebase module.
pub const EVENT_KNOWLEDGEBASE_SEARCH: &str = "knowledgebase.search";
/// Ticket context request (answered by the tickets module when it
/// implements it; absence only means no transcript is merged).
pub const EVENT_TICKET_CONTEXT_REQUESTED: &str = "ticket.context_requested";
/// Chat context request (answered by the chat module when it
/// implements it; absence only means no transcript is merged).
pub const EVENT_CHAT_CONTEXT_REQUESTED: &str = "chat.context_requested";

// ---------------------------------------------------------------------------
// Permissions (checked through the `role.check` hook; fail closed).
// ---------------------------------------------------------------------------

/// May generate completions and drafts.
pub const PERM_AI_USE: &str = "ai:use";
/// May create, update, delete, and test provider connections.
pub const PERM_AI_MANAGE_PROVIDERS: &str = "ai:manage_providers";
/// May approve or reject drafts.
pub const PERM_AI_APPROVE: &str = "ai:approve";
/// May read usage records and the usage summary.
pub const PERM_AI_READ_USAGE: &str = "ai:read_usage";

// ---------------------------------------------------------------------------
// Limits.
// ---------------------------------------------------------------------------

/// Longest provider HTTP timeout.
const PROVIDER_TIMEOUT_SECS: u64 = 30;
/// Longest provider connect timeout.
const PROVIDER_CONNECT_TIMEOUT_SECS: u64 = 10;
/// Longest `Retry-After` delay honoured from a provider.
const MAX_RETRY_AFTER: Duration = Duration::from_secs(120);
/// Default per-provider rate limit (calls per minute).
const DEFAULT_RATE_LIMIT_PER_MINUTE: u32 = 60;
/// Hardest per-provider rate limit an admin may configure.
const MAX_RATE_LIMIT_PER_MINUTE: u32 = 10_000;
/// Maximum prompt / instruction length in characters.
const MAX_PROMPT_LEN: usize = 20_000;
/// Maximum caller-supplied context text in characters.
const MAX_CONTEXT_TEXT_LEN: usize = 20_000;
/// Maximum name length for a provider connection.
const MAX_NAME_LEN: usize = 120;
/// Default completion token budget.
const DEFAULT_MAX_TOKENS: u32 = 1024;
/// Largest completion token budget a caller may request.
const MAX_MAX_TOKENS: u32 = 8192;
/// How many knowledgebase articles ground one answer.
const GROUNDING_LIMIT: u64 = 5;
/// User-Agent sent to provider APIs.
const USER_AGENT: &str = concat!("unified/", env!("CARGO_PKG_VERSION"), " ai");

/// Development fallback secret, used only when `app.secret_key` is
/// empty (same pattern as the app-sync token signer). Production
/// config validation requires a real secret of at least 32 chars.
const DEV_ENCRYPTION_SECRET: &str = "unified-ai-development-secret";

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
// Credential encryption (write-only storage).
// ---------------------------------------------------------------------------

type HmacSha256 = Hmac<Sha256>;

fn hmac_sha256(key: &[u8], message: &[u8]) -> Vec<u8> {
    let mut mac = HmacSha256::new_from_slice(key).expect("HMAC key length is always valid");
    mac.update(message);
    mac.finalize().into_bytes().to_vec()
}

fn sha256_hex_of(data: &[u8]) -> String {
    Sha256::digest(data).iter().map(|byte| format!("{byte:02x}")).collect()
}

/// The encryption secret: `app.secret_key`, or the development
/// fallback when none is configured.
fn encryption_secret(ctx: &AppContext) -> String {
    let config = ctx.config.read().expect("config lock poisoned");
    if config.app.secret_key.trim().is_empty() {
        DEV_ENCRYPTION_SECRET.to_string()
    } else {
        config.app.secret_key.clone()
    }
}

/// Keystream for the XOR stream cipher: HMAC-SHA256 blocks keyed by
/// the SHA-256 of the secret, over `nonce || counter`.
fn keystream(key: &[u8], nonce: &[u8], len: usize) -> Vec<u8> {
    let mut out: Vec<u8> = Vec::with_capacity(len);
    let mut counter: u32 = 0;
    while out.len() < len {
        let mut message = nonce.to_vec();
        message.extend_from_slice(&counter.to_be_bytes());
        out.extend_from_slice(&hmac_sha256(key, &message));
        counter += 1;
    }
    out.truncate(len);
    out
}

/// Constant-time equality for authentication tags.
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

/// Encrypt an API key. Output format: `v1.<base64url(nonce ||
/// ciphertext || tag)>`. The tag covers nonce + ciphertext.
fn encrypt_api_key(secret: &str, plaintext: &str) -> String {
    let key = Sha256::digest(secret.as_bytes());
    let nonce: [u8; 16] = rand::random();
    let stream = keystream(&key, &nonce, plaintext.len());
    let ciphertext: Vec<u8> = plaintext
        .as_bytes()
        .iter()
        .zip(stream.iter())
        .map(|(plain, mask)| plain ^ mask)
        .collect();
    let mut tag_message = nonce.to_vec();
    tag_message.extend_from_slice(&ciphertext);
    let tag = hmac_sha256(&key, &tag_message);
    let mut blob = nonce.to_vec();
    blob.extend_from_slice(&ciphertext);
    blob.extend_from_slice(&tag);
    format!(
        "v1.{}",
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(blob)
    )
}

/// Decrypt a value produced by `encrypt_api_key`. The error message
/// never includes any part of the stored value.
fn decrypt_api_key(secret: &str, stored: &str) -> UnifiedResult<String> {
    let encoded = stored
        .strip_prefix("v1.")
        .ok_or_else(|| UnifiedError::internal("Stored provider key has an unknown format"))?;
    let blob = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(encoded)
        .map_err(|_| UnifiedError::internal("Stored provider key cannot be decoded"))?;
    if blob.len() < 16 + 32 {
        return Err(UnifiedError::internal(
            "Stored provider key cannot be decrypted",
        ));
    }
    let (nonce, rest) = blob.split_at(16);
    let (ciphertext, tag) = rest.split_at(rest.len() - 32);
    let key = Sha256::digest(secret.as_bytes());
    let mut tag_message = nonce.to_vec();
    tag_message.extend_from_slice(ciphertext);
    let expected = hmac_sha256(&key, &tag_message);
    if !constant_time_eq(&expected, tag) {
        return Err(UnifiedError::internal(
            "Stored provider key failed authentication",
        ));
    }
    let stream = keystream(&key, nonce, ciphertext.len());
    let plaintext: Vec<u8> = ciphertext
        .iter()
        .zip(stream.iter())
        .map(|(cipher, mask)| cipher ^ mask)
        .collect();
    String::from_utf8(plaintext)
        .map_err(|_| UnifiedError::internal("Stored provider key cannot be decrypted"))
}

/// Short fingerprint prefix of a key, for the masked read view. Only
/// the first 12 hex characters of the SHA-256 are exposed, never the
/// key itself.
fn key_fingerprint(plaintext: &str) -> String {
    sha256_hex_of(plaintext.as_bytes()).chars().take(12).collect()
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
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl AiProviderConnection {
    fn view(&self) -> AiProviderConnectionView {
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

/// Per-key rate-limit bucket: calls in the current minute window.
#[derive(Debug, Clone)]
struct RateBucket {
    window_start: Instant,
    count: u32,
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
    fn from_hook(ctx: &HookContext) -> Self {
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

/// Parse `knowledgebase.search` hook results into grounding sources.
/// The knowledgebase handler contributes `{ "articles": [...] }`; any
/// other shape is ignored rather than trusted. (Same parsing contract
/// as the support assistant's `parse_sources`.)
pub fn parse_grounding_sources(ctx: &HookContext) -> Vec<GroundingSource> {
    let mut sources: Vec<GroundingSource> = Vec::new();
    for result in &ctx.results {
        let articles = match result.get("articles").and_then(|value| value.as_array()) {
            Some(articles) => articles.clone(),
            None => match result.as_array() {
                Some(articles) => articles.clone(),
                None => continue,
            },
        };
        for article in articles {
            let article_id = article
                .get("id")
                .and_then(|value| value.as_str())
                .unwrap_or("")
                .to_string();
            let title = article
                .get("title")
                .and_then(|value| value.as_str())
                .unwrap_or("")
                .to_string();
            if article_id.is_empty() || title.is_empty() {
                continue;
            }
            sources.push(GroundingSource {
                article_id,
                title,
                url: article
                    .get("url")
                    .and_then(|value| value.as_str())
                    .unwrap_or("")
                    .to_string(),
                score: article
                    .get("score")
                    .and_then(|value| value.as_f64())
                    .unwrap_or(0.0),
                excerpt: article
                    .get("excerpt")
                    .and_then(|value| value.as_str())
                    .map(str::to_string),
            });
        }
    }
    sources
}

/// Build the shared site context. `include_site_hooks` controls
/// whether `site.context_requested` is emitted to collect facts from
/// other modules: draft flows pass `true`; the
/// `site.context_requested` hook handler itself passes `false` so it
/// never re-emits the event it is answering.
pub fn assemble_shared_context(
    hooks: Option<&HookBus>,
    query: &str,
    site: &SiteContextInput,
    include_site_hooks: bool,
) -> SharedContext {
    let mut grounding_articles: Vec<GroundingSource> = Vec::new();
    let mut extra: serde_json::Map<String, Value> = serde_json::Map::new();
    let mut site_name = site.site_name.clone();
    let mut site_url = site.site_url.clone();
    let mut locale = site.locale.clone();
    if let Some(bus) = hooks {
        if !query.trim().is_empty() {
            let ctx = bus.emit(
                EVENT_KNOWLEDGEBASE_SEARCH,
                json!({ "query": query, "limit": GROUNDING_LIMIT }),
            );
            grounding_articles = parse_grounding_sources(&ctx);
        }
        if include_site_hooks {
            let ctx = bus.emit(
                EVENT_SITE_CONTEXT_REQUESTED,
                json!({ "query": query }),
            );
            for result in &ctx.results {
                if let Some(object) = result.as_object() {
                    for (key, value) in object {
                        match key.as_str() {
                            "site_name" if site_name.is_none() => {
                                site_name = value.as_str().map(str::to_string);
                            }
                            "site_url" if site_url.is_none() => {
                                site_url = value.as_str().map(str::to_string);
                            }
                            "locale" if locale.is_none() => {
                                locale = value.as_str().map(str::to_string);
                            }
                            "query" => {}
                            _ => {
                                extra.insert(key.clone(), value.clone());
                            }
                        }
                    }
                }
            }
        }
    }
    SharedContext {
        site_name,
        site_url,
        locale,
        grounding_articles,
        extra,
        assembled_at: Utc::now(),
    }
}

/// Render the shared context as prompt text for a provider call.
fn shared_context_prompt(context: &SharedContext) -> String {
    let mut out = String::new();
    if let Some(name) = &context.site_name {
        out.push_str(&format!("Site name: {name}\n"));
    }
    if let Some(url) = &context.site_url {
        out.push_str(&format!("Site URL: {url}\n"));
    }
    if let Some(locale) = &context.locale {
        out.push_str(&format!("Locale: {locale}\n"));
    }
    if !context.grounding_articles.is_empty() {
        out.push_str("Knowledgebase grounding (published articles only):\n");
        for source in &context.grounding_articles {
            out.push_str(&format!("- {} ({})\n", source.title, source.url));
            if let Some(excerpt) = &source.excerpt {
                out.push_str(&format!("  {excerpt}\n"));
            }
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Cost estimates.
// ---------------------------------------------------------------------------

/// Estimated USD cost from reported token counts, using list prices
/// per million tokens for known models. Returns `None` for unknown
/// models so no caller ever sees an invented price.
fn estimate_cost_usd(model: &str, prompt_tokens: u32, completion_tokens: u32) -> Option<f64> {
    // (prompt price, completion price) per million tokens.
    let (prompt_price, completion_price) = if model.starts_with("claude-sonnet-4") {
        (3.0, 15.0)
    } else if model.starts_with("claude-3-5-sonnet") {
        (3.0, 15.0)
    } else if model.starts_with("claude-3-haiku") {
        (0.25, 1.25)
    } else if model.starts_with("gpt-4o-mini") {
        (0.15, 0.60)
    } else if model.starts_with("gpt-4o") {
        (2.50, 10.0)
    } else if model.starts_with("gpt-3.5-turbo") {
        (0.50, 1.50)
    } else {
        return None;
    };
    let cost = (prompt_tokens as f64 * prompt_price
        + completion_tokens as f64 * completion_price)
        / 1_000_000.0;
    Some((cost * 1_000_000.0).round() / 1_000_000.0)
}

// ---------------------------------------------------------------------------
// Provider registry: the plug-in point for adapters.
// ---------------------------------------------------------------------------

/// One completion call to a provider adapter.
#[derive(Debug, Clone)]
pub struct CompletionCall {
    pub prompt: String,
    pub system_prompt: Option<String>,
    pub max_tokens: u32,
    pub temperature: Option<f32>,
}

/// Raw provider answer, before it is stored as a draft.
#[derive(Debug, Clone)]
pub struct RawCompletion {
    pub text: String,
    pub prompt_tokens: Option<u32>,
    pub completion_tokens: Option<u32>,
    pub model: String,
}

/// Result of a live connection test.
#[derive(Debug, Clone, Serialize)]
pub struct ProviderTestResult {
    pub provider_id: Uuid,
    pub kind: String,
    pub reachable: bool,
    pub latency_ms: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model_count: Option<usize>,
    pub detail: String,
}

/// The interface every AI provider adapter implements. A new provider
/// plugs in by implementing this trait and registering a factory (see
/// `AiProviderRegistry::register_factory`); no other code changes.
#[async_trait]
pub trait AiProviderAdapter: Send + Sync {
    fn kind(&self) -> AiProviderKind;
    fn model(&self) -> &str;
    async fn complete(&self, call: &CompletionCall) -> UnifiedResult<RawCompletion>;
    async fn test_connection(&self) -> UnifiedResult<ProviderTestResult>;
}

/// Factory that builds an adapter for one stored connection plus its
/// decrypted key (`None` for a keyless local endpoint).
pub type AiProviderFactory = Arc<
    dyn Fn(&AiProviderConnection, Option<String>) -> UnifiedResult<Arc<dyn AiProviderAdapter>>
        + Send
        + Sync,
>;

pub struct AiProviderRegistry {
    factories: RwLock<HashMap<AiProviderKind, AiProviderFactory>>,
}

impl AiProviderRegistry {
    fn new() -> Self {
        Self {
            factories: RwLock::new(HashMap::new()),
        }
    }

    /// Register (or replace) the factory for a kind. This is how a
    /// new provider plugs in: call it once at startup, or from an
    /// extension, instead of editing this module.
    pub fn register_factory(&self, kind: AiProviderKind, factory: AiProviderFactory) {
        self.factories
            .write()
            .expect("ai provider registry lock poisoned")
            .insert(kind, factory);
    }

    pub fn build(
        &self,
        connection: &AiProviderConnection,
        api_key: Option<String>,
    ) -> UnifiedResult<Arc<dyn AiProviderAdapter>> {
        let factories = self
            .factories
            .read()
            .expect("ai provider registry lock poisoned");
        let factory = factories.get(&connection.kind).ok_or_else(|| {
            UnifiedError::internal(format!(
                "No AI provider adapter registered for kind \"{}\"",
                connection.kind.as_str()
            ))
        })?;
        factory(connection, api_key)
    }

    pub fn registered_kinds(&self) -> Vec<AiProviderKind> {
        let factories = self
            .factories
            .read()
            .expect("ai provider registry lock poisoned");
        let mut kinds: Vec<AiProviderKind> = factories.keys().copied().collect();
        kinds.sort_by_key(|kind| kind.as_str());
        kinds
    }
}

static REGISTRY: std::sync::OnceLock<AiProviderRegistry> = std::sync::OnceLock::new();

/// The shared registry, with the built-in adapters registered on
/// first use.
pub fn ai_provider_registry() -> &'static AiProviderRegistry {
    REGISTRY.get_or_init(|| {
        let registry = AiProviderRegistry::new();
        registry.register_factory(
            AiProviderKind::Anthropic,
            Arc::new(|connection, api_key| {
                Ok(Arc::new(AnthropicAdapter::new(connection, api_key)?)
                    as Arc<dyn AiProviderAdapter>)
            }),
        );
        registry.register_factory(
            AiProviderKind::OpenAi,
            Arc::new(|connection, api_key| {
                Ok(Arc::new(OpenAiCompatibleAdapter::new(connection, api_key)?)
                    as Arc<dyn AiProviderAdapter>)
            }),
        );
        registry.register_factory(
            AiProviderKind::OpenAiCompatible,
            Arc::new(|connection, api_key| {
                Ok(Arc::new(OpenAiCompatibleAdapter::new(connection, api_key)?)
                    as Arc<dyn AiProviderAdapter>)
            }),
        );
        registry.register_factory(
            AiProviderKind::Local,
            Arc::new(|connection, api_key| {
                Ok(Arc::new(OpenAiCompatibleAdapter::new(connection, api_key)?)
                    as Arc<dyn AiProviderAdapter>)
            }),
        );
        registry
    })
}

// ---------------------------------------------------------------------------
// Shared HTTP helpers.
// ---------------------------------------------------------------------------

fn build_client() -> UnifiedResult<reqwest::Client> {
    reqwest::Client::builder()
        .user_agent(USER_AGENT)
        .timeout(Duration::from_secs(PROVIDER_TIMEOUT_SECS))
        .connect_timeout(Duration::from_secs(PROVIDER_CONNECT_TIMEOUT_SECS))
        .build()
        .map_err(|error| UnifiedError::internal(format!("Cannot build HTTP client: {error}")))
}

/// Parse a `Retry-After` header (seconds only; HTTP dates are
/// ignored), capped so a misbehaving provider cannot stall a caller
/// indefinitely.
fn parse_retry_after(response: &reqwest::Response) -> Option<Duration> {
    let value = response.headers().get(RETRY_AFTER)?.to_str().ok()?;
    value
        .trim()
        .parse::<u64>()
        .ok()
        .map(|secs| Duration::from_secs(secs).min(MAX_RETRY_AFTER))
}

/// Map a provider HTTP response to an error. A 429 keeps its status
/// and carries the `Retry-After` delay in metadata; other failures
/// stay generic so provider response bodies (which may contain
/// account data) are never leaked into the client-visible message.
fn http_status_error(
    status: StatusCode,
    provider: &str,
    retry_after: Option<Duration>,
) -> UnifiedError {
    let message = format!("Provider \"{provider}\" request failed with HTTP {status}");
    match status {
        StatusCode::TOO_MANY_REQUESTS => {
            let mut error = UnifiedError::new(message, 429)
                .with_metadata("provider", Value::String(provider.to_string()));
            if let Some(delay) = retry_after {
                error = error.with_metadata("retry_after_secs", Value::from(delay.as_secs()));
            }
            error
        }
        StatusCode::UNAUTHORIZED => UnifiedError::unauthorized(format!(
            "Provider \"{provider}\" rejected the credentials (HTTP 401)"
        )),
        StatusCode::FORBIDDEN => UnifiedError::forbidden(format!(
            "Provider \"{provider}\" refused the request (HTTP 403): the credentials lack permission for this operation"
        )),
        StatusCode::NOT_FOUND => {
            UnifiedError::not_found(format!("Provider \"{provider}\" resource not found (HTTP 404)"))
        }
        _ if status.is_client_error() => {
            UnifiedError::bad_request(message)
                .with_metadata("provider", Value::String(provider.to_string()))
        }
        _ => UnifiedError::internal(message)
            .with_metadata("provider", Value::String(provider.to_string())),
    }
}

/// Send a request and return the parsed JSON body, or a mapped error.
async fn send_json(
    client: &reqwest::Client,
    request: reqwest::RequestBuilder,
    provider: &str,
) -> UnifiedResult<Value> {
    let response = request.send().await.map_err(|error| {
        UnifiedError::internal(format!("Provider \"{provider}\" request failed: {error}"))
    })?;
    let status = response.status();
    if !status.is_success() {
        return Err(http_status_error(status, provider, parse_retry_after(&response)));
    }
    response.json::<Value>().await.map_err(|error| {
        UnifiedError::internal(format!(
            "Provider \"{provider}\" returned an unreadable response: {error}"
        ))
    })
}

/// Validate a base URL: http(s), a host, no embedded credentials.
/// Fixed-host providers additionally require an allow-listed host.
fn validate_base_url(base_url: &str, kind: AiProviderKind) -> UnifiedResult<()> {
    let url = Url::parse(base_url.trim()).map_err(|_| {
        UnifiedError::bad_request(format!("Invalid base URL \"{base_url}\""))
    })?;
    if !matches!(url.scheme(), "http" | "https") {
        return Err(UnifiedError::bad_request(
            "Base URL must use http or https",
        ));
    }
    if url.host_str().is_none_or(|host| host.is_empty()) {
        return Err(UnifiedError::bad_request("Base URL must include a host"));
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(UnifiedError::bad_request(
            "Base URL must not embed credentials; use the API key field",
        ));
    }
    let allowed = kind.allowed_hosts();
    if !allowed.is_empty() {
        let host = url.host_str().unwrap_or("");
        if !allowed.contains(&host) {
            return Err(UnifiedError::bad_request(format!(
                "Base URL host \"{host}\" is not an allowed {} API host",
                kind.as_str()
            )));
        }
    }
    Ok(())
}

/// Resolve the effective base URL and model for a connection input:
/// explicit values win, then the kind defaults; compatible and local
/// providers require both explicitly.
fn resolve_base_url_and_model(
    kind: AiProviderKind,
    base_url: &Option<String>,
    model: &Option<String>,
) -> UnifiedResult<(String, String)> {
    let base_url = match base_url.as_deref().map(str::trim).filter(|value| !value.is_empty()) {
        Some(url) => url.to_string(),
        None => match kind.default_base_url() {
            Some(default) => default.to_string(),
            None => {
                return Err(UnifiedError::bad_request(format!(
                    "Provider \"{}\" requires base_url (the API endpoint)",
                    kind.as_str()
                )))
            }
        },
    };
    validate_base_url(&base_url, kind)?;
    let model = match model.as_deref().map(str::trim).filter(|value| !value.is_empty()) {
        Some(model) => model.to_string(),
        None => match kind.default_model() {
            Some(default) => default.to_string(),
            None => {
                return Err(UnifiedError::bad_request(format!(
                    "Provider \"{}\" requires model",
                    kind.as_str()
                )))
            }
        },
    };
    Ok((base_url, model))
}

// ---------------------------------------------------------------------------
// Adapters.
// ---------------------------------------------------------------------------

/// Anthropic Messages API adapter (`POST {base}/v1/messages`).
pub struct AnthropicAdapter {
    kind: AiProviderKind,
    base_url: String,
    model: String,
    api_key: Option<String>,
    provider_id: Uuid,
    client: reqwest::Client,
}

impl AnthropicAdapter {
    fn new(
        connection: &AiProviderConnection,
        api_key: Option<String>,
    ) -> UnifiedResult<Self> {
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
    kind: AiProviderKind,
    base_url: String,
    model: String,
    api_key: Option<String>,
    provider_id: Uuid,
    client: reqwest::Client,
}

impl OpenAiCompatibleAdapter {
    fn new(
        connection: &AiProviderConnection,
        api_key: Option<String>,
    ) -> UnifiedResult<Self> {
        Ok(Self {
            kind: connection.kind,
            base_url: connection.base_url.clone(),
            model: connection.model.clone(),
            api_key,
            provider_id: connection.id,
            client: build_client()?,
        })
    }

    fn models_url(&self) -> String {
        format!("{}/models", self.base_url.trim_end_matches('/'))
    }

    fn completions_url(&self) -> String {
        format!("{}/chat/completions", self.base_url.trim_end_matches('/'))
    }

    fn auth_header(&self) -> Option<HeaderValue> {
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

// ---------------------------------------------------------------------------
// Shared state.
// ---------------------------------------------------------------------------

/// In-memory stores for the AI module. The hook bus is attached in
/// `init` (the bus lives in `AppContext`); handlers use the
/// `AppContext` bus directly, while the synchronous hook handlers
/// registered here read the bus from this state.
#[derive(Clone, Default)]
pub struct AiState {
    pub providers: Arc<DashMap<Uuid, AiProviderConnection>>,
    pub drafts: Arc<DashMap<Uuid, AiDraft>>,
    pub usage: Arc<DashMap<Uuid, AiUsageRecord>>,
    pub rate_buckets: Arc<DashMap<Uuid, RateBucket>>,
    pub hooks: Arc<RwLock<Option<Arc<HookBus>>>>,
}

impl AiState {
    fn emit(&self, event: &str, data: Value) -> Option<HookContext> {
        self.hooks
            .read()
            .expect("hooks lock poisoned")
            .clone()
            .map(|bus| bus.emit(event, data))
    }
}

// ---------------------------------------------------------------------------
// Request bodies.
// ---------------------------------------------------------------------------

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
}

fn default_rate_limit() -> Option<u32> {
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
}

fn default_content_type() -> String {
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
}

// ---------------------------------------------------------------------------
// Helpers: caller, permissions, validation.
// ---------------------------------------------------------------------------

/// The caller, from the headers every module in this codebase uses:
/// `X-User-Id` identifies the actor, `X-Role` carries the role slug
/// or id the capability check runs against.
struct Caller {
    actor: String,
    role: String,
}

fn caller(req: &HttpRequest) -> UnifiedResult<Caller> {
    let actor = req
        .headers()
        .get("x-user-id")
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .ok_or_else(|| UnifiedError::unauthorized("Missing X-User-Id header"))?;
    let role = req
        .headers()
        .get("x-role")
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .ok_or_else(|| UnifiedError::unauthorized("Missing X-Role header"))?;
    Ok(Caller { actor, role })
}

/// Ask the roles module whether `role` holds `permission` through
/// the `role.check` hook. Fails closed: no answer means not allowed.
/// (Same fail-closed shape as the tickets module's capability check.)
fn check_capability(ctx: &AppContext, role: &str, permission: &str) -> bool {
    let context = ctx.hooks.emit(
        "role.check",
        json!({ "role": role, "permission": permission, "allowed": false }),
    );
    context
        .get("allowed")
        .and_then(|value| value.as_bool())
        .unwrap_or(false)
}

fn require_permission(ctx: &AppContext, role: &str, permission: &str) -> UnifiedResult<()> {
    if check_capability(ctx, role, permission) {
        Ok(())
    } else {
        Err(UnifiedError::forbidden(format!(
            "Role \"{role}\" does not hold permission \"{permission}\""
        )))
    }
}

fn require_text(value: &str, field: &str, max: usize) -> UnifiedResult<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err(UnifiedError::bad_request(format!("{field} must not be empty")));
    }
    if trimmed.len() > max {
        return Err(UnifiedError::bad_request(format!(
            "{field} must be at most {max} characters"
        )));
    }
    Ok(trimmed.to_string())
}

fn optional_text(value: &Option<String>, field: &str, max: usize) -> UnifiedResult<Option<String>> {
    match value {
        None => Ok(None),
        Some(text) => Ok(Some(require_text(text, field, max)?)),
    }
}

fn parse_uuid(raw: &str, what: &str) -> UnifiedResult<Uuid> {
    Uuid::parse_str(raw)
        .map_err(|_| UnifiedError::bad_request(format!("Invalid {what} id: {raw}")))
}

fn ok<T: Serialize>(data: T) -> HttpResponse {
    json::success_response(data, 200)
}

fn created<T: Serialize>(data: T) -> HttpResponse {
    json::success_response(data, 201)
}

fn err(error: UnifiedError, debug: bool) -> HttpResponse {
    json::error_response(&error, debug)
}

fn paginate<T: Serialize + Clone>(items: Vec<T>, params: &PaginationParams) -> PageResult<T> {
    let pagination = params.normalized();
    let total = items.len() as u64;
    let start = ((pagination.page - 1) * pagination.per_page) as usize;
    let page_items: Vec<T> = items
        .into_iter()
        .skip(start)
        .take(pagination.per_page as usize)
        .collect();
    PageResult::new(page_items, total, &pagination)
}

/// Emit an audit-shaped event on the hook bus: the audit module
/// reads `actor_id`, `resource`, and `id` from the payload. The
/// audit module only records events listed in its `AUDITED_EVENTS`,
/// so the integrator must add the `ai.*` names there.
fn emit_audit(ctx: &AppContext, event: &str, actor: &str, resource: &str, extra: Value) {
    let mut payload = match extra {
        Value::Object(map) => map,
        _ => serde_json::Map::new(),
    };
    payload.insert("actor_id".to_string(), Value::String(actor.to_string()));
    payload.insert("resource".to_string(), Value::String(resource.to_string()));
    ctx.hooks.emit(event, Value::Object(payload));
}

// ---------------------------------------------------------------------------
// Helpers: rate limits and usage recording.
// ---------------------------------------------------------------------------

/// Enforce the per-key rate limit for one provider connection.
/// Returns a 429 with `retry_after_secs` in metadata when the minute
/// window is exhausted.
fn check_rate_limit(state: &AiState, connection: &AiProviderConnection) -> UnifiedResult<()> {
    let limit = connection.rate_limit_per_minute;
    if limit == 0 {
        return Ok(());
    }
    let now = Instant::now();
    let mut bucket = state
        .rate_buckets
        .entry(connection.id)
        .or_insert(RateBucket {
            window_start: now,
            count: 0,
        });
    if now.duration_since(bucket.window_start) >= Duration::from_secs(60) {
        bucket.window_start = now;
        bucket.count = 0;
    }
    if bucket.count >= limit {
        let retry_after = 60 - now.duration_since(bucket.window_start).as_secs().min(60);
        return Err(UnifiedError::new(
            format!(
                "Rate limit exceeded for provider \"{}\": at most {limit} calls per minute",
                connection.name
            ),
            429,
        )
        .with_metadata("provider_id", Value::String(connection.id.to_string()))
        .with_metadata("retry_after_secs", Value::from(retry_after)));
    }
    bucket.count += 1;
    Ok(())
}

fn record_usage(
    state: &AiState,
    connection: &AiProviderConnection,
    actor: &str,
    use_case: &str,
    completion: Option<&RawCompletion>,
    latency_ms: u64,
    error_code: Option<u16>,
) -> AiUsageRecord {
    let (prompt_tokens, completion_tokens) = match completion {
        Some(raw) => (raw.prompt_tokens, raw.completion_tokens),
        None => (None, None),
    };
    let total_tokens = match (prompt_tokens, completion_tokens) {
        (Some(prompt), Some(completion)) => Some(prompt + completion),
        _ => None,
    };
    let estimated_cost_usd = match (prompt_tokens, completion_tokens) {
        (Some(prompt), Some(completion)) => {
            estimate_cost_usd(&connection.model, prompt, completion)
        }
        _ => None,
    };
    let record = AiUsageRecord {
        id: Uuid::now_v7(),
        provider_id: connection.id,
        provider_kind: connection.kind.as_str().to_string(),
        actor: actor.to_string(),
        use_case: use_case.to_string(),
        model: connection.model.clone(),
        prompt_tokens,
        completion_tokens,
        total_tokens,
        estimated_cost_usd,
        latency_ms,
        status: if error_code.is_none() {
            "success".to_string()
        } else {
            "error".to_string()
        },
        error_code,
        created_at: Utc::now(),
    };
    state.usage.insert(record.id, record.clone());
    record
}

/// Resolve the provider connection for a call: the requested one, or
/// the earliest-created enabled connection when none was requested.
fn resolve_connection(
    state: &AiState,
    provider_id: Option<Uuid>,
) -> UnifiedResult<AiProviderConnection> {
    match provider_id {
        Some(id) => state
            .providers
            .get(&id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found(format!("AI provider connection not found: {id}"))),
        None => {
            let mut enabled: Vec<AiProviderConnection> = state
                .providers
                .iter()
                .map(|entry| entry.value().clone())
                .filter(|connection| connection.enabled)
                .collect();
            enabled.sort_by(|a, b| a.created_at.cmp(&b.created_at));
            enabled.into_iter().next().ok_or_else(|| {
                UnifiedError::bad_request(
                    "No enabled AI provider connection exists: create one first, or pass provider_id",
                )
            })
        }
    }
}

/// Outcome of one provider call, with its usage record attached.
struct CompletionOutcome {
    text: String,
    usage: AiUsageRecord,
    provider_id: Uuid,
    provider_kind: String,
    model: String,
}

/// Run one provider call end to end: resolve and rate-limit the
/// connection, decrypt the key, call the adapter, record usage, and
/// emit the audit event. A disabled connection is refused before any
/// provider traffic.
async fn run_completion(
    state: &AiState,
    ctx: &Arc<AppContext>,
    actor: &str,
    provider_id: Option<Uuid>,
    call: &CompletionCall,
    use_case: &str,
) -> UnifiedResult<CompletionOutcome> {
    let connection = resolve_connection(state, provider_id)?;
    if !connection.enabled {
        return Err(UnifiedError::bad_request(format!(
            "AI provider connection \"{}\" is disabled",
            connection.name
        )));
    }
    check_rate_limit(state, &connection)?;
    let secret = encryption_secret(ctx);
    let api_key = match &connection.api_key_encrypted {
        Some(stored) => Some(decrypt_api_key(&secret, stored)?),
        None => None,
    };
    let adapter = ai_provider_registry().build(&connection, api_key)?;
    let started = Instant::now();
    let result = adapter.complete(call).await;
    let latency_ms = started.elapsed().as_millis() as u64;
    match result {
        Ok(raw) => {
            let usage = record_usage(state, &connection, actor, use_case, Some(&raw), latency_ms, None);
            emit_audit(
                ctx,
                EVENT_AI_CALL_COMPLETED,
                actor,
                &format!("ai-provider:{}", connection.id),
                json!({
                    "id": usage.id.to_string(),
                    "provider_id": connection.id.to_string(),
                    "provider_kind": connection.kind.as_str(),
                    "use_case": use_case,
                    "model": connection.model,
                    "prompt_tokens": raw.prompt_tokens,
                    "completion_tokens": raw.completion_tokens,
                    "usage_id": usage.id.to_string(),
                }),
            );
            Ok(CompletionOutcome {
                text: raw.text,
                usage,
                provider_id: connection.id,
                provider_kind: connection.kind.as_str().to_string(),
                model: connection.model.clone(),
            })
        }
        Err(error) => {
            let usage = record_usage(state, &connection, actor, use_case, None, latency_ms, Some(error.code));
            emit_audit(
                ctx,
                EVENT_AI_CALL_FAILED,
                actor,
                &format!("ai-provider:{}", connection.id),
                json!({
                    "id": usage.id.to_string(),
                    "provider_id": connection.id.to_string(),
                    "provider_kind": connection.kind.as_str(),
                    "use_case": use_case,
                    "model": connection.model,
                    "error_code": error.code,
                    "usage_id": usage.id.to_string(),
                }),
            );
            Err(error)
        }
    }
}

// ---------------------------------------------------------------------------
// Helpers: drafts and hook-gathered context.
// ---------------------------------------------------------------------------

#[allow(clippy::too_many_arguments)]
fn store_draft(
    state: &AiState,
    ctx: &Arc<AppContext>,
    kind: DraftKind,
    title: Option<String>,
    subject: Option<String>,
    body: String,
    prompt: Option<String>,
    outcome: &CompletionOutcome,
    actor: &str,
    target_type: Option<String>,
    target_id: Option<String>,
    sources: Vec<GroundingSource>,
) -> AiDraft {
    let now = Utc::now();
    let draft = AiDraft {
        id: Uuid::now_v7(),
        kind,
        title,
        subject,
        body,
        prompt,
        provider_id: Some(outcome.provider_id),
        provider_kind: outcome.provider_kind.clone(),
        model: outcome.model.clone(),
        actor: actor.to_string(),
        status: DraftStatus::Draft,
        ai_generated: true,
        target_type,
        target_id,
        sources,
        usage_id: Some(outcome.usage.id),
        reviewed_by: None,
        reviewed_at: None,
        created_at: now,
        updated_at: now,
    };
    state.drafts.insert(draft.id, draft.clone());
    emit_audit(
        ctx,
        EVENT_AI_DRAFT_CREATED,
        actor,
        &format!("ai-draft:{}", draft.id),
        json!({
            "id": draft.id.to_string(),
            "kind": kind.as_str(),
            "status": "draft",
            "ai_generated": true,
            "provider_id": outcome.provider_id.to_string(),
            "usage_id": outcome.usage.id.to_string(),
            "target_type": draft.target_type,
            "target_id": draft.target_id,
        }),
    );
    draft
}

/// Gather a ticket or chat transcript through the hook bus. The
/// owning module may answer with `{ "transcript": "..." }`, with
/// `{ "messages": [{ "role", "content" }] }`, or with `{ "title",
/// "body" }`. Returns `None` when no handler answers, so the caller
/// falls back to its own supplied text.
fn hook_transcript(ctx: &AppContext, event: &str, payload: Value) -> Option<String> {
    let hook = ctx.hooks.emit(event, payload);
    let mut lines: Vec<String> = Vec::new();
    for result in &hook.results {
        if let Some(transcript) = result.get("transcript").and_then(|value| value.as_str()) {
            if !transcript.trim().is_empty() {
                lines.push(transcript.trim().to_string());
            }
        }
        if let Some(messages) = result.get("messages").and_then(|value| value.as_array()) {
            for message in messages {
                let role = message
                    .get("role")
                    .and_then(|value| value.as_str())
                    .unwrap_or("message");
                let content = message
                    .get("content")
                    .or_else(|| message.get("body"))
                    .and_then(|value| value.as_str())
                    .unwrap_or("");
                if !content.trim().is_empty() {
                    lines.push(format!("{role}: {}", content.trim()));
                }
            }
        }
        let title = result.get("title").and_then(|value| value.as_str());
        let body = result.get("body").and_then(|value| value.as_str());
        if let (Some(title), Some(body)) = (title, body) {
            lines.push(format!("{title}\n{body}"));
        }
    }
    if lines.is_empty() {
        None
    } else {
        Some(lines.join("\n"))
    }
}

/// Validate the shared completion options (token budget and
/// temperature) from any draft/completion input.
fn completion_options(
    max_tokens: Option<u32>,
    temperature: Option<f32>,
) -> UnifiedResult<(u32, Option<f32>)> {
    let max_tokens = max_tokens.unwrap_or(DEFAULT_MAX_TOKENS);
    if max_tokens == 0 || max_tokens > MAX_MAX_TOKENS {
        return Err(UnifiedError::bad_request(format!(
            "max_tokens must be between 1 and {MAX_MAX_TOKENS}"
        )));
    }
    let temperature = match temperature {
        Some(value) if !(0.0..=2.0).contains(&value) => {
            return Err(UnifiedError::bad_request(
                "temperature must be between 0.0 and 2.0",
            ));
        }
        value => value,
    };
    Ok((max_tokens, temperature))
}

// ---------------------------------------------------------------------------
// Module.
// ---------------------------------------------------------------------------

pub struct AiModule {
    state: AiState,
}

impl AiModule {
    pub fn new() -> Self {
        Self {
            state: AiState::default(),
        }
    }
}

impl Default for AiModule {
    fn default() -> Self {
        Self::new()
    }
}

pub fn register(registry: &ModuleRegistry) {
    registry.register(Arc::new(AiModule::new()));
}

impl Module for AiModule {
    fn id(&self) -> &'static str {
        "ai"
    }

    fn name(&self) -> &'static str {
        "AI"
    }

    fn description(&self) -> &'static str {
        "AI provider abstraction (Anthropic, OpenAI, OpenAI-compatible, local) with human-approved drafts for support, content, and email, plus usage tracking and a shared site context."
    }

    fn requires(&self) -> &[&'static str] {
        &[
            "roles",
            "audit",
            "knowledgebase",
            "tickets",
            "chat",
            "content",
            "site-builder",
            "newsletter",
            "email_providers",
        ]
    }

    fn init(&self, ctx: &AppContext) -> UnifiedResult<()> {
        *self.state.hooks.write().expect("hooks lock poisoned") = Some(Arc::clone(&ctx.hooks));
        Ok(())
    }

    fn register_hooks(&self, bus: &HookBus) {
        // Answer the shared-context request: the same context the AI
        // features use. The handler passes `include_site_hooks:
        // false` so it never re-emits the event it is answering;
        // other modules (including the public server module) answer
        // the same event with their own site facts.
        for event in [EVENT_SITE_CONTEXT_REQUESTED, EVENT_AI_CONTEXT_REQUESTED] {
            let state = self.state.clone();
            bus.register(
                event,
                "ai",
                Arc::new(move |ctx: &mut HookContext| {
                    let query = ctx
                        .get("query")
                        .and_then(|value| value.as_str())
                        .unwrap_or("")
                        .to_string();
                    let site = SiteContextInput::from_hook(ctx);
                    let hooks = state
                        .hooks
                        .read()
                        .expect("hooks lock poisoned")
                        .clone();
                    let shared =
                        assemble_shared_context(hooks.as_deref(), &query, &site, false);
                    ctx.add_result(json!({
                        "site_name": shared.site_name,
                        "site_url": shared.site_url,
                        "locale": shared.locale,
                        "grounding_articles": shared.grounding_articles,
                        "extra": shared.extra,
                        "assembled_at": shared.assembled_at,
                    }));
                }),
            );
        }
    }

    fn route_info(&self) -> Vec<RouteInfo> {
        vec![
            RouteInfo { method: "GET", path: "/api/v1/ai/providers", owner: "ai", description: "List provider kinds" },
            RouteInfo { method: "GET", path: "/api/v1/ai/provider-connections", owner: "ai", description: "List provider connections (masked)" },
            RouteInfo { method: "POST", path: "/api/v1/ai/provider-connections", owner: "ai", description: "Create provider connection" },
            RouteInfo { method: "GET", path: "/api/v1/ai/provider-connections/{id}", owner: "ai", description: "Get provider connection (masked)" },
            RouteInfo { method: "PATCH", path: "/api/v1/ai/provider-connections/{id}", owner: "ai", description: "Update provider connection" },
            RouteInfo { method: "DELETE", path: "/api/v1/ai/provider-connections/{id}", owner: "ai", description: "Delete provider connection" },
            RouteInfo { method: "POST", path: "/api/v1/ai/provider-connections/{id}/test", owner: "ai", description: "Test provider connection" },
            RouteInfo { method: "POST", path: "/api/v1/ai/complete", owner: "ai", description: "Run a completion, stored as a draft" },
            RouteInfo { method: "POST", path: "/api/v1/ai/support-draft", owner: "ai", description: "Draft a ticket reply (draft only)" },
            RouteInfo { method: "POST", path: "/api/v1/ai/chat-draft", owner: "ai", description: "Draft a chat reply (draft only)" },
            RouteInfo { method: "POST", path: "/api/v1/ai/content-draft", owner: "ai", description: "Draft post/page text (draft only)" },
            RouteInfo { method: "POST", path: "/api/v1/ai/email-draft", owner: "ai", description: "Draft email subject + body (draft only)" },
            RouteInfo { method: "POST", path: "/api/v1/ai/answer", owner: "ai", description: "Knowledgebase-grounded answer (draft only)" },
            RouteInfo { method: "GET", path: "/api/v1/ai/drafts", owner: "ai", description: "List drafts" },
            RouteInfo { method: "GET", path: "/api/v1/ai/drafts/{id}", owner: "ai", description: "Get draft" },
            RouteInfo { method: "POST", path: "/api/v1/ai/drafts/{id}/approve", owner: "ai", description: "Approve a draft (human review; does not send or publish)" },
            RouteInfo { method: "POST", path: "/api/v1/ai/drafts/{id}/reject", owner: "ai", description: "Reject a draft" },
            RouteInfo { method: "DELETE", path: "/api/v1/ai/drafts/{id}", owner: "ai", description: "Delete a draft" },
            RouteInfo { method: "GET", path: "/api/v1/ai/usage", owner: "ai", description: "List usage records" },
            RouteInfo { method: "GET", path: "/api/v1/ai/usage/{id}", owner: "ai", description: "Get usage record" },
            RouteInfo { method: "GET", path: "/api/v1/ai/usage/summary", owner: "ai", description: "Usage summary" },
            RouteInfo { method: "GET", path: "/api/v1/ai/context", owner: "ai", description: "Get the shared site context" },
        ]
    }

    fn configure_routes(&self, cfg: &mut web::ServiceConfig) {
        cfg.app_data(web::Data::new(self.state.clone()));
        cfg.service(
            web::scope("/api/v1/ai")
                .route("/providers", web::get().to(list_providers))
                .route("/provider-connections", web::get().to(list_provider_connections))
                .route("/provider-connections", web::post().to(create_provider_connection))
                .route("/provider-connections/{id}", web::get().to(get_provider_connection))
                .route("/provider-connections/{id}", web::patch().to(update_provider_connection))
                .route("/provider-connections/{id}", web::delete().to(delete_provider_connection))
                .route("/provider-connections/{id}/test", web::post().to(test_provider_connection))
                .route("/complete", web::post().to(complete))
                .route("/support-draft", web::post().to(support_draft))
                .route("/chat-draft", web::post().to(chat_draft))
                .route("/content-draft", web::post().to(content_draft))
                .route("/email-draft", web::post().to(email_draft))
                .route("/answer", web::post().to(answer))
                .route("/drafts", web::get().to(list_drafts))
                .route("/drafts/{id}", web::get().to(get_draft))
                .route("/drafts/{id}/approve", web::post().to(approve_draft))
                .route("/drafts/{id}/reject", web::post().to(reject_draft))
                .route("/drafts/{id}", web::delete().to(delete_draft))
                .route("/usage", web::get().to(list_usage))
                .route("/usage/summary", web::get().to(usage_summary))
                .route("/usage/{id}", web::get().to(get_usage))
                .route("/context", web::get().to(get_context)),
        );
    }
}

// ---------------------------------------------------------------------------
// Provider connection handlers.
// ---------------------------------------------------------------------------

async fn list_providers(req: HttpRequest) -> HttpResponse {
    let result: UnifiedResult<Vec<AiProviderCatalogEntry>> = (|| {
        let _caller = caller(&req)?;
        Ok(AiProviderKind::all()
            .iter()
            .map(|kind| AiProviderCatalogEntry {
                kind: kind.as_str().to_string(),
                display_name: kind.display_name().to_string(),
                default_base_url: kind.default_base_url().map(str::to_string),
                default_model: kind.default_model().map(str::to_string),
                key_required: kind.key_required(),
            })
            .collect())
    })();
    match result {
        Ok(entries) => ok(entries),
        Err(error) => err(error, false),
    }
}

async fn list_provider_connections(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<AiState>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_AI_MANAGE_PROVIDERS)?;
        let params = query.into_inner();
        let kind_filter = params.filters.get("kind").cloned();
        let enabled_filter = params.filters.get("enabled").cloned();
        let search = params.search.clone().unwrap_or_default().to_lowercase();
        let mut items: Vec<AiProviderConnectionView> = state
            .providers
            .iter()
            .map(|entry| entry.value().view())
            .filter(|view| {
                if let Some(kind) = &kind_filter {
                    if view.kind != *kind {
                        return false;
                    }
                }
                if let Some(enabled) = &enabled_filter {
                    let expected = match enabled.as_str() {
                        "true" => true,
                        "false" => false,
                        _ => return false,
                    };
                    if view.enabled != expected {
                        return false;
                    }
                }
                if !search.is_empty() {
                    let haystack = format!("{} {} {}", view.name, view.kind, view.model)
                        .to_lowercase();
                    if !haystack.contains(&search) {
                        return false;
                    }
                }
                true
            })
            .collect();
        let sort = params.allowed_sort(&["name", "kind", "created_at"], "created_at");
        let descending = params.normalized().order == "desc";
        items.sort_by(|a, b| {
            let ordering = match sort.as_str() {
                "name" => a.name.cmp(&b.name),
                "kind" => a.kind.cmp(&b.kind),
                _ => a.created_at.cmp(&b.created_at),
            };
            if descending {
                ordering.reverse()
            } else {
                ordering
            }
        });
        Ok(paginate(items, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn create_provider_connection(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<AiState>,
    req: HttpRequest,
    body: web::Json<CreateProviderConnectionInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let input = body.into_inner();
    let result: UnifiedResult<AiProviderConnectionView> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_AI_MANAGE_PROVIDERS)?;
        let name = require_text(&input.name, "name", MAX_NAME_LEN)?;
        let (base_url, model) =
            resolve_base_url_and_model(input.kind, &input.base_url, &input.model)?;
        let rate_limit = input.rate_limit_per_minute.unwrap_or(DEFAULT_RATE_LIMIT_PER_MINUTE);
        if rate_limit > MAX_RATE_LIMIT_PER_MINUTE {
            return Err(UnifiedError::bad_request(format!(
                "rate_limit_per_minute must be at most {MAX_RATE_LIMIT_PER_MINUTE}"
            )));
        }
        let key = input.api_key.as_deref().map(str::trim).filter(|key| !key.is_empty());
        if input.kind.key_required() && key.is_none() {
            return Err(UnifiedError::bad_request(format!(
                "Provider \"{}\" requires api_key",
                input.kind.as_str()
            )));
        }
        let secret = encryption_secret(&ctx);
        let (api_key_encrypted, key_fingerprint) = match key {
            Some(plain) => (
                Some(encrypt_api_key(&secret, plain)),
                Some(key_fingerprint(plain)),
            ),
            None => (None, None),
        };
        if state
            .providers
            .iter()
            .any(|entry| entry.value().name == name)
        {
            return Err(UnifiedError::conflict(format!(
                "An AI provider connection named \"{name}\" already exists"
            )));
        }
        let now = Utc::now();
        let connection = AiProviderConnection {
            id: Uuid::now_v7(),
            name,
            kind: input.kind,
            base_url,
            model,
            api_key_encrypted,
            key_fingerprint,
            enabled: input.enabled.unwrap_or(true),
            rate_limit_per_minute: rate_limit,
            created_at: now,
            updated_at: now,
        };
        state.providers.insert(connection.id, connection.clone());
        emit_audit(
            &ctx,
            EVENT_AI_PROVIDER_CREATED,
            &caller.actor,
            &format!("ai-provider:{}", connection.id),
            json!({
                "id": connection.id.to_string(),
                "kind": connection.kind.as_str(),
                "name": connection.name,
                "key_configured": connection.api_key_encrypted.is_some(),
            }),
        );
        Ok(connection.view())
    })();
    match result {
        Ok(view) => created(view),
        Err(error) => err(error, debug),
    }
}

async fn get_provider_connection(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<AiState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<AiProviderConnectionView> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_AI_MANAGE_PROVIDERS)?;
        let id = parse_uuid(&path.into_inner(), "provider connection")?;
        state
            .providers
            .get(&id)
            .map(|entry| entry.value().view())
            .ok_or_else(|| {
                UnifiedError::not_found(format!("AI provider connection not found: {id}"))
            })
    })();
    match result {
        Ok(view) => ok(view),
        Err(error) => err(error, debug),
    }
}

async fn update_provider_connection(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<AiState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdateProviderConnectionInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let input = body.into_inner();
    let result: UnifiedResult<AiProviderConnectionView> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_AI_MANAGE_PROVIDERS)?;
        let id = parse_uuid(&path.into_inner(), "provider connection")?;
        let mut connection = state
            .providers
            .get(&id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| {
                UnifiedError::not_found(format!("AI provider connection not found: {id}"))
            })?;
        if let Some(name) = &input.name {
            let name = require_text(name, "name", MAX_NAME_LEN)?;
            if state
                .providers
                .iter()
                .any(|entry| entry.value().name == name && entry.value().id != id)
            {
                return Err(UnifiedError::conflict(format!(
                    "An AI provider connection named \"{name}\" already exists"
                )));
            }
            connection.name = name;
        }
        if input.base_url.is_some() || input.model.is_some() {
            let (base_url, model) = resolve_base_url_and_model(
                connection.kind,
                &input.base_url.or(Some(connection.base_url.clone())),
                &input.model.or(Some(connection.model.clone())),
            )?;
            connection.base_url = base_url;
            connection.model = model;
        }
        if let Some(key_change) = &input.api_key {
            let secret = encryption_secret(&ctx);
            match key_change {
                None => {
                    connection.api_key_encrypted = None;
                    connection.key_fingerprint = None;
                }
                Some(plain) if plain.trim().is_empty() => {
                    connection.api_key_encrypted = None;
                    connection.key_fingerprint = None;
                }
                Some(plain) => {
                    connection.api_key_encrypted = Some(encrypt_api_key(&secret, plain.trim()));
                    connection.key_fingerprint = Some(key_fingerprint(plain.trim()));
                }
            }
        }
        if connection.kind.key_required() && connection.api_key_encrypted.is_none() {
            return Err(UnifiedError::bad_request(format!(
                "Provider \"{}\" requires api_key",
                connection.kind.as_str()
            )));
        }
        if let Some(enabled) = input.enabled {
            connection.enabled = enabled;
        }
        if let Some(rate_limit) = input.rate_limit_per_minute {
            if rate_limit > MAX_RATE_LIMIT_PER_MINUTE {
                return Err(UnifiedError::bad_request(format!(
                    "rate_limit_per_minute must be at most {MAX_RATE_LIMIT_PER_MINUTE}"
                )));
            }
            connection.rate_limit_per_minute = rate_limit;
        }
        connection.updated_at = Utc::now();
        state.providers.insert(connection.id, connection.clone());
        Ok(connection.view())
    })();
    match result {
        Ok(view) => ok(view),
        Err(error) => err(error, debug),
    }
}

async fn delete_provider_connection(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<AiState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_AI_MANAGE_PROVIDERS)?;
        let id = parse_uuid(&path.into_inner(), "provider connection")?;
        let connection = state
            .providers
            .remove(&id)
            .map(|(_, connection)| connection)
            .ok_or_else(|| {
                UnifiedError::not_found(format!("AI provider connection not found: {id}"))
            })?;
        state.rate_buckets.remove(&id);
        emit_audit(
            &ctx,
            EVENT_AI_PROVIDER_DELETED,
            &caller.actor,
            &format!("ai-provider:{id}"),
            json!({
                "id": id.to_string(),
                "kind": connection.kind.as_str(),
                "name": connection.name,
            }),
        );
        Ok(json!({ "deleted": true, "provider_id": id.to_string() }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn test_provider_connection(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<AiState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<AiProviderConnection> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_AI_MANAGE_PROVIDERS)?;
        let id = parse_uuid(&path.into_inner(), "provider connection")?;
        let connection = state
            .providers
            .get(&id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| {
                UnifiedError::not_found(format!("AI provider connection not found: {id}"))
            })?;
        Ok(connection)
    })();
    let connection = match result {
        Ok(connection) => connection,
        Err(error) => return err(error, debug),
    };
    let caller = match caller(&req) {
        Ok(caller) => caller,
        Err(error) => return err(error, debug),
    };
    let started = Instant::now();
    let secret = encryption_secret(&ctx);
    let api_key = match &connection.api_key_encrypted {
        Some(stored) => match decrypt_api_key(&secret, stored) {
            Ok(key) => Some(key),
            Err(error) => return err(error, debug),
        },
        None => None,
    };
    let adapter = match ai_provider_registry().build(&connection, api_key) {
        Ok(adapter) => adapter,
        Err(error) => return err(error, debug),
    };
    match adapter.test_connection().await {
        Ok(test) => {
            let usage = record_usage(&state, &connection, &caller.actor, "test", None, test.latency_ms, None);
            emit_audit(
                &ctx,
                EVENT_AI_CALL_COMPLETED,
                &caller.actor,
                &format!("ai-provider:{}", connection.id),
                json!({
                    "id": usage.id.to_string(),
                    "provider_id": connection.id.to_string(),
                    "provider_kind": connection.kind.as_str(),
                    "use_case": "test",
                    "usage_id": usage.id.to_string(),
                }),
            );
            ok(test)
        }
        Err(error) => {
            let latency_ms = started.elapsed().as_millis() as u64;
            let usage = record_usage(&state, &connection, &caller.actor, "test", None, latency_ms, Some(error.code));
            emit_audit(
                &ctx,
                EVENT_AI_CALL_FAILED,
                &caller.actor,
                &format!("ai-provider:{}", connection.id),
                json!({
                    "id": usage.id.to_string(),
                    "provider_id": connection.id.to_string(),
                    "provider_kind": connection.kind.as_str(),
                    "use_case": "test",
                    "error_code": error.code,
                    "usage_id": usage.id.to_string(),
                }),
            );
            // A failed test is a result, not an API error: report it
            // with `reachable: false` and the (sanitized) reason.
            ok(ProviderTestResult {
                provider_id: connection.id,
                kind: connection.kind.as_str().to_string(),
                reachable: false,
                latency_ms,
                model_count: None,
                detail: error.message.clone(),
            })
        }
    }
}

// ---------------------------------------------------------------------------
// Completion and draft handlers.
// ---------------------------------------------------------------------------

async fn complete(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<AiState>,
    req: HttpRequest,
    body: web::Json<CompleteInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let input = body.into_inner();
    let caller = match caller(&req) {
        Ok(caller) => caller,
        Err(error) => return err(error, debug),
    };
    if let Err(error) = require_permission(&ctx, &caller.role, PERM_AI_USE) {
        return err(error, debug);
    }
    let prompt = match require_text(&input.prompt, "prompt", MAX_PROMPT_LEN) {
        Ok(prompt) => prompt,
        Err(error) => return err(error, debug),
    };
    let system_prompt = match optional_text(&input.system_prompt, "system_prompt", MAX_PROMPT_LEN) {
        Ok(system_prompt) => system_prompt,
        Err(error) => return err(error, debug),
    };
    let (max_tokens, temperature) = match completion_options(input.max_tokens, input.temperature) {
        Ok(options) => options,
        Err(error) => return err(error, debug),
    };
    let site = input.site.clone().unwrap_or_default();
    let grounding_query = input
        .grounding_query
        .clone()
        .unwrap_or_else(|| prompt.clone());
    let shared = assemble_shared_context(Some(&ctx.hooks), &grounding_query, &site, true);
    let context_text = shared_context_prompt(&shared);
    let full_system = match system_prompt {
        Some(system) if !context_text.is_empty() => {
            Some(format!("{system}\n\nSite context:\n{context_text}"))
        }
        Some(system) => Some(system),
        None if !context_text.is_empty() => Some(format!("Site context:\n{context_text}")),
        None => None,
    };
    let call = CompletionCall {
        prompt: prompt.clone(),
        system_prompt: full_system,
        max_tokens,
        temperature,
    };
    let use_case = input.use_case.clone().unwrap_or_else(|| "complete".to_string());
    match run_completion(&state, &ctx, &caller.actor, input.provider_id, &call, &use_case).await {
        Ok(outcome) => {
            let draft = store_draft(
                &state,
                &ctx,
                DraftKind::Generic,
                None,
                None,
                outcome.text.clone(),
                Some(prompt),
                &outcome,
                &caller.actor,
                None,
                None,
                shared.grounding_articles,
            );
            created(draft)
        }
        Err(error) => err(error, debug),
    }
}

/// Shared implementation for the ticket-reply and chat-reply draft
/// endpoints. Context is gathered through the hook bus first
/// (`ticket.context_requested` / `chat.context_requested`) and falls
/// back to the caller-supplied `context_text`.
async fn build_reply_draft(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<AiState>,
    req: HttpRequest,
    body: web::Json<SupportDraftInput>,
    kind: DraftKind,
    use_case: &str,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let input = body.into_inner();
    let caller = match caller(&req) {
        Ok(caller) => caller,
        Err(error) => return err(error, debug),
    };
    if let Err(error) = require_permission(&ctx, &caller.role, PERM_AI_USE) {
        return err(error, debug);
    }
    let instruction = match require_text(&input.prompt, "prompt", MAX_PROMPT_LEN) {
        Ok(instruction) => instruction,
        Err(error) => return err(error, debug),
    };
    let (max_tokens, temperature) = match completion_options(input.max_tokens, input.temperature) {
        Ok(options) => options,
        Err(error) => return err(error, debug),
    };
    let mut context_parts: Vec<String> = Vec::new();
    if let Some(ticket_id) = &input.ticket_id {
        if let Some(transcript) = hook_transcript(
            &ctx,
            EVENT_TICKET_CONTEXT_REQUESTED,
            json!({ "ticket_id": ticket_id }),
        ) {
            context_parts.push(format!("Ticket {ticket_id}:\n{transcript}"));
        }
    }
    if let Some(conversation_id) = &input.conversation_id {
        if let Some(transcript) = hook_transcript(
            &ctx,
            EVENT_CHAT_CONTEXT_REQUESTED,
            json!({ "conversation_id": conversation_id }),
        ) {
            context_parts.push(format!("Chat conversation {conversation_id}:\n{transcript}"));
        }
    }
    if let Some(context_text) = &input.context_text {
        if let Ok(text) = require_text(context_text, "context_text", MAX_CONTEXT_TEXT_LEN) {
            context_parts.push(text);
        }
    }
    if context_parts.is_empty() {
        return err(
            UnifiedError::bad_request(
                "Provide ticket_id, conversation_id, or context_text so the draft can be grounded in the conversation",
            ),
            debug,
        );
    }
    let site = input.site.clone().unwrap_or_default();
    let shared = assemble_shared_context(Some(&ctx.hooks), &instruction, &site, true);
    let context_text = shared_context_prompt(&shared);
    let system_prompt = if context_text.is_empty() {
        Some(
            "You draft support replies. Use only the conversation context below; do not invent facts, prices, dates, or promises. Return only the reply text."
                .to_string(),
        )
    } else {
        Some(format!(
            "You draft support replies. Use only the conversation context below; do not invent facts, prices, dates, or promises. Return only the reply text.\n\nSite context:\n{context_text}"
        ))
    };
    let prompt = format!(
        "Instruction from staff: {instruction}\n\nConversation context:\n{}",
        context_parts.join("\n\n")
    );
    let call = CompletionCall {
        prompt: prompt.clone(),
        system_prompt,
        max_tokens,
        temperature,
    };
    let (target_type, target_id) = match (&input.ticket_id, &input.conversation_id) {
        (Some(ticket_id), _) => (Some("ticket".to_string()), Some(ticket_id.clone())),
        (None, Some(conversation_id)) => (
            Some("chat_conversation".to_string()),
            Some(conversation_id.clone()),
        ),
        _ => (None, None),
    };
    match run_completion(&state, &ctx, &caller.actor, input.provider_id, &call, use_case).await {
        Ok(outcome) => {
            let draft = store_draft(
                &state,
                &ctx,
                kind,
                None,
                None,
                outcome.text.clone(),
                Some(prompt),
                &outcome,
                &caller.actor,
                target_type,
                target_id,
                shared.grounding_articles,
            );
            created(draft)
        }
        Err(error) => err(error, debug),
    }
}

async fn support_draft(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<AiState>,
    req: HttpRequest,
    body: web::Json<SupportDraftInput>,
) -> HttpResponse {
    build_reply_draft(ctx, state, req, body, DraftKind::SupportReply, "support_reply").await
}

async fn chat_draft(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<AiState>,
    req: HttpRequest,
    body: web::Json<SupportDraftInput>,
) -> HttpResponse {
    build_reply_draft(ctx, state, req, body, DraftKind::ChatReply, "chat_reply").await
}

async fn content_draft(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<AiState>,
    req: HttpRequest,
    body: web::Json<ContentDraftInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let input = body.into_inner();
    let caller = match caller(&req) {
        Ok(caller) => caller,
        Err(error) => return err(error, debug),
    };
    if let Err(error) = require_permission(&ctx, &caller.role, PERM_AI_USE) {
        return err(error, debug);
    }
    let content_type = match input.content_type.as_str() {
        "post" | "page" => input.content_type.clone(),
        other => {
            return err(
                UnifiedError::bad_request(format!(
                    "content_type must be \"post\" or \"page\", got \"{other}\""
                )),
                debug,
            )
        }
    };
    let prompt = match require_text(&input.prompt, "prompt", MAX_PROMPT_LEN) {
        Ok(prompt) => prompt,
        Err(error) => return err(error, debug),
    };
    let title = match optional_text(&input.title, "title", 200) {
        Ok(title) => title,
        Err(error) => return err(error, debug),
    };
    let (max_tokens, temperature) = match completion_options(input.max_tokens, input.temperature) {
        Ok(options) => options,
        Err(error) => return err(error, debug),
    };
    let site = input.site.clone().unwrap_or_default();
    let shared = assemble_shared_context(Some(&ctx.hooks), &prompt, &site, true);
    let context_text = shared_context_prompt(&shared);
    let system_prompt = if context_text.is_empty() {
        Some(format!(
            "You draft {content_type} content. Return only the draft text, in Markdown. Do not publish anything."
        ))
    } else {
        Some(format!(
            "You draft {content_type} content. Return only the draft text, in Markdown. Do not publish anything.\n\nSite context:\n{context_text}"
        ))
    };
    let call = CompletionCall {
        prompt: prompt.clone(),
        system_prompt,
        max_tokens,
        temperature,
    };
    match run_completion(&state, &ctx, &caller.actor, input.provider_id, &call, "content").await {
        Ok(outcome) => {
            let draft = store_draft(
                &state,
                &ctx,
                DraftKind::Content,
                title,
                None,
                outcome.text.clone(),
                Some(prompt),
                &outcome,
                &caller.actor,
                Some(content_type),
                None,
                shared.grounding_articles,
            );
            created(draft)
        }
        Err(error) => err(error, debug),
    }
}

async fn email_draft(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<AiState>,
    req: HttpRequest,
    body: web::Json<EmailDraftInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let input = body.into_inner();
    let caller = match caller(&req) {
        Ok(caller) => caller,
        Err(error) => return err(error, debug),
    };
    if let Err(error) = require_permission(&ctx, &caller.role, PERM_AI_USE) {
        return err(error, debug);
    }
    let prompt = match require_text(&input.prompt, "prompt", MAX_PROMPT_LEN) {
        Ok(prompt) => prompt,
        Err(error) => return err(error, debug),
    };
    let audience = match optional_text(&input.audience, "audience", 500) {
        Ok(audience) => audience,
        Err(error) => return err(error, debug),
    };
    let (max_tokens, temperature) = match completion_options(input.max_tokens, input.temperature) {
        Ok(options) => options,
        Err(error) => return err(error, debug),
    };
    let site = input.site.clone().unwrap_or_default();
    let shared = assemble_shared_context(Some(&ctx.hooks), &prompt, &site, true);
    let context_text = shared_context_prompt(&shared);
    let system_prompt = if context_text.is_empty() {
        Some(
            "You draft email copy. Start with a line \"Subject: <subject>\", then a blank line, then the email body. Return only that text. Do not send anything."
                .to_string(),
        )
    } else {
        Some(format!(
            "You draft email copy. Start with a line \"Subject: <subject>\", then a blank line, then the email body. Return only that text. Do not send anything.\n\nSite context:\n{context_text}"
        ))
    };
    let full_prompt = match &audience {
        Some(audience) => format!("Audience: {audience}\n\nBrief: {prompt}"),
        None => prompt.clone(),
    };
    let call = CompletionCall {
        prompt: full_prompt,
        system_prompt,
        max_tokens,
        temperature,
    };
    match run_completion(&state, &ctx, &caller.actor, input.provider_id, &call, "email").await {
        Ok(outcome) => {
            // Split a leading "Subject: ..." line off the body.
            let (subject, body_text) = match outcome.text.split_once('\n') {
                Some((first, rest))
                    if first.trim_start().to_lowercase().starts_with("subject:") =>
                {
                    let subject = first.trim_start()[8..].trim().to_string();
                    (
                        if subject.is_empty() { None } else { Some(subject) },
                        rest.trim_start_matches('\n').trim().to_string(),
                    )
                }
                _ => (None, outcome.text.clone()),
            };
            let (target_type, target_id) = match &input.list_id {
                Some(list_id) => (Some("newsletter".to_string()), Some(list_id.clone())),
                None => (Some("email".to_string()), None),
            };
            let draft = store_draft(
                &state,
                &ctx,
                DraftKind::Email,
                None,
                subject,
                body_text,
                Some(prompt),
                &outcome,
                &caller.actor,
                target_type,
                target_id,
                shared.grounding_articles,
            );
            created(draft)
        }
        Err(error) => err(error, debug),
    }
}

async fn answer(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<AiState>,
    req: HttpRequest,
    body: web::Json<AnswerInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let input = body.into_inner();
    let caller = match caller(&req) {
        Ok(caller) => caller,
        Err(error) => return err(error, debug),
    };
    if let Err(error) = require_permission(&ctx, &caller.role, PERM_AI_USE) {
        return err(error, debug);
    }
    let question = match require_text(&input.question, "question", MAX_PROMPT_LEN) {
        Ok(question) => question,
        Err(error) => return err(error, debug),
    };
    let (max_tokens, temperature) = match completion_options(input.max_tokens, input.temperature) {
        Ok(options) => options,
        Err(error) => return err(error, debug),
    };
    let site = input.site.clone().unwrap_or_default();
    // Grounding uses the knowledgebase module's real search path
    // (`knowledgebase.search`, published articles only) through the
    // shared-context assembler.
    let shared = assemble_shared_context(Some(&ctx.hooks), &question, &site, true);
    let grounded = !shared.grounding_articles.is_empty();
    let context_text = shared_context_prompt(&shared);
    let system_prompt = if grounded {
        format!(
            "Answer the question using ONLY the knowledgebase articles below. Cite the article titles. If the articles do not cover the question, say so plainly instead of guessing.\n\n{context_text}"
        )
    } else {
        "No knowledgebase articles matched the question. Say plainly that no grounded answer is available and suggest asking a human agent. Do not invent an answer.".to_string()
    };
    let call = CompletionCall {
        prompt: question.clone(),
        system_prompt: Some(system_prompt),
        max_tokens,
        temperature,
    };
    match run_completion(&state, &ctx, &caller.actor, input.provider_id, &call, "kb_answer").await {
        Ok(outcome) => {
            let draft = store_draft(
                &state,
                &ctx,
                DraftKind::KbAnswer,
                None,
                None,
                outcome.text.clone(),
                Some(question),
                &outcome,
                &caller.actor,
                None,
                None,
                shared.grounding_articles,
            );
            created(draft)
        }
        Err(error) => err(error, debug),
    }
}

// ---------------------------------------------------------------------------
// Draft review handlers.
//
// Approval records a human decision only. There is deliberately no
// endpoint here that sends, publishes, or delivers a draft: the
// owning module must do that through its own existing human endpoint
// after reading the approved draft.
// ---------------------------------------------------------------------------

/// Load a draft, enforcing ownership: the author may read it, and a
/// role holding `ai:approve` may read any draft for review.
fn load_draft_for(
    state: &AiState,
    ctx: &AppContext,
    caller: &Caller,
    id: Uuid,
) -> UnifiedResult<AiDraft> {
    let draft = state
        .drafts
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("AI draft not found: {id}")))?;
    let can_review = check_capability(ctx, &caller.role, PERM_AI_APPROVE);
    if draft.actor != caller.actor && !can_review {
        return Err(UnifiedError::forbidden(
            "Draft belongs to a different user",
        ));
    }
    Ok(draft)
}

async fn list_drafts(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<AiState>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_AI_USE)?;
        // Reviewers see every draft; everyone else sees only their own.
        let can_review = check_capability(&ctx, &caller.role, PERM_AI_APPROVE);
        let params = query.into_inner();
        let kind_filter = params.filters.get("kind").cloned();
        let status_filter = params.filters.get("status").cloned();
        let provider_filter = params.filters.get("provider_id").cloned();
        let search = params.search.clone().unwrap_or_default().to_lowercase();
        let mut items: Vec<AiDraft> = state
            .drafts
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|draft| {
                if !can_review && draft.actor != caller.actor {
                    return false;
                }
                if let Some(kind) = &kind_filter {
                    if draft.kind.as_str() != kind {
                        return false;
                    }
                }
                if let Some(status) = &status_filter {
                    if draft.status.as_str() != status {
                        return false;
                    }
                }
                if let Some(provider_id) = &provider_filter {
                    if draft.provider_id.map(|id| id.to_string()) != Some(provider_id.clone()) {
                        return false;
                    }
                }
                if !search.is_empty() {
                    let haystack = format!(
                        "{} {} {}",
                        draft.title.clone().unwrap_or_default(),
                        draft.subject.clone().unwrap_or_default(),
                        draft.body
                    )
                    .to_lowercase();
                    if !haystack.contains(&search) {
                        return false;
                    }
                }
                true
            })
            .collect();
        let sort = params.allowed_sort(&["created_at", "updated_at", "kind", "status"], "created_at");
        let descending = params.normalized().order == "desc";
        items.sort_by(|a, b| {
            let ordering = match sort.as_str() {
                "updated_at" => a.updated_at.cmp(&b.updated_at),
                "kind" => a.kind.as_str().cmp(b.kind.as_str()),
                "status" => a.status.as_str().cmp(b.status.as_str()),
                _ => a.created_at.cmp(&b.created_at),
            };
            if descending {
                ordering.reverse()
            } else {
                ordering
            }
        });
        Ok(paginate(items, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn get_draft(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<AiState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<AiDraft> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_AI_USE)?;
        let id = parse_uuid(&path.into_inner(), "draft")?;
        load_draft_for(&state, &ctx, &caller, id)
    })();
    match result {
        Ok(draft) => ok(draft),
        Err(error) => err(error, debug),
    }
}

async fn approve_draft(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<AiState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<AiDraft> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_AI_APPROVE)?;
        let id = parse_uuid(&path.into_inner(), "draft")?;
        let mut draft = load_draft_for(&state, &ctx, &caller, id)?;
        if draft.status != DraftStatus::Draft {
            return Err(UnifiedError::conflict(format!(
                "Only a draft can be approved; this draft is {}",
                draft.status.as_str()
            )));
        }
        draft.status = DraftStatus::Approved;
        draft.reviewed_by = Some(caller.actor.clone());
        draft.reviewed_at = Some(Utc::now());
        draft.updated_at = Utc::now();
        state.drafts.insert(draft.id, draft.clone());
        // Approval is a review decision, not a delivery: the payload
        // says so, and no send/publish hook is emitted here.
        emit_audit(
            &ctx,
            EVENT_AI_DRAFT_APPROVED,
            &caller.actor,
            &format!("ai-draft:{}", draft.id),
            json!({
                "id": draft.id.to_string(),
                "kind": draft.kind.as_str(),
                "status": "approved",
                "ai_generated": true,
                "requires_human_delivery": true,
                "target_type": draft.target_type,
                "target_id": draft.target_id,
            }),
        );
        Ok(draft)
    })();
    match result {
        Ok(draft) => ok(draft),
        Err(error) => err(error, debug),
    }
}

async fn reject_draft(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<AiState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<AiDraft> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_AI_APPROVE)?;
        let id = parse_uuid(&path.into_inner(), "draft")?;
        let mut draft = load_draft_for(&state, &ctx, &caller, id)?;
        if draft.status != DraftStatus::Draft {
            return Err(UnifiedError::conflict(format!(
                "Only a draft can be rejected; this draft is {}",
                draft.status.as_str()
            )));
        }
        draft.status = DraftStatus::Rejected;
        draft.reviewed_by = Some(caller.actor.clone());
        draft.reviewed_at = Some(Utc::now());
        draft.updated_at = Utc::now();
        state.drafts.insert(draft.id, draft.clone());
        emit_audit(
            &ctx,
            EVENT_AI_DRAFT_REJECTED,
            &caller.actor,
            &format!("ai-draft:{}", draft.id),
            json!({
                "id": draft.id.to_string(),
                "kind": draft.kind.as_str(),
                "status": "rejected",
                "ai_generated": true,
                "target_type": draft.target_type,
                "target_id": draft.target_id,
            }),
        );
        Ok(draft)
    })();
    match result {
        Ok(draft) => ok(draft),
        Err(error) => err(error, debug),
    }
}

async fn delete_draft(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<AiState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_AI_USE)?;
        let id = parse_uuid(&path.into_inner(), "draft")?;
        let _draft = load_draft_for(&state, &ctx, &caller, id)?;
        state.drafts.remove(&id);
        Ok(json!({ "deleted": true, "draft_id": id.to_string() }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

// ---------------------------------------------------------------------------
// Usage handlers.
// ---------------------------------------------------------------------------

fn usage_matches(record: &AiUsageRecord, params: &PaginationParams) -> bool {
    if let Some(provider_id) = params.filters.get("provider_id") {
        if record.provider_id.to_string() != *provider_id {
            return false;
        }
    }
    if let Some(actor) = params.filters.get("actor") {
        if record.actor != *actor {
            return false;
        }
    }
    if let Some(use_case) = params.filters.get("use_case") {
        if record.use_case != *use_case {
            return false;
        }
    }
    if let Some(status) = params.filters.get("status") {
        if record.status != *status {
            return false;
        }
    }
    if let Some(search) = &params.search {
        if !search.trim().is_empty() {
            let haystack = format!("{} {} {} {}", record.actor, record.use_case, record.model, record.provider_kind)
                .to_lowercase();
            if !haystack.contains(&search.trim().to_lowercase()) {
                return false;
            }
        }
    }
    true
}

fn summarize_usage(records: &[AiUsageRecord]) -> AiUsageSummary {
    let mut summary = AiUsageSummary {
        calls: records.len() as u64,
        successful_calls: 0,
        failed_calls: 0,
        prompt_tokens: 0,
        completion_tokens: 0,
        total_tokens: 0,
        estimated_cost_usd: None,
    };
    let mut cost = 0.0f64;
    let mut has_cost = false;
    for record in records {
        match record.status.as_str() {
            "success" => summary.successful_calls += 1,
            _ => summary.failed_calls += 1,
        }
        summary.prompt_tokens += record.prompt_tokens.unwrap_or(0) as u64;
        summary.completion_tokens += record.completion_tokens.unwrap_or(0) as u64;
        summary.total_tokens += record.total_tokens.unwrap_or(0) as u64;
        if let Some(value) = record.estimated_cost_usd {
            cost += value;
            has_cost = true;
        }
    }
    if has_cost {
        summary.estimated_cost_usd = Some((cost * 1_000_000.0).round() / 1_000_000.0);
    }
    summary
}

async fn list_usage(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<AiState>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_AI_READ_USAGE)?;
        let params = query.into_inner();
        let mut items: Vec<AiUsageRecord> = state
            .usage
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|record| usage_matches(record, &params))
            .collect();
        let sort = params.allowed_sort(&["created_at", "model", "use_case"], "created_at");
        let descending = params.normalized().order == "desc";
        items.sort_by(|a, b| {
            let ordering = match sort.as_str() {
                "model" => a.model.cmp(&b.model),
                "use_case" => a.use_case.cmp(&b.use_case),
                _ => a.created_at.cmp(&b.created_at),
            };
            if descending {
                ordering.reverse()
            } else {
                ordering
            }
        });
        Ok(paginate(items, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn get_usage(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<AiState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<AiUsageRecord> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_AI_READ_USAGE)?;
        let id = parse_uuid(&path.into_inner(), "usage record")?;
        state
            .usage
            .get(&id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found(format!("AI usage record not found: {id}")))
    })();
    match result {
        Ok(record) => ok(record),
        Err(error) => err(error, debug),
    }
}

async fn usage_summary(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<AiState>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<AiUsageSummary> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_AI_READ_USAGE)?;
        let params = query.into_inner();
        let records: Vec<AiUsageRecord> = state
            .usage
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|record| usage_matches(record, &params))
            .collect();
        Ok(summarize_usage(&records))
    })();
    match result {
        Ok(summary) => ok(summary),
        Err(error) => err(error, debug),
    }
}

// ---------------------------------------------------------------------------
// Shared-context handler.
// ---------------------------------------------------------------------------

async fn get_context(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<SharedContext> = (|| {
        let _caller = caller(&req)?;
        let params = query.into_inner();
        let query_text = params.search.clone().unwrap_or_default();
        let site = SiteContextInput {
            site_name: params.filters.get("site_name").cloned(),
            site_url: params.filters.get("site_url").cloned(),
            locale: params.filters.get("locale").cloned(),
        };
        Ok(assemble_shared_context(Some(&ctx.hooks), &query_text, &site, true))
    })();
    match result {
        Ok(context) => ok(context),
        Err(error) => err(error, debug),
    }
}
