// Sign module - core e-signature: envelopes (signing
// requests) over multiple documents, signers with hashed
// signing tokens, signature capture with tamper evidence,
// reusable templates, data-driven reminder schedules, and
// completion certificates bound to a hash-chained audit trail.
//
// Routes live under `/api/v1/sign`:
//
//   Public (token-gated - the token is the credential; see the
//   public-access note below):
//     GET  /public/sign/{token} - sign page data.
//     POST /public/sign/{token} - record a signature.
//     POST /public/sign/{token}/decline - decline.
//     GET  /public/verify/{certificate_hash} - verify a
//       completion certificate.
//   Envelopes:
//     GET/POST /envelopes - list envelopes / create a draft.
//     GET/PATCH/DELETE /envelopes/{envelope_id}.
//     POST /envelopes/{envelope_id}/send - draft -> sent,
//       tokens issued (raw tokens returned once).
//     POST /envelopes/{envelope_id}/cancel - cancel.
//     POST /envelopes/{envelope_id}/void - void.
//     GET  /envelopes/{envelope_id}/certificate - the
//       completion certificate with its audit-chain check.
//     GET  /envelopes/{envelope_id}/audit - the hash-chained
//       audit trail.
//     POST /envelopes/{envelope_id}/remind - remind every
//       signer who has not signed yet.
//     GET  /reminders/due - envelopes whose next reminder is
//       due (read-only).
//   Signers:
//     GET/POST /envelopes/{envelope_id}/signers - list / add.
//     GET/PATCH/DELETE
//       /envelopes/{envelope_id}/signers/{signer_id}.
//     POST /envelopes/{envelope_id}/signers/{signer_id}/resend
//       - rotate the token and resend.
//   Templates:
//     GET/POST /templates - list templates / create one.
//     GET/PATCH/DELETE /templates/{template_id}.
//     POST /templates/{template_id}/use - build a draft
//       envelope from the template.
//   Settings:
//     GET  /settings - per-tenant sign settings.
//     PATCH /settings (manager).
//
// HONESTY - READ THIS FIRST. What is real in this module:
// in-memory envelope, signer, template, audit, certificate,
// and settings management; the envelope status machine (draft
// -> sent -> partially_signed -> completed, plus declined,
// expired, cancelled, voided) and the signer status machine
// (pending -> sent -> viewed -> signed, plus declined),
// enforced by single pure transition functions; sequential
// signing order enforced at sign time (a signer may only sign
// once every lower-order signer has signed); signing tokens
// following the contracts module pattern (48 random
// alphanumeric characters, raw token returned exactly once at
// send / resend / add, only the SHA-256 hash stored, resend
// rotates, tokens revoked on cancel / void / decline /
// completion); signature capture as hashes only (drawn
// strokes, typed names, and uploaded images are never stored
// or sent - the client sends the SHA-256 of the signature
// bytes plus the SHA-256 of the document as displayed, which
// must match the hash recorded for that document at send
// time); a hash-chained audit trail (every view, sign,
// decline, resend, reminder, send, and status change) with a
// verify function that recomputes the chain; reminder
// schedules as data (interval, cap, first-reminder delay) with
// pure due-date math; completion certificates whose hash is
// recomputed from the stored signing record at verify time;
// tenant scoping on every row with cross-tenant access reading
// as 404, never 403. What is NOT built here: durable storage
// until the migration runs (the DashMap stores are
// authoritative; migration `2026-11-22-000114_sign` is the
// persistence target); email / push delivery (the
// notifications module owns delivery - this module only emits
// `notification.requested` and `sign.reminder.due`); document
// rendering or PDF sealing (the documents module owns the
// files - this module links them by id and records hashes);
// and any provider integration beyond the built-in
// hash-and-audit provider (the provider name in settings only
// selects configuration).
//
// Public-access gate: the API Manager module enforces public
// access through its admin-managed `ApiAccessRule` records
// (default deny: an endpoint is public only when a rule exists
// with `public_access: true`, and denied outright when a rule
// exists with `allowed: false`). There is no reusable
// per-request "is this endpoint public?" helper to call from a
// handler - the check lives in the API Manager middleware /
// `resolve_public` path - so the public routes below rely on
// that gate: an administrator must create the matching
// `ApiAccessRule` rows for `GET /api/v1/sign/public/*` and
// `POST /api/v1/sign/public/*`. The routes are registered in
// `route_info()` with `route_version_support()` defaulting to
// `All`, so the API Manager's per-version registry picks them
// up. Do NOT treat the absence of a capability check on a
// public route as an open door: without the rule the API
// Manager denies the call. The token itself is a second gate:
// unknown, expired, and revoked tokens all answer 404 with the
// same message, so tokens cannot be probed apart.
//
// Actor identity: the caller is the server-verified caller from
// `unified_core::caller::require_caller` (user id plus
// server-resolved role, both required for capability checks),
// exactly like the forums module. Capability checks use the
// `role.check` hook and fail closed (no answer means not allowed).
// Handler signatures take `web::Data<Arc<AppContext>>` because
// the server registers `Arc<AppContext>` (see
// `src/http/server.rs`: `run_server(ctx: Arc<AppContext>)`);
// `web::Data<AppContext>` would fail to extract.
//
// Events emitted on the hook bus:
//   - `sign.envelope.created`, `sign.envelope.completed`,
//     `sign.envelope.declined`, `sign.envelope.cancelled`,
//     `sign.envelope.voided`, `sign.envelope.expired`
//   - `sign.signer.signed`, `sign.signer.resent`
//   - `sign.reminder.due` (reminder sent; the notifications
//     module delivers it)
//   - `notification.requested` (envelope sent, signer added to
//     a sent envelope, token resent, reminder sent, in the
//     notifications module's payload shape).
// Events this module listens to:
//   - `documents.deleted` - voids every live envelope
//     referencing the deleted document and revokes its tokens;
//     completed envelopes keep their certificate.
//   - `user.deleted` - anonymizes the user's envelopes and
//     templates to `deleted-user` and their audit entries;
//     signer rows are anonymized when the payload carries the
//     deleted user's email.

pub mod ai_capabilities;
mod handlers_envelopes;
mod handlers_settings;
mod handlers_signers;
mod handlers_templates;
mod helpers;
mod hooks;
mod module;
pub mod state;
pub mod types;

pub use self::ai_capabilities::AI_CAPABILITIES;
pub use self::module::SignModule;
pub use self::module::register;
pub use self::state::SignState;
pub use self::types::CreateEnvelopeRequest;
pub use self::types::CreateTemplateRequest;
pub use self::types::DeclineRequest;
pub use self::types::EnvelopeStatus;
pub use self::types::ReminderSchedule;
pub use self::types::SignAuditEntry;
pub use self::types::SignCertificate;
pub use self::types::SignEnvelope;
pub use self::types::SignRequest;
pub use self::types::SignSettings;
pub use self::types::SignSigner;
pub use self::types::SignTemplate;
pub use self::types::SignatureCapture;
pub use self::types::SignerInput;
pub use self::types::SignerStatus;
pub use self::types::TemplateField;
pub use self::types::TemplateSignerRole;
pub use self::types::UpdateEnvelopeRequest;
pub use self::types::UpdateSettingsRequest;
pub use self::types::UpdateTemplateRequest;
pub use self::types::UseTemplateRequest;
