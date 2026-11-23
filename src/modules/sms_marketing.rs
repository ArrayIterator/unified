// SMS Marketing module — admin-configurable SMS
// providers as data (a generic HTTP adapter posting JSON to
// the configured endpoint with the configured auth, plus a
// log-only test provider), plain-text templates with
// {{variable}} substitution and GSM-7 / UCS-2 segment
// counting, contact lists with tags and per-contact
// variables, campaigns with a draft / scheduled / sending /
// sent / paused / cancelled status machine and audience
// selection by list reference plus data segment filters
// plus explicit numbers, a send pass that blocks opted-out
// numbers and enforces per-tenant and per-provider
// per-minute limits at enqueue time, per-message delivery
// tracking with shared-secret delivery and inbound
// webhooks, opt-out keywords as data with a per-tenant
// opt-out list, and an append-only audit trail.
//
// Routes live under `/api/v1/sms-marketing`:
//
//   Providers:
//     GET/POST /providers — list / create.
//     GET/PATCH/DELETE /providers/{provider_id}.
//     POST /providers/{provider_id}/rotate-webhook-secret.
//     POST /providers/{provider_id}/test — dry-run config
//       check, no network call, no message sent.
//   Templates:
//     GET/POST /templates — list / create.
//     GET/PATCH/DELETE /templates/{template_id}.
//     POST /templates/{template_id}/preview — rendered body,
//       missing variables, segment count.
//   Contact lists:
//     GET/POST /contact-lists — list / create.
//     GET/PATCH/DELETE /contact-lists/{list_id}.
//     GET/POST /contact-lists/{list_id}/contacts.
//     DELETE /contact-lists/{list_id}/contacts/{contact_id}.
//   Campaigns:
//     GET/POST /campaigns — list / create.
//     GET/PATCH/DELETE /campaigns/{campaign_id}.
//     POST /campaigns/{campaign_id}/schedule,
//       /cancel, /pause, /send-now.
//   Messages:
//     GET  /messages — list.
//     GET  /messages/{message_id}.
//     POST /messages/{message_id}/retry.
//   Opt-outs:
//     GET/POST /opt-outs — list / add.
//     DELETE /opt-outs/{opt_out_id} — re-opt-in.
//   Keywords:
//     GET/POST /opt-out-keywords — list / create.
//     PATCH/DELETE /opt-out-keywords/{keyword_id}.
//   Settings:
//     GET/PUT /settings/rate-limit — per-tenant sends per
//       minute.
//   Webhooks (public, shared-secret gated):
//     POST /webhooks/delivery/{provider_id}.
//     POST /webhooks/inbound/{provider_id}.
//   Audit:
//     GET  /audit — list the audit trail.
//   AI:
//     POST /ai/{capability} — one registered capability
//       through the AI module's gated runner.
//
// HONESTY — READ THIS FIRST. What is real in this module:
// in-memory provider, template, contact-list, contact,
// campaign, message, opt-out, keyword, rate-limit, and
// audit management; providers as data (no real provider
// name or credential is baked in — the only kinds are the
// generic `http` adapter and the `log_only` test provider);
// credentials write-only and encrypted with the holidays
// module's `v1.` HMAC-keystream format under
// `app.secret_key`, webhook secrets hash-stored with the
// raw secret returned exactly once; templates as plain text
// with `{{variable}}` substitution (missing variables are
// reported, never silently blanked) and exact GSM-7 / UCS-2
// segment math; campaigns with the status machine enforced
// as a pure function; audience resolution from list
// references plus data segment filters plus explicit
// numbers, deduplicated; a send pass that blocks opted-out
// numbers at send time, enforces per-tenant and
// per-provider per-minute limits at enqueue time (over-limit
// recipients stay queued, never dropped), and dispatches
// through the adapter with per-message status rows;
// delivery-status and inbound webhooks gated by the
// provider's shared secret; opt-out keywords as data rows
// (seeded defaults are data, matching always reads the
// rows); tenant scoping on every row with cross-tenant
// access reading as 404, never 403; hook events for
// campaign creation / scheduling / sending, message sending
// / delivery / failure, opt-out addition, and provider
// creation; `user.deleted` anonymization of identity fields
// and acknowledgement of `marketing.campaign.sent`. What is
// NOT built here: durable storage until the migration runs
// (the DashMap stores are authoritative; migration
// `2026-11-22-000121_sms_marketing` is the persistence
// target); live delivery — the HTTP adapter really posts to
// the configured endpoint, but no provider and no credential
// is seeded, so nothing can be delivered until a tenant
// configures a real provider, and the log-only provider
// never touches the network; a scheduler — `schedule` only
// records `send_at` and emits `sms.campaign.scheduled` for
// a scheduler to pick up, and this module runs none;
// billing — no price, cost, or charge is stored or
// returned anywhere, so `f64` never appears; any check that
// a contact-list id or recipient number names a real person
// in another module — contacts are this module's own data
// rows, and the marketing module's segments are email
// segments, so SMS audience selection deliberately does not
// reuse them (the marketing module is not duplicated: it
// owns email campaigns, sequences, and scores, and this
// module only listens to its `marketing.campaign.sent`
// completion event).
//
// Public-access gate: the two webhook routes are public in
// `route_info()` because a provider cannot hold a user
// session; the shared webhook secret is the credential, and
// a provider with no secret configured rejects every
// callback. Like the referral module's token-gated public
// link, these routes also rely on the API Manager's
// admin-managed `ApiAccessRule` records (default deny): an
// administrator must create the matching rule for each
// webhook path. Do NOT treat the absence of a capability
// check on these routes as an open door: without the rule
// the API Manager denies the call, and without the secret
// the handler answers 401.
//
// Actor identity: the caller is the server-verified caller from
// `unified_core::caller::require_caller` (user id plus
// server-resolved role, both required for capability checks),
// exactly like the sign module. Capability checks use the
// `role.check` hook and fail closed (no answer means not allowed).
// Handler signatures take `web::Data<Arc<AppContext>>` because
// the server registers `Arc<AppContext>` (see
// `src/http/server.rs`: `run_server(ctx: Arc<AppContext>)`);
// `web::Data<AppContext>` would fail to extract.
//
// Events emitted on the hook bus:
//   - `sms.provider.created`
//   - `sms.campaign.created`, `sms.campaign.scheduled`,
//     `sms.campaign.sent`
//   - `sms.message.sent`, `sms.message.delivered`,
//     `sms.message.failed`
//   - `sms.optout.added`
//   - `notification.requested` (helper available; no handler
//     sends one today — delivery belongs to the notifications
//     module when a future flow needs it).
// Events this module listens to:
//   - `user.deleted` — anonymizes the identity fields
//     (`created_by` / `updated_by` / `actor`) of every row
//     the user owns to `deleted-user`; rows are never
//     deleted. Recipient phone numbers cannot be attributed
//     to the deleted user: `user.deleted` carries only the
//     user id (verified from the users module), and no row
//     in this module links a user id to a phone number, so
//     no recipient row is redacted.
//   - `marketing.campaign.sent` — acknowledges the email
//     campaign's completion with its counts; the payload
//     carries no tenant (verified from the marketing
//     service), so no tenant-scoped row is written.
//     `marketing.campaign.send` is not subscribed to: it is
//     an inner event name carried inside a
//     `scheduler.enqueue` envelope, not a directly
//     subscribable hook event.
//   - `ai.summarize.requested`, `ai.readiness.requested` —
//     answer with the campaign's stored facts.

pub mod adapter;
pub mod ai_capabilities;
mod crypto;
mod handlers_ai;
mod handlers_campaigns;
mod handlers_lists;
mod handlers_messages;
mod handlers_optouts;
mod handlers_providers;
mod handlers_settings;
mod handlers_templates;
mod helpers;
mod hooks;
mod module;
pub mod state;
pub mod types;

pub use self::ai_capabilities::AI_CAPABILITIES;
pub use self::module::SmsMarketingModule;
pub use self::module::register;
pub use self::state::SmsMarketingState;
pub use self::types::{
    AuditRecord, AuthType, CampaignStatus, CreateCampaignRequest, CreateContactListRequest,
    CreateContactRequest, CreateKeywordRequest, CreateOptOutRequest, CreateProviderRequest,
    CreateTemplateRequest, KeywordKind, MessageStatus, ProviderKind, SegmentEncoding, SegmentInfo,
    SmsCampaign, SmsContact, SmsContactList, SmsMessage, SmsOptOut, SmsOptOutKeyword, SmsProvider,
    SmsRateLimit, SmsTemplate, UpdateCampaignRequest, UpdateContactListRequest,
    UpdateKeywordRequest, UpdateProviderRequest, UpdateRateLimitRequest, UpdateTemplateRequest,
    billable_units, campaign_can_transition, encoding_of, is_gsm7, match_keyword,
    message_can_transition, normalize_phone, remaining_sends, render_template,
    segment_filter_matches, segment_info, sends_allowed, template_variables,
};
