// KYC module — Know Your Customer: identity verification
// cases with linked identity documents (references only),
// an AI verification pipeline under a stored default
// verification context plus versioned extra instructions,
// a human admin review queue, risk levels as data, and a
// hash-chained audit trail.
//
// Routes live under `/api/v1/kyc`:
//
//   Cases:
//     GET/POST /cases — list cases / open a draft case.
//     GET/PATCH/DELETE /cases/{case_id}.
//     POST /cases/{case_id}/submit — draft -> submitted (or
//       more_info_requested -> submitted on re-submission).
//     GET  /cases/{case_id}/status — lightweight status read.
//     GET  /cases/{case_id}/audit — hash-chained audit trail.
//   Documents (metadata only — references and hashes, never
//   file bytes):
//     GET/POST /cases/{case_id}/documents — list / link.
//     GET/DELETE /cases/{case_id}/documents/{document_id}.
//   Verification and review (manager):
//     POST /cases/{case_id}/verify — run the AI pipeline.
//     POST /cases/{case_id}/approve — human approval.
//     POST /cases/{case_id}/reject — human rejection (reason
//       required).
//     POST /cases/{case_id}/request-more-info — reason
//       required, subject notified.
//     POST /cases/{case_id}/escalate.
//     POST /cases/{case_id}/expire.
//     GET  /admin/queue — review queue with counts by status.
//   AI:
//     POST /ai/{capability} — run a registered KYC AI
//       capability through the AI module's gated runner.
//   Configuration (manager):
//     GET/POST /document-types, GET/PATCH/DELETE
//       /document-types/{row_id}.
//     GET/POST /subject-types, GET/PATCH/DELETE
//       /subject-types/{row_id}.
//     GET/POST /contexts, GET/PATCH/DELETE
//       /contexts/{row_id}.
//     GET  /contexts/resolved — the active default context
//       merged with its active extra instructions.
//     GET/POST /instructions, GET/PATCH/DELETE
//       /instructions/{row_id}.
//     GET/POST /risk-levels, GET/PATCH/DELETE
//       /risk-levels/{row_id}.
//     GET  /settings, PATCH /settings.
//
// HONESTY — READ THIS FIRST. What is real in this module:
// in-memory case, document, document-type, subject-type,
// verification-context, instruction, risk-level, audit, and
// settings management; the case status machine (draft ->
// submitted -> verifying -> submitted, plus
// more_info_requested, approved, rejected, expired,
// enforced by a single pure transition function); required
// documents per subject type, checked at submit; document
// changes invalidating any earlier AI verdict so the case
// must be re-verified; the AI verification pipeline, which
// resolves the tenant's active default verification context
// with its active extra instructions and sends the case's
// document metadata to the AI module's gated runner
// (`run_ai_capability`, capability
// `verify_identity_documents`), with the resolved context
// and instructions reaching the model as grounding through
// the `kyc.ai.verify_identity_documents_requested` hook;
// verdict parsing (verified / uncertain / rejected,
// confidence, reasons, failed checks) with an unparseable
// model output recorded as an uncertain verdict, never a
// silent success; risk scores computed from the verdict and
// resolved against the tenant's risk-level rows; the human
// review queue with approve / reject / request-more-info /
// escalate / expire, notifications to the subject through
// `notification.requested`, and a hash-chained audit trail;
// tenant scoping on every row with cross-tenant access
// reading as 404, never 403; and subjects seeing only their
// own cases. What is NOT built here: durable storage until
// the migration runs (the DashMap stores are authoritative;
// migration `2026-11-22-000115_kyc` is the persistence
// target); the AI provider itself (the ai module owns
// providers, gating, and usage — this module only declares
// capabilities and calls the runner); the MCP server itself
// (the mcp module owns servers, tools, and the JSON-RPC
// endpoint — this module only answers the `mcp.context.kyc`
// source through the MCP module's existing hook contract);
// document storage (the documents module owns files — this
// module stores references and hashes only); and email /
// push delivery (the notifications module owns delivery —
// this module only emits `notification.requested`).
//
// Default context + declared instructions — HOW IT WORKS.
// The MCP module has no per-module default-context
// declaration mechanism: it exposes servers, tools,
// resources, prompts, and `mcp.context.<source>` hook
// sources, but no registry where a module declares a
// default context or extra instructions. The declaration
// therefore lives inside this module, as data, and flows
// through the AI module's existing contracts:
//   - `KycVerificationContext` rows are the DEFAULT CONTEXT:
//     one active row per tenant, naming which fields and
//     document metadata the AI may see (`visible_fields`)
//     and which checks it must run (`checks` — values are
//     data). Content changes bump `version`.
//   - `KycInstruction` rows are ADDITIONAL INSTRUCTIONS
//     declared on top: admin-editable text, optionally
//     scoped to one context (NULL means tenant-global),
//     ordered by `priority`, and versioned — changing the
//     text creates a new row (version + 1) and deactivates
//     the old one, so history is never rewritten.
//   - `resolve_verification_context` (pure, in `types.rs`)
//     merges the active context with its active
//     instructions into the resolved context.
//   - The verify handler sends that resolved context to
//     `ai::run_ai_capability`; the runner emits
//     `kyc.ai.verify_identity_documents_requested`, and the
//     hook in `hooks.rs` answers with the resolved context,
//     the instruction texts, and the case's document
//     metadata as grounding, which the runner merges into
//     the system prompt. The stored verdict records the
//     context version and instruction versions it ran under.
//   - The MCP surface used is only the existing
//     `mcp.context.kyc` hook source: MCP servers can read
//     per-tenant case summaries (counts by status, recent
//     cases, no subject identity beyond the subject type).
// No file in the ai or mcp modules was modified.
//
// Actor identity: the caller is the server-verified caller
// from `unified_core::caller::require_caller` (user id plus
// server-resolved role, both required for capability checks),
// exactly like the forums module. Capability checks use the
// `role.check` hook and fail closed (no answer means not allowed).
// All handlers take `web::Data<Arc<AppContext>>`, because the
// server registers `Arc<AppContext>` (see
// `src/http/server.rs`).
//
// Events emitted on the hook bus:
//   - `kyc.case.created`, `kyc.case.submitted`,
//     `kyc.case.verified`, `kyc.case.approved`,
//     `kyc.case.rejected`, `kyc.case.more_info_requested`,
//     `kyc.case.escalated`, `kyc.case.expired`,
//     `kyc.case.deleted`
//   - `notification.requested` (case submitted, decision,
//     auto-approval, more information requested, expiry — in
//     the notifications module's payload shape).
// Events this module listens to:
//   - `user.deleted` — anonymizes the user's cases, linked
//     documents, and audit entries to `deleted-user`.
//   - `kyc.ai.verify_identity_documents_requested` —
//     grounds the AI verification run with the resolved
//     default context, the active extra instructions, and the
//     case's document metadata.
//   - `mcp.context.kyc` — serves per-tenant case summaries
//     to MCP servers (requires `tenant_id` in the payload;
//     without one it contributes nothing, so the MCP module
//     reports the gap honestly).
// No company onboarding events exist to listen to: the
// company module emits no events (no `emit` call and no
// hook registration in `src/modules/company/`), so there is
// nothing to subscribe to there.

pub mod ai_capabilities;
mod handlers_ai;
mod handlers_cases;
mod handlers_config;
mod handlers_documents;
mod handlers_review;
mod helpers;
mod hooks;
mod module;
pub mod state;
pub mod types;

pub use self::ai_capabilities::AI_CAPABILITIES;
pub use self::module::register;
pub use self::module::KycModule;
pub use self::state::KycState;
pub use self::types::CreateCaseRequest;
pub use self::types::CreateContextRequest;
pub use self::types::CreateDocumentTypeRequest;
pub use self::types::CreateInstructionRequest;
pub use self::types::CreateRiskLevelRequest;
pub use self::types::CreateSubjectTypeRequest;
pub use self::types::DecisionRequest;
pub use self::types::KycAiVerdict;
pub use self::types::KycAuditEntry;
pub use self::types::KycCase;
pub use self::types::KycCaseStatus;
pub use self::types::KycDocument;
pub use self::types::KycDocumentType;
pub use self::types::KycInstruction;
pub use self::types::KycRiskLevel;
pub use self::types::KycSettings;
pub use self::types::KycSubjectType;
pub use self::types::KycVerdictOutcome;
pub use self::types::KycVerificationContext;
pub use self::types::LinkDocumentRequest;
pub use self::types::RequestMoreInfoRequest;
pub use self::types::UpdateCaseRequest;
pub use self::types::UpdateContextRequest;
pub use self::types::UpdateDocumentTypeRequest;
pub use self::types::UpdateInstructionRequest;
pub use self::types::UpdateRiskLevelRequest;
pub use self::types::UpdateSettingsRequest;
pub use self::types::UpdateSubjectTypeRequest;
