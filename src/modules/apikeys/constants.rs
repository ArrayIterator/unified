//! apikeys module - Key format, tiers, rotation limits and scope actions.

// ---------------------------------------------------------------------------
// Constants.
// ---------------------------------------------------------------------------

/// Prefix of every generated key. The prefix is part of the masked
/// display value, never a secret by itself.
pub const KEY_PREFIX: &str = "unf_";

/// Random characters after the prefix.
pub(crate) const KEY_RANDOM_LEN: usize = 40;

/// Default grace period (seconds) during which a rotated-out key keeps
/// working: 24 hours.
pub(crate) const DEFAULT_ROTATION_GRACE_SECS: i64 = 86_400;

/// Maximum grace period accepted on rotation: 30 days.
pub(crate) const MAX_ROTATION_GRACE_SECS: i64 = 2_592_000;

/// Maximum expiry accepted on creation: 10 years.
pub(crate) const MAX_EXPIRY_DAYS: u32 = 3_650;

/// Rate-limit tiers: requests per minute per key.
pub(crate) const TIER_LIMITS: &[(&str, u32)] =
    &[("standard", 60), ("elevated", 300), ("high", 1_000)];

/// Actions allowed in a scope.
pub(crate) const SCOPE_ACTIONS: &[&str] = &["read", "write", "admin", "*"];

/// Capabilities checked through the `role.check` hook (fail closed:
/// no answer means not allowed). `apikeys.view` gates read-only
/// management endpoints (list, get, usage, scopes, verify);
/// `apikeys.manage` gates every mutation (create, update, rotate,
/// revoke). Key management is admin-only: no management endpoint is
/// reachable without one of these capabilities.
pub(crate) const PERM_APIKEYS_VIEW: &str = "apikeys.view";
pub(crate) const PERM_APIKEYS_MANAGE: &str = "apikeys.manage";

/// Environment variable holding a dedicated server-side pepper for
/// new key hashes. When set (non-empty) it takes precedence over the
/// pepper stashed from the application secret at module init.
pub(crate) const APIKEY_PEPPER_ENV: &str = "UNIFIED_APIKEY_PEPPER";

/// Prefix marking a stored hash as HMAC-SHA256 under the server
/// pepper (see `service::hash_key_value_peppered`). Legacy keys
/// store a bare 64-hex SHA-256 digest with no prefix.
pub(crate) const HMAC_HASH_PREFIX: &str = "hmac1:";
