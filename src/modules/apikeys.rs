// API Keys module — enterprise API key management.
//
// Routes under `/api/v1/apikeys`:
//
//   GET    /                  list keys (paginated, filterable)
//   POST   /                  create a key (the full key is returned ONCE)
//   GET    /scopes            scope catalog: registered modules + actions
//   GET    /{id}              get one key (masked, never the secret)
//   PUT    /{id}              update name, scopes, rate tier, expiry
//   POST   /{id}/rotate       issue a replacement key, grace period for old
//   POST   /{id}/revoke       revoke a key immediately
//   GET    /{id}/usage        usage statistics for one key
//   POST   /verify            verify a presented key, return its scopes/tier
//
// Storage model: only the HMAC-SHA256 digest of a key (peppered, stored as
// `hmac1:<hex>`) is stored for new keys, plus a masked prefix (`unf_ab12...`)
// and the last four characters. Keys issued before the pepper was introduced
// are still stored as a plain SHA-256 hex digest and keep authenticating
// (dual-read); rotating one migrates it to the peppered form. The full
// key value exists only in the create/rotate response and is never
// logged and never returned again. This store is the single source of
// truth for API keys: the public client API module keeps no key store
// of its own and delegates its key endpoints to the service functions
// in this file (`create_api_key`, `all_key_views`, `revoke_api_key`).
// The same functions back this module's own HTTP handlers, so both
// surfaces always read and write the same keys.
//
// Scopes look like `module:action`, for example `users:read` or
// `store:write`. The module part is validated against the registered
// module ids, so a scope can never name a module that does not exist.
// The action part is one of `read`, `write`, `admin`, or `*`.
//
// Hooks emitted: `apikey.created`, `apikey.rotated`, `apikey.revoked`,
// `apikey.used` (usage tracking). Auth middleware calls
// `authenticate_key` to validate a presented key and `note_used` to
// record usage; see the middleware snippet in the coordinator report.

pub mod apikey_events;
pub mod constants;
mod handlers;
pub mod module;
pub mod service;
mod store;
pub mod types;

pub use self::constants::KEY_PREFIX;
pub use self::module::{ApiKeysModule, register};
pub use self::service::update_api_key_access;
pub use self::service::{CreateApiKeyParams, all_key_views, create_api_key, revoke_api_key};
pub use self::service::{authenticate_key, hash_key_value, key_has_scope, note_used};
pub use self::types::AuthenticatedKey;
pub use self::types::{ApiKeyCreatedView, ApiKeyRecord, ApiKeyStatus, ApiKeyView};
