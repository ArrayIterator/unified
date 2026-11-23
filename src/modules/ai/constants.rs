//! ai module - Event names, permissions and provider limits.

use std::time::Duration;

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
pub(crate) const PROVIDER_TIMEOUT_SECS: u64 = 30;

/// Longest provider connect timeout.
pub(crate) const PROVIDER_CONNECT_TIMEOUT_SECS: u64 = 10;

/// Longest `Retry-After` delay honoured from a provider.
pub(crate) const MAX_RETRY_AFTER: Duration = Duration::from_secs(120);

/// Default per-provider rate limit (calls per minute).
pub(crate) const DEFAULT_RATE_LIMIT_PER_MINUTE: u32 = 60;

/// Hardest per-provider rate limit an admin may configure.
pub(crate) const MAX_RATE_LIMIT_PER_MINUTE: u32 = 10_000;

/// Maximum prompt / instruction length in characters.
pub(crate) const MAX_PROMPT_LEN: usize = 20_000;

/// Maximum caller-supplied context text in characters.
pub(crate) const MAX_CONTEXT_TEXT_LEN: usize = 20_000;

/// Maximum name length for a provider connection.
pub(crate) const MAX_NAME_LEN: usize = 120;

/// Default completion token budget.
pub(crate) const DEFAULT_MAX_TOKENS: u32 = 1024;

/// Fixed token estimate for a provider / personal-key connection
/// test call: a test fetches the model list, it does not run a
/// completion, but it still spends provider traffic, so the cap
/// check must see a non-zero estimate instead of 0 (a 0 estimate
/// makes the cap check a no-op and the test traffic free).
pub(crate) const TEST_CALL_PROMPT_TOKENS: u32 = 128;
pub(crate) const TEST_CALL_COMPLETION_TOKENS: u32 = 128;

/// How long a cap reservation lives: a reservation is created at
/// authorization time and reconciled (removed) once the call's
/// actual usage is recorded. A reservation older than this is
/// stale — its call never recorded usage — and is ignored by cap
/// checks, so a leaked reservation cannot block caps forever.
pub(crate) const CAP_RESERVATION_TTL_SECS: u64 = 600;

/// Largest completion token budget a caller may request.
pub(crate) const MAX_MAX_TOKENS: u32 = 8192;

/// How many knowledgebase articles ground one answer.
pub(crate) const GROUNDING_LIMIT: u64 = 5;

/// User-Agent sent to provider APIs.
pub(crate) const USER_AGENT: &str = concat!("unified/", env!("CARGO_PKG_VERSION"), " ai");

/// Every use case an AI call can have. A policy's `allowed_features`
/// is a subset of this list; `test` is the connection-test use case.
/// `ai_capability` is the generic use case for per-module AI
/// capabilities run through `run_ai_capability`; a capability may
/// also declare its own module-scoped use case (for example
/// `exams.draft_questions`), which `validate_policy_features`
/// accepts when that capability is registered (see the AI
/// Capabilities Contract section).
pub(crate) const KNOWN_USE_CASES: &[&str] = &[
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
pub(crate) const MAX_POLICY_LIST_LEN: usize = 100;

/// Longest single model name in a policy list.
pub(crate) const MAX_MODEL_NAME_LEN: usize = 200;

/// Longest personal API key / MCP auth token accepted.
pub(crate) const MAX_SECRET_LEN: usize = 500;

/// Longest personal MCP server name.
pub(crate) const MAX_MCP_NAME_LEN: usize = 120;

/// Longest personal MCP endpoint URL.
pub(crate) const MAX_MCP_URL_LEN: usize = 2048;

/// Most personal MCP servers one user may configure.
pub(crate) const MAX_PERSONAL_MCP_SERVERS: usize = 10;

/// Hardest monthly token cap a policy or personal key may set.
pub(crate) const MAX_MONTHLY_TOKEN_CAP: u64 = 1_000_000_000;

/// Hardest monthly cost cap (USD) a policy or personal key may set.
pub(crate) const MAX_MONTHLY_COST_CAP_USD: f64 = 1_000_000.0;
