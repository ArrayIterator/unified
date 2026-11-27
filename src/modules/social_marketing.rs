// Social Marketing module - admin-defined social
// networks as data (API base, auth type, publish path,
// supported post types, text and media limits, rate
// limit), connected accounts with write-only encrypted
// credentials, token expiry, and a metrics-ingest webhook
// secret, scheduled posts with a draft / scheduled /
// publishing / published / failed / cancelled status
// machine publishing once per target account through the
// generic HTTP adapter or the log-only test adapter with
// one stored result per account, campaigns grouping posts
// with a goal as data and live post-count and metrics
// rollups, and analytics metrics that arrive only through
// the provider verification-token webhook or manual entry
// (never live-fetched), with an append-only audit trail.
//
// Routes live under `/api/v1/social-marketing`:
//
//   Networks:
//     GET/POST /networks - list / create.
//     GET/PATCH/DELETE /networks/{network_id}.
//   Accounts:
//     GET/POST /accounts - list / create.
//     GET/PATCH/DELETE /accounts/{account_id}.
//     POST /accounts/{account_id}/connect, /disconnect,
//       /rotate-webhook-secret, /test.
//     GET  /accounts/{account_id}/rollup.
//   Posts:
//     GET/POST /posts - list / create.
//     GET/PATCH/DELETE /posts/{post_id}.
//     POST /posts/{post_id}/schedule, /publish-now,
//       /cancel, /retry.
//     POST /posts/{post_id}/metrics - record one metric
//       by hand.
//   Campaigns:
//     GET/POST /campaigns - list / create.
//     GET/PATCH/DELETE /campaigns/{campaign_id}.
//     GET  /campaigns/{campaign_id}/posts.
//     GET  /campaigns/{campaign_id}/rollup.
//   Metrics:
//     GET  /metrics - list metric rows.
//     POST /metrics-ingest/{account_id} - public,
//       webhook-secret gated.
//   Audit:
//     GET  /audit - list the audit trail.
//   AI:
//     POST /ai/{capability} - one registered capability
//       through the AI module's gated runner.
//
// HONESTY - READ THIS FIRST. What is real in this module:
// in-memory network, account, post, campaign, metric, and
// audit management; networks as data (no real network name
// or credential is baked in - the only kinds are the
// generic `http` adapter and the `log_only` test provider);
// account credentials write-only and encrypted with the
// holidays module's `v1.` HMAC-keystream format under
// `app.secret_key`, webhook secrets hash-stored with the
// raw secret returned exactly once; posts with the status
// machine enforced as a pure function, content limits
// enforced from the network row, media as document /
// storage ids only (bytes are never stored or fetched
// here), hashtags as normalized data; a publish pass that
// checks every target account (connected, enabled,
// unexpired token, decryptable credential), enforces the
// per-network per-minute limit before any publish, and
// dispatches once per account through the adapter with one
// stored result per account; campaigns with the status
// machine enforced as a pure function and live post-count
// and metrics rollups computed from the post and metric
// rows (integer counts only, engagement rate in basis
// points, no floating point); metrics that arrive only
// through the verification-token webhook or manual entry,
// upserted per (post, metric, source) so re-ingestion
// never double-counts; tenant scoping on every row with
// cross-tenant access reading as 404, never 403; hook
// events for account connection / disconnection, post
// scheduling / publishing / failure, and campaign creation;
// `user.deleted` anonymization of identity fields and
// acknowledgement of `marketing.campaign.created` and
// `marketing.campaign.sent`. What is NOT built here:
// durable storage until the migration runs (the DashMap
// stores are authoritative; migration
// `2026-11-22-000122_social_marketing` is the persistence
// target); live publishing - the HTTP adapter really posts
// to the configured endpoint, but no network, no account,
// and no credential is seeded, so nothing can be published
// until a tenant configures a real network and connects a
// real account, and the log-only network never touches the
// network; a scheduled-post runner - `schedule` only
// records `send_at` and emits `social.post.scheduled` for
// a scheduler to pick up, and this module runs none;
// live analytics fetching - metric rows are reported
// values only, and a network that never reports metrics
// shows zeroes, never invented numbers; billing - no
// price, cost, or charge is stored or returned anywhere,
// so `f64` never appears; any check that a media id names
// a real document - media ids are plain references, and
// the documents / storage modules own the bytes (the
// marketing module is not duplicated: it owns email
// campaigns, sequences, and scores, and this module only
// listens to its `marketing.campaign.created` and
// `marketing.campaign.sent` events).
//
// Public-access gate: the metrics-ingest route is public
// in `route_info()` because a network cannot hold a user
// session; the shared webhook secret is the credential,
// and an account with no secret configured rejects every
// callback. Like the SMS module's token-gated webhooks,
// this route also relies on the API Manager's
// admin-managed `ApiAccessRule` records (default deny): an
// administrator must create the matching rule for the
// ingest path. Do NOT treat the absence of a capability
// check on this route as an open door: without the rule
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
//   - `social.account.connected`, `social.account.disconnected`
//   - `social.post.scheduled`, `social.post.published`,
//     `social.post.failed`
//   - `social.campaign.created`
//   - `notification.requested` (helper available; no handler
//     sends one today - delivery belongs to the notifications
//     module when a future flow needs it).
// Events this module listens to:
//   - `user.deleted` - anonymizes the identity fields
//     (`created_by` / `actor`) of every row the user owns
//     to `deleted-user`; rows are never deleted. Account
//     handles are the account's public handle on its
//     network and `user.deleted` carries only the user id
//     (verified from the users module), so no handle is
//     redacted.
//   - `marketing.campaign.created`, `marketing.campaign.sent`
//     - acknowledge the email campaign's creation /
//     completion with its counts; the payloads carry no
//     tenant (verified from the marketing handlers and
//     service), so no tenant-scoped row is written.
//   - `ai.summarize.requested`, `ai.readiness.requested` -
//     answer with the campaign's stored facts.

pub mod adapter;
pub mod ai_capabilities;
mod crypto;
mod handlers_accounts;
mod handlers_ai;
mod handlers_audit;
mod handlers_campaigns;
mod handlers_metrics;
mod handlers_networks;
mod handlers_posts;
mod helpers;
mod hooks;
mod module;
pub mod state;
pub mod types;

pub use self::ai_capabilities::AI_CAPABILITIES;
pub use self::module::SocialMarketingModule;
pub use self::module::register;
pub use self::state::SocialMarketingState;
pub use self::types::{
    AuditRecord, AuthType, CampaignPostCounts, CampaignStatus, ConnectAccountRequest,
    CreateAccountRequest, CreateCampaignRequest, CreateNetworkRequest, CreatePostRequest,
    MetricIngestItem, MetricTotals, MetricsIngestRequest, PostStatus, ProviderKind, PublishResult,
    RecordMetricRequest, SchedulePostRequest, SocialAccount, SocialCampaign, SocialMetric,
    SocialNetwork, SocialPost, UpdateAccountRequest, UpdateCampaignRequest, UpdateNetworkRequest,
    UpdatePostRequest, campaign_can_transition, engagement_bps, is_future_schedule, is_https_url,
    is_valid_metric_name, is_valid_slug, merge_totals, normalize_hashtag, normalize_hashtags,
    post_can_transition, post_type_allowed, publishes_allowed, remaining_publishes, sum_metrics,
    text_fits_limit, token_expired, token_expiring_within,
};
