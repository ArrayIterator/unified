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

pub mod adapters;
pub mod capabilities;
pub mod constants;
pub mod context;
mod crypto;
pub mod gating;
mod handlers_drafts;
mod handlers_gating;
mod handlers_personal;
mod handlers_providers;
mod handlers_usage;
pub mod helpers;
pub mod inputs;
pub mod module;
pub mod providers;
pub mod state;
pub mod types;
pub mod validation;

pub use self::adapters::{AnthropicAdapter, OpenAiCompatibleAdapter};
pub use self::capabilities::CapabilityCallInput;
pub use self::capabilities::{AI_CAPABILITIES, AiCapability, AiCapabilityRegistry};
pub use self::capabilities::{ai_capability_registry, register_ai_capabilities, run_ai_capability};
pub use self::constants::{EVENT_AI_ACCESS_DENIED, EVENT_AI_PERSONAL_KEY_CREATED};
pub use self::constants::{EVENT_AI_ACCESS_POLICY_CHANGED, EVENT_AI_GATING_CHANGED};
pub use self::constants::{EVENT_AI_CALL_COMPLETED, EVENT_AI_CALL_FAILED, EVENT_AI_DRAFT_CREATED};
pub use self::constants::{EVENT_AI_CONTEXT_REQUESTED, EVENT_SITE_CONTEXT_REQUESTED};
pub use self::constants::{EVENT_AI_DRAFT_APPROVED, EVENT_AI_DRAFT_REJECTED};
pub use self::constants::{EVENT_AI_PERSONAL_KEY_DELETED, EVENT_AI_PERSONAL_KEY_ROTATED};
pub use self::constants::{EVENT_AI_PRODUCT_INCLUDES_AI, EVENT_USER_COMPANY_REQUESTED};
pub use self::constants::{EVENT_AI_PROVIDER_CREATED, EVENT_AI_PROVIDER_DELETED};
pub use self::constants::{EVENT_CHAT_CONTEXT_REQUESTED, PERM_AI_MANAGE_PROVIDERS, PERM_AI_USE};
pub use self::constants::{EVENT_KNOWLEDGEBASE_SEARCH, EVENT_TICKET_CONTEXT_REQUESTED};
pub use self::constants::{PERM_AI_APPROVE, PERM_AI_READ_USAGE};
pub use self::context::{assemble_shared_context, parse_grounding_sources};
pub use self::gating::{AiAuthorization, AiCallRequest, AuthorizedPersonalKey, EffectiveGateQuery};
pub use self::gating::{UpdateCompanyGateInput, UpdateGlobalSettingsInput, UpdateUserGateInput};
pub use self::helpers::{Caller, caller, require_permission, resolve_caller_company};
pub use self::inputs::{AnswerInput, CreateCompanyPolicyInput, UpdateCompanyPolicyInput};
pub use self::inputs::{CompleteInput, ContentDraftInput, EmailDraftInput, SupportDraftInput};
pub use self::inputs::{CreatePersonalMcpInput, UpdatePersonalMcpInput};
pub use self::inputs::{CreateProviderConnectionInput, UpdateProviderConnectionInput};
pub use self::inputs::{CreateUserPolicyInput, UpdateUserPolicyInput, UpsertPersonalKeyInput};
pub use self::module::{AiModule, register};
pub use self::providers::{AiProviderAdapter, CompletionCall, ProviderTestResult, RawCompletion};
pub use self::providers::{AiProviderFactory, AiProviderRegistry, ai_provider_registry};
pub use self::state::AiState;
pub use self::types::SharedContext;
pub use self::types::{AiCompanyPolicy, AiUsageRecord, AiUsageSummary, AiUserPolicy};
pub use self::types::{AiDraft, AiProviderCatalogEntry, DraftKind, DraftStatus, GroundingSource};
pub use self::types::{AiGateDecision, AiGateLayerOutcome, AiGlobalSettings, AiPersonalKey};
pub use self::types::{AiPersonalKeyView, AiUserMcpServer, AiUserMcpServerView, SiteContextInput};
pub use self::types::{AiProviderConnection, AiProviderConnectionView, AiProviderKind};
pub use self::validation::resolve_capability_company;
