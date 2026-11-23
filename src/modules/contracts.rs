// Contracts & E-Signature module — contract templates with
// placeholders, contracts with a validated status machine, an
// in-app signing flow with unguessable token links, signing order,
// a full audit trail, expiry / renewal alerts, and a billing
// linkage event when a contract becomes active.
//
// Routes live under `/api/v1/contracts`:
//
//   Templates:
//     GET/POST /templates — list templates / create a template.
//     GET/PATCH/DELETE /templates/{id}.
//     POST /templates/{id}/render — render a template body with
//       supplied values (values are escaped; unknown or missing
//       placeholders are rejected).
//   Contracts:
//     GET/POST / — list contracts / create a contract.
//     GET/PATCH/DELETE /{id}.
//     POST /{id}/send — draft -> sent (signing links are issued).
//     POST /{id}/activate — signed -> active (emits the billing
//       linkage event; no invoice is created here).
//     POST /{id}/void — void a contract (audited; terminal).
//     POST /{id}/terminate — end a contract early (audited).
//     POST /{id}/check-expiry — materialise expiry when the end
//       date passed (also driven by the scheduler listener).
//     POST /{id}/renew — create a new draft contract copying the
//       terms, parties, and value of this one.
//     GET  /{id}/audit — full audit trail of the contract.
//   Signers:
//     GET  /{id}/signers — list signers of a contract.
//     POST /{id}/signers — add a signer (draft / sent only).
//     DELETE /{id}/signers/{signer_id} — remove a pending signer.
//     POST /{id}/signers/{signer_id}/resend — issue a fresh
//       signing token (old token stops working; audited).
//   Signing links (token-gated; see the public-access note):
//     GET  /sign/{token} — read the contract a signer was asked
//       to sign, plus that signer's status.
//     POST /sign/{token} — sign (sequential order enforced).
//     POST /sign/{token}/decline — decline with a reason.
//   Settings:
//     GET/PATCH /settings — expiry alert offsets in days before
//       the end date (default 30/14/7/1).
//
// HONESTY — READ THIS FIRST. What is real in this module:
// in-memory template, contract, signer, and audit-trail
// management; server-side template rendering with escaped values
// and strict placeholder validation; the contract status machine
// (draft -> sent -> partially_signed -> signed -> active ->
// expired / terminated, with voided as an audited terminal state);
// token-based signing links (48 random characters, only the
// SHA-256 hash is stored, 30-day default expiry); sequential and
// parallel signing order; a sealed content hash recorded when the
// last signer signs, after which edits are rejected (the audited
// escape hatch is voiding the contract and renewing it into a new
// draft); an audit trail of every view / sign / decline / send /
// activate / void / terminate / expire / renew action carrying the
// actor, a SHA-256 hash of the IP, a SHA-256 hash of the user
// agent, and a timestamp — raw IPs and raw user agents are never
// stored; expiry alerts queued through `scheduler.enqueue` with
// kind `contracts.expiry_alert` and delivered through
// `notification.requested`; and a `billing.contract_activated`
// event so the billing module can create invoices. What is NOT
// built here: this is an in-app signing flow with a full audit
// trail, NOT a certified third-party e-signature provider — there
// are no qualified certificates and no legal eIDAS / QES claim;
// invoices are never created here (billing owns them); no email
// or push delivery happens here (the notifications module owns
// delivery — this module only emits `notification.requested`);
// and no PDF rendering of the signed contract exists.
//
// Public-access gate: the API Manager module enforces public
// access through its admin-managed `ApiAccessRule` records
// (default deny). The three `/sign/{token}` routes carry no
// capability check on purpose: the unguessable 48-character token
// in the path IS the credential, it expires, and every access is
// written to the audit trail. An administrator must create the
// matching `ApiAccessRule` rows for
// `GET|POST /api/v1/contracts/sign/*` for the links to work at
// all. Do NOT treat the absence of a capability check on those
// routes as an open door: without the rule the API Manager denies
// the call, and without the token nothing is disclosed (an
// unknown or expired token answers 404 either way, so tokens
// cannot be probed apart).
//
// Actor identity: until the auth module provides an extractor, the
// caller is identified by the `X-User-Id` header (required) and
// `X-Role` carries the role slug the capability check runs
// against, exactly like the ai module. Capability checks use the
// `role.check` hook and fail closed (no answer means not allowed).
//
// Events emitted on the hook bus:
//   - `contracts.created`, `contracts.sent`, `contracts.signed`,
//     `contracts.activated`, `contracts.voided`,
//     `contracts.terminated`, `contracts.expired`,
//     `contracts.renewed`, `contracts.deleted`
//   - `contracts.signer.signed`, `contracts.signer.declined`,
//     `contracts.signer.added`, `contracts.signer.removed`,
//     `contracts.signer.token_resent`
//   - `contracts.template.created`, `contracts.template.updated`,
//     `contracts.template.deleted`
//   - `billing.contract_activated` — payload `{"contract_id",
//     "value", "currency", "counterparty_name",
//     "counterparty_email", "starts_at", "ends_at"}` for the
//     billing module to invoice against. No invoice is created
//     here.
//   - `notification.requested` (expiry alerts) with the
//     notifications module's payload shape.
//   - `scheduler.enqueue` with `{"kind":
//     "contracts.expiry_alert", "payload": {"contract_id",
//     "days_before", "send_at"}}` when a contract is sent or
//     activated.
// Events this module listens to:
//   - `contracts.expiry_alert_requested` — the event the
//     scheduler emits when a `contracts.expiry_alert` job fires,
//     following its naming rule (`backup.run` ->
//     `backup.run_requested`); sends the due alert through
//     `notification.requested`, and marks the contract expired
//     (emitting `contracts.expired`) when its end date passed.
//
// Scheduler note: `contracts.expiry_alert` IS in the scheduler's
// `KNOWN_KINDS` list (scheduler/types.rs) with a dispatch arm in
// scheduler/service.rs emitting `contracts.expiry_alert_requested`
// with the alert payload (inspector defect C4 resolved).

// Split into one file per concern under `src/modules/contracts/`.
// This root only declares the submodules and re-exports the
// public API at its original paths.

pub mod ai_capabilities;
pub mod constants;
pub mod types;
pub mod requests;
pub mod state;
pub mod helpers;
pub mod templates;
pub mod contract_handlers;
pub mod signers;
pub mod signing_links;
pub mod settings_handlers;
pub mod scheduler;
pub mod module;

pub(crate) use self::constants::{MAX_NAME_LEN, MAX_TITLE_LEN, MAX_BODY_LEN, MAX_TERMS_LEN, MAX_EMAIL_LEN, MAX_REASON_LEN, MAX_PLACEHOLDERS, MAX_PLACEHOLDER_NAME_LEN, MAX_SIGNERS_PER_CONTRACT, MAX_ALERT_DAYS, SIGNING_TOKEN_LEN, DEFAULT_TOKEN_TTL_DAYS, DEFAULT_ALERT_DAYS, PERM_CONTRACTS_VIEW, PERM_CONTRACTS_MANAGE, EXPIRY_ALERT_REQUESTED};
pub use self::ai_capabilities::AI_CAPABILITIES;
pub use self::types::{ContractStatus, SignerStatus, SigningOrder, ContractTemplate, Contract, ContractSigner, ContractAuditEntry};
pub use self::requests::{CreateTemplateRequest, UpdateTemplateRequest, RenderTemplateRequest, CreateContractRequest, UpdateContractRequest, AddSignerRequest, SignRequest, DeclineRequest, VoidRequest, TerminateRequest, RenewRequest, UpdateSettingsRequest};
pub use self::state::{ContractsState};
pub(crate) use self::helpers::{Caller, caller, caller_company, require_tenant, check_capability, require_permission, parse_uuid, require_text, ok, created, err, paginate, valid_email, valid_price, valid_currency, hash_value, generate_token, escape_html, valid_placeholder_name, extract_placeholders, validate_template_body, render_template, validate_alert_days, request_ip, request_user_agent, record_audit, load_template, load_contract, load_signer, signers_of, effective_status, seal_content_hash, transition, reject_activation_if_ended, schedule_expiry_alerts, materialise_expiry, contract_view, signer_view};
pub(crate) use self::templates::{list_templates, create_template, get_template, update_template, delete_template, render_template_handler};
pub(crate) use self::contract_handlers::{list_contracts, create_contract, get_contract, update_contract, delete_contract, send_contract, activate_contract, void_contract, terminate_contract, check_expiry_contract, renew_contract, list_audit};
pub(crate) use self::signers::{list_signers, add_signer, remove_signer, resend_signer_token};
pub(crate) use self::signing_links::{load_signer_by_token, get_signing_link, sign_via_link, decline_via_link};
pub(crate) use self::settings_handlers::{get_settings, update_settings};
pub(crate) use self::scheduler::{on_expiry_alert_requested};
pub use self::module::{ContractsModule, register};
