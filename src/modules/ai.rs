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
//   - `GET  /usage/by-user`, `GET /usage/by-company` - scoped usage.
//   - `GET/POST /access/companies`, `GET/PATCH/DELETE /access/companies/{id}`
//     - company access policies (admin; a company admin manages only
//     their own company).
//   - `GET/POST /access/users`, `GET/PATCH/DELETE /access/users/{id}`
//     - user access policies (admin; same company scoping).
//   - `GET/POST/DELETE /my-key`, `POST /my-key/test` - the caller's
//     own personal API key (metadata only, never the key).
//   - `GET/POST /my-mcp`, `GET/PATCH/DELETE /my-mcp/{id}` - the
//     caller's own personal MCP servers.
//   - `GET  /gating/effective` - the effective layered gate decision
//     for a user / feature, with the per-layer breakdown.
//   - `PATCH /gating/company/{company_id}`, `PATCH /gating/user/{user_id}`
//     - company / user on-off switches and per-feature toggles.
//   - `GET/PATCH /gating/settings` - the global kill-switch and
//     global per-feature switches (platform admin).
//   - `GET  /context` - the shared site context (see below).
//   - `GET  /capabilities` - list every registered AI capability.
//   - `GET  /capabilities/{module_id}` - list one module's AI capabilities.
//   - `POST /capabilities/{module_id}/{capability}` - run one
//     registered AI capability (see the AI Capabilities Contract
//     section; body `{"input": {...}}`).
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
use crate::core::middleware::TenantContext;
use crate::core::modules::{Module, ModuleRegistry, RouteInfo};
use crate::core::tenancy;
use crate::core::pagination::{PageResult, PaginationParams};
use crate::modules::privacy;
use actix_web::{web, HttpRequest, HttpResponse};
use async_trait::async_trait;
use base64::Engine as _;
use chrono::{DateTime, Datelike, TimeZone, Utc};
use dashmap::DashMap;
use hmac::{Hmac, Mac};
use reqwest::header::{HeaderValue, AUTHORIZATION, CONTENT_TYPE, RETRY_AFTER};
use reqwest::StatusCode;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::net::{IpAddr, ToSocketAddrs};
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
/// Emitted when an AI call is denied by the allowlist gate
/// (`authorize_ai_call`). Payload carries `reason` (a non-leaking
/// reason code) and the caller's own ids; it never carries any
/// policy's limits.
pub const EVENT_AI_ACCESS_DENIED: &str = "ai.access.denied";
/// Emitted when a caller stores their first personal API key.
pub const EVENT_AI_PERSONAL_KEY_CREATED: &str = "ai.personal_key.created";
/// Emitted when a caller replaces their personal API key.
pub const EVENT_AI_PERSONAL_KEY_ROTATED: &str = "ai.personal_key.rotated";
/// Emitted when a caller deletes their personal API key.
pub const EVENT_AI_PERSONAL_KEY_DELETED: &str = "ai.personal_key.deleted";
/// Emitted when a company or user access policy is created, updated,
/// or deleted. Payload carries the policy id, the company id, and
/// the enabled flag only.
pub const EVENT_AI_ACCESS_POLICY_CHANGED: &str = "ai.access.policy_changed";
/// Emitted when a layered-gate setting changes: a company gate, a
/// user gate, or the global settings. Payload carries `layer`
/// (`settings` / `product` / `company` / `user`), the target id, the
/// field name, and the old -> new values. It never carries prompt
/// content. The audit module records it once the integrator adds
/// the name to `AUDITED_EVENTS` in `audit.rs`.
pub const EVENT_AI_GATING_CHANGED: &str = "ai.gating.changed";
/// Hook query answered by the products / billing module: the
/// purchased-product layer of the gate asks whether the caller's
/// product (or subscription) includes AI. Request payload:
/// `{ "product_id": "<uuid>", "user_id": "<uuid>" | null,
/// "company_id": "<uuid>" | null }`. The answering handler sets
/// `includes_ai` on the hook context (`ctx.set("includes_ai",
/// json!(true))`). No answer means the product cannot be confirmed
/// and the gate denies the call (fail closed) with reason
/// `ai_product_unknown`. No product context on the call (no
/// `X-Product-Id` header) means the product layer defaults to allow
/// and the decision records that.
pub const EVENT_AI_PRODUCT_INCLUDES_AI: &str = "ai.product_includes_ai";
/// Emitted to resolve a caller's company: the users module answers
/// with `{ "company_id": "<uuid>" }` for `{ "user_id": "<actor>" }`.
/// No answer means the company is unknown and the allowlist gate
/// denies the call (fail closed) once enforcement is active.
pub const EVENT_USER_COMPANY_REQUESTED: &str = "user.company_requested";
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

/// Every use case an AI call can have. A policy's `allowed_features`
/// is a subset of this list; `test` is the connection-test use case.
/// `ai_capability` is the generic use case for per-module AI
/// capabilities run through `run_ai_capability`; a capability may
/// also declare its own module-scoped use case (for example
/// `exams.draft_questions`), which `validate_policy_features`
/// accepts when that capability is registered (see the AI
/// Capabilities Contract section).
const KNOWN_USE_CASES: &[&str] = &[
    "complete",
    "support_reply",
    "chat_reply",
    "content",
    "email",
    "kb_answer",
    "test",
    "ai_capability",
];

/// Most model names a policy may list, and most entries in one list.
const MAX_POLICY_LIST_LEN: usize = 100;
/// Longest single model name in a policy list.
const MAX_MODEL_NAME_LEN: usize = 200;
/// Longest personal API key / MCP auth token accepted.
const MAX_SECRET_LEN: usize = 500;
/// Longest personal MCP server name.
const MAX_MCP_NAME_LEN: usize = 120;
/// Longest personal MCP endpoint URL.
const MAX_MCP_URL_LEN: usize = 2048;
/// Most personal MCP servers one user may configure.
const MAX_PERSONAL_MCP_SERVERS: usize = 10;
/// Hardest monthly token cap a policy or personal key may set.
const MAX_MONTHLY_TOKEN_CAP: u64 = 1_000_000_000;
/// Hardest monthly cost cap (USD) a policy or personal key may set.
const MAX_MONTHLY_COST_CAP_USD: f64 = 1_000_000.0;

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
    /// Owning user, parsed from the `X-User-Id` header. `None` for
    /// records written before allowlists existed, and for calls whose
    /// actor id is not a UUID.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user_id: Option<Uuid>,
    /// Owning company, resolved from the `X-Company-Id` header or
    /// the `user.company_requested` hook. `None` for legacy records
    /// and for callers with no company.
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
    /// Canonical feature (use-case) names switched off globally.
    /// Empty means no feature is switched off globally.
    pub disabled_features: Vec<String>,
    pub updated_at: DateTime<Utc>,
}

impl Default for AiGlobalSettings {
    fn default() -> Self {
        Self {
            global_enabled: true,
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
    fn view(&self) -> AiPersonalKeyView {
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
    fn view(&self) -> AiUserMcpServerView {
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
    /// Company access policies, keyed by `company_id` (one policy
    /// per company). Empty store + empty user-policy store means the
    /// allowlist has never been switched on: see
    /// `authorize_ai_call`.
    pub company_policies: Arc<DashMap<Uuid, AiCompanyPolicy>>,
    /// User access policies, keyed by `user_id` (one policy per
    /// user).
    pub user_policies: Arc<DashMap<Uuid, AiUserPolicy>>,
    /// Personal API keys, keyed by `user_id` (one key per user).
    /// Ciphertext only; the plaintext is never stored.
    pub personal_keys: Arc<DashMap<Uuid, AiPersonalKey>>,
    /// Personal MCP servers, keyed by server id. Every lookup
    /// filters on `user_id`: a server is never visible to another
    /// user.
    pub personal_mcp_servers: Arc<DashMap<Uuid, AiUserMcpServer>>,
    /// Global AI settings (kill-switch + global feature switches).
    /// Read fresh on every gate evaluation; never cached.
    pub global_settings: Arc<RwLock<AiGlobalSettings>>,
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

// ---------------------------------------------------------------------------
// Access allowlist request bodies.
// ---------------------------------------------------------------------------

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

// ---------------------------------------------------------------------------
// Helpers: caller, permissions, validation.
// ---------------------------------------------------------------------------

/// The caller, from the headers every module in this codebase uses:
/// `X-User-Id` identifies the actor, `X-Role` carries the role slug
/// or id the capability check runs against.
///
/// Public so owning modules can build a `Caller` and hand it to
/// `run_ai_capability` from their own AI endpoints.
#[derive(Debug, Clone)]
pub struct Caller {
    pub actor: String,
    pub role: String,
}

pub fn caller(req: &HttpRequest) -> UnifiedResult<Caller> {
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

/// Public so owning modules can enforce `ai:use` on their own AI
/// endpoints before delegating to `run_ai_capability`.
pub fn require_permission(ctx: &AppContext, role: &str, permission: &str) -> UnifiedResult<()> {
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
    user_id: Option<Uuid>,
    company_id: Option<Uuid>,
    use_case: &str,
    completion: Option<&RawCompletion>,
    latency_ms: u64,
    error_code: Option<u16>,
    used_personal_key: bool,
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
        user_id,
        company_id,
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
        denial_reason: None,
        used_personal_key,
        created_at: Utc::now(),
    };
    state.usage.insert(record.id, record.clone());
    record
}

/// Record a call the allowlist gate denied. There is no provider
/// connection for a denied call, so the record carries the requested
/// provider/model (or the zero UUID / `"unknown"` when none was
/// resolved) and the non-leaking reason code. Token counts stay
/// zero: a denied call never consumed anything.
fn record_denied_usage(
    state: &AiState,
    actor: &str,
    user_id: Option<Uuid>,
    company_id: Option<Uuid>,
    use_case: &str,
    model: &str,
    provider_id: Uuid,
    provider_kind: &str,
    reason: &str,
) -> AiUsageRecord {
    let record = AiUsageRecord {
        id: Uuid::now_v7(),
        provider_id,
        provider_kind: provider_kind.to_string(),
        actor: actor.to_string(),
        user_id,
        company_id,
        use_case: use_case.to_string(),
        model: model.to_string(),
        prompt_tokens: None,
        completion_tokens: None,
        total_tokens: None,
        estimated_cost_usd: None,
        latency_ms: 0,
        status: "error".to_string(),
        error_code: Some(403),
        denial_reason: Some(reason.to_string()),
        used_personal_key: false,
        created_at: Utc::now(),
    };
    state.usage.insert(record.id, record.clone());
    record
}

// ---------------------------------------------------------------------------
// Access allowlists: caller identity, validation, authorization.
//
// Enforcement model
// -----------------
// The allowlist is switched on by creating the first policy. While
// BOTH policy stores are empty, `authorize_ai_call` runs in legacy
// open mode: every caller holding `ai:use` may call, and usage is
// still recorded with whatever user/company ids resolve. This keeps
// existing deployments working until an admin opts in. The moment
// the first company or user policy exists, enforcement is strict
// and default-deny: a company or user without an enabled policy is
// refused. Create the company policy BEFORE the first user policy,
// or the users of that company are locked out immediately.
//
// Company membership is resolved from the `X-Company-Id` header
// (same header-identity trust model as `X-User-Id` / `X-Role`: the
// trusted edge sets these headers) with a fallback to the
// `user.company_requested` hook answered by the users module.
// ---------------------------------------------------------------------------

/// The caller's user id, parsed from the `X-User-Id` header. The
/// actor string is the only identity this module trusts; a body
/// field is never accepted as the user id. `None` when the actor id
/// is not a UUID (legacy / non-user actors).
fn caller_user_id(caller: &Caller) -> Option<Uuid> {
    Uuid::parse_str(&caller.actor).ok()
}

/// The caller's company id: `X-Company-Id` header first, then the
/// `user.company_requested` hook. `None` when neither resolves;
/// callers without a company are platform admins.
///
/// Public so owning modules can resolve the company id to pass to
/// `run_ai_capability` from their own AI endpoints.
pub fn resolve_caller_company(ctx: &AppContext, req: &HttpRequest, caller: &Caller) -> Option<Uuid> {
    if let Some(raw) = req
        .headers()
        .get("x-company-id")
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        if let Ok(id) = Uuid::parse_str(raw) {
            return Some(id);
        }
    }
    let hook = ctx
        .hooks
        .emit(EVENT_USER_COMPANY_REQUESTED, json!({ "user_id": caller.actor }));
    hook.get("company_id")
        .and_then(|value| value.as_str())
        .and_then(|raw| Uuid::parse_str(raw).ok())
}

/// Tenant-first company resolution for AI capability calls
/// (tenant-scoped data access rule). The middleware attaches a
/// verified `TenantContext` (resolved from the Host header) to
/// tenant requests; on a tenant request that context is
/// authoritative for the company id, because it is the tenant's
/// own identity, not a caller-supplied claim. The resolved
/// context is validated through the sanctioned tenancy extractor
/// (`crate::core::tenancy::TenantContext::from_resolved`, which
/// rejects a tenant id that is not a UUID): a present-but-invalid
/// tenant context, or a tenant context with no parseable company
/// id, resolves to `None` (the gate then denies the call when
/// enforcement is on) instead of falling back to the weaker header
/// claim — a broken tenant context must never be overridden by a
/// header. Control-plane requests carry no tenant context, so they
/// fall back to `resolve_caller_company` (header, then hook) — the
/// pre-existing control-plane path. Nothing here falls back to a
/// `public`-schema query: this module's stores are in-memory and
/// no capability path touches Postgres at all (see the tenant
/// scoping notes on the Capabilities Contract).
pub fn resolve_capability_company(
    ctx: &AppContext,
    req: &HttpRequest,
    caller: &Caller,
) -> Option<Uuid> {
    if let Some(resolved) = TenantContext::from_request(req) {
        let tenant = tenancy::TenantContext::from_resolved(&resolved).ok()?;
        return tenant
            .company_id
            .as_deref()
            .and_then(|id| Uuid::parse_str(id).ok());
    }
    resolve_caller_company(ctx, req, caller)
}

/// Company scoping for admin writes: a caller who belongs to a
/// company may only manage policies of that company. A caller with
/// no resolvable company is a platform admin and may manage any
/// company. The capability check (`ai:manage_providers`) happens
/// separately in every handler.
fn require_company_scope(caller_company: Option<Uuid>, target_company: Uuid) -> UnifiedResult<()> {
    match caller_company {
        Some(company) if company != target_company => Err(UnifiedError::forbidden(
            "A company admin may only manage policies of their own company",
        )),
        _ => Ok(()),
    }
}

/// Validate one model name for a policy list: non-empty, bounded,
/// and made only of safe characters. Known provider families are
/// accepted by prefix; anything else must still be a plausible
/// custom-model name (openai-compatible / local endpoints accept
/// arbitrary model ids). The call-time check in
/// `authorize_ai_call` additionally requires fixed-kind providers
/// to use their known families.
fn validate_policy_model_name(model: &str) -> UnifiedResult<String> {
    let model = model.trim();
    if model.is_empty() || model.len() > MAX_MODEL_NAME_LEN {
        return Err(UnifiedError::bad_request(format!(
            "Model names must be between 1 and {MAX_MODEL_NAME_LEN} characters"
        )));
    }
    if !model
        .chars()
        .all(|character| character.is_ascii_alphanumeric() || "-._/:".contains(character))
    {
        return Err(UnifiedError::bad_request(format!(
            "Model name \"{model}\" contains characters outside [A-Za-z0-9._/:]"
        )));
    }
    Ok(model.to_string())
}

/// Validate a policy model list: bounded, de-duplicated, every entry
/// a valid model name. `None` (field absent) means unrestricted.
fn validate_policy_models(models: &Option<Vec<String>>) -> UnifiedResult<Option<Vec<String>>> {
    let Some(models) = models else {
        return Ok(None);
    };
    if models.len() > MAX_POLICY_LIST_LEN {
        return Err(UnifiedError::bad_request(format!(
            "allowed_models must contain at most {MAX_POLICY_LIST_LEN} entries"
        )));
    }
    let mut cleaned: Vec<String> = Vec::with_capacity(models.len());
    for model in models {
        let model = validate_policy_model_name(model)?;
        if !cleaned.contains(&model) {
            cleaned.push(model);
        }
    }
    Ok(Some(cleaned))
}

/// Validate a policy feature list: every entry must be a known use
/// case, or the use case of a capability registered through the AI
/// Capabilities Contract (`register_ai_capabilities`). Capability
/// use cases are module-scoped (for example
/// `exams.draft_questions`) and are declared per-module in the
/// registry, never in a hard-coded list here. `None` (field absent)
/// means unrestricted.
fn validate_policy_features(features: &Option<Vec<String>>) -> UnifiedResult<Option<Vec<String>>> {
    let Some(features) = features else {
        return Ok(None);
    };
    if features.len() > MAX_POLICY_LIST_LEN {
        return Err(UnifiedError::bad_request(format!(
            "allowed_features must contain at most {MAX_POLICY_LIST_LEN} entries"
        )));
    }
    let mut cleaned: Vec<String> = Vec::with_capacity(features.len());
    for feature in features {
        let feature = feature.trim();
        if !KNOWN_USE_CASES.contains(&feature)
            && !ai_capability_registry().has_use_case(feature)
        {
            return Err(UnifiedError::bad_request(format!(
                "Unknown AI feature \"{feature}\": expected one of {} or the use case of a registered AI capability",
                KNOWN_USE_CASES.join(", ")
            )));
        }
        if !cleaned.iter().any(|item| item == feature) {
            cleaned.push(feature.to_string());
        }
    }
    Ok(Some(cleaned))
}

/// Validate a monthly token cap: bounded. `None` means no cap.
fn validate_token_cap(cap: Option<u64>) -> UnifiedResult<Option<u64>> {
    match cap {
        Some(cap) if cap > MAX_MONTHLY_TOKEN_CAP => Err(UnifiedError::bad_request(format!(
            "monthly_token_cap must be at most {MAX_MONTHLY_TOKEN_CAP}"
        ))),
        cap => Ok(cap),
    }
}

/// Validate a monthly cost cap (USD): finite, non-negative, bounded.
/// `None` means no cap.
fn validate_cost_cap(cap: Option<f64>) -> UnifiedResult<Option<f64>> {
    match cap {
        Some(cap) if !cap.is_finite() || cap < 0.0 || cap > MAX_MONTHLY_COST_CAP_USD => {
            Err(UnifiedError::bad_request(format!(
                "monthly_cost_cap_usd must be between 0 and {MAX_MONTHLY_COST_CAP_USD}"
            )))
        }
        cap => Ok(cap),
    }
}

/// Validate a caller-supplied use case against the known list.
fn validate_use_case(use_case: &str) -> UnifiedResult<String> {
    let use_case = use_case.trim();
    if !KNOWN_USE_CASES.contains(&use_case) {
        return Err(UnifiedError::bad_request(format!(
            "Unknown AI use case \"{use_case}\": expected one of {}",
            KNOWN_USE_CASES.join(", ")
        )));
    }
    Ok(use_case.to_string())
}

/// True when the model belongs to the provider kind's known family.
/// Fixed-kind providers only: compatible and local endpoints accept
/// any model id. Fine-tuned OpenAI models (`ft:...`) are accepted.
fn is_known_model(kind: AiProviderKind, model: &str) -> bool {
    match kind {
        AiProviderKind::Anthropic => model.starts_with("claude-"),
        AiProviderKind::OpenAi => {
            model.starts_with("gpt-")
                || model.starts_with("o1")
                || model.starts_with("o3")
                || model.starts_with("chatgpt-")
                || model.starts_with("ft:")
        }
        AiProviderKind::OpenAiCompatible | AiProviderKind::Local => !model.trim().is_empty(),
    }
}

/// Rough token estimate for a prompt: ~4 characters per token.
/// Used only for pre-call cap checks; actual billing uses the
/// provider's reported counts.
fn estimate_prompt_tokens(text: &str) -> u32 {
    (text.chars().count() as u32).div_ceil(4)
}

/// Start of the current UTC calendar month: the window monthly caps
/// are measured over.
fn month_start() -> DateTime<Utc> {
    let now = Utc::now();
    Utc.with_ymd_and_hms(now.year(), now.month(), 1, 0, 0, 0)
        .single()
        .unwrap_or(now)
}

/// Tokens and estimated cost consumed in the current month by one
/// user / one company / one user's personal key. Only successful
/// and failed provider calls count; denied calls carry zero tokens.
#[derive(Debug, Clone, Copy, Default)]
struct MonthlyUsage {
    tokens: u64,
    cost_usd: f64,
}

fn monthly_usage_for_user(state: &AiState, user_id: Uuid, since: DateTime<Utc>) -> MonthlyUsage {
    sum_monthly_usage(
        state
            .usage
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|record| record.user_id == Some(user_id) && record.created_at >= since),
    )
}

fn monthly_usage_for_company(
    state: &AiState,
    company_id: Uuid,
    since: DateTime<Utc>,
) -> MonthlyUsage {
    sum_monthly_usage(
        state
            .usage
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|record| record.company_id == Some(company_id) && record.created_at >= since),
    )
}

fn monthly_personal_key_usage(
    state: &AiState,
    user_id: Uuid,
    since: DateTime<Utc>,
) -> MonthlyUsage {
    sum_monthly_usage(
        state
            .usage
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|record| {
                record.user_id == Some(user_id)
                    && record.used_personal_key
                    && record.created_at >= since
            }),
    )
}

fn sum_monthly_usage(records: impl Iterator<Item = AiUsageRecord>) -> MonthlyUsage {
    let mut usage = MonthlyUsage::default();
    for record in records {
        usage.tokens += record.total_tokens.unwrap_or(0) as u64;
        usage.cost_usd += record.estimated_cost_usd.unwrap_or(0.0);
    }
    usage
}

/// The effective monthly token cap for a user: the user's own cap
/// when it is set and greater than zero, otherwise the company cap.
/// `None` means no cap at all.
fn effective_user_cap(
    user_cap: Option<u64>,
    company_cap: Option<u64>,
) -> Option<u64> {
    match user_cap {
        Some(cap) if cap > 0 => Some(cap),
        _ => company_cap,
    }
}

/// The effective monthly cost cap for a user: same inheritance rule
/// as `effective_user_cap`.
fn effective_user_cost_cap(
    user_cap: Option<f64>,
    company_cap: Option<f64>,
) -> Option<f64> {
    match user_cap {
        Some(cap) if cap > 0.0 => Some(cap),
        _ => company_cap,
    }
}

/// What a call needs the allowlist gate to decide.
pub struct AiCallRequest<'a> {
    pub use_case: &'a str,
    pub model: &'a str,
    pub provider_id: Uuid,
    pub provider_kind: &'a str,
    /// Prompt estimate + requested completion budget.
    pub estimated_tokens: u32,
    pub use_personal_key: bool,
    /// Purchased-product context for the product gate layer
    /// (`X-Product-Id` header, re-resolved from the request on
    /// every call). `None` means no product context is present:
    /// the product layer defaults to allow and the gate decision
    /// records that. Never cached across calls.
    pub product_id: Option<Uuid>,
}

/// The outcome of a successful authorization, carried into
/// `run_completion` so the call runs with the right key and the
/// usage record carries the right identity.
pub struct AiAuthorization {
    pub user_id: Option<Uuid>,
    pub company_id: Option<Uuid>,
    pub use_personal_key: bool,
    /// The caller's decrypted personal key, present only when the
    /// call runs on it. Lives only for the duration of the call.
    pub personal_key: Option<AuthorizedPersonalKey>,
}

pub struct AuthorizedPersonalKey {
    pub key_id: Uuid,
    pub provider_kind: AiProviderKind,
    pub decrypted_key: String,
}

/// Map a personal-key load failure to a denial: a 403 carries its
/// reason code into the denial recorder (so the attempt is recorded
/// and audited like every other denial); anything else — a stored
/// key that cannot be decrypted — propagates unchanged.
fn map_personal_key_denial(
    state: &AiState,
    deny: &dyn Fn(&AiState, &str) -> UnifiedError,
    error: UnifiedError,
) -> UnifiedError {
    if error.code != 403 {
        return error;
    }
    let reason = error
        .metadata
        .get("reason")
        .and_then(|value| value.as_str())
        .unwrap_or("ai_personal_key_not_configured")
        .to_string();
    deny(state, &reason)
}

/// The single allowlist gate. EVERY completion / draft entry point
/// and the connection test call this FIRST, before any provider
/// traffic. Checks run in this order; the first failure denies the
/// call, records a denied usage row, and emits `ai.access.denied`:
///   1. company allowed (policy exists and is enabled),
///   2. user allowed (policy exists and is enabled),
///   3. feature allowed for this use case,
///   4. model allowed (known family for the kind, then policy lists),
///   5. company monthly token / cost cap,
///   6. user monthly token / cost cap (or the inherited company cap),
///   7. personal-key quota when a personal key is used.
///
/// Before any of the steps above, the layered gate runs first
/// (`evaluate_ai_gate`): the global kill-switch, the purchased
/// product layer, the company and user on/off switches, and the
/// per-feature toggles. The allowlist answers "is this caller
/// allowed at all"; the gate answers "is AI on for this caller /
/// feature right now". Both must allow.
///
/// Denials are 403s with a non-leaking reason code in the `reason`
/// metadata field. The message and the audit event never reveal
/// another user's or the company's limits.
fn authorize_ai_call(
    ctx: &AppContext,
    state: &AiState,
    caller: &Caller,
    company_id: Option<Uuid>,
    request: &AiCallRequest<'_>,
) -> UnifiedResult<AiAuthorization> {
    let user_id = caller_user_id(caller);
    let enforcement_on =
        !state.company_policies.is_empty() || !state.user_policies.is_empty();
    // Deny helper: record the denial, emit the audit event, and
    // return the non-leaking 403. Defined once so every check below
    // denies exactly the same way.
    let deny = |state: &AiState, reason: &str| -> UnifiedError {
        let denied = record_denied_usage(
            state,
            &caller.actor,
            user_id,
            company_id,
            request.use_case,
            request.model,
            request.provider_id,
            request.provider_kind,
            reason,
        );
        emit_audit(
            ctx,
            EVENT_AI_ACCESS_DENIED,
            &caller.actor,
            "ai-access",
            json!({
                "id": denied.id.to_string(),
                "reason": reason,
                "use_case": request.use_case,
                "user_id": user_id.map(|id| id.to_string()),
                "company_id": company_id.map(|id| id.to_string()),
            }),
        );
        UnifiedError::forbidden("AI access is not allowed for this caller")
            .with_metadata("reason", Value::String(reason.to_string()))
            .with_metadata("use_case", Value::String(request.use_case.to_string()))
    };

    // Layered gating, evaluated FIRST on every call and re-checked
    // on every call (the decision is never cached): the global
    // kill-switch, the purchased-product layer, the company and
    // user on/off switches, and the per-feature toggles
    // (`evaluate_ai_gate`). The allowlist checks below (steps 1-7)
    // still run afterwards: they enforce caps, models, and
    // personal-key quotas, which the gate does not. A gate denial
    // records a denied usage row and emits `ai.access.denied`
    // through the same `deny` helper as every other denial, so a
    // denied read is logged with the reason code and no prompt
    // content.
    let gate = evaluate_ai_gate(
        ctx,
        state,
        user_id,
        company_id,
        request.use_case,
        request.product_id,
    );
    if !gate.allowed {
        return Err(deny(
            state,
            gate.reason_code.as_deref().unwrap_or("ai_not_allowed"),
        ));
    }

    // Legacy open mode: no policy exists anywhere, so there is
    // nothing to enforce against. Personal-key calls still need a
    // key, and the key still has to match the connection.
    if !enforcement_on {
        let personal_key = if request.use_personal_key {
            let key = load_personal_key_for_call(state, ctx, user_id, request).map_err(
                |error| map_personal_key_denial(state, &deny, error),
            )?;
            Some(key)
        } else {
            None
        };
        return Ok(AiAuthorization {
            user_id,
            company_id,
            use_personal_key: request.use_personal_key,
            personal_key,
        });
    }

    // 1. Company allowed.
    let company_policy = match company_id {
        Some(company_id) => match state.company_policies.get(&company_id) {
            Some(policy) if policy.enabled => policy.value().clone(),
            Some(_) => return Err(deny(state, "ai_not_allowed_for_company")),
            None => return Err(deny(state, "ai_not_allowed_for_company")),
        },
        None => return Err(deny(state, "ai_company_unknown")),
    };
    // 2. User allowed.
    let user_policy = match user_id {
        Some(user_id) => match state.user_policies.get(&user_id) {
            Some(policy) if policy.enabled => policy.value().clone(),
            Some(_) => return Err(deny(state, "ai_not_allowed_for_user")),
            None => return Err(deny(state, "ai_not_allowed_for_user")),
        },
        None => return Err(deny(state, "ai_user_unknown")),
    };
    // 3. Feature allowed for this use case.
    for allowed in [&company_policy.allowed_features, &user_policy.allowed_features] {
        if let Some(features) = allowed {
            if !features.iter().any(|feature| feature == request.use_case) {
                return Err(deny(state, "ai_feature_not_allowed"));
            }
        }
    }
    // 4. Model allowed: known family for the kind, then policy lists.
    let kind = AiProviderKind::from_str(request.provider_kind).unwrap_or(AiProviderKind::Local);
    if !is_known_model(kind, request.model) {
        return Err(deny(state, "ai_model_not_allowed"));
    }
    for allowed in [&company_policy.allowed_models, &user_policy.allowed_models] {
        if let Some(models) = allowed {
            if !models.iter().any(|model| model == request.model) {
                return Err(deny(state, "ai_model_not_allowed"));
            }
        }
    }
    // 5. Company monthly caps.
    let since = month_start();
    let company_usage = monthly_usage_for_company(state, company_policy.company_id, since);
    if let Some(cap) = company_policy.monthly_token_cap {
        if company_usage.tokens + request.estimated_tokens as u64 > cap {
            return Err(deny(state, "ai_company_token_cap_exceeded"));
        }
    }
    if let Some(cap) = company_policy.monthly_cost_cap_usd {
        let estimated = estimate_cost_usd(request.model, request.estimated_tokens, 0);
        if let Some(estimated) = estimated {
            if company_usage.cost_usd + estimated > cap {
                return Err(deny(state, "ai_company_cost_cap_exceeded"));
            }
        }
    }
    // 6. User monthly caps (own cap, or the inherited company cap).
    let user_id = user_policy.user_id;
    let user_usage = monthly_usage_for_user(state, user_id, since);
    if let Some(cap) = effective_user_cap(
        user_policy.monthly_token_cap,
        company_policy.monthly_token_cap,
    ) {
        if user_usage.tokens + request.estimated_tokens as u64 > cap {
            return Err(deny(state, "ai_user_token_cap_exceeded"));
        }
    }
    if let Some(cap) = effective_user_cost_cap(
        user_policy.monthly_cost_cap_usd,
        company_policy.monthly_cost_cap_usd,
    ) {
        let estimated = estimate_cost_usd(request.model, request.estimated_tokens, 0);
        if let Some(estimated) = estimated {
            if user_usage.cost_usd + estimated > cap {
                return Err(deny(state, "ai_user_cost_cap_exceeded"));
            }
        }
    }
    // 7. Personal key: existence, kind/model match, own quota.
    let personal_key = if request.use_personal_key {
        let key = load_personal_key_for_call(state, ctx, Some(user_id), request)
            .map_err(|error| map_personal_key_denial(state, &deny, error))?;
        // The key's own quota, when set; otherwise the effective
        // user cap already checked in step 6 covers it.
        if let Some(cap) = key_quota(&state, user_id) {
            let key_usage = monthly_personal_key_usage(state, user_id, since);
            if key_usage.tokens + request.estimated_tokens as u64 > cap {
                return Err(deny(state, "ai_personal_key_quota_exceeded"));
            }
        }
        Some(key)
    } else {
        None
    };
    Ok(AiAuthorization {
        user_id: Some(user_id),
        company_id: Some(company_policy.company_id),
        use_personal_key: request.use_personal_key,
        personal_key,
    })
}

/// Load and decrypt the caller's personal key for a call, checking
/// it exists, is enabled, and matches the connection kind (and the
/// key's model when one is set). Failures are 403s whose metadata
/// carries the non-leaking reason code; `authorize_ai_call` maps
/// them through the denial recorder so the attempt is audited.
/// A decryption failure is an internal error, not a denial, and
/// propagates unchanged.
fn load_personal_key_for_call(
    state: &AiState,
    ctx: &AppContext,
    user_id: Option<Uuid>,
    request: &AiCallRequest<'_>,
) -> UnifiedResult<AuthorizedPersonalKey> {
    let user_id = user_id.ok_or_else(|| {
        UnifiedError::forbidden("AI access is not allowed for this caller")
            .with_metadata("reason", Value::String("ai_user_unknown".to_string()))
    })?;
    let key = state.personal_keys.get(&user_id).ok_or_else(|| {
        UnifiedError::forbidden("AI access is not allowed for this caller").with_metadata(
            "reason",
            Value::String("ai_personal_key_not_configured".to_string()),
        )
    })?;
    let key = key.value().clone();
    if !key.enabled {
        return Err(UnifiedError::forbidden("AI access is not allowed for this caller")
            .with_metadata(
                "reason",
                Value::String("ai_personal_key_not_configured".to_string()),
            ));
    }
    if key.provider_kind.as_str() != request.provider_kind {
        return Err(UnifiedError::forbidden("AI access is not allowed for this caller")
            .with_metadata(
                "reason",
                Value::String("ai_personal_key_mismatch".to_string()),
            ));
    }
    if let Some(model) = &key.model {
        if model != request.model {
            return Err(UnifiedError::forbidden("AI access is not allowed for this caller")
                .with_metadata(
                    "reason",
                    Value::String("ai_personal_key_mismatch".to_string()),
                ));
        }
    }
    let secret = encryption_secret(ctx);
    let decrypted = decrypt_api_key(&secret, &key.api_key_encrypted)?;
    Ok(AuthorizedPersonalKey {
        key_id: key.id,
        provider_kind: key.provider_kind,
        decrypted_key: decrypted,
    })
}

/// The personal key's own monthly token cap, when one is set and
/// greater than zero. `None` means "inherit the effective user cap".
fn key_quota(state: &AiState, user_id: Uuid) -> Option<u64> {
    state
        .personal_keys
        .get(&user_id)
        .and_then(|entry| entry.monthly_token_cap)
        .filter(|cap| *cap > 0)
}

// ---------------------------------------------------------------------------
// Personal MCP endpoint validation (SSRF guard).
// ---------------------------------------------------------------------------

/// True when the address must never be an MCP target: private,
/// link-local, unspecified, broadcast, multicast, documentation,
/// benchmarking, or reserved ranges (v4), unique-local / link-local
/// / multicast / unspecified (v6). Mirrors the webhook module's
/// `is_forbidden_ip`, with loopback included: personal MCP servers
/// are remote services, so loopback is never a valid target.
fn is_forbidden_mcp_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(addr) => {
            addr.is_loopback()
                || addr.is_private()
                || addr.is_link_local()
                || addr.is_unspecified()
                || addr.is_broadcast()
                || addr.is_multicast()
                || addr.is_documentation()
                || addr.is_benchmarking()
                || addr.is_reserved()
        }
        IpAddr::V6(addr) => {
            addr.is_loopback()
                || addr.is_unspecified()
                || addr.is_multicast()
                || addr.is_unique_local()
                || (addr.segments()[0] & 0xfe00) == 0xfc00 // unique-local fc00::/7
                || (addr.segments()[0] & 0xffc0) == 0xfe80 // link-local fe80::/10
        }
    }
}

/// Validate a personal MCP endpoint URL at write time: https only,
/// no embedded credentials, no localhost / internal hostname, and
/// the host must resolve to at least one public address. A hostname
/// that does not resolve is refused: an unresolvable endpoint can
/// never serve context. Residual risk: DNS rebinding between
/// validation and use. This module never fetches an MCP endpoint
/// (servers are offered as grounding references only), so there is
/// no request-time fetch to rebind against; re-check the host
/// before any future live MCP fetch is added.
fn validate_mcp_endpoint_url(raw: &str) -> UnifiedResult<Url> {
    let raw = raw.trim();
    if raw.is_empty() || raw.len() > MAX_MCP_URL_LEN {
        return Err(UnifiedError::bad_request(format!(
            "endpoint_url must be between 1 and {MAX_MCP_URL_LEN} characters"
        )));
    }
    let url = Url::parse(raw)
        .map_err(|_| UnifiedError::bad_request(format!("Invalid MCP endpoint URL \"{raw}\"")))?;
    if url.scheme() != "https" {
        return Err(UnifiedError::bad_request(
            "MCP endpoint URL must use https",
        ));
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(UnifiedError::bad_request(
            "MCP endpoint URL must not embed credentials; use the auth token field",
        ));
    }
    let host = url.host_str().ok_or_else(|| {
        UnifiedError::bad_request("MCP endpoint URL must include a host")
    })?;
    let lower = host.to_ascii_lowercase();
    if lower == "localhost"
        || lower.ends_with(".localhost")
        || lower.ends_with(".local")
        || lower.ends_with(".internal")
        || lower.ends_with(".lan")
        || lower.ends_with(".home")
        || lower.ends_with(".corp")
        || lower.ends_with(".intranet")
    {
        return Err(UnifiedError::bad_request(
            "MCP endpoint host must be a public host, not a local or internal name",
        ));
    }
    if let Ok(ip) = host.parse::<IpAddr>() {
        if is_forbidden_mcp_ip(ip) {
            return Err(UnifiedError::bad_request(
                "MCP endpoint host must be a public IP address, not a private, loopback, or link-local address",
            ));
        }
        return Ok(url);
    }
    // Hostname: it must resolve, and every resolved address must be
    // public. A name that resolves only to private addresses (or
    // that mixes public and private ones) is refused outright.
    let port = url.port_or_known_default().unwrap_or(443);
    let addresses = (host, port)
        .to_socket_addrs()
        .map_err(|_| UnifiedError::bad_request(format!("MCP endpoint host \"{host}\" does not resolve")))?;
    let mut any = false;
    for socket in addresses {
        any = true;
        if is_forbidden_mcp_ip(socket.ip()) {
            return Err(UnifiedError::bad_request(format!(
                "MCP endpoint host \"{host}\" resolves to a private, loopback, or link-local address"
            )));
        }
    }
    if !any {
        return Err(UnifiedError::bad_request(format!(
            "MCP endpoint host \"{host}\" does not resolve"
        )));
    }
    Ok(url)
}

/// The caller's enabled personal MCP servers as grounding sources
/// for the shared context: each server contributes its name and
/// endpoint URL through the existing `SharedContext` grounding
/// assembly (`assemble_shared_context` /
/// `shared_context_prompt`), so the prompt carries the reference
/// without duplicating context code. The servers are references
/// only: this module never fetches an MCP endpoint at call time.
/// A server is never offered to another user.
fn personal_mcp_grounding_sources(
    state: &AiState,
    user_id: Option<Uuid>,
) -> Vec<GroundingSource> {
    let Some(user_id) = user_id else {
        return Vec::new();
    };
    let mut servers: Vec<AiUserMcpServer> = state
        .personal_mcp_servers
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|server| server.user_id == user_id && server.enabled)
        .collect();
    servers.sort_by(|a, b| a.name.cmp(&b.name).then(a.created_at.cmp(&b.created_at)));
    servers
        .into_iter()
        .map(|server| GroundingSource {
            article_id: format!("mcp:{}", server.id),
            title: format!("Personal MCP server: {}", server.name),
            url: server.endpoint_url.clone(),
            score: 0.0,
            excerpt: Some("Personal MCP context source configured by the caller".to_string()),
        })
        .collect()
}

/// In strict mode, a personal key / MCP change requires the caller
/// to be an allowed user (enabled user policy). In legacy open mode
/// any caller holding `ai:use` may configure their own.
fn require_allowed_user(state: &AiState, user_id: Uuid) -> UnifiedResult<()> {
    let enforcement_on =
        !state.company_policies.is_empty() || !state.user_policies.is_empty();
    if !enforcement_on {
        return Ok(());
    }
    match state.user_policies.get(&user_id) {
        Some(policy) if policy.enabled => Ok(()),
        _ => Err(UnifiedError::forbidden("AI access is not allowed for this caller")
            .with_metadata("reason", Value::String("ai_not_allowed_for_user".to_string()))),
    }
}

// ---------------------------------------------------------------------------
// Layered AI gating.
//
// Effective access is the AND of four layers; any layer can disable
// AI, and every layer must allow for a call to run:
//
//   (a) SETTINGS - the global kill-switch (`ai.global_enabled`)
//       plus the global per-feature switches
//       (`ai.features.<name>.enabled`). The kill-switch off denies
//       everything immediately, regardless of the other layers.
//   (b) PRODUCT - the purchased product / subscription includes AI
//       or not, resolved through the `ai.product_includes_ai` hook
//       query (see `EVENT_AI_PRODUCT_INCLUDES_AI`). No product
//       context on the call means the product layer defaults to
//       allow and the decision records that; a product context the
//       hook cannot confirm denies the call (fail closed).
//   (c) COMPANY - the company policy's allowlist switch (`enabled`)
//       AND its gate switch (`ai_enabled`), plus the company
//       per-feature off switches (`disabled_features`).
//   (d) USER - the user policy's allowlist switch (`enabled`) AND
//       its gate switch (`ai_enabled`), plus the user per-feature
//       off switches (`disabled_features`). A disabled user is
//       denied for every feature; a switched-off feature denies
//       only that feature.
//
// The allowlist answers "is this caller allowed at all" (identity,
// model lists, caps, personal-key quota); the gate answers "is AI
// on for this caller / feature right now". ONE function composes
// them: `evaluate_ai_gate`, called FIRST inside
// `authorize_ai_call` on every completion / draft entry point, so
// the combined evaluation order is:
//
//   settings kill-switch -> global feature switch -> product ->
//   company allowlist + gate -> user allowlist + gate ->
//   feature toggles (allowlist lists, then per-layer off switches)
//   -> (back in `authorize_ai_call`) model, caps, personal key.
//
// The decision is computed fresh from the live stores on every
// call: no gate decision is cached across calls. A denied call is
// logged by the caller's `deny` helper in `authorize_ai_call`
// (denied usage row + `ai.access.denied` event, count + reason
// code, never prompt content). Every change to any layer emits
// `ai.gating.changed` with the old -> new values through
// `emit_gating_change`, on top of the policy-change audit the
// policy endpoints already emit.
// ---------------------------------------------------------------------------

/// Canonical feature (use-case) names the gate understands, plus
/// the friendly aliases admins may use in gating requests.
/// `support` / `drafting` mean ticket-reply drafting
/// (`support_reply`); `insights` means insight generation, which
/// runs through the generic completion use case (`complete`), so
/// switching `insights` off switches `complete` off as well until
/// a dedicated insights use case exists.
fn canonical_gate_feature(raw: &str) -> Option<&'static str> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "complete" | "insights" | "insight" => Some("complete"),
        "support_reply" | "support" | "drafting" | "support_drafting" => {
            Some("support_reply")
        }
        "chat_reply" | "chat" => Some("chat_reply"),
        "content" => Some("content"),
        "email" => Some("email"),
        "kb_answer" | "kb" | "knowledgebase" => Some("kb_answer"),
        "test" => Some("test"),
        _ => None,
    }
}

/// Validate one feature name for a gating request, canonicalizing
/// friendly aliases to the use-case name the gate checks.
fn validate_gate_feature_name(raw: &str) -> UnifiedResult<String> {
    canonical_gate_feature(raw).map(str::to_string).ok_or_else(|| {
        UnifiedError::bad_request(format!(
            "Unknown AI feature \"{raw}\": expected one of {}",
            KNOWN_USE_CASES.join(", ")
        ))
    })
}

/// Validate a per-feature off-switch list: canonicalize every
/// entry, reject unknown names, drop duplicates. `None` means no
/// feature is switched off here.
fn validate_gate_features(
    features: &Option<Vec<String>>,
) -> UnifiedResult<Option<Vec<String>>> {
    let Some(features) = features else {
        return Ok(None);
    };
    if features.len() > MAX_POLICY_LIST_LEN {
        return Err(UnifiedError::bad_request(format!(
            "disabled_features must contain at most {MAX_POLICY_LIST_LEN} entries"
        )));
    }
    let mut cleaned: Vec<String> = Vec::with_capacity(features.len());
    for feature in features {
        let canonical = validate_gate_feature_name(feature)?;
        if !cleaned.iter().any(|item| item == &canonical) {
            cleaned.push(canonical);
        }
    }
    Ok(Some(cleaned))
}

/// The caller's purchased-product context: the `X-Product-Id`
/// header, re-read from the request on every call. `None` means no
/// product context is present and the product layer defaults to
/// allow.
fn resolve_caller_product(req: &HttpRequest) -> Option<Uuid> {
    req.headers()
        .get("x-product-id")
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .and_then(|raw| Uuid::parse_str(raw).ok())
}

/// One user's company id through the `user.company_requested`
/// hook, for the effective-gate read of a user other than the
/// caller. `None` when the users module does not answer.
fn resolve_user_company(ctx: &AppContext, user_id: Uuid) -> Option<Uuid> {
    let hook = ctx
        .hooks
        .emit(EVENT_USER_COMPANY_REQUESTED, json!({ "user_id": user_id.to_string() }));
    hook.get("company_id")
        .and_then(|value| value.as_str())
        .and_then(|raw| Uuid::parse_str(raw).ok())
}

/// Ask the products / billing module whether the purchased product
/// includes AI (`ai.product_includes_ai` hook query; contract on
/// `EVENT_AI_PRODUCT_INCLUDES_AI`). `Some(true)` = the product
/// includes AI; `Some(false)` = it excludes AI; `None` = no handler
/// answered, so the caller cannot be confirmed.
fn product_includes_ai(
    ctx: &AppContext,
    product_id: Uuid,
    user_id: Option<Uuid>,
    company_id: Option<Uuid>,
) -> Option<bool> {
    let hook = ctx.hooks.emit(
        EVENT_AI_PRODUCT_INCLUDES_AI,
        json!({
            "product_id": product_id.to_string(),
            "user_id": user_id.map(|id| id.to_string()),
            "company_id": company_id.map(|id| id.to_string()),
        }),
    );
    hook.get("includes_ai").and_then(|value| value.as_bool())
}

fn gate_layer_outcome(layer: &str, allowed: bool, note: &str) -> AiGateLayerOutcome {
    AiGateLayerOutcome {
        layer: layer.to_string(),
        allowed,
        note: Some(note.to_string()),
    }
}

fn gate_denied(
    mut layers: Vec<AiGateLayerOutcome>,
    layer: &str,
    feature: &str,
    reason: &str,
    note: &str,
) -> AiGateDecision {
    layers.push(gate_layer_outcome(layer, false, note));
    AiGateDecision {
        allowed: false,
        denied_layer: Some(layer.to_string()),
        denied_feature: Some(feature.to_string()),
        reason_code: Some(reason.to_string()),
        layers,
    }
}

/// The single composed gate: the allowlist ("is this caller
/// allowed at all") AND the layered on/off switches ("is AI on for
/// this caller / feature right now"), evaluated in this exact
/// order on every call, with no caching:
///
///   1. settings kill-switch (`ai.global_enabled`),
///   2. global per-feature switch for this feature,
///   3. product layer (`ai.product_includes_ai` hook),
///   4. company allowlist (`enabled`) + gate (`ai_enabled`),
///   5. user allowlist (`enabled`) + gate (`ai_enabled`),
///   6. feature toggles: allowlist `allowed_features` lists, then
///      the company and user `disabled_features` off switches.
///
/// A missing company or user policy denies only while the
/// allowlist is switched on (at least one policy exists anywhere);
/// in legacy open mode (no policy exists) the company / user
/// layers allow, because no admin has switched anything off, while
/// the settings and product layers still apply. The caller maps a
/// denial to the non-leaking 403 through its own `deny` helper.
fn evaluate_ai_gate(
    ctx: &AppContext,
    state: &AiState,
    user_id: Option<Uuid>,
    company_id: Option<Uuid>,
    feature: &str,
    product_id: Option<Uuid>,
) -> AiGateDecision {
    let mut layers: Vec<AiGateLayerOutcome> = Vec::new();

    // 1-2. Settings: kill-switch, then the global feature switch.
    let global = state
        .global_settings
        .read()
        .expect("ai global settings lock poisoned")
        .clone();
    if !global.global_enabled {
        return gate_denied(
            layers,
            "settings",
            feature,
            "ai_globally_disabled",
            "Global AI kill-switch is off",
        );
    }
    layers.push(gate_layer_outcome(
        "settings",
        true,
        "Global AI kill-switch is on",
    ));
    if global.disabled_features.iter().any(|name| name == feature) {
        return gate_denied(
            layers,
            "settings",
            feature,
            "ai_feature_disabled",
            "Feature is switched off globally",
        );
    }

    // 3. Product: only when a product context is present.
    match product_id {
        Some(product_id) => match product_includes_ai(ctx, product_id, user_id, company_id) {
            Some(true) => layers.push(gate_layer_outcome(
                "product",
                true,
                "Purchased product includes AI",
            )),
            Some(false) => {
                return gate_denied(
                    layers,
                    "product",
                    feature,
                    "ai_product_excludes_ai",
                    "Purchased product does not include AI",
                );
            }
            None => {
                return gate_denied(
                    layers,
                    "product",
                    feature,
                    "ai_product_unknown",
                    "Purchased product could not be confirmed",
                );
            }
        },
        None => layers.push(gate_layer_outcome(
            "product",
            true,
            "No product context: product layer defaults to allow",
        )),
    }

    // 4-5. Company and user: allowlist switch + gate switch. In
    // legacy open mode there is no policy to switch anything off.
    let enforcement_on =
        !state.company_policies.is_empty() || !state.user_policies.is_empty();
    let company_policy: Option<AiCompanyPolicy> = if enforcement_on {
        match company_id {
            Some(company_id) => match state.company_policies.get(&company_id) {
                Some(entry) => {
                    let policy = entry.value().clone();
                    if !policy.enabled {
                        return gate_denied(
                            layers,
                            "company",
                            feature,
                            "ai_not_allowed_for_company",
                            "Company is not on the AI allowlist",
                        );
                    }
                    if !policy.ai_enabled {
                        return gate_denied(
                            layers,
                            "company",
                            feature,
                            "ai_disabled_for_company",
                            "AI is switched off for the company",
                        );
                    }
                    layers.push(gate_layer_outcome(
                        "company",
                        true,
                        "Company allowlist and gate allow AI",
                    ));
                    Some(policy)
                }
                None => {
                    return gate_denied(
                        layers,
                        "company",
                        feature,
                        "ai_not_allowed_for_company",
                        "Company is not on the AI allowlist",
                    );
                }
            },
            None => {
                return gate_denied(
                    layers,
                    "company",
                    feature,
                    "ai_company_unknown",
                    "Caller has no resolvable company",
                );
            }
        }
    } else {
        layers.push(gate_layer_outcome(
            "company",
            true,
            "Allowlist is off: no company policy switches AI off",
        ));
        None
    };
    let user_policy: Option<AiUserPolicy> = if enforcement_on {
        match user_id {
            Some(user_id) => match state.user_policies.get(&user_id) {
                Some(entry) => {
                    let policy = entry.value().clone();
                    if !policy.enabled {
                        return gate_denied(
                            layers,
                            "user",
                            feature,
                            "ai_not_allowed_for_user",
                            "User is not on the AI allowlist",
                        );
                    }
                    if !policy.ai_enabled {
                        return gate_denied(
                            layers,
                            "user",
                            feature,
                            "ai_disabled_for_user",
                            "AI is switched off for the user",
                        );
                    }
                    layers.push(gate_layer_outcome(
                        "user",
                        true,
                        "User allowlist and gate allow AI",
                    ));
                    Some(policy)
                }
                None => {
                    return gate_denied(
                        layers,
                        "user",
                        feature,
                        "ai_not_allowed_for_user",
                        "User is not on the AI allowlist",
                    );
                }
            },
            None => {
                return gate_denied(
                    layers,
                    "user",
                    feature,
                    "ai_user_unknown",
                    "Caller has no user id",
                );
            }
        }
    } else {
        layers.push(gate_layer_outcome(
            "user",
            true,
            "Allowlist is off: no user policy switches AI off",
        ));
        None
    };

    // 6. Feature toggles: allowlist lists first, then the explicit
    // per-layer off switches. A switched-off feature denies only
    // that feature.
    if let Some(policy) = &company_policy {
        if let Some(features) = &policy.allowed_features {
            if !features.iter().any(|name| name == feature) {
                return gate_denied(
                    layers,
                    "feature",
                    feature,
                    "ai_feature_not_allowed",
                    "Feature is not on the company allowlist",
                );
            }
        }
        if let Some(disabled) = &policy.disabled_features {
            if disabled.iter().any(|name| name == feature) {
                return gate_denied(
                    layers,
                    "company",
                    feature,
                    "ai_feature_disabled_for_company",
                    "Feature is switched off for the company",
                );
            }
        }
    }
    if let Some(policy) = &user_policy {
        if let Some(features) = &policy.allowed_features {
            if !features.iter().any(|name| name == feature) {
                return gate_denied(
                    layers,
                    "feature",
                    feature,
                    "ai_feature_not_allowed",
                    "Feature is not on the user allowlist",
                );
            }
        }
        if let Some(disabled) = &policy.disabled_features {
            if disabled.iter().any(|name| name == feature) {
                return gate_denied(
                    layers,
                    "user",
                    feature,
                    "ai_feature_disabled_for_user",
                    "Feature is switched off for the user",
                );
            }
        }
    }
    layers.push(gate_layer_outcome(
        "feature",
        true,
        "Feature is allowed by every layer",
    ));
    AiGateDecision {
        allowed: true,
        denied_layer: None,
        denied_feature: None,
        reason_code: None,
        layers,
    }
}

/// Audit one gate-layer change: the event the audit module records
/// (`ai.gating.changed`), carrying the layer, the target, the
/// field, and the old -> new values. Never carries prompt content.
fn emit_gating_change(
    ctx: &AppContext,
    actor: &str,
    layer: &str,
    target_id: &str,
    field: &str,
    old_value: Value,
    new_value: Value,
) {
    emit_audit(
        ctx,
        EVENT_AI_GATING_CHANGED,
        actor,
        &format!("ai-gating:{layer}:{target_id}"),
        json!({
            "layer": layer,
            "target_id": target_id,
            "field": field,
            "old_value": old_value,
            "new_value": new_value,
        }),
    );
}

// ---------------------------------------------------------------------------
// Layered-gate admin endpoints (all admin-only:
// `ai:manage_providers`, same capability as the policy endpoints).
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct EffectiveGateQuery {
    #[serde(default)]
    pub user_id: Option<String>,
    #[serde(default)]
    pub feature: Option<String>,
    #[serde(default)]
    pub product_id: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateCompanyGateInput {
    #[serde(default)]
    pub ai_enabled: Option<bool>,
    #[serde(default)]
    pub disabled_features: Option<Option<Vec<String>>>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateUserGateInput {
    #[serde(default)]
    pub ai_enabled: Option<bool>,
    #[serde(default)]
    pub disabled_features: Option<Option<Vec<String>>>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateGlobalSettingsInput {
    #[serde(default)]
    pub global_enabled: Option<bool>,
    #[serde(default)]
    pub disabled_features: Option<Option<Vec<String>>>,
}

/// The effective gate decision for a user / feature, with the
/// per-layer breakdown that explains which layer decides. Defaults
/// to the caller and the `complete` feature; `feature` accepts the
/// friendly aliases (`support`, `drafting`, `insights`, `chat`).
async fn get_effective_gate(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<AiState>,
    req: HttpRequest,
    query: web::Query<EffectiveGateQuery>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<AiGateDecision> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_AI_MANAGE_PROVIDERS)?;
        let params = query.into_inner();
        let target_user_id = match &params.user_id {
            Some(raw) => Some(parse_uuid(raw, "user")?),
            None => caller_user_id(&caller),
        };
        let feature = match &params.feature {
            Some(raw) => validate_gate_feature_name(raw)?,
            None => "complete".to_string(),
        };
        let product_id = match &params.product_id {
            Some(raw) => Some(parse_uuid(raw, "product")?),
            None => resolve_caller_product(&req),
        };
        // Company scope: a company admin inspects only their own
        // company. The target user's company comes from the users
        // module; the caller's own company from the header / hook.
        let caller_company = resolve_caller_company(&ctx, &req, &caller);
        let caller_own_user_id = caller_user_id(&caller);
        let target_company = if target_user_id == caller_own_user_id {
            caller_company
        } else {
            match target_user_id {
                Some(target_user_id) => {
                    let resolved = resolve_user_company(&ctx, target_user_id);
                    match resolved {
                        Some(company_id) => {
                            require_company_scope(caller_company, company_id)?;
                            Some(company_id)
                        }
                        None => {
                            if caller_company.is_some() {
                                return Err(UnifiedError::forbidden(
                                    "Cannot verify the target user's company",
                                ));
                            }
                            None
                        }
                    }
                }
                None => caller_company,
            }
        };
        Ok(evaluate_ai_gate(
            &ctx,
            &state,
            target_user_id,
            target_company,
            &feature,
            product_id,
        ))
    })();
    match result {
        Ok(decision) => ok(decision),
        Err(error) => err(error, debug),
    }
}

/// Switch AI on/off for a company, or switch single features off
/// for it. The company must already have an access policy (create
/// one through `POST /access/companies` first): creating a policy
/// shell here would switch the allowlist on for the company, which
/// would deny every user without a user policy.
async fn patch_company_gate(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<AiState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdateCompanyGateInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let input = body.into_inner();
    let result: UnifiedResult<AiCompanyPolicy> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_AI_MANAGE_PROVIDERS)?;
        let caller_company = resolve_caller_company(&ctx, &req, &caller);
        let company_id = parse_uuid(&path.into_inner(), "company")?;
        require_company_scope(caller_company, company_id)?;
        let mut policy = state
            .company_policies
            .get(&company_id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| {
                UnifiedError::not_found(format!(
                    "No AI access policy exists for company {company_id}: create one through POST /access/companies first"
                ))
            })?;
        if let Some(ai_enabled) = input.ai_enabled {
            if policy.ai_enabled != ai_enabled {
                emit_gating_change(
                    &ctx,
                    &caller.actor,
                    "company",
                    &policy.company_id.to_string(),
                    "ai_enabled",
                    json!(policy.ai_enabled),
                    json!(ai_enabled),
                );
                policy.ai_enabled = ai_enabled;
            }
        }
        if let Some(disabled) = input.disabled_features {
            let validated = validate_gate_features(&disabled)?;
            if policy.disabled_features != validated {
                emit_gating_change(
                    &ctx,
                    &caller.actor,
                    "company",
                    &policy.company_id.to_string(),
                    "disabled_features",
                    json!(policy.disabled_features),
                    json!(validated),
                );
                policy.disabled_features = validated;
            }
        }
        policy.updated_at = Utc::now();
        state
            .company_policies
            .insert(policy.company_id, policy.clone());
        emit_audit(
            &ctx,
            EVENT_AI_ACCESS_POLICY_CHANGED,
            &caller.actor,
            &format!("ai-company-policy:{}", policy.id),
            json!({
                "id": policy.id.to_string(),
                "company_id": policy.company_id.to_string(),
                "enabled": policy.enabled,
                "ai_enabled": policy.ai_enabled,
            }),
        );
        Ok(policy)
    })();
    match result {
        Ok(policy) => ok(policy),
        Err(error) => err(error, debug),
    }
}

/// Switch AI on/off for a user, or switch single features off for
/// them. The user must already have an access policy (create one
/// through `POST /access/users` first).
async fn patch_user_gate(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<AiState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdateUserGateInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let input = body.into_inner();
    let result: UnifiedResult<AiUserPolicy> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_AI_MANAGE_PROVIDERS)?;
        let caller_company = resolve_caller_company(&ctx, &req, &caller);
        let user_id = parse_uuid(&path.into_inner(), "user")?;
        let mut policy = state
            .user_policies
            .get(&user_id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| {
                UnifiedError::not_found(format!(
                    "No AI access policy exists for user {user_id}: create one through POST /access/users first"
                ))
            })?;
        require_company_scope(caller_company, policy.company_id)?;
        if let Some(ai_enabled) = input.ai_enabled {
            if policy.ai_enabled != ai_enabled {
                emit_gating_change(
                    &ctx,
                    &caller.actor,
                    "user",
                    &policy.user_id.to_string(),
                    "ai_enabled",
                    json!(policy.ai_enabled),
                    json!(ai_enabled),
                );
                policy.ai_enabled = ai_enabled;
            }
        }
        if let Some(disabled) = input.disabled_features {
            let validated = validate_gate_features(&disabled)?;
            if policy.disabled_features != validated {
                emit_gating_change(
                    &ctx,
                    &caller.actor,
                    "user",
                    &policy.user_id.to_string(),
                    "disabled_features",
                    json!(policy.disabled_features),
                    json!(validated),
                );
                policy.disabled_features = validated;
            }
        }
        policy.updated_at = Utc::now();
        state.user_policies.insert(policy.user_id, policy.clone());
        emit_audit(
            &ctx,
            EVENT_AI_ACCESS_POLICY_CHANGED,
            &caller.actor,
            &format!("ai-user-policy:{}", policy.id),
            json!({
                "id": policy.id.to_string(),
                "user_id": policy.user_id.to_string(),
                "company_id": policy.company_id.to_string(),
                "enabled": policy.enabled,
                "ai_enabled": policy.ai_enabled,
            }),
        );
        Ok(policy)
    })();
    match result {
        Ok(policy) => ok(policy),
        Err(error) => err(error, debug),
    }
}

/// Read the global AI settings (kill-switch + global feature
/// switches).
async fn get_global_settings(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<AiState>,
    req: HttpRequest,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<AiGlobalSettings> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_AI_MANAGE_PROVIDERS)?;
        Ok(state
            .global_settings
            .read()
            .expect("ai global settings lock poisoned")
            .clone())
    })();
    match result {
        Ok(settings) => ok(settings),
        Err(error) => err(error, debug),
    }
}

/// Change the global AI settings. Turning `global_enabled` off is
/// the kill-switch: it denies every AI call immediately,
/// regardless of the other layers.
async fn patch_global_settings(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<AiState>,
    req: HttpRequest,
    body: web::Json<UpdateGlobalSettingsInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let input = body.into_inner();
    let result: UnifiedResult<AiGlobalSettings> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_AI_MANAGE_PROVIDERS)?;
        let mut settings = state
            .global_settings
            .read()
            .expect("ai global settings lock poisoned")
            .clone();
        if let Some(global_enabled) = input.global_enabled {
            if settings.global_enabled != global_enabled {
                emit_gating_change(
                    &ctx,
                    &caller.actor,
                    "settings",
                    "global",
                    "ai.global_enabled",
                    json!(settings.global_enabled),
                    json!(global_enabled),
                );
                settings.global_enabled = global_enabled;
            }
        }
        if let Some(disabled) = input.disabled_features {
            let validated = validate_gate_features(&disabled)?.unwrap_or_default();
            if settings.disabled_features != validated {
                emit_gating_change(
                    &ctx,
                    &caller.actor,
                    "settings",
                    "global",
                    "ai.features.disabled",
                    json!(settings.disabled_features),
                    json!(validated),
                );
                settings.disabled_features = validated;
            }
        }
        settings.updated_at = Utc::now();
        *state
            .global_settings
            .write()
            .expect("ai global settings lock poisoned") = settings.clone();
        Ok(settings)
    })();
    match result {
        Ok(settings) => ok(settings),
        Err(error) => err(error, debug),
    }
}
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

// ---------------------------------------------------------------------------
// AI Capabilities Contract.
//
// Every dynamic module exposes its AI features through this registry
// instead of hard-coding them here: a module declares its
// capabilities (a `&[AiCapability]` const in its own file) and calls
// `register_ai_capabilities` once from its `init`. The AI module
// never keeps a hard-coded match list of module capabilities; the
// registry is the single source of truth, and `run_ai_capability`
// is the single runner every module AI endpoint delegates to.
//
// Flow for one capability call (`run_ai_capability`):
//   (a) look the capability up in the registry (unknown = 404),
//   (b) validate the input object contains exactly the declared
//       `input_fields` (missing or unexpected field = 422),
//   (c) redact PII through the privacy module when the capability
//       does not allow it (`allows_pii == false`),
//   (d) emit the grounding hook `<module>.ai.<capability>_requested`
//       with `{"request_id", "module_id", "capability", "caller",
//       "input"}` and collect grounding from the results (first
//       result wins, the rest merge into `extra_context`),
//   (e) run the allowlist gate (`authorize_ai_call`, with the
//       capability's `use_case`) FIRST, before any provider traffic,
//   (e2) package-entitlement gate (after authorization, still
//       before any provider traffic): for a sellable non-core
//       module, the calling tenant's entitlement is checked
//       through the `entitlements.check_requested` hook query,
//       fail closed — only an explicit `allowed: true` passes.
//       Core modules, modules that are not sellable, and
//       control-plane calls (no company id) skip the hook; see the
//       gate in the function body.
//   (f) run the completion through the same `run_completion` path
//       the draft endpoints use (default provider connection: the
//       earliest-created enabled connection; the connection key,
//       never a personal key),
//   (g) record usage and emit `ai.call.completed` / `ai.call.failed`
//       through `run_completion`, exactly like the draft endpoints,
//   (h) return the output inline; the owning module decides whether
//       the output is stored, reviewed, or published. Capability
//       outputs are deliberately NOT stored as `AiDraft`s here: the
//       draft endpoints own the human-review draft flow, and a
//       capability result is data the owning module handles.
//
// Security notes:
//   - The gate + allowlist run on EVERY capability call and are
//     never cached; `authorize_ai_call` re-evaluates the layered
//     gate and the policy caps on each call.
//   - Undeclared input fields are never passed to the model.
//   - No prompt content is ever logged or put in an audit payload:
//     audit events carry ids and reason codes only.
//   - Capability calls carry no purchased-product context (there is
//     no request to read `X-Product-Id` from in this signature), so
//     the product layer defaults to allow exactly as it does for
//     any call without that header. The module-level package
//     entitlement is checked separately by the gate in (e2): the
//     tenant's entitlement for the called module is resolved
//     through the `entitlements.check_requested` hook, fail closed,
//     before any provider traffic.
//   - A capability's `use_case` is the string the gate and the
//     policy `allowed_features` lists match on. Built-in use cases
//     live in `KNOWN_USE_CASES`; a module-scoped use case (for
//     example `exams.draft_questions`) is accepted by
//     `validate_policy_features` once its capability is registered.
//
// Tenant scoping (standing rule: tenant-scoped data access).
//   - Tenant data: capability calls, usage records, and grounding
//     data are tenant-scoped. The company id on every capability
//     call comes from `resolve_capability_company` — the verified
//     `TenantContext` first (middleware-attached, resolved from
//     the Host header), then the pre-existing header / hook path
//     for control-plane requests. Usage is recorded per company
//     (`AiUsageRecord.company_id`). Grounding hooks receive the
//     calling tenant's company id in the payload and must only
//     ever return that tenant's data.
//   - Control-plane data: provider connections and the global AI
//     settings are platform data; when persisted they belong in
//     the `public` schema, never in a tenant schema. Company and
//     user access policies are per-tenant data.
//   - Current state, honestly: every store in this module (and in
//     the wired modules) is in-memory with no tenant partition
//     and no Postgres access at all, so there is no
//     `public`-schema fallback to remove — but also no
//     tenant-column filtering to enforce yet. The tenancy plumbing
//     itself DOES exist (`crate::core::tenancy`: `schema_name`,
//     `with_tenant_connection`, the `TenantContext` extractor, the
//     `ControlPlaneGuard`); the convergence points for persistence
//     are: (1) when AI state gains Postgres persistence, usage /
//     policies / capability results must be written through
//     `with_tenant_connection` in the tenant schema resolved from
//     the tenant context, and provider connections / global
//     settings through `public` (via `ControlPlaneGuard`);
//     (2) grounding handlers must filter by the payload's caller
//     company id once their stores carry one; (3) the
//     package-entitlement gate in `run_ai_capability` (step (e2))
//     asks `entitlements.check_requested` and fails closed while
//     the entitlements module is not registered and answering, so
//     sellable-module capability calls stay denied until the
//     integrator registers it in `mod.rs`.
// The AI module's own state is shared with this runner through a
// process-wide handle (`AI_STATE`), set once in `AiModule::init`
// from the same `AiState` the HTTP scope serves, so the providers,
// usage, and policy stores are the same instance on both paths.
// This follows the static-state pattern already used by other
// modules in this codebase for cross-module access.
// ---------------------------------------------------------------------------

/// One AI capability a module exposes. Capability ids, module ids,
/// and use cases are declared per-module (registry), never matched
/// on in a hard-coded list here. Field names and order are part of
/// the contract: parallel builders write against this exact shape.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct AiCapability {
    pub id: &'static str,
    pub module_id: &'static str,
    pub name: &'static str,
    pub description: &'static str,
    pub input_fields: &'static [&'static str],
    pub allows_pii: bool,
    pub use_case: &'static str,
}

/// The AI module's own capabilities, registered in `AiModule::init`
/// so the registry is self-describing from startup.
pub const AI_CAPABILITIES: &[AiCapability] = &[
    AiCapability {
        id: "summarize",
        module_id: "ai",
        name: "Summarize text",
        description: "Summarize caller-supplied text into a short summary. No personal data is accepted: input is redacted before it reaches the model.",
        input_fields: &["text"],
        allows_pii: false,
        use_case: "complete",
    },
    AiCapability {
        id: "draft",
        module_id: "ai",
        name: "Draft text",
        description: "Draft text from a topic and a short brief. The output is returned inline; it is never published or sent by this module.",
        input_fields: &["topic", "brief"],
        allows_pii: false,
        use_case: "content",
    },
    AiCapability {
        id: "analyze",
        module_id: "ai",
        name: "Analyze text",
        description: "Analyze caller-supplied text and return key points, tone, and suggested follow-ups. No personal data is accepted: input is redacted before it reaches the model.",
        input_fields: &["text"],
        allows_pii: false,
        use_case: "complete",
    },
    AiCapability {
        id: "translate",
        module_id: "ai",
        name: "Translate text",
        description: "Translate caller-supplied text into the requested target language. No personal data is accepted: input is redacted before it reaches the model.",
        input_fields: &["text", "target_language"],
        allows_pii: false,
        use_case: "complete",
    },
];

/// Registry of per-module AI capabilities, keyed by module id.
/// Backed by a `DashMap` so modules can register from `init`
/// without a lock held across the call.
pub struct AiCapabilityRegistry {
    capabilities: DashMap<String, Vec<AiCapability>>,
}

impl AiCapabilityRegistry {
    fn new() -> Self {
        Self {
            capabilities: DashMap::new(),
        }
    }

    /// Register (or replace) the full capability list of one
    /// module. Call once per module, from that module's `init`,
    /// with the module's complete list.
    pub fn register(&self, module_id: &str, capabilities: &[AiCapability]) {
        let mut owned: Vec<AiCapability> = capabilities.to_vec();
        owned.sort_by(|a, b| a.id.cmp(b.id));
        owned.dedup_by(|a, b| a.id == b.id);
        self.capabilities.insert(module_id.to_string(), owned);
    }

    /// Owned copies of one module's capabilities, sorted by id.
    /// An unregistered module yields an empty list.
    pub fn capabilities_for(&self, module_id: &str) -> Vec<AiCapability> {
        self.capabilities
            .get(module_id)
            .map(|entry| entry.value().clone())
            .unwrap_or_default()
    }

    /// Owned copies of every registered capability, sorted by
    /// module id, then capability id.
    pub fn all_capabilities(&self) -> Vec<AiCapability> {
        let mut out: Vec<AiCapability> = self
            .capabilities
            .iter()
            .flat_map(|entry| entry.value().clone())
            .collect();
        out.sort_by(|a, b| {
            a.module_id
                .cmp(b.module_id)
                .then_with(|| a.id.cmp(b.id))
        });
        out
    }

    /// One capability by module id and capability id.
    pub fn find(&self, module_id: &str, capability: &str) -> Option<AiCapability> {
        self.capabilities.get(module_id).and_then(|entry| {
            entry
                .value()
                .iter()
                .find(|item| item.id == capability)
                .copied()
        })
    }

    /// True when any registered capability declares this use case.
    /// Used by policy validation so module-scoped use cases are
    /// allowlistable without a hard-coded list.
    pub fn has_use_case(&self, use_case: &str) -> bool {
        self.capabilities.iter().any(|entry| {
            entry
                .value()
                .iter()
                .any(|capability| capability.use_case == use_case)
        })
    }
}

static CAPABILITY_REGISTRY: std::sync::OnceLock<AiCapabilityRegistry> =
    std::sync::OnceLock::new();

/// The shared capability registry, created on first use.
pub fn ai_capability_registry() -> &'static AiCapabilityRegistry {
    CAPABILITY_REGISTRY.get_or_init(AiCapabilityRegistry::new)
}

/// Register one module's AI capabilities. Called from the owning
/// module's `init` with that module's complete capability list.
pub fn register_ai_capabilities(module_id: &'static str, capabilities: &[AiCapability]) {
    ai_capability_registry().register(module_id, capabilities);
}

/// Process-wide handle on the AI module's state, set once in
/// `AiModule::init` from the same `AiState` the HTTP scope serves.
static AI_STATE: std::sync::OnceLock<AiState> = std::sync::OnceLock::new();

/// The shared AI state, or an internal error when the AI module
/// has not initialised yet. Fail closed: no state means no
/// capability call runs.
fn ai_state() -> UnifiedResult<AiState> {
    AI_STATE.get().cloned().ok_or_else(|| {
        UnifiedError::internal("AI module is not initialised: capability calls are unavailable")
    })
}

/// Split grounding-hook results: the first result is the grounding,
/// every later result merges into `extra_context` (object results
/// merge by key; other results land under `result_{index}`).
fn split_grounding(results: &[Value]) -> (Value, Value) {
    let grounding = match results.first() {
        Some(Value::Object(_)) => results[0].clone(),
        Some(other) => json!({ "result": other }),
        None => Value::Null,
    };
    let mut extra = serde_json::Map::new();
    for (index, result) in results.iter().skip(1).enumerate() {
        match result {
            Value::Object(map) => {
                for (key, value) in map {
                    extra.insert(key.clone(), value.clone());
                }
            }
            other => {
                extra.insert(format!("result_{index}"), other.clone());
            }
        }
    }
    (grounding, Value::Object(extra))
}

/// True when a grounding value carries real content: a non-empty
/// string, a non-empty array / object (recursively), or any number
/// or boolean. Empty objects, empty arrays, empty strings, and
/// nulls carry nothing.
fn grounding_has_content(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(_) | Value::Number(_) => true,
        Value::String(text) => !text.trim().is_empty(),
        Value::Array(items) => items.iter().any(grounding_has_content),
        Value::Object(map) => map.values().any(grounding_has_content),
    }
}

/// Render the validated, redacted input fields as prompt text, in
/// declared field order. Non-string values render as compact JSON.
fn render_capability_input(capability: &AiCapability, input: &Value) -> String {
    let mut lines: Vec<String> = Vec::new();
    for field in capability.input_fields {
        match input.get(*field) {
            Some(Value::String(text)) => lines.push(format!("{field}: {text}")),
            Some(other) => lines.push(format!("{field}: {other}")),
            None => lines.push(format!("{field}: (missing)")),
        }
    }
    lines.join("\n")
}

/// Truncate text to at most `max` characters, keeping whole
/// characters.
fn truncate_chars(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        text.to_string()
    } else {
        text.chars().take(max).collect()
    }
}

/// Run one registered AI capability end to end. See the section
/// header for the step order and the security notes. Denials are
/// 403s with reason codes; unknown module / capability is a 404
/// with reason `ai_capability_not_found` in metadata; input
/// validation failures are 422s. No prompt content is ever logged
/// or placed in an audit payload.
pub async fn run_ai_capability(
    ctx: &AppContext,
    caller: &Caller,
    company_id: Option<Uuid>,
    module_id: &str,
    capability: &str,
    input: Value,
) -> UnifiedResult<Value> {
    let request_id = Uuid::now_v7();
    // (a) Look the capability up in the registry.
    let found = ai_capability_registry()
        .find(module_id, capability)
        .ok_or_else(|| {
            UnifiedError::not_found(format!(
                "AI capability not found: {module_id}.{capability}"
            ))
            .with_metadata(
                "reason",
                Value::String("ai_capability_not_found".to_string()),
            )
            .with_metadata("module_id", Value::String(module_id.to_string()))
            .with_metadata("capability", Value::String(capability.to_string()))
        })?;
    // (b) The input must be an object with exactly the declared fields.
    let object = input.as_object().ok_or_else(|| {
        UnifiedError::unprocessable("input must be a JSON object")
            .with_metadata("module_id", Value::String(module_id.to_string()))
            .with_metadata("capability", Value::String(capability.to_string()))
    })?;
    for field in found.input_fields {
        if !object.contains_key(*field) {
            return Err(UnifiedError::unprocessable(format!(
                "input is missing required field \"{field}\""
            ))
            .with_metadata("field", Value::String(field.to_string()))
            .with_metadata("module_id", Value::String(module_id.to_string()))
            .with_metadata("capability", Value::String(capability.to_string())));
        }
    }
    for key in object.keys() {
        if !found
            .input_fields
            .iter()
            .any(|field| *field == key.as_str())
        {
            return Err(UnifiedError::unprocessable(format!(
                "input contains unexpected field \"{key}\""
            ))
            .with_metadata("field", Value::String(key.clone()))
            .with_metadata("module_id", Value::String(module_id.to_string()))
            .with_metadata("capability", Value::String(capability.to_string())));
        }
    }
    if input.to_string().len() > MAX_PROMPT_LEN {
        return Err(UnifiedError::bad_request(format!(
            "input must be at most {MAX_PROMPT_LEN} characters in total"
        )));
    }
    // (c) Redact PII unless the capability explicitly allows it.
    // The privacy module owns redaction (`redact_value`); there is
    // no `privacy.redact_requested` hook, so the public helper is
    // called directly. Names are not redacted here (`false`):
    // field values that name a person stay usable for drafting,
    // while emails, phones, and ID-like numbers are masked.
    let (effective_input, pii_redacted) = if found.allows_pii {
        (input.clone(), false)
    } else {
        (privacy::redact_value(&input, false), true)
    };
    // (d) Grounding hook: `<module>.ai.<capability>_requested`.
    let event = format!("{module_id}.ai.{capability}_requested");
    let hook = ctx.hooks.emit(
        &event,
        json!({
            "request_id": request_id.to_string(),
            "module_id": module_id,
            "capability": capability,
            "caller": {
                "user_id": caller_user_id(caller).map(|id| id.to_string()),
                "company_id": company_id.map(|id| id.to_string()),
            },
            "input": effective_input,
        }),
    );
    let (grounding, extra_context) = split_grounding(&hook.results);
    let grounded = grounding_has_content(&grounding);
    let grounding_text = if grounded {
        truncate_chars(
            &serde_json::to_string_pretty(&grounding).unwrap_or_default(),
            MAX_CONTEXT_TEXT_LEN,
        )
    } else {
        String::new()
    };
    let extra_text = if grounding_has_content(&extra_context) {
        truncate_chars(
            &serde_json::to_string_pretty(&extra_context).unwrap_or_default(),
            MAX_CONTEXT_TEXT_LEN,
        )
    } else {
        String::new()
    };
    // Prompt: the validated input fields, rendered in declared
    // order. Grounding travels in the system prompt only, like the
    // draft endpoints do.
    let prompt_text = truncate_chars(
        &render_capability_input(&found, &effective_input),
        MAX_PROMPT_LEN,
    );
    let system_prompt = if grounded || !extra_text.is_empty() {
        let mut text = format!(
            "You are the \"{}\" capability of the \"{}\" module. Return only the output text, in the language of the input unless the capability asks otherwise. Do not publish, send, or act on anything; only return text.",
            found.name, found.module_id
        );
        if !grounding_text.is_empty() {
            text.push_str(&format!("\n\nGrounding (use only this context; do not invent facts):\n{grounding_text}"));
        }
        if !extra_text.is_empty() {
            text.push_str(&format!("\n\nAdditional context:\n{extra_text}"));
        }
        Some(text)
    } else {
        Some(format!(
            "You are the \"{}\" capability of the \"{}\" module. Return only the output text, in the language of the input unless the capability asks otherwise. Do not publish, send, or act on anything; only return text.",
            found.name, found.module_id
        ))
    };
    // Resolve the provider connection (not provider traffic), then
    // (e) run the allowlist gate FIRST, before any provider traffic.
    let state = ai_state()?;
    let connection = resolve_connection(&state, None)?;
    let auth = authorize_ai_call(
        ctx,
        &state,
        caller,
        company_id,
        &AiCallRequest {
            use_case: found.use_case,
            model: &connection.model,
            provider_id: connection.id,
            provider_kind: connection.kind.as_str(),
            estimated_tokens: estimate_prompt_tokens(&prompt_text) + DEFAULT_MAX_TOKENS,
            use_personal_key: false,
            product_id: None,
        },
    )?;
    // (e2) Package-entitlement gate, after authorization and
    // before any provider traffic. A sellable non-core module may
    // only run a capability for a tenant that holds an entitlement
    // for it, resolved through the `entitlements.check_requested`
    // hook query (answered by the entitlements module from its
    // stored rows; the event name is the literal of
    // `entitlements::EVENT_CHECK_REQUESTED` — the const cannot be
    // referenced here until that module is declared in `mod.rs`).
    // The answer is read fail-closed, like `role.check`: only an
    // explicit `allowed: true` — on the payload first, then in the
    // contributed results — passes. No answer, an explicit
    // `false`, or a malformed answer denies the call with a 403
    // and the provider is never contacted. Core modules and
    // modules that are not sellable pass from the registry alone,
    // mirroring the SaaS packages hook path. A capability
    // registered for a module the registry does not know is
    // inconsistent state and also denies. Control-plane calls
    // carry no company id, so there is no tenant to entitle; they
    // are covered by the allowlist gate in (e) above.
    if let Some(tenant_company_id) = company_id {
        let gated = match ctx.modules.get(module_id) {
            Some(module) => module.available_as_product() && !module.is_core(),
            None => true,
        };
        if gated {
            let answered = ctx.hooks.emit(
                "entitlements.check_requested",
                json!({
                    "company_id": tenant_company_id.to_string(),
                    "module_id": module_id,
                    "allowed": false,
                }),
            );
            let allowed = if answered.results.is_empty() {
                false
            } else {
                answered
                    .get("allowed")
                    .and_then(|value| value.as_bool())
                    .or_else(|| {
                        answered
                            .results
                            .iter()
                            .filter_map(|value| {
                                value.get("allowed").and_then(|allowed| allowed.as_bool())
                            })
                            .next()
                    })
                    .unwrap_or(false)
            };
            if !allowed {
                return Err(UnifiedError::forbidden(format!(
                    "Tenant is not entitled to module \"{module_id}\""
                ))
                .with_metadata(
                    "reason",
                    Value::String("entitlement.required".to_string()),
                )
                .with_metadata("module_id", Value::String(module_id.to_string()))
                .with_metadata("capability", Value::String(capability.to_string())));
            }
        }
    }
    // (f) Run the completion through the shared path; usage
    // recording and the `ai.call.completed` / `ai.call.failed`
    // events happen inside `run_completion` (g).
    let call = CompletionCall {
        prompt: prompt_text,
        system_prompt,
        max_tokens: DEFAULT_MAX_TOKENS,
        temperature: None,
    };
    let outcome =
        run_completion(&state, ctx, &caller.actor, &connection, &call, found.use_case, &auth)
            .await?;
    // (h) Return the output inline.
    Ok(json!({
        "capability": found.id,
        "module_id": found.module_id,
        "output": outcome.text,
        "model": outcome.model,
        "grounded": grounded,
        "pii_redacted": pii_redacted,
        "usage_id": outcome.usage.id.to_string(),
    }))
}

// ---------------------------------------------------------------------------
// Capability HTTP handlers.
// ---------------------------------------------------------------------------

/// Body of `POST /capabilities/{module_id}/{capability}`.
#[derive(Debug, Deserialize)]
pub struct CapabilityCallInput {
    pub input: Value,
}

/// List every registered capability, paginated and filterable by
/// `module_id` and free-text search over id / name / description.
async fn list_capabilities(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_AI_USE)?;
        let params = query.into_inner();
        let module_filter = params.filters.get("module_id").cloned();
        let search = params.search.clone().unwrap_or_default().to_lowercase();
        let mut items: Vec<AiCapability> = ai_capability_registry()
            .all_capabilities()
            .into_iter()
            .filter(|capability| {
                if let Some(module_id) = &module_filter {
                    if capability.module_id != module_id {
                        return false;
                    }
                }
                if !search.is_empty() {
                    let haystack = format!(
                        "{} {} {} {}",
                        capability.module_id,
                        capability.id,
                        capability.name,
                        capability.description
                    )
                    .to_lowercase();
                    if !haystack.contains(&search) {
                        return false;
                    }
                }
                true
            })
            .collect();
        items.sort_by(|a, b| {
            a.module_id
                .cmp(b.module_id)
                .then_with(|| a.id.cmp(b.id))
        });
        Ok(paginate(items, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

/// List one module's registered capabilities, paginated.
async fn list_module_capabilities(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let module_id = path.into_inner();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_AI_USE)?;
        let params = query.into_inner();
        let search = params.search.clone().unwrap_or_default().to_lowercase();
        let mut items: Vec<AiCapability> = ai_capability_registry()
            .capabilities_for(&module_id)
            .into_iter()
            .filter(|capability| {
                if search.is_empty() {
                    return true;
                }
                let haystack = format!(
                    "{} {} {}",
                    capability.id, capability.name, capability.description
                )
                .to_lowercase();
                haystack.contains(&search)
            })
            .collect();
        items.sort_by(|a, b| a.id.cmp(b.id));
        Ok(paginate(items, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

/// Run one registered capability: `POST
/// /capabilities/{module_id}/{capability}` with body
/// `{"input": {...}}`. Requires `ai:use`; delegates to
/// `run_ai_capability`.
async fn run_capability_endpoint(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    path: web::Path<(String, String)>,
    body: web::Json<CapabilityCallInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let (module_id, capability) = path.into_inner();
    let input = body.into_inner().input;
    let caller = match caller(&req) {
        Ok(caller) => caller,
        Err(error) => return err(error, debug),
    };
    if let Err(error) = require_permission(&ctx, &caller.role, PERM_AI_USE) {
        return err(error, debug);
    }
    let company_id = resolve_capability_company(&ctx, &req, &caller);
    match run_ai_capability(&ctx, &caller, company_id, &module_id, &capability, input).await {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

/// Run one provider call end to end: resolve and rate-limit the
/// connection, decrypt the key, call the adapter, record usage, and
/// emit the audit event. A disabled connection is refused before any
/// provider traffic.
async fn run_completion(
    state: &AiState,
    ctx: &AppContext,
    actor: &str,
    connection: &AiProviderConnection,
    call: &CompletionCall,
    use_case: &str,
    auth: &AiAuthorization,
) -> UnifiedResult<CompletionOutcome> {
    if !connection.enabled {
        return Err(UnifiedError::bad_request(format!(
            "AI provider connection \"{}\" is disabled",
            connection.name
        )));
    }
    check_rate_limit(state, connection)?;
    // The key comes from the authorization: the caller's personal key
    // when the call runs on it, otherwise the connection's key. The
    // personal key was decrypted once by `authorize_ai_call` and
    // lives only for this call.
    let api_key = if let Some(personal) = &auth.personal_key {
        Some(personal.decrypted_key.clone())
    } else {
        let secret = encryption_secret(ctx);
        match &connection.api_key_encrypted {
            Some(stored) => Some(decrypt_api_key(&secret, stored)?),
            None => None,
        }
    };
    let adapter = ai_provider_registry().build(connection, api_key)?;
    let started = Instant::now();
    let result = adapter.complete(call).await;
    let latency_ms = started.elapsed().as_millis() as u64;
    match result {
        Ok(raw) => {
            let usage = record_usage(
                state,
                connection,
                actor,
                auth.user_id,
                auth.company_id,
                use_case,
                Some(&raw),
                latency_ms,
                None,
                auth.use_personal_key,
            );
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
                    "model": connection.model.clone(),
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
            let usage = record_usage(
                state,
                connection,
                actor,
                auth.user_id,
                auth.company_id,
                use_case,
                None,
                latency_ms,
                Some(error.code),
                auth.use_personal_key,
            );
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
                    "model": connection.model.clone(),
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
        "AI provider abstraction (Anthropic, OpenAI, OpenAI-compatible, local) with human-approved drafts for support, content, and email, plus usage tracking, a shared site context, company/user access allowlists with personal API keys and personal MCP servers, layered AI gating (global kill-switch, purchased-product layer, company and user on/off switches, per-feature toggles) enforced on every call, and a registry of per-module AI capabilities run through one gated runner."
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
        // Share this module's state with the capability runner
        // (`run_ai_capability`); the first initialisation wins.
        let _ = AI_STATE.set(self.state.clone());
        // Register the AI module's own capabilities so the registry
        // is self-describing from startup.
        register_ai_capabilities("ai", AI_CAPABILITIES);
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
            RouteInfo { method: "GET", path: "/api/v1/ai/usage/by-user", owner: "ai", description: "Usage records for one user (own records, or any user for usage readers)" },
            RouteInfo { method: "GET", path: "/api/v1/ai/usage/by-company", owner: "ai", description: "Usage records for one company (own company, or any company for platform usage readers)" },
            RouteInfo { method: "GET", path: "/api/v1/ai/access/companies", owner: "ai", description: "List company access policies" },
            RouteInfo { method: "POST", path: "/api/v1/ai/access/companies", owner: "ai", description: "Create company access policy" },
            RouteInfo { method: "GET", path: "/api/v1/ai/access/companies/{id}", owner: "ai", description: "Get company access policy" },
            RouteInfo { method: "PATCH", path: "/api/v1/ai/access/companies/{id}", owner: "ai", description: "Update company access policy" },
            RouteInfo { method: "DELETE", path: "/api/v1/ai/access/companies/{id}", owner: "ai", description: "Delete company access policy" },
            RouteInfo { method: "GET", path: "/api/v1/ai/access/users", owner: "ai", description: "List user access policies" },
            RouteInfo { method: "POST", path: "/api/v1/ai/access/users", owner: "ai", description: "Create user access policy" },
            RouteInfo { method: "GET", path: "/api/v1/ai/access/users/{id}", owner: "ai", description: "Get user access policy" },
            RouteInfo { method: "PATCH", path: "/api/v1/ai/access/users/{id}", owner: "ai", description: "Update user access policy" },
            RouteInfo { method: "DELETE", path: "/api/v1/ai/access/users/{id}", owner: "ai", description: "Delete user access policy" },
            RouteInfo { method: "GET", path: "/api/v1/ai/my-key", owner: "ai", description: "Get own personal API key metadata (never the key)" },
            RouteInfo { method: "POST", path: "/api/v1/ai/my-key", owner: "ai", description: "Create or rotate own personal API key" },
            RouteInfo { method: "DELETE", path: "/api/v1/ai/my-key", owner: "ai", description: "Delete own personal API key" },
            RouteInfo { method: "POST", path: "/api/v1/ai/my-key/test", owner: "ai", description: "Test own personal API key" },
            RouteInfo { method: "GET", path: "/api/v1/ai/my-mcp", owner: "ai", description: "List own personal MCP servers" },
            RouteInfo { method: "POST", path: "/api/v1/ai/my-mcp", owner: "ai", description: "Create own personal MCP server" },
            RouteInfo { method: "GET", path: "/api/v1/ai/my-mcp/{id}", owner: "ai", description: "Get own personal MCP server" },
            RouteInfo { method: "PATCH", path: "/api/v1/ai/my-mcp/{id}", owner: "ai", description: "Update own personal MCP server" },
            RouteInfo { method: "DELETE", path: "/api/v1/ai/my-mcp/{id}", owner: "ai", description: "Delete own personal MCP server" },
            RouteInfo { method: "GET", path: "/api/v1/ai/context", owner: "ai", description: "Get the shared site context" },
            RouteInfo { method: "GET", path: "/api/v1/ai/capabilities", owner: "ai", description: "List every registered AI capability" },
            RouteInfo { method: "GET", path: "/api/v1/ai/capabilities/{module_id}", owner: "ai", description: "List one module's registered AI capabilities" },
            RouteInfo { method: "POST", path: "/api/v1/ai/capabilities/{module_id}/{capability}", owner: "ai", description: "Run one registered AI capability" },
            RouteInfo { method: "GET", path: "/api/v1/ai/gating/effective", owner: "ai", description: "Effective layered gate decision for a user / feature, with per-layer breakdown" },
            RouteInfo { method: "PATCH", path: "/api/v1/ai/gating/company/{company_id}", owner: "ai", description: "Switch AI on/off or switch features off for a company" },
            RouteInfo { method: "PATCH", path: "/api/v1/ai/gating/user/{user_id}", owner: "ai", description: "Switch AI on/off or switch features off for a user" },
            RouteInfo { method: "GET", path: "/api/v1/ai/gating/settings", owner: "ai", description: "Read the global AI settings (kill-switch + global feature switches)" },
            RouteInfo { method: "PATCH", path: "/api/v1/ai/gating/settings", owner: "ai", description: "Change the global AI settings (kill-switch + global feature switches)" },
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
                .route("/usage/by-user", web::get().to(usage_by_user))
                .route("/usage/by-company", web::get().to(usage_by_company))
                .route("/usage/{id}", web::get().to(get_usage))
                .route("/access/companies", web::get().to(list_company_policies))
                .route("/access/companies", web::post().to(create_company_policy))
                .route("/access/companies/{id}", web::get().to(get_company_policy))
                .route("/access/companies/{id}", web::patch().to(update_company_policy))
                .route("/access/companies/{id}", web::delete().to(delete_company_policy))
                .route("/access/users", web::get().to(list_user_policies))
                .route("/access/users", web::post().to(create_user_policy))
                .route("/access/users/{id}", web::get().to(get_user_policy))
                .route("/access/users/{id}", web::patch().to(update_user_policy))
                .route("/access/users/{id}", web::delete().to(delete_user_policy))
                .route("/my-key", web::get().to(get_my_key))
                .route("/my-key", web::post().to(upsert_my_key))
                .route("/my-key", web::delete().to(delete_my_key))
                .route("/my-key/test", web::post().to(test_my_key))
                .route("/my-mcp", web::get().to(list_my_mcp))
                .route("/my-mcp", web::post().to(create_my_mcp))
                .route("/my-mcp/{id}", web::get().to(get_my_mcp))
                .route("/my-mcp/{id}", web::patch().to(update_my_mcp))
                .route("/my-mcp/{id}", web::delete().to(delete_my_mcp))
                .route("/gating/effective", web::get().to(get_effective_gate))
                .route("/gating/company/{company_id}", web::patch().to(patch_company_gate))
                .route("/gating/user/{user_id}", web::patch().to(patch_user_gate))
                .route("/gating/settings", web::get().to(get_global_settings))
                .route("/gating/settings", web::patch().to(patch_global_settings))
                .route("/context", web::get().to(get_context))
                .route("/capabilities", web::get().to(list_capabilities))
                .route("/capabilities/{module_id}", web::get().to(list_module_capabilities))
                .route("/capabilities/{module_id}/{capability}", web::post().to(run_capability_endpoint)),
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
    // The connection test is a provider call, so the allowlist gate
    // runs first: a test on a connection the caller's company or user
    // is not allowed to use is denied before any provider traffic.
    // A failed test itself is still recorded as a usage row below.
    let user_id = caller_user_id(&caller);
    let company_id = resolve_caller_company(&ctx, &req, &caller);
    if let Err(error) = authorize_ai_call(
        &ctx,
        &state,
        &caller,
        company_id,
        &AiCallRequest {
            use_case: "test",
            model: &connection.model,
            provider_id: connection.id,
            provider_kind: connection.kind.as_str(),
            estimated_tokens: 0,
            use_personal_key: false,
            product_id: resolve_caller_product(&req),
        },
    ) {
        return err(error, debug);
    }
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
            let usage = record_usage(
                &state,
                &connection,
                &caller.actor,
                user_id,
                company_id,
                "test",
                None,
                test.latency_ms,
                None,
                false,
            );
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
            let usage = record_usage(
                &state,
                &connection,
                &caller.actor,
                user_id,
                company_id,
                "test",
                None,
                latency_ms,
                Some(error.code),
                false,
            );
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

/// Resolve the provider connection and run the allowlist gate for
/// one draft / completion call, after the prompt is built and before
/// any provider traffic. Returns the resolved connection plus the
/// authorization. Every completion / draft entry point calls this.
fn authorize_call(
    ctx: &Arc<AppContext>,
    state: &AiState,
    req: &HttpRequest,
    caller: &Caller,
    provider_id: Option<Uuid>,
    prompt_text: &str,
    max_tokens: u32,
    use_case: &str,
    use_personal_key: bool,
) -> UnifiedResult<(AiProviderConnection, AiAuthorization)> {
    let connection = resolve_connection(state, provider_id)?;
    let company_id = resolve_caller_company(ctx, req, caller);
    let auth = authorize_ai_call(
        ctx,
        state,
        caller,
        company_id,
        &AiCallRequest {
            use_case,
            model: &connection.model,
            provider_id: connection.id,
            provider_kind: connection.kind.as_str(),
            estimated_tokens: estimate_prompt_tokens(prompt_text) + max_tokens,
            use_personal_key,
            product_id: resolve_caller_product(req),
        },
    )?;
    Ok((connection, auth))
}

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
    let mut shared = assemble_shared_context(Some(&ctx.hooks), &grounding_query, &site, true);
    shared
        .grounding_articles
        .extend(personal_mcp_grounding_sources(&state, caller_user_id(&caller)));
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
    let use_case = match input.use_case.clone() {
        Some(use_case) => match validate_use_case(&use_case) {
            Ok(use_case) => use_case,
            Err(error) => return err(error, debug),
        },
        None => "complete".to_string(),
    };
    let (connection, auth) = match authorize_call(
        &ctx,
        &state,
        &req,
        &caller,
        input.provider_id,
        &prompt,
        max_tokens,
        &use_case,
        input.use_personal_key,
    ) {
        Ok(authorization) => authorization,
        Err(error) => return err(error, debug),
    };
    match run_completion(&state, &ctx, &caller.actor, &connection, &call, &use_case, &auth).await {
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
    let mut shared = assemble_shared_context(Some(&ctx.hooks), &instruction, &site, true);
    shared
        .grounding_articles
        .extend(personal_mcp_grounding_sources(&state, caller_user_id(&caller)));
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
    let (connection, auth) = match authorize_call(
        &ctx,
        &state,
        &req,
        &caller,
        input.provider_id,
        &prompt,
        max_tokens,
        use_case,
        input.use_personal_key,
    ) {
        Ok(authorization) => authorization,
        Err(error) => return err(error, debug),
    };
    match run_completion(&state, &ctx, &caller.actor, &connection, &call, use_case, &auth).await {
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
    let mut shared = assemble_shared_context(Some(&ctx.hooks), &prompt, &site, true);
    shared
        .grounding_articles
        .extend(personal_mcp_grounding_sources(&state, caller_user_id(&caller)));
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
    let (connection, auth) = match authorize_call(
        &ctx,
        &state,
        &req,
        &caller,
        input.provider_id,
        &prompt,
        max_tokens,
        "content",
        input.use_personal_key,
    ) {
        Ok(authorization) => authorization,
        Err(error) => return err(error, debug),
    };
    match run_completion(&state, &ctx, &caller.actor, &connection, &call, "content", &auth).await {
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
    let mut shared = assemble_shared_context(Some(&ctx.hooks), &prompt, &site, true);
    shared
        .grounding_articles
        .extend(personal_mcp_grounding_sources(&state, caller_user_id(&caller)));
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
        prompt: full_prompt.clone(),
        system_prompt,
        max_tokens,
        temperature,
    };
    let (connection, auth) = match authorize_call(
        &ctx,
        &state,
        &req,
        &caller,
        input.provider_id,
        &full_prompt,
        max_tokens,
        "email",
        input.use_personal_key,
    ) {
        Ok(authorization) => authorization,
        Err(error) => return err(error, debug),
    };
    match run_completion(&state, &ctx, &caller.actor, &connection, &call, "email", &auth).await {
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
    let mut shared = assemble_shared_context(Some(&ctx.hooks), &question, &site, true);
    shared
        .grounding_articles
        .extend(personal_mcp_grounding_sources(&state, caller_user_id(&caller)));
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
    let (connection, auth) = match authorize_call(
        &ctx,
        &state,
        &req,
        &caller,
        input.provider_id,
        &question,
        max_tokens,
        "kb_answer",
        input.use_personal_key,
    ) {
        Ok(authorization) => authorization,
        Err(error) => return err(error, debug),
    };
    match run_completion(&state, &ctx, &caller.actor, &connection, &call, "kb_answer", &auth).await {
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
// Access allowlist handlers.
//
// Admin endpoints (`/access/...`) require `ai:manage_providers`.
// A caller who belongs to a company may only manage policies of
// that company (`require_company_scope`); a caller with no
// resolvable company is a platform admin. Personal endpoints
// (`/my-key`, `/my-mcp`) derive the owner from the `X-User-Id`
// header only: a caller can never see or touch another user's key
// or servers, and keys / tokens are never returned, only metadata.
// ---------------------------------------------------------------------------

/// Find a company policy by its policy id (the store is keyed by
/// `company_id`, so this scans).
fn find_company_policy(state: &AiState, id: Uuid) -> Option<AiCompanyPolicy> {
    state
        .company_policies
        .iter()
        .map(|entry| entry.value().clone())
        .find(|policy| policy.id == id)
}

/// Find a user policy by its policy id (the store is keyed by
/// `user_id`, so this scans).
fn find_user_policy(state: &AiState, id: Uuid) -> Option<AiUserPolicy> {
    state
        .user_policies
        .iter()
        .map(|entry| entry.value().clone())
        .find(|policy| policy.id == id)
}

/// Load a personal MCP server the caller owns. A foreign or missing
/// id is the same 404: existence is never leaked across users.
fn load_own_mcp_server(
    state: &AiState,
    user_id: Uuid,
    id: Uuid,
) -> UnifiedResult<AiUserMcpServer> {
    match state.personal_mcp_servers.get(&id) {
        Some(entry) if entry.user_id == user_id => Ok(entry.value().clone()),
        _ => Err(UnifiedError::not_found(format!(
            "Personal MCP server not found: {id}"
        ))),
    }
}

async fn list_company_policies(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<AiState>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_AI_MANAGE_PROVIDERS)?;
        let caller_company = resolve_caller_company(&ctx, &req, &caller);
        let params = query.into_inner();
        let company_filter = params.filters.get("company_id").cloned();
        let enabled_filter = params.filters.get("enabled").cloned();
        let search = params.search.clone().unwrap_or_default().to_lowercase();
        let mut items: Vec<AiCompanyPolicy> = state
            .company_policies
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|policy| {
                if let Some(company) = caller_company {
                    if policy.company_id != company {
                        return false;
                    }
                }
                if let Some(company_id) = &company_filter {
                    if policy.company_id.to_string() != *company_id {
                        return false;
                    }
                }
                if let Some(enabled) = &enabled_filter {
                    let expected = match enabled.as_str() {
                        "true" => true,
                        "false" => false,
                        _ => return false,
                    };
                    if policy.enabled != expected {
                        return false;
                    }
                }
                if !search.is_empty() && !policy.company_id.to_string().contains(&search) {
                    return false;
                }
                true
            })
            .collect();
        let sort = params.allowed_sort(&["created_at", "company_id"], "created_at");
        let descending = params.normalized().order == "desc";
        items.sort_by(|a, b| {
            let ordering = match sort.as_str() {
                "company_id" => a.company_id.cmp(&b.company_id),
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

async fn create_company_policy(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<AiState>,
    req: HttpRequest,
    body: web::Json<CreateCompanyPolicyInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let input = body.into_inner();
    let result: UnifiedResult<AiCompanyPolicy> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_AI_MANAGE_PROVIDERS)?;
        let caller_company = resolve_caller_company(&ctx, &req, &caller);
        require_company_scope(caller_company, input.company_id)?;
        let allowed_models = validate_policy_models(&input.allowed_models)?;
        let allowed_features = validate_policy_features(&input.allowed_features)?;
        let monthly_token_cap = validate_token_cap(input.monthly_token_cap)?;
        let monthly_cost_cap_usd = validate_cost_cap(input.monthly_cost_cap_usd)?;
        let disabled_features = validate_gate_features(&input.disabled_features)?;
        if state.company_policies.contains_key(&input.company_id) {
            return Err(UnifiedError::conflict(format!(
                "A company access policy already exists for company {}",
                input.company_id
            )));
        }
        let now = Utc::now();
        let policy = AiCompanyPolicy {
            id: Uuid::now_v7(),
            company_id: input.company_id,
            allowed_models,
            allowed_features,
            monthly_token_cap,
            monthly_cost_cap_usd,
            enabled: input.enabled.unwrap_or(true),
            ai_enabled: input.ai_enabled.unwrap_or(true),
            disabled_features,
            created_at: now,
            updated_at: now,
        };
        state
            .company_policies
            .insert(policy.company_id, policy.clone());
        emit_audit(
            &ctx,
            EVENT_AI_ACCESS_POLICY_CHANGED,
            &caller.actor,
            &format!("ai-company-policy:{}", policy.id),
            json!({
                "id": policy.id.to_string(),
                "company_id": policy.company_id.to_string(),
                "enabled": policy.enabled,
            }),
        );
        Ok(policy)
    })();
    match result {
        Ok(policy) => created(policy),
        Err(error) => err(error, debug),
    }
}

async fn get_company_policy(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<AiState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<AiCompanyPolicy> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_AI_MANAGE_PROVIDERS)?;
        let caller_company = resolve_caller_company(&ctx, &req, &caller);
        let id = parse_uuid(&path.into_inner(), "company policy")?;
        let policy = find_company_policy(&state, id).ok_or_else(|| {
            UnifiedError::not_found(format!("Company access policy not found: {id}"))
        })?;
        require_company_scope(caller_company, policy.company_id)?;
        Ok(policy)
    })();
    match result {
        Ok(policy) => ok(policy),
        Err(error) => err(error, debug),
    }
}

async fn update_company_policy(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<AiState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdateCompanyPolicyInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let input = body.into_inner();
    let result: UnifiedResult<AiCompanyPolicy> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_AI_MANAGE_PROVIDERS)?;
        let caller_company = resolve_caller_company(&ctx, &req, &caller);
        let id = parse_uuid(&path.into_inner(), "company policy")?;
        let mut policy = find_company_policy(&state, id).ok_or_else(|| {
            UnifiedError::not_found(format!("Company access policy not found: {id}"))
        })?;
        require_company_scope(caller_company, policy.company_id)?;
        if let Some(models) = &input.allowed_models {
            policy.allowed_models = validate_policy_models(models)?;
        }
        if let Some(features) = &input.allowed_features {
            policy.allowed_features = validate_policy_features(features)?;
        }
        if let Some(cap) = input.monthly_token_cap {
            policy.monthly_token_cap = validate_token_cap(cap)?;
        }
        if let Some(cap) = input.monthly_cost_cap_usd {
            policy.monthly_cost_cap_usd = validate_cost_cap(cap)?;
        }
        if let Some(enabled) = input.enabled {
            policy.enabled = enabled;
        }
        if let Some(ai_enabled) = input.ai_enabled {
            if policy.ai_enabled != ai_enabled {
                emit_gating_change(
                    &ctx,
                    &caller.actor,
                    "company",
                    &policy.company_id.to_string(),
                    "ai_enabled",
                    json!(policy.ai_enabled),
                    json!(ai_enabled),
                );
                policy.ai_enabled = ai_enabled;
            }
        }
        if let Some(disabled) = input.disabled_features {
            let validated = validate_gate_features(&disabled)?;
            if policy.disabled_features != validated {
                emit_gating_change(
                    &ctx,
                    &caller.actor,
                    "company",
                    &policy.company_id.to_string(),
                    "disabled_features",
                    json!(policy.disabled_features),
                    json!(validated),
                );
                policy.disabled_features = validated;
            }
        }
        policy.updated_at = Utc::now();
        state
            .company_policies
            .insert(policy.company_id, policy.clone());
        emit_audit(
            &ctx,
            EVENT_AI_ACCESS_POLICY_CHANGED,
            &caller.actor,
            &format!("ai-company-policy:{}", policy.id),
            json!({
                "id": policy.id.to_string(),
                "company_id": policy.company_id.to_string(),
                "enabled": policy.enabled,
            }),
        );
        Ok(policy)
    })();
    match result {
        Ok(policy) => ok(policy),
        Err(error) => err(error, debug),
    }
}

async fn delete_company_policy(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<AiState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_AI_MANAGE_PROVIDERS)?;
        let caller_company = resolve_caller_company(&ctx, &req, &caller);
        let id = parse_uuid(&path.into_inner(), "company policy")?;
        let policy = find_company_policy(&state, id).ok_or_else(|| {
            UnifiedError::not_found(format!("Company access policy not found: {id}"))
        })?;
        require_company_scope(caller_company, policy.company_id)?;
        state.company_policies.remove(&policy.company_id);
        emit_audit(
            &ctx,
            EVENT_AI_ACCESS_POLICY_CHANGED,
            &caller.actor,
            &format!("ai-company-policy:{}", policy.id),
            json!({
                "id": policy.id.to_string(),
                "company_id": policy.company_id.to_string(),
                "enabled": false,
                "deleted": true,
            }),
        );
        Ok(json!({ "deleted": true, "policy_id": id.to_string() }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn list_user_policies(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<AiState>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_AI_MANAGE_PROVIDERS)?;
        let caller_company = resolve_caller_company(&ctx, &req, &caller);
        let params = query.into_inner();
        let user_filter = params.filters.get("user_id").cloned();
        let company_filter = params.filters.get("company_id").cloned();
        let enabled_filter = params.filters.get("enabled").cloned();
        let search = params.search.clone().unwrap_or_default().to_lowercase();
        let mut items: Vec<AiUserPolicy> = state
            .user_policies
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|policy| {
                if let Some(company) = caller_company {
                    if policy.company_id != company {
                        return false;
                    }
                }
                if let Some(user_id) = &user_filter {
                    if policy.user_id.to_string() != *user_id {
                        return false;
                    }
                }
                if let Some(company_id) = &company_filter {
                    if policy.company_id.to_string() != *company_id {
                        return false;
                    }
                }
                if let Some(enabled) = &enabled_filter {
                    let expected = match enabled.as_str() {
                        "true" => true,
                        "false" => false,
                        _ => return false,
                    };
                    if policy.enabled != expected {
                        return false;
                    }
                }
                if !search.is_empty()
                    && !policy.user_id.to_string().contains(&search)
                    && !policy.company_id.to_string().contains(&search)
                {
                    return false;
                }
                true
            })
            .collect();
        let sort = params.allowed_sort(&["created_at", "user_id", "company_id"], "created_at");
        let descending = params.normalized().order == "desc";
        items.sort_by(|a, b| {
            let ordering = match sort.as_str() {
                "user_id" => a.user_id.cmp(&b.user_id),
                "company_id" => a.company_id.cmp(&b.company_id),
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

async fn create_user_policy(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<AiState>,
    req: HttpRequest,
    body: web::Json<CreateUserPolicyInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let input = body.into_inner();
    let result: UnifiedResult<AiUserPolicy> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_AI_MANAGE_PROVIDERS)?;
        let caller_company = resolve_caller_company(&ctx, &req, &caller);
        // A company admin limits only their own company's users.
        require_company_scope(caller_company, input.company_id)?;
        let allowed_models = validate_policy_models(&input.allowed_models)?;
        let allowed_features = validate_policy_features(&input.allowed_features)?;
        let monthly_token_cap = validate_token_cap(input.monthly_token_cap)?;
        let monthly_cost_cap_usd = validate_cost_cap(input.monthly_cost_cap_usd)?;
        let disabled_features = validate_gate_features(&input.disabled_features)?;
        if state.user_policies.contains_key(&input.user_id) {
            return Err(UnifiedError::conflict(format!(
                "A user access policy already exists for user {}",
                input.user_id
            )));
        }
        let now = Utc::now();
        let policy = AiUserPolicy {
            id: Uuid::now_v7(),
            user_id: input.user_id,
            company_id: input.company_id,
            allowed_models,
            allowed_features,
            monthly_token_cap,
            monthly_cost_cap_usd,
            enabled: input.enabled.unwrap_or(true),
            ai_enabled: input.ai_enabled.unwrap_or(true),
            disabled_features,
            created_at: now,
            updated_at: now,
        };
        state.user_policies.insert(policy.user_id, policy.clone());
        emit_audit(
            &ctx,
            EVENT_AI_ACCESS_POLICY_CHANGED,
            &caller.actor,
            &format!("ai-user-policy:{}", policy.id),
            json!({
                "id": policy.id.to_string(),
                "user_id": policy.user_id.to_string(),
                "company_id": policy.company_id.to_string(),
                "enabled": policy.enabled,
            }),
        );
        Ok(policy)
    })();
    match result {
        Ok(policy) => created(policy),
        Err(error) => err(error, debug),
    }
}

async fn get_user_policy(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<AiState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<AiUserPolicy> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_AI_MANAGE_PROVIDERS)?;
        let caller_company = resolve_caller_company(&ctx, &req, &caller);
        let id = parse_uuid(&path.into_inner(), "user policy")?;
        let policy = find_user_policy(&state, id).ok_or_else(|| {
            UnifiedError::not_found(format!("User access policy not found: {id}"))
        })?;
        require_company_scope(caller_company, policy.company_id)?;
        Ok(policy)
    })();
    match result {
        Ok(policy) => ok(policy),
        Err(error) => err(error, debug),
    }
}

async fn update_user_policy(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<AiState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdateUserPolicyInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let input = body.into_inner();
    let result: UnifiedResult<AiUserPolicy> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_AI_MANAGE_PROVIDERS)?;
        let caller_company = resolve_caller_company(&ctx, &req, &caller);
        let id = parse_uuid(&path.into_inner(), "user policy")?;
        let mut policy = find_user_policy(&state, id).ok_or_else(|| {
            UnifiedError::not_found(format!("User access policy not found: {id}"))
        })?;
        require_company_scope(caller_company, policy.company_id)?;
        if let Some(models) = &input.allowed_models {
            policy.allowed_models = validate_policy_models(models)?;
        }
        if let Some(features) = &input.allowed_features {
            policy.allowed_features = validate_policy_features(features)?;
        }
        if let Some(cap) = input.monthly_token_cap {
            policy.monthly_token_cap = validate_token_cap(cap)?;
        }
        if let Some(cap) = input.monthly_cost_cap_usd {
            policy.monthly_cost_cap_usd = validate_cost_cap(cap)?;
        }
        if let Some(enabled) = input.enabled {
            policy.enabled = enabled;
        }
        if let Some(ai_enabled) = input.ai_enabled {
            if policy.ai_enabled != ai_enabled {
                emit_gating_change(
                    &ctx,
                    &caller.actor,
                    "user",
                    &policy.user_id.to_string(),
                    "ai_enabled",
                    json!(policy.ai_enabled),
                    json!(ai_enabled),
                );
                policy.ai_enabled = ai_enabled;
            }
        }
        if let Some(disabled) = input.disabled_features {
            let validated = validate_gate_features(&disabled)?;
            if policy.disabled_features != validated {
                emit_gating_change(
                    &ctx,
                    &caller.actor,
                    "user",
                    &policy.user_id.to_string(),
                    "disabled_features",
                    json!(policy.disabled_features),
                    json!(validated),
                );
                policy.disabled_features = validated;
            }
        }
        policy.updated_at = Utc::now();
        state.user_policies.insert(policy.user_id, policy.clone());
        emit_audit(
            &ctx,
            EVENT_AI_ACCESS_POLICY_CHANGED,
            &caller.actor,
            &format!("ai-user-policy:{}", policy.id),
            json!({
                "id": policy.id.to_string(),
                "user_id": policy.user_id.to_string(),
                "company_id": policy.company_id.to_string(),
                "enabled": policy.enabled,
            }),
        );
        Ok(policy)
    })();
    match result {
        Ok(policy) => ok(policy),
        Err(error) => err(error, debug),
    }
}

async fn delete_user_policy(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<AiState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_AI_MANAGE_PROVIDERS)?;
        let caller_company = resolve_caller_company(&ctx, &req, &caller);
        let id = parse_uuid(&path.into_inner(), "user policy")?;
        let policy = find_user_policy(&state, id).ok_or_else(|| {
            UnifiedError::not_found(format!("User access policy not found: {id}"))
        })?;
        require_company_scope(caller_company, policy.company_id)?;
        state.user_policies.remove(&policy.user_id);
        emit_audit(
            &ctx,
            EVENT_AI_ACCESS_POLICY_CHANGED,
            &caller.actor,
            &format!("ai-user-policy:{}", policy.id),
            json!({
                "id": policy.id.to_string(),
                "user_id": policy.user_id.to_string(),
                "company_id": policy.company_id.to_string(),
                "enabled": false,
                "deleted": true,
            }),
        );
        Ok(json!({ "deleted": true, "policy_id": id.to_string() }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

/// The caller's own user id, or a 400 when the actor id is not a
/// UUID. Personal endpoints are keyed by user id, so a non-UUID
/// actor cannot own a key or a server.
fn own_user_id(caller: &Caller) -> UnifiedResult<Uuid> {
    caller_user_id(caller).ok_or_else(|| {
        UnifiedError::bad_request("Caller id is not a user id: personal AI settings need a UUID user id")
    })
}

async fn get_my_key(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<AiState>,
    req: HttpRequest,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<AiPersonalKeyView> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_AI_USE)?;
        let user_id = own_user_id(&caller)?;
        state
            .personal_keys
            .get(&user_id)
            .map(|entry| entry.value().view())
            .ok_or_else(|| {
                UnifiedError::not_found("No personal API key is configured for this user")
            })
    })();
    match result {
        Ok(view) => ok(view),
        Err(error) => err(error, debug),
    }
}

async fn upsert_my_key(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<AiState>,
    req: HttpRequest,
    body: web::Json<UpsertPersonalKeyInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let input = body.into_inner();
    let result: UnifiedResult<(AiPersonalKeyView, bool)> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_AI_USE)?;
        let user_id = own_user_id(&caller)?;
        // In strict mode only an allowed user may configure a key.
        require_allowed_user(&state, user_id)?;
        let key = input.api_key.trim();
        if key.is_empty() || key.len() > MAX_SECRET_LEN {
            return Err(UnifiedError::bad_request(format!(
                "api_key must be between 1 and {MAX_SECRET_LEN} characters"
            )));
        }
        let base_url = match &input.base_url {
            Some(url) => {
                validate_base_url(url, input.provider_kind)?;
                Some(url.trim().to_string())
            }
            None => None,
        };
        let model = match &input.model {
            Some(model) => {
                let model = validate_policy_model_name(model)?;
                if !is_known_model(input.provider_kind, &model) {
                    return Err(UnifiedError::bad_request(format!(
                        "Model \"{model}\" is not a known {} model",
                        input.provider_kind.as_str()
                    )));
                }
                Some(model)
            }
            None => None,
        };
        let monthly_token_cap = validate_token_cap(input.monthly_token_cap)?;
        let monthly_cost_cap_usd = validate_cost_cap(input.monthly_cost_cap_usd)?;
        let secret = encryption_secret(&ctx);
        let now = Utc::now();
        let existing = state.personal_keys.get(&user_id).map(|entry| entry.value().clone());
        let (record, is_new) = match existing {
            Some(mut record) => {
                record.provider_kind = input.provider_kind;
                record.base_url = base_url;
                record.model = model;
                record.api_key_encrypted = encrypt_api_key(&secret, key);
                record.key_fingerprint = key_fingerprint(key);
                record.monthly_token_cap = monthly_token_cap;
                record.monthly_cost_cap_usd = monthly_cost_cap_usd;
                record.enabled = input.enabled.unwrap_or(record.enabled);
                record.updated_at = now;
                (record, false)
            }
            None => (
                AiPersonalKey {
                    id: Uuid::now_v7(),
                    user_id,
                    provider_kind: input.provider_kind,
                    base_url,
                    model,
                    api_key_encrypted: encrypt_api_key(&secret, key),
                    key_fingerprint: key_fingerprint(key),
                    monthly_token_cap,
                    monthly_cost_cap_usd,
                    enabled: input.enabled.unwrap_or(true),
                    created_at: now,
                    updated_at: now,
                },
                true,
            ),
        };
        state.personal_keys.insert(user_id, record.clone());
        emit_audit(
            &ctx,
            if is_new {
                EVENT_AI_PERSONAL_KEY_CREATED
            } else {
                EVENT_AI_PERSONAL_KEY_ROTATED
            },
            &caller.actor,
            &format!("ai-personal-key:{}", record.id),
            json!({
                "id": record.id.to_string(),
                "user_id": user_id.to_string(),
                "provider_kind": record.provider_kind.as_str(),
                "key_configured": true,
            }),
        );
        Ok((record.view(), is_new))
    })();
    match result {
        Ok((view, true)) => created(view),
        Ok((view, false)) => ok(view),
        Err(error) => err(error, debug),
    }
}

async fn delete_my_key(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<AiState>,
    req: HttpRequest,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_AI_USE)?;
        let user_id = own_user_id(&caller)?;
        let (_, record) = state.personal_keys.remove(&user_id).ok_or_else(|| {
            UnifiedError::not_found("No personal API key is configured for this user")
        })?;
        emit_audit(
            &ctx,
            EVENT_AI_PERSONAL_KEY_DELETED,
            &caller.actor,
            &format!("ai-personal-key:{}", record.id),
            json!({
                "id": record.id.to_string(),
                "user_id": user_id.to_string(),
                "provider_kind": record.provider_kind.as_str(),
            }),
        );
        Ok(json!({ "deleted": true, "key_id": record.id.to_string() }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn test_my_key(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<AiState>,
    req: HttpRequest,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let caller = match caller(&req) {
        Ok(caller) => caller,
        Err(error) => return err(error, debug),
    };
    if let Err(error) = require_permission(&ctx, &caller.role, PERM_AI_USE) {
        return err(error, debug);
    }
    let user_id = match own_user_id(&caller) {
        Ok(user_id) => user_id,
        Err(error) => return err(error, debug),
    };
    let key = match state.personal_keys.get(&user_id) {
        Some(entry) => entry.value().clone(),
        None => {
            return err(
                UnifiedError::not_found("No personal API key is configured for this user"),
                debug,
            )
        }
    };
    if !key.enabled {
        return err(
            UnifiedError::bad_request("Personal API key is disabled"),
            debug,
        );
    }
    // A synthetic connection carries the key's kind and model so the
    // existing adapters can test it; it is never stored. The test
    // itself runs through the allowlist gate like any other call.
    let (base_url, model) = match resolve_base_url_and_model(
        key.provider_kind,
        &key.base_url,
        &key.model,
    ) {
        Ok(resolved) => resolved,
        Err(error) => return err(error, debug),
    };
    let connection = AiProviderConnection {
        id: key.id,
        name: format!("personal-key:{}", key.id),
        kind: key.provider_kind,
        base_url,
        model: model.clone(),
        api_key_encrypted: None,
        key_fingerprint: Some(key.key_fingerprint.clone()),
        enabled: true,
        rate_limit_per_minute: DEFAULT_RATE_LIMIT_PER_MINUTE,
        created_at: key.created_at,
        updated_at: key.updated_at,
    };
    let company_id = resolve_caller_company(&ctx, &req, &caller);
    if let Err(error) = authorize_ai_call(
        &ctx,
        &state,
        &caller,
        company_id,
        &AiCallRequest {
            use_case: "test",
            model: &model,
            provider_id: key.id,
            provider_kind: key.provider_kind.as_str(),
            estimated_tokens: 0,
            use_personal_key: true,
            product_id: resolve_caller_product(&req),
        },
    ) {
        return err(error, debug);
    }
    let started = Instant::now();
    let secret = encryption_secret(&ctx);
    let api_key = match decrypt_api_key(&secret, &key.api_key_encrypted) {
        Ok(api_key) => Some(api_key),
        Err(error) => return err(error, debug),
    };
    let adapter = match ai_provider_registry().build(&connection, api_key) {
        Ok(adapter) => adapter,
        Err(error) => return err(error, debug),
    };
    match adapter.test_connection().await {
        Ok(test) => {
            record_usage(
                &state,
                &connection,
                &caller.actor,
                Some(user_id),
                company_id,
                "test",
                None,
                test.latency_ms,
                None,
                true,
            );
            ok(test)
        }
        Err(error) => {
            let latency_ms = started.elapsed().as_millis() as u64;
            record_usage(
                &state,
                &connection,
                &caller.actor,
                Some(user_id),
                company_id,
                "test",
                None,
                latency_ms,
                Some(error.code),
                true,
            );
            ok(ProviderTestResult {
                provider_id: key.id,
                kind: key.provider_kind.as_str().to_string(),
                reachable: false,
                latency_ms,
                model_count: None,
                detail: error.message.clone(),
            })
        }
    }
}

async fn list_my_mcp(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<AiState>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_AI_USE)?;
        let user_id = own_user_id(&caller)?;
        let params = query.into_inner();
        let enabled_filter = params.filters.get("enabled").cloned();
        let search = params.search.clone().unwrap_or_default().to_lowercase();
        let mut items: Vec<AiUserMcpServerView> = state
            .personal_mcp_servers
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|server| server.user_id == user_id)
            .filter(|server| {
                if let Some(enabled) = &enabled_filter {
                    let expected = match enabled.as_str() {
                        "true" => true,
                        "false" => false,
                        _ => return false,
                    };
                    if server.enabled != expected {
                        return false;
                    }
                }
                if !search.is_empty() {
                    let haystack = format!("{} {}", server.name, server.endpoint_url)
                        .to_lowercase();
                    if !haystack.contains(&search) {
                        return false;
                    }
                }
                true
            })
            .map(|server| server.view())
            .collect();
        let sort = params.allowed_sort(&["name", "created_at"], "created_at");
        let descending = params.normalized().order == "desc";
        items.sort_by(|a, b| {
            let ordering = match sort.as_str() {
                "name" => a.name.cmp(&b.name),
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

async fn create_my_mcp(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<AiState>,
    req: HttpRequest,
    body: web::Json<CreatePersonalMcpInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let input = body.into_inner();
    let result: UnifiedResult<AiUserMcpServerView> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_AI_USE)?;
        let user_id = own_user_id(&caller)?;
        // In strict mode only an allowed user may configure servers.
        require_allowed_user(&state, user_id)?;
        let name = require_text(&input.name, "name", MAX_MCP_NAME_LEN)?;
        let endpoint = validate_mcp_endpoint_url(&input.endpoint_url)?;
        let token = match &input.auth_token {
            Some(token) => {
                let token = token.trim();
                if token.is_empty() || token.len() > MAX_SECRET_LEN {
                    return Err(UnifiedError::bad_request(format!(
                        "auth_token must be between 1 and {MAX_SECRET_LEN} characters"
                    )));
                }
                Some(token.to_string())
            }
            None => None,
        };
        let owned = state
            .personal_mcp_servers
            .iter()
            .filter(|entry| entry.value().user_id == user_id)
            .count();
        if owned >= MAX_PERSONAL_MCP_SERVERS {
            return Err(UnifiedError::bad_request(format!(
                "At most {MAX_PERSONAL_MCP_SERVERS} personal MCP servers per user"
            )));
        }
        let secret = encryption_secret(&ctx);
        let (auth_token_encrypted, token_fingerprint) = match &token {
            Some(plain) => (
                Some(encrypt_api_key(&secret, plain)),
                Some(key_fingerprint(plain)),
            ),
            None => (None, None),
        };
        let now = Utc::now();
        let server = AiUserMcpServer {
            id: Uuid::now_v7(),
            user_id,
            name,
            endpoint_url: endpoint.to_string(),
            auth_token_encrypted,
            token_fingerprint,
            enabled: input.enabled.unwrap_or(true),
            created_at: now,
            updated_at: now,
        };
        state
            .personal_mcp_servers
            .insert(server.id, server.clone());
        Ok(server.view())
    })();
    match result {
        Ok(view) => created(view),
        Err(error) => err(error, debug),
    }
}

async fn get_my_mcp(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<AiState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<AiUserMcpServerView> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_AI_USE)?;
        let user_id = own_user_id(&caller)?;
        let id = parse_uuid(&path.into_inner(), "personal MCP server")?;
        Ok(load_own_mcp_server(&state, user_id, id)?.view())
    })();
    match result {
        Ok(view) => ok(view),
        Err(error) => err(error, debug),
    }
}

async fn update_my_mcp(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<AiState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdatePersonalMcpInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let input = body.into_inner();
    let result: UnifiedResult<AiUserMcpServerView> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_AI_USE)?;
        let user_id = own_user_id(&caller)?;
        let id = parse_uuid(&path.into_inner(), "personal MCP server")?;
        let mut server = load_own_mcp_server(&state, user_id, id)?;
        if let Some(name) = &input.name {
            server.name = require_text(name, "name", MAX_MCP_NAME_LEN)?;
        }
        if let Some(endpoint_url) = &input.endpoint_url {
            server.endpoint_url = validate_mcp_endpoint_url(endpoint_url)?.to_string();
        }
        if let Some(token_change) = &input.auth_token {
            let secret = encryption_secret(&ctx);
            match token_change {
                None => {
                    server.auth_token_encrypted = None;
                    server.token_fingerprint = None;
                }
                Some(plain) if plain.trim().is_empty() => {
                    server.auth_token_encrypted = None;
                    server.token_fingerprint = None;
                }
                Some(plain) => {
                    let plain = plain.trim();
                    if plain.len() > MAX_SECRET_LEN {
                        return Err(UnifiedError::bad_request(format!(
                            "auth_token must be at most {MAX_SECRET_LEN} characters"
                        )));
                    }
                    server.auth_token_encrypted = Some(encrypt_api_key(&secret, plain));
                    server.token_fingerprint = Some(key_fingerprint(plain));
                }
            }
        }
        if let Some(enabled) = input.enabled {
            server.enabled = enabled;
        }
        server.updated_at = Utc::now();
        state
            .personal_mcp_servers
            .insert(server.id, server.clone());
        Ok(server.view())
    })();
    match result {
        Ok(view) => ok(view),
        Err(error) => err(error, debug),
    }
}

async fn delete_my_mcp(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<AiState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_AI_USE)?;
        let user_id = own_user_id(&caller)?;
        let id = parse_uuid(&path.into_inner(), "personal MCP server")?;
        let _server = load_own_mcp_server(&state, user_id, id)?;
        state.personal_mcp_servers.remove(&id);
        Ok(json!({ "deleted": true, "server_id": id.to_string() }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn usage_by_user(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<AiState>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        // Usage readers may query any user; everyone else sees only
        // their own records.
        let can_read_usage = check_capability(&ctx, &caller.role, PERM_AI_READ_USAGE);
        if !can_read_usage {
            require_permission(&ctx, &caller.role, PERM_AI_USE)?;
        }
        let params = query.into_inner();
        let own_id = caller_user_id(&caller);
        let target = match params.filters.get("user_id") {
            Some(raw) => {
                let target = parse_uuid(raw, "user")?;
                if Some(target) != own_id {
                    if !can_read_usage {
                        return Err(UnifiedError::forbidden(
                            "Reading another user's AI usage requires ai:read_usage",
                        ));
                    }
                    if let Some(company) = resolve_caller_company(&ctx, &req, &caller) {
                        match state.user_policies.get(&target) {
                            Some(policy) if policy.company_id == company => {}
                            _ => {
                                return Err(UnifiedError::forbidden(
                                    "AI usage of a user outside your company is not visible",
                                ))
                            }
                        }
                    }
                }
                target
            }
            None => own_id.ok_or_else(|| {
                UnifiedError::bad_request("Caller id is not a user id")
            })?,
        };
        let mut items: Vec<AiUsageRecord> = state
            .usage
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|record| record.user_id == Some(target) && usage_matches(record, &params))
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

async fn usage_by_company(
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
        // A company-scoped caller is pinned to their own company; a
        // platform usage reader names the company in the query.
        let target = match resolve_caller_company(&ctx, &req, &caller) {
            Some(company) => company,
            None => {
                let raw = params.filters.get("company_id").ok_or_else(|| {
                    UnifiedError::bad_request(
                        "company_id query parameter is required for platform usage readers",
                    )
                })?;
                parse_uuid(raw, "company")?
            }
        };
        let mut items: Vec<AiUsageRecord> = state
            .usage
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|record| record.company_id == Some(target) && usage_matches(record, &params))
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
