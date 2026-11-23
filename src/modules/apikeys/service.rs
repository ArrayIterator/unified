//! apikeys module - Hashing, generation, scopes, authentication and key service.
//!
//! Key storage and migration notes:
//!
//! * New keys are stored as an HMAC-SHA256 digest under a
//!   server-side pepper (see [`hash_key_value_peppered`]), prefixed
//!   with `hmac1:`. The pepper comes from the `UNIFIED_APIKEY_PEPPER`
//!   environment variable when set, otherwise from the application
//!   secret stashed at module init (domain-separated before use).
//! * Keys issued before the pepper fix store a plain SHA-256 digest
//!   (see [`hash_key_value`]). Verification tries the peppered
//!   digest first and falls back to the plain digest, so
//!   already-issued keys keep working without re-issue. Rotate a
//!   legacy key to migrate it to a peppered hash. The fallback ends
//!   at the legacy sunset (see
//!   [`LEGACY_API_KEY_HASH_SUNSET_DEFAULT`]): on or after that date
//!   a legacy-hash key is rejected and its owner must rotate it.
//! * Rotating the pepper invalidates keys hashed under the previous
//!   pepper; rotate those keys to re-issue them. Never change the
//!   pepper silently in production without a rotation plan.

use unified_core::error::{UnifiedError, UnifiedResult};
use chrono::{DateTime, Utc};
use hmac::{Hmac, Mac};
use rand::Rng;
use rand::distr::Alphanumeric;
use sha2::{Digest, Sha256};
use std::sync::OnceLock;
use uuid::Uuid;

use super::apikey_events;
use super::constants::APIKEY_PEPPER_ENV;
use super::constants::HMAC_HASH_PREFIX;
use super::constants::KEY_PREFIX;
use super::constants::KEY_RANDOM_LEN;
use super::constants::MAX_EXPIRY_DAYS;
use super::constants::SCOPE_ACTIONS;
use super::constants::TIER_LIMITS;
use super::store::emit;
use super::store::hash_index;
use super::store::keys_store;
use super::types::ApiKeyCreatedView;
use super::types::ApiKeyRecord;
use super::types::ApiKeyStatus;
use super::types::ApiKeyView;
use super::types::AuthenticatedKey;

// ---------------------------------------------------------------------------
// Key generation and hashing.
// ---------------------------------------------------------------------------

/// Default sunset for legacy unpeppered API-key hashes: on or
/// after this date (UTC) a key stored under the plain SHA-256 digest
/// stops authenticating and its owner must rotate it (see
/// [`authenticate_key`]). Operators may move the date with
/// [`set_legacy_hash_sunset`] (wired from the application config at
/// module init); the default gives already-issued keys a rotation
/// window of about three months from the sunset's introduction.
pub(crate) const LEGACY_API_KEY_HASH_SUNSET_DEFAULT: &str = "2027-03-01";

/// Configured sunset for legacy unpeppered hashes, stashed at module
/// init. Set once; later calls are ignored, mirroring `KEY_PEPPER`.
static LEGACY_HASH_SUNSET: OnceLock<String> = OnceLock::new();

/// Stash the sunset date (`YYYY-MM-DD`) for legacy unpeppered
/// hashes. Called once from module init with the configured value.
pub(crate) fn set_legacy_hash_sunset(value: String) {
    let _ = LEGACY_HASH_SUNSET.set(value);
}

/// Active legacy-hash sunset date: the value stashed at module init,
/// or the default when init did not set one.
pub(crate) fn legacy_hash_sunset() -> String {
    LEGACY_HASH_SUNSET
        .get()
        .cloned()
        .unwrap_or_else(|| LEGACY_API_KEY_HASH_SUNSET_DEFAULT.to_string())
}

/// True when the legacy-hash sunset date has been reached. An
/// unparseable configured value fails closed (treated as reached),
/// so a malformed date can never silently extend the fallback.
pub(crate) fn legacy_hash_sunset_reached() -> bool {
    legacy_hash_sunset_reached_on(&legacy_hash_sunset(), Utc::now().date_naive())
}

/// Pure sunset comparison, unit-testable without touching the
/// process-global configured date.
fn legacy_hash_sunset_reached_on(sunset: &str, today: chrono::NaiveDate) -> bool {
    use chrono::NaiveDate;
    let Ok(date) = NaiveDate::parse_from_str(sunset, "%Y-%m-%d") else {
        return true;
    };
    today >= date
}

/// Server-side pepper stashed at module init from the application
/// secret (see `module.rs`). Set once; later calls are ignored so a
/// re-init cannot silently swap the pepper under live keys.
static KEY_PEPPER: OnceLock<String> = OnceLock::new();

/// Stash the server-side pepper for new key hashes. Called once from
/// module init with the application secret; the value is
/// domain-separated before use (see [`key_pepper`]), so the session
/// secret itself is never an HMAC key. Prefer the dedicated
/// `UNIFIED_APIKEY_PEPPER` environment variable in production.
pub(crate) fn set_key_pepper(pepper: String) {
    let _ = KEY_PEPPER.set(pepper);
}

/// Active pepper for new key hashes: the
/// `UNIFIED_APIKEY_PEPPER` environment variable when set and
/// non-empty, otherwise the value stashed at module init,
/// domain-separated with an `apikeys-v1:` prefix, otherwise empty
/// (HMAC still applies, but with no pepper -- set a pepper in
/// production).
pub(crate) fn key_pepper() -> String {
    if let Ok(pepper) = std::env::var(APIKEY_PEPPER_ENV) {
        if !pepper.trim().is_empty() {
            return pepper;
        }
    }
    match KEY_PEPPER.get() {
        Some(stashed) if !stashed.is_empty() => format!("apikeys-v1:{stashed}"),
        _ => String::new(),
    }
}

/// Legacy plain SHA-256 hex digest of a presented key value.
///
/// Kept for verification of keys issued before the pepper fix.
/// New keys must use [`hash_key_value_peppered`] instead; never
/// store a new key under this digest.
pub fn hash_key_value(presented: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(presented.as_bytes());
    {
        const HEX: &[u8; 16] = b"0123456789abcdef";
        let digest = hasher.finalize();
        let mut out = String::with_capacity(digest.len() * 2);
        for &byte in digest.iter() {
            out.push(HEX[(byte >> 4) as usize] as char);
            out.push(HEX[(byte & 0x0f) as usize] as char);
        }
        out
    }
}

/// HMAC-SHA256 hex digest of a presented key value under the active
/// server-side pepper (see [`key_pepper`]), prefixed with `hmac1:`.
/// This is the digest stored for every newly created or rotated key:
/// a stolen hash table alone cannot be used for offline verification
/// without the pepper. Verification accepts both this format and the
/// legacy plain digest (see [`authenticate_key`]).
pub fn hash_key_value_peppered(presented: &str) -> String {
    type HmacSha256 = Hmac<Sha256>;
    let mut mac =
        HmacSha256::new_from_slice(key_pepper().as_bytes()).expect("HMAC accepts any key length");
    mac.update(presented.as_bytes());
    let hex: String = {
        const HEX: &[u8; 16] = b"0123456789abcdef";
        let tag = mac.finalize().into_bytes();
        let mut out = String::with_capacity(tag.len() * 2);
        for &byte in tag.iter() {
            out.push(HEX[(byte >> 4) as usize] as char);
            out.push(HEX[(byte & 0x0f) as usize] as char);
        }
        out
    };
    format!("{HMAC_HASH_PREFIX}{hex}")
}

pub(crate) fn generate_key_value() -> String {
    let random: String = rand::rng()
        .sample_iter(&Alphanumeric)
        .take(KEY_RANDOM_LEN)
        .map(char::from)
        .collect();
    format!("{KEY_PREFIX}{random}")
}

pub(crate) fn tier_limit(tier: &str) -> Option<u32> {
    TIER_LIMITS
        .iter()
        .find(|(name, _)| *name == tier)
        .map(|(_, limit)| *limit)
}

/// Validate scope strings against the registered module ids.
/// `*` alone means every module and action; `module:*` means every
/// action on one module; `module:action` names one action.
pub(crate) fn validate_scopes(scopes: &[String], module_ids: &[String]) -> UnifiedResult<()> {
    if scopes.is_empty() {
        return Err(UnifiedError::bad_request(
            "scopes must contain at least one scope",
        ));
    }
    for scope in scopes {
        if scope == "*" {
            continue;
        }
        let (module, action) = scope.split_once(':').ok_or_else(|| {
            UnifiedError::bad_request(format!(
                "Scope \"{scope}\" must look like \"module:action\" (for example \"users:read\")"
            ))
        })?;
        if module != "*" && !module_ids.iter().any(|id| id == module) {
            return Err(UnifiedError::bad_request(format!(
                "Scope \"{scope}\" names unknown module \"{module}\""
            )));
        }
        if !SCOPE_ACTIONS.contains(&action) {
            return Err(UnifiedError::bad_request(format!(
                "Scope \"{scope}\" has unknown action \"{action}\"; expected one of {}",
                SCOPE_ACTIONS.join(", ")
            )));
        }
    }
    Ok(())
}

/// True when the key's scopes grant `module:action`. `*` and
/// `module:*` match broadly; `admin` on a module also grants `read`
/// and `write` on that module.
///
/// Note the deliberate asymmetry, documented here so operators are
/// not misled by the scope name: `module:admin` grants `read` and
/// `write` only. It never grants `admin` itself. Admin is a separate
/// action that must be granted explicitly (for example with
/// `module:*` or `*`); no scope implies it. The scope name is kept
/// as-is for backward compatibility with stored grants.
pub fn key_has_scope(scopes: &[String], module: &str, action: &str) -> bool {
    for scope in scopes {
        if scope == "*" {
            return true;
        }
        if let Some((scope_module, scope_action)) = scope.split_once(':') {
            if scope_module != "*" && scope_module != module {
                continue;
            }
            if scope_action == "*" || scope_action == action {
                return true;
            }
            if scope_action == "admin" && (action == "read" || action == "write") {
                return true;
            }
        }
    }
    false
}

// ---------------------------------------------------------------------------
// Authentication and usage tracking (public, for middleware).
// ---------------------------------------------------------------------------

/// Validate a presented key value. Returns the authenticated key on
/// success. A rotating key past its grace period is revoked lazily
/// here and then rejected; an expired key is marked expired and
/// rejected. A rotating key with no grace period at all is denied
/// outright (and revoked lazily): a rotation must always carry a
/// bounded grace period, so `grace_until: None` on a rotating record
/// can never authenticate indefinitely.
///
/// Lookup is dual-read for backward compatibility: the peppered
/// HMAC digest is tried first, then the legacy plain SHA-256
/// digest, so keys issued before the pepper fix keep working.
/// Call `note_used` after a successful authentication to record
/// usage.
pub fn authenticate_key(presented: &str) -> UnifiedResult<AuthenticatedKey> {
    let presented = presented.trim();
    if presented.is_empty() {
        return Err(UnifiedError::unauthorized("Missing API key"));
    }
    // New keys are stored under the peppered HMAC digest; keys
    // issued before the pepper fix are stored under the plain
    // SHA-256 digest. Try both so already-issued keys keep working
    // -- until the legacy sunset: on or after it, a legacy-hash
    // match is rejected with an error that tells the caller to
    // rotate the key for a peppered replacement.
    let peppered = hash_key_value_peppered(presented);
    let legacy = hash_key_value(presented);
    let (id, legacy_match) = match hash_index().get(&peppered) {
        Some(entry) => (*entry.value(), false),
        None => match hash_index().get(&legacy) {
            Some(entry) => (*entry.value(), true),
            None => {
                return Err(UnifiedError::unauthorized("Invalid API key"));
            }
        },
    };
    if legacy_match && legacy_hash_sunset_reached() {
        return Err(UnifiedError::unauthorized(
            "This API key uses a legacy unpeppered hash that is no longer accepted; rotate the key to receive a new peppered key",
        ));
    }
    let Some(mut record) = keys_store().get_mut(&id) else {
        return Err(UnifiedError::unauthorized("Invalid API key"));
    };
    let now = Utc::now();
    if record.status == ApiKeyStatus::Revoked {
        return Err(UnifiedError::unauthorized("API key has been revoked"));
    }
    if let Some(expires_at) = record.expires_at {
        if now > expires_at {
            record.status = ApiKeyStatus::Expired;
            return Err(UnifiedError::unauthorized("API key has expired"));
        }
    }
    if record.status == ApiKeyStatus::Rotating {
        match record.grace_until {
            Some(grace_until) if now > grace_until => {
                record.status = ApiKeyStatus::Revoked;
                emit(
                    apikey_events::REVOKED,
                    serde_json::json!({
                        "key_id": record.id,
                        "reason": "rotation_grace_expired",
                    }),
                );
                return Err(UnifiedError::unauthorized(
                    "API key grace period has ended; use the replacement key",
                ));
            }
            Some(_) => {}
            // No grace period on a rotating key: deny outright.
            // Rotation always sets a bounded grace period, so this
            // state is corrupt or tampered with; it must never
            // authenticate indefinitely.
            None => {
                record.status = ApiKeyStatus::Revoked;
                emit(
                    apikey_events::REVOKED,
                    serde_json::json!({
                        "key_id": record.id,
                        "reason": "rotation_grace_missing",
                    }),
                );
                return Err(UnifiedError::unauthorized(
                    "API key rotation has no grace period; use the replacement key",
                ));
            }
        }
    }
    Ok(AuthenticatedKey {
        id: record.id,
        owner_id: record.owner_id,
        scopes: record.scopes.clone(),
        rate_tier: record.rate_tier.clone(),
        requests_per_minute: record.requests_per_minute,
    })
}

/// Record one use of a key: bump the call count, stamp last-used, and
/// emit `apikey.used`. Called by auth middleware after a successful
/// `authenticate_key`. Never fails the request when the key is gone.
pub fn note_used(key_id: Uuid) {
    let mut last_used_at: Option<DateTime<Utc>> = None;
    let mut usage_count: u64 = 0;
    if let Some(mut record) = keys_store().get_mut(&key_id) {
        record.usage_count += 1;
        record.last_used_at = Some(Utc::now());
        last_used_at = record.last_used_at;
        usage_count = record.usage_count;
    } else {
        return;
    }
    emit(
        apikey_events::USED,
        serde_json::json!({
            "key_id": key_id,
            "last_used_at": last_used_at,
            "usage_count": usage_count,
        }),
    );
}

// ---------------------------------------------------------------------------
// Service functions.
//
// The API Manager's key endpoints delegate to these, so this module
// is the single source of truth for key storage, hashing, and
// verification. The HTTP handlers below are thin wrappers around the
// same functions. The store itself is the process-global static
// above, so the functions work regardless of whether this module is
// currently enabled as a module.
// ---------------------------------------------------------------------------

/// Parameters for creating a key through the service API.
pub struct CreateApiKeyParams {
    pub name: String,
    pub owner_id: Option<Uuid>,
    /// Scopes for the key. Required: `None` is rejected. New keys
    /// no longer default to `["*"]` -- an omitted scope list used to
    /// mint an omnipotent key. Callers must pass explicit scopes.
    pub scopes: Option<Vec<String>>,
    /// Rate tier name (`standard`, `elevated`, `high`).
    /// `None` means `standard`.
    pub rate_tier: Option<String>,
    /// Explicit per-minute limit; overrides the tier when present.
    pub requests_per_minute: Option<u32>,
    /// Lifetime in days from creation (1..=3650).
    /// `None` means the key never expires.
    pub expires_in_days: Option<u32>,
}

/// Create a key: validate the name, scopes, tier, and expiry,
/// generate the value, store only its peppered HMAC digest, and
/// return the view plus the full key value (shown exactly once).
/// `module_ids` is the list of registered module ids that scopes
/// are validated against. Scopes are required: an omitted scope
/// list is rejected instead of defaulting to `["*"]`.
pub fn create_api_key(
    params: CreateApiKeyParams,
    module_ids: &[String],
) -> UnifiedResult<ApiKeyCreatedView> {
    let name = params.name.trim().to_string();
    if name.is_empty() || name.chars().count() > 120 {
        return Err(UnifiedError::bad_request(
            "name is required and must be at most 120 characters",
        ));
    }
    let scopes = params.scopes.ok_or_else(|| {
        UnifiedError::bad_request(
            "scopes are required: pass explicit scopes for the new key; \
             new keys no longer default to [\"*\"]",
        )
    })?;
    validate_scopes(&scopes, module_ids)?;
    let rate_tier = params.rate_tier.unwrap_or_else(|| "standard".to_string());
    let mut requests_per_minute = tier_limit(&rate_tier).ok_or_else(|| {
        UnifiedError::bad_request("rate_tier must be one of: standard, elevated, high")
    })?;
    if let Some(limit) = params.requests_per_minute {
        if limit == 0 || limit > 100_000 {
            return Err(UnifiedError::bad_request(
                "requests_per_minute must be between 1 and 100000",
            ));
        }
        requests_per_minute = limit;
    }
    let expires_at = match params.expires_in_days {
        Some(days) => {
            if days == 0 || days > MAX_EXPIRY_DAYS {
                return Err(UnifiedError::bad_request(format!(
                    "expires_in_days must be between 1 and {MAX_EXPIRY_DAYS}"
                )));
            }
            Some(Utc::now() + chrono::Duration::days(i64::from(days)))
        }
        None => None,
    };
    let full_key = generate_key_value();
    let now = Utc::now();
    let record = ApiKeyRecord {
        id: Uuid::now_v7(),
        name,
        owner_id: params.owner_id,
        prefix: format!("{}...", &full_key[..8]),
        last4: full_key[full_key.len() - 4..].to_string(),
        key_hash: hash_key_value_peppered(&full_key),
        scopes,
        rate_tier,
        requests_per_minute,
        status: ApiKeyStatus::Active,
        expires_at,
        grace_until: None,
        rotated_from: None,
        rotated_to: None,
        created_at: now,
        updated_at: now,
        last_used_at: None,
        usage_count: 0,
    };
    let view = record.view();
    hash_index().insert(record.key_hash.clone(), record.id);
    keys_store().insert(record.id, record);
    emit(
        apikey_events::CREATED,
        serde_json::json!({
            "key_id": view.id,
            "name": view.name.clone(),
            "owner_id": view.owner_id,
            "scopes": view.scopes.clone(),
            "rate_tier": view.rate_tier.clone(),
            "expires_at": view.expires_at,
        }),
    );
    Ok(ApiKeyCreatedView {
        key: view,
        full_key,
    })
}

/// Public views of every stored key, in no particular order.
/// Callers sort and filter for their own listing.
pub fn all_key_views() -> Vec<ApiKeyView> {
    keys_store()
        .iter()
        .map(|entry| entry.value().view())
        .collect()
}

/// Revoke a key immediately: marks it revoked, clears any rotation
/// grace period, and emits `apikey.revoked`. An already-revoked key
/// is a 409 conflict; an unknown id is a 404.
pub fn revoke_api_key(id: Uuid) -> UnifiedResult<ApiKeyView> {
    let mut record = keys_store()
        .get_mut(&id)
        .ok_or_else(|| UnifiedError::not_found("API key not found"))?;
    if record.status == ApiKeyStatus::Revoked {
        return Err(UnifiedError::conflict("API key is already revoked"));
    }
    record.status = ApiKeyStatus::Revoked;
    record.grace_until = None;
    record.updated_at = Utc::now();
    let view = record.view();
    drop(record);
    emit(
        apikey_events::REVOKED,
        serde_json::json!({ "key_id": id, "reason": "manual_revoke" }),
    );
    Ok(view)
}

/// Update only the access-control fields of a key (scopes, rate
/// tier, per-minute limit). Used by the API Manager's key-access
/// admin endpoint; the API Keys module stays the single store for
/// key data, so the manager never mutates key records itself. Each
/// argument is `None` to leave that field unchanged. Validation
/// mirrors the HTTP update handler: scopes are checked against the
/// registered module ids, the tier must be a known tier, and the
/// limit must be between 1 and 100000. Revoked, expired, and
/// rotating keys cannot be updated: a key mid-rotation must keep
/// the scopes and tier it was rotated with.
pub fn update_api_key_access(
    id: Uuid,
    scopes: Option<Vec<String>>,
    rate_tier: Option<String>,
    requests_per_minute: Option<u32>,
    module_ids: &[String],
) -> UnifiedResult<ApiKeyView> {
    if let Some(scopes) = &scopes {
        validate_scopes(scopes, module_ids)?;
    }
    if let Some(tier) = &rate_tier {
        if tier_limit(tier).is_none() {
            return Err(UnifiedError::bad_request(
                "rate_tier must be one of: standard, elevated, high",
            ));
        }
    }
    if let Some(limit) = requests_per_minute {
        if limit == 0 || limit > 100_000 {
            return Err(UnifiedError::bad_request(
                "requests_per_minute must be between 1 and 100000",
            ));
        }
    }
    let mut record = keys_store()
        .get_mut(&id)
        .ok_or_else(|| UnifiedError::not_found("API key not found"))?;
    if record.status == ApiKeyStatus::Revoked
        || record.status == ApiKeyStatus::Expired
        || record.status == ApiKeyStatus::Rotating
    {
        return Err(UnifiedError::conflict(format!(
            "Cannot update an API key in status \"{}\"",
            record.status.as_str()
        )));
    }
    if let Some(scopes) = scopes {
        record.scopes = scopes;
    }
    if let Some(tier) = &rate_tier {
        record.rate_tier = tier.clone();
        if requests_per_minute.is_none() {
            record.requests_per_minute = tier_limit(tier).unwrap_or(60);
        }
    }
    if let Some(limit) = requests_per_minute {
        record.requests_per_minute = limit;
    }
    record.updated_at = Utc::now();
    Ok(record.view())
}

#[cfg(test)]
mod tests {
    use super::super::store::{hash_index, keys_store};
    use super::super::types::{ApiKeyRecord, ApiKeyStatus};
    use super::CreateApiKeyParams;
    use super::authenticate_key;
    use super::create_api_key;
    use super::generate_key_value;
    use super::hash_key_value;
    use super::hash_key_value_peppered;
    use super::key_has_scope;
    use super::tier_limit;
    use super::update_api_key_access;
    use super::validate_scopes;
    use crate::modules::apikeys::KEY_PREFIX;
    use crate::modules::apikeys::constants::HMAC_HASH_PREFIX;
    use chrono::Utc;
    use uuid::Uuid;

    fn scopes(items: &[&str]) -> Vec<String> {
        items.iter().map(|item| item.to_string()).collect()
    }

    fn module_ids() -> Vec<String> {
        scopes(&["users", "orders"])
    }

    /// Insert a record directly into the stores, for
    /// authentication tests that must control the stored hash format
    /// (legacy plain SHA-256 vs peppered HMAC) and lifecycle state.
    /// Returns the record id and the full key value that
    /// authenticates against it.
    fn seed_record_with(
        status: ApiKeyStatus,
        grace_until: Option<chrono::DateTime<Utc>>,
        legacy_hash: bool,
    ) -> (Uuid, String) {
        let full_key = generate_key_value();
        let digest = if legacy_hash {
            hash_key_value(&full_key)
        } else {
            hash_key_value_peppered(&full_key)
        };
        let now = Utc::now();
        let id = Uuid::now_v7();
        keys_store().insert(
            id,
            ApiKeyRecord {
                id,
                name: "seeded".to_string(),
                owner_id: None,
                prefix: format!("{}...", &full_key[..8]),
                last4: full_key[full_key.len() - 4..].to_string(),
                key_hash: digest.clone(),
                scopes: scopes(&["users:read"]),
                rate_tier: "standard".to_string(),
                requests_per_minute: 60,
                status,
                expires_at: None,
                grace_until,
                rotated_from: None,
                rotated_to: None,
                created_at: now,
                updated_at: now,
                last_used_at: None,
                usage_count: 0,
            },
        );
        hash_index().insert(digest, id);
        (id, full_key)
    }

    fn seed_record(legacy_hash: bool) -> (Uuid, String) {
        seed_record_with(ApiKeyStatus::Active, None, legacy_hash)
    }

    #[test]
    fn hash_is_stable_64_hex() {
        let first = hash_key_value("some-presented-key");
        assert_eq!(first.len(), 64);
        assert!(first.chars().all(|c| c.is_ascii_hexdigit()));
        assert_eq!(first, hash_key_value("some-presented-key"));
        assert_ne!(first, hash_key_value("some-other-key"));
    }

    #[test]
    fn scope_exact_match() {
        assert!(key_has_scope(&scopes(&["users:read"]), "users", "read"));
        assert!(!key_has_scope(&scopes(&["users:read"]), "users", "write"));
        assert!(!key_has_scope(&scopes(&["users:read"]), "orders", "read"));
    }

    #[test]
    fn scope_module_wildcard() {
        assert!(key_has_scope(&scopes(&["users:*"]), "users", "admin"));
        assert!(!key_has_scope(&scopes(&["users:*"]), "orders", "read"));
    }

    // `users:admin` grants `read` and `write` on `users`, but it
    // deliberately never grants `admin` itself: admin is a separate
    // action, never implied by the scope name (see `key_has_scope`).
    #[test]
    fn scope_admin_grants_read_and_write_but_not_admin() {
        assert!(key_has_scope(&scopes(&["users:admin"]), "users", "read"));
        assert!(key_has_scope(&scopes(&["users:admin"]), "users", "write"));
        assert!(!key_has_scope(&scopes(&["users:admin"]), "users", "admin"));
    }

    #[test]
    fn scope_star_grants_everything() {
        assert!(key_has_scope(&scopes(&["*"]), "anything", "read"));
    }

    #[test]
    fn tier_limits_match_table() {
        assert_eq!(tier_limit("standard"), Some(60));
        assert_eq!(tier_limit("elevated"), Some(300));
        assert_eq!(tier_limit("high"), Some(1000));
        assert_eq!(tier_limit("unknown"), None);
    }

    #[test]
    fn generated_key_has_prefix_and_length() {
        let value = generate_key_value();
        assert!(value.starts_with(KEY_PREFIX));
        assert_eq!(value.len(), KEY_PREFIX.len() + 40);
    }

    #[test]
    fn generated_keys_differ() {
        assert_ne!(generate_key_value(), generate_key_value());
    }

    #[test]
    fn scopes_accept_known_modules() {
        let modules = scopes(&["users", "orders"]);
        assert!(validate_scopes(&scopes(&["users:read"]), &modules).is_ok());
        assert!(validate_scopes(&scopes(&["*"]), &modules).is_ok());
        assert!(validate_scopes(&scopes(&["users:*"]), &modules).is_ok());
    }

    #[test]
    fn scopes_reject_unknown_module() {
        let modules = scopes(&["users"]);
        assert!(validate_scopes(&scopes(&["orders:read"]), &modules).is_err());
    }

    #[test]
    fn scopes_reject_bad_shape() {
        let modules = scopes(&["users"]);
        assert!(validate_scopes(&scopes(&["read"]), &modules).is_err());
        assert!(validate_scopes(&scopes(&["users:fly"]), &modules).is_err());
        assert!(validate_scopes(&[], &modules).is_err());
    }

    #[test]
    fn create_rejects_omitted_scopes() {
        // New keys no longer default to ["*"]: an omitted scope list
        // must fail instead of minting an omnipotent key.
        let result = create_api_key(
            CreateApiKeyParams {
                name: "no scopes".to_string(),
                owner_id: None,
                scopes: None,
                rate_tier: None,
                requests_per_minute: None,
                expires_in_days: None,
            },
            &module_ids(),
        );
        assert!(result.is_err());
    }

    #[test]
    fn create_rejects_empty_scopes_and_accepts_explicit_scopes() {
        let empty = create_api_key(
            CreateApiKeyParams {
                name: "empty scopes".to_string(),
                owner_id: None,
                scopes: Some(vec![]),
                rate_tier: None,
                requests_per_minute: None,
                expires_in_days: None,
            },
            &module_ids(),
        );
        assert!(empty.is_err());
        let created = create_api_key(
            CreateApiKeyParams {
                name: "explicit scopes".to_string(),
                owner_id: None,
                scopes: Some(scopes(&["users:read"])),
                rate_tier: None,
                requests_per_minute: None,
                expires_in_days: None,
            },
            &module_ids(),
        )
        .unwrap();
        assert_eq!(created.key.scopes, scopes(&["users:read"]));
        assert_ne!(created.key.scopes, scopes(&["*"]));
    }

    #[test]
    fn peppered_hash_is_prefixed_deterministic_and_differs_from_legacy() {
        let first = hash_key_value_peppered("some-presented-key");
        assert!(first.starts_with(HMAC_HASH_PREFIX));
        assert_eq!(first, hash_key_value_peppered("some-presented-key"));
        assert_ne!(first, hash_key_value_peppered("some-other-key"));
        assert_ne!(first, hash_key_value("some-presented-key"));
    }

    #[test]
    fn authenticate_accepts_peppered_key() {
        let (id, full_key) = seed_record(false);
        let authenticated = authenticate_key(&full_key).unwrap();
        assert_eq!(authenticated.id, id);
        assert_eq!(authenticated.scopes, scopes(&["users:read"]));
    }

    #[test]
    fn authenticate_still_accepts_legacy_sha256_key() {
        // Dual-read backward compatibility: a key issued before the
        // pepper fix (plain SHA-256 digest) must keep working.
        let (id, full_key) = seed_record(true);
        let authenticated = authenticate_key(&full_key).unwrap();
        assert_eq!(authenticated.id, id);
    }

    #[test]
    fn legacy_hash_sunset_dates() {
        use chrono::NaiveDate;
        let today = NaiveDate::from_ymd_opt(2026, 11, 22).expect("valid date");
        // Before the sunset, legacy hashes are still accepted.
        assert!(!legacy_hash_sunset_reached_on("2027-03-01", today));
        assert!(!legacy_hash_sunset_reached_on("2026-11-23", today));
        // On the sunset date itself, and after it, they are not.
        assert!(legacy_hash_sunset_reached_on("2026-11-22", today));
        assert!(legacy_hash_sunset_reached_on("2026-11-01", today));
        assert!(legacy_hash_sunset_reached_on(
            "2027-03-01",
            NaiveDate::from_ymd_opt(2027, 3, 1).expect("valid date")
        ));
        // A malformed configured date fails closed (treated as reached).
        assert!(legacy_hash_sunset_reached_on("not-a-date", today));
        assert!(legacy_hash_sunset_reached_on("", today));
    }

    #[test]
    fn authenticate_denies_rotating_key_without_grace_period() {
        let (_id, full_key) = seed_record_with(ApiKeyStatus::Rotating, None, false);
        assert!(authenticate_key(&full_key).is_err());
    }

    #[test]
    fn authenticate_allows_rotating_key_inside_grace_period() {
        let (id, full_key) = seed_record_with(
            ApiKeyStatus::Rotating,
            Some(Utc::now() + chrono::Duration::hours(1)),
            false,
        );
        assert_eq!(authenticate_key(&full_key).unwrap().id, id);
    }

    #[test]
    fn authenticate_denies_rotating_key_past_grace_period() {
        let (_id, full_key) = seed_record_with(
            ApiKeyStatus::Rotating,
            Some(Utc::now() - chrono::Duration::hours(1)),
            false,
        );
        assert!(authenticate_key(&full_key).is_err());
    }

    #[test]
    fn update_access_blocks_rotating_keys() {
        let (id, _full_key) = seed_record_with(
            ApiKeyStatus::Rotating,
            Some(Utc::now() + chrono::Duration::hours(1)),
            false,
        );
        let result = update_api_key_access(
            id,
            Some(scopes(&["orders:read"])),
            None,
            None,
            &module_ids(),
        );
        assert!(result.is_err());
        assert_eq!(
            keys_store().get(&id).unwrap().scopes,
            scopes(&["users:read"])
        );
    }
}
