// Cloudflare connector module.
//
// API-token authenticated access to the Cloudflare v4 API
// (https://api.cloudflare.com only — the module never calls any other
// host): account and zone listing, DNS record CRUD with strict
// validation, cache purge / development mode / cache level, and
// rate-limit handling that honours `Retry-After` on HTTP 429.
//
// The API token lives in this module's settings (persisted through the
// `settings.merge` hook under the `cloudflare` namespace, the same
// pattern the Auth Manager uses). It is never logged and never
// returned by any endpoint; status responses only say whether a token
// is configured.
//
// Interconnection happens only through the hook bus:
//   - `cloudflare.connected` after a successful connection test.
//   - `cloudflare.dns.changed` after a DNS record is created,
//     updated, or deleted (zone id, record id, type, name).
//   - `cloudflare.cache.purged` after a cache purge.
// The Audit module subscribes to the DNS and purge events, so every
// change and purge is recorded in the audit log.

mod accounts_zones;
mod cache_handlers;
mod client;
mod constants;
mod dns_handlers;
mod dns_validation;
mod helpers;
pub mod module;
mod requests;
pub mod state;
mod status_handlers;

// Re-exported from `state`.
pub use self::state::CfPurgeRecord;
pub use self::state::CfRateLimitState;
pub use self::state::CfTestResult;
pub use self::state::CloudflareState;
// Re-exported from `module`.
pub use self::module::register;
pub use self::module::CloudflareModule;
