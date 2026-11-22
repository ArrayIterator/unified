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
// Storage model: only the SHA-256 hex digest of a key is stored, plus a
// masked prefix (`unf_ab12...`) and the last four characters. The full
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

use crate::core::context::AppContext;
use crate::core::error::{UnifiedError, UnifiedResult};
use crate::core::hooks::HookBus;
use crate::core::json;
use crate::core::modules::{Module, ModuleRegistry};
use crate::core::pagination::{PageResult, PaginationParams};
use actix_web::{web, HttpResponse};
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use rand::{distr::Alphanumeric, Rng};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::sync::{Arc, OnceLock};
use uuid::Uuid;

// ---------------------------------------------------------------------------
// Constants.
// ---------------------------------------------------------------------------

/// Prefix of every generated key. The prefix is part of the masked
/// display value, never a secret by itself.
pub const KEY_PREFIX: &str = "unf_";
/// Random characters after the prefix.
const KEY_RANDOM_LEN: usize = 40;
/// Default grace period (seconds) during which a rotated-out key keeps
/// working: 24 hours.
const DEFAULT_ROTATION_GRACE_SECS: i64 = 86_400;
/// Maximum grace period accepted on rotation: 30 days.
const MAX_ROTATION_GRACE_SECS: i64 = 2_592_000;
/// Maximum expiry accepted on creation: 10 years.
const MAX_EXPIRY_DAYS: u32 = 3_650;
/// Rate-limit tiers: requests per minute per key.
const TIER_LIMITS: &[(&str, u32)] = &[
    ("standard", 60),
    ("elevated", 300),
    ("high", 1_000),
];

/// Well-known events emitted by this module.
pub mod apikey_events {
    pub const CREATED: &str = "apikey.created";
    pub const ROTATED: &str = "apikey.rotated";
    pub const REVOKED: &str = "apikey.revoked";
    pub const USED: &str = "apikey.used";
}

/// Actions allowed in a scope.
const SCOPE_ACTIONS: &[&str] = &["read", "write", "admin", "*"];

// ---------------------------------------------------------------------------
// State.
// ---------------------------------------------------------------------------

/// Hook bus stashed in `init` so `note_used` (called from middleware,
/// outside any handler) can emit `apikey.used`.
static HOOK_BUS: OnceLock<Arc<HookBus>> = OnceLock::new();

static KEYS: OnceLock<DashMap<Uuid, ApiKeyRecord>> = OnceLock::new();
/// Index from SHA-256 hex digest to key id, for authentication lookups.
static KEYS_BY_HASH: OnceLock<DashMap<String, Uuid>> = OnceLock::new();

fn keys_store() -> &'static DashMap<Uuid, ApiKeyRecord> {
    KEYS.get_or_init(DashMap::new)
}

fn hash_index() -> &'static DashMap<String, Uuid> {
    KEYS_BY_HASH.get_or_init(DashMap::new)
}

fn emit(event: &str, payload: Value) {
    if let Some(bus) = HOOK_BUS.get() {
        bus.emit(event, payload);
    }
}

// ---------------------------------------------------------------------------
// Types.
// ---------------------------------------------------------------------------

/// Lifecycle of a key. `rotating` means a replacement was issued and
/// the key is inside its grace period; it becomes `revoked` when the
/// grace period ends (lazily, on the next authentication attempt).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ApiKeyStatus {
    Active,
    Rotating,
    Revoked,
    Expired,
}

impl ApiKeyStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Rotating => "rotating",
            Self::Revoked => "revoked",
            Self::Expired => "expired",
        }
    }
}

/// One stored key. The full key value is never stored here.
#[derive(Debug, Clone)]
pub struct ApiKeyRecord {
    pub id: Uuid,
    pub name: String,
    /// Optional owner (user id) the key was issued for.
    pub owner_id: Option<Uuid>,
    /// Masked display prefix, for example `unf_ab12...`.
    pub prefix: String,
    /// Last four characters of the key, for operator recognition.
    pub last4: String,
    /// SHA-256 hex digest of the full key. Never serialized.
    pub key_hash: String,
    pub scopes: Vec<String>,
    /// Rate-limit tier name: `standard`, `elevated`, or `high`.
    pub rate_tier: String,
    /// Effective requests-per-minute limit for this key.
    pub requests_per_minute: u32,
    pub status: ApiKeyStatus,
    pub expires_at: Option<DateTime<Utc>>,
    /// End of the rotation grace period (rotating keys only).
    pub grace_until: Option<DateTime<Utc>>,
    pub rotated_from: Option<Uuid>,
    pub rotated_to: Option<Uuid>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub last_used_at: Option<DateTime<Utc>>,
    pub usage_count: u64,
}

/// Public view of a key: everything except the secret material.
#[derive(Debug, Clone, Serialize)]
pub struct ApiKeyView {
    pub id: Uuid,
    pub name: String,
    pub owner_id: Option<Uuid>,
    pub prefix: String,
    pub last4: String,
    pub scopes: Vec<String>,
    pub rate_tier: String,
    pub requests_per_minute: u32,
    pub status: String,
    pub expires_at: Option<DateTime<Utc>>,
    pub grace_until: Option<DateTime<Utc>>,
    pub rotated_from: Option<Uuid>,
    pub rotated_to: Option<Uuid>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub last_used_at: Option<DateTime<Utc>>,
    pub usage_count: u64,
}

/// Create/rotate response: the public view plus the full key value,
/// shown exactly once. Callers must store it now; it cannot be
/// retrieved again through any endpoint.
#[derive(Debug, Clone, Serialize)]
pub struct ApiKeyCreatedView {
    #[serde(flatten)]
    pub key: ApiKeyView,
    /// The full key value. Shown exactly once, at creation/rotation.
    pub full_key: String,
}

impl ApiKeyRecord {
    fn view(&self) -> ApiKeyView {
        ApiKeyView {
            id: self.id,
            name: self.name.clone(),
            owner_id: self.owner_id,
            prefix: self.prefix.clone(),
            last4: self.last4.clone(),
            scopes: self.scopes.clone(),
            rate_tier: self.rate_tier.clone(),
            requests_per_minute: self.requests_per_minute,
            status: self.status.as_str().to_string(),
            expires_at: self.expires_at,
            grace_until: self.grace_until,
            rotated_from: self.rotated_from,
            rotated_to: self.rotated_to,
            created_at: self.created_at,
            updated_at: self.updated_at,
            last_used_at: self.last_used_at,
            usage_count: self.usage_count,
        }
    }
}

/// Result of a successful key authentication, for middleware use.
#[derive(Debug, Clone)]
pub struct AuthenticatedKey {
    pub id: Uuid,
    pub owner_id: Option<Uuid>,
    pub scopes: Vec<String>,
    pub rate_tier: String,
    pub requests_per_minute: u32,
}

// ---------------------------------------------------------------------------
// Key generation and hashing.
// ---------------------------------------------------------------------------

/// SHA-256 hex digest of a presented key value.
pub fn hash_key_value(presented: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(presented.as_bytes());
    hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn generate_key_value() -> String {
    let random: String = rand::rng()
        .sample_iter(&Alphanumeric)
        .take(KEY_RANDOM_LEN)
        .map(char::from)
        .collect();
    format!("{KEY_PREFIX}{random}")
}

fn tier_limit(tier: &str) -> Option<u32> {
    TIER_LIMITS
        .iter()
        .find(|(name, _)| *name == tier)
        .map(|(_, limit)| *limit)
}

/// Validate scope strings against the registered module ids.
/// `*` alone means every module and action; `module:*` means every
/// action on one module; `module:action` names one action.
fn validate_scopes(scopes: &[String], module_ids: &[String]) -> UnifiedResult<()> {
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
/// rejected. Call `note_used` after a successful authentication to
/// record usage.
pub fn authenticate_key(presented: &str) -> UnifiedResult<AuthenticatedKey> {
    let presented = presented.trim();
    if presented.is_empty() {
        return Err(UnifiedError::unauthorized("Missing API key"));
    }
    let digest = hash_key_value(presented);
    let Some(id) = hash_index().get(&digest).map(|entry| *entry.value()) else {
        return Err(UnifiedError::unauthorized("Invalid API key"));
    };
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
        if let Some(grace_until) = record.grace_until {
            if now > grace_until {
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
    /// Scopes for the key. `None` means the default (`["*"]`).
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
/// generate the value, store only its SHA-256 digest, and return
/// the view plus the full key value (shown exactly once).
/// `module_ids` is the list of registered module ids that scopes
/// are validated against.
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
    let scopes = params.scopes.unwrap_or_else(|| vec!["*".to_string()]);
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
        key_hash: hash_key_value(&full_key),
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
    Ok(ApiKeyCreatedView { key: view, full_key })
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
/// limit must be between 1 and 100000. Revoked and expired keys
/// cannot be updated.
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
    if record.status == ApiKeyStatus::Revoked || record.status == ApiKeyStatus::Expired {
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

// ---------------------------------------------------------------------------
// Request types.
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct CreateKeyRequest {
    name: String,
    #[serde(default)]
    owner_id: Option<Uuid>,
    #[serde(default)]
    scopes: Option<Vec<String>>,
    /// Rate tier: `standard` (60/min), `elevated` (300/min), `high` (1000/min).
    #[serde(default)]
    rate_tier: Option<String>,
    /// Explicit per-minute limit; overrides the tier when present.
    #[serde(default)]
    requests_per_minute: Option<u32>,
    /// Lifetime in days from creation (1..=3650). Omitted: never expires.
    #[serde(default)]
    expires_in_days: Option<u32>,
}

#[derive(Debug, Deserialize)]
struct UpdateKeyRequest {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    scopes: Option<Vec<String>>,
    #[serde(default)]
    rate_tier: Option<String>,
    #[serde(default)]
    requests_per_minute: Option<u32>,
    /// New expiry in days from now (1..=3650). Explicit null clears it.
    #[serde(default)]
    expires_in_days: Option<Option<u32>>,
}

#[derive(Debug, Deserialize)]
struct RotateKeyRequest {
    /// Grace period in seconds for the old key (1..=2592000, default 86400).
    #[serde(default)]
    grace_secs: Option<i64>,
}

#[derive(Debug, Deserialize)]
struct VerifyKeyRequest {
    key: String,
}

fn paginate<T: Serialize + Clone>(items: Vec<T>, params: &PaginationParams) -> PageResult<T> {
    let pagination = params.normalized();
    let total = items.len() as u64;
    let start = ((pagination.page - 1) * pagination.per_page) as usize;
    let end = (start + pagination.per_page as usize).min(items.len());
    let page_items = if start >= items.len() {
        Vec::new()
    } else {
        items[start..end].to_vec()
    };
    PageResult::new(page_items, total, &pagination)
}

fn find_key(id: Uuid) -> UnifiedResult<ApiKeyRecord> {
    keys_store()
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found("API key not found"))
}

// ---------------------------------------------------------------------------
// Handlers.
// ---------------------------------------------------------------------------

async fn create_key(
    ctx: web::Data<Arc<AppContext>>,
    body: web::Json<CreateKeyRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result = create_api_key(
        CreateApiKeyParams {
            name: body.name.clone(),
            owner_id: body.owner_id,
            scopes: body.scopes.clone(),
            rate_tier: body.rate_tier.clone(),
            requests_per_minute: body.requests_per_minute,
            expires_in_days: body.expires_in_days,
        },
        &ctx.modules.ids(),
    );
    match result {
        Ok(view) => json::success_response(view, 201),
        Err(error) => json::error_response(&error, debug),
    }
}

async fn list_keys(
    ctx: web::Data<Arc<AppContext>>,
    params: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let mut items: Vec<ApiKeyView> = all_key_views();
        if let Some(search) = &params.search {
            let needle = search.to_lowercase();
            items.retain(|key| {
                key.name.to_lowercase().contains(&needle)
                    || key.prefix.to_lowercase().contains(&needle)
            });
        }
        if let Some(status) = params.filters.get("status") {
            items.retain(|key| key.status == *status);
        }
        if let Some(owner) = params.filters.get("owner_id") {
            items.retain(|key| {
                key.owner_id
                    .map(|id| id.to_string() == *owner)
                    .unwrap_or(false)
            });
        }
        if let Some(scope) = params.filters.get("scope") {
            items.retain(|key| key.scopes.iter().any(|item| item == scope));
        }
        let sort = params.allowed_sort(&["created_at", "name", "usage_count", "last_used_at"], "created_at");
        match sort.as_str() {
            "name" => items.sort_by(|a, b| a.name.cmp(&b.name)),
            "usage_count" => items.sort_by(|a, b| a.usage_count.cmp(&b.usage_count)),
            "last_used_at" => items.sort_by(|a, b| a.last_used_at.cmp(&b.last_used_at)),
            _ => items.sort_by(|a, b| a.created_at.cmp(&b.created_at)),
        }
        if params.normalized().order == "desc" {
            items.reverse();
        }
        Ok(paginate(items, &params).to_json())
    })();
    json::respond(result, debug)
}

async fn get_key(ctx: web::Data<Arc<AppContext>>, id: web::Path<Uuid>) -> HttpResponse {
    let debug = ctx.is_debug();
    json::respond(find_key(id.into_inner()).map(|record| record.view()), debug)
}

async fn list_scopes(ctx: web::Data<Arc<AppContext>>) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let modules = ctx.modules.summary();
        let scopes: Vec<Value> = modules
            .iter()
            .map(|module| {
                serde_json::json!({
                    "module": module.id,
                    "name": module.name,
                    "actions": SCOPE_ACTIONS,
                    "scopes": SCOPE_ACTIONS.iter().map(|action| format!("{}:{action}", module.id)).collect::<Vec<_>>(),
                })
            })
            .collect();
        Ok(serde_json::json!({
            "modules": scopes,
            "wildcard": "*",
            "actions": SCOPE_ACTIONS,
        }))
    })();
    json::respond(result, debug)
}

async fn update_key(
    ctx: web::Data<Arc<AppContext>>,
    id: web::Path<Uuid>,
    body: web::Json<UpdateKeyRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let id = id.into_inner();
    let result: UnifiedResult<ApiKeyView> = (|| {
        if let Some(name) = &body.name {
            let name = name.trim();
            if name.is_empty() || name.chars().count() > 120 {
                return Err(UnifiedError::bad_request(
                    "name must be non-empty and at most 120 characters",
                ));
            }
        }
        if let Some(scopes) = &body.scopes {
            validate_scopes(scopes, &ctx.modules.ids())?;
        }
        if let Some(tier) = &body.rate_tier {
            if tier_limit(tier).is_none() {
                return Err(UnifiedError::bad_request(
                    "rate_tier must be one of: standard, elevated, high",
                ));
            }
        }
        if let Some(limit) = body.requests_per_minute {
            if limit == 0 || limit > 100_000 {
                return Err(UnifiedError::bad_request(
                    "requests_per_minute must be between 1 and 100000",
                ));
            }
        }
        let expires_at = match body.expires_in_days {
            Some(Some(days)) => {
                if days == 0 || days > MAX_EXPIRY_DAYS {
                    return Err(UnifiedError::bad_request(format!(
                        "expires_in_days must be between 1 and {MAX_EXPIRY_DAYS}"
                    )));
                }
                Some(Some(Utc::now() + chrono::Duration::days(i64::from(days))))
            }
            Some(None) => Some(None),
            None => None,
        };
        let mut record = keys_store()
            .get_mut(&id)
            .ok_or_else(|| UnifiedError::not_found("API key not found"))?;
        if record.status == ApiKeyStatus::Revoked || record.status == ApiKeyStatus::Expired {
            return Err(UnifiedError::conflict(format!(
                "Cannot update an API key in status \"{}\"",
                record.status.as_str()
            )));
        }
        if let Some(name) = &body.name {
            record.name = name.trim().to_string();
        }
        if let Some(scopes) = &body.scopes {
            record.scopes = scopes.clone();
        }
        if let Some(tier) = &body.rate_tier {
            record.rate_tier = tier.clone();
            if body.requests_per_minute.is_none() {
                record.requests_per_minute = tier_limit(tier).unwrap_or(60);
            }
        }
        if let Some(limit) = body.requests_per_minute {
            record.requests_per_minute = limit;
        }
        if let Some(next) = expires_at {
            record.expires_at = next;
        }
        record.updated_at = Utc::now();
        Ok(record.view())
    })();
    json::respond(result, debug)
}

async fn rotate_key(
    ctx: web::Data<Arc<AppContext>>,
    id: web::Path<Uuid>,
    body: web::Json<RotateKeyRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let id = id.into_inner();
    let result: UnifiedResult<ApiKeyCreatedView> = (|| {
        let grace_secs = body.grace_secs.unwrap_or(DEFAULT_ROTATION_GRACE_SECS);
        if grace_secs < 1 || grace_secs > MAX_ROTATION_GRACE_SECS {
            return Err(UnifiedError::bad_request(format!(
                "grace_secs must be between 1 and {MAX_ROTATION_GRACE_SECS}"
            )));
        }
        let mut old = keys_store()
            .get_mut(&id)
            .ok_or_else(|| UnifiedError::not_found("API key not found"))?;
        if old.status == ApiKeyStatus::Revoked || old.status == ApiKeyStatus::Expired {
            return Err(UnifiedError::conflict(format!(
                "Cannot rotate an API key in status \"{}\"",
                old.status.as_str()
            )));
        }
        if old.rotated_to.is_some() {
            return Err(UnifiedError::conflict(
                "This API key was already rotated; rotate the replacement key instead",
            ));
        }
        let full_key = generate_key_value();
        let now = Utc::now();
        let grace_until = now + chrono::Duration::seconds(grace_secs);
        let new_record = ApiKeyRecord {
            id: Uuid::now_v7(),
            name: old.name.clone(),
            owner_id: old.owner_id,
            prefix: format!("{}...", &full_key[..8]),
            last4: full_key[full_key.len() - 4..].to_string(),
            key_hash: hash_key_value(&full_key),
            scopes: old.scopes.clone(),
            rate_tier: old.rate_tier.clone(),
            requests_per_minute: old.requests_per_minute,
            status: ApiKeyStatus::Active,
            expires_at: old.expires_at,
            grace_until: None,
            rotated_from: Some(old.id),
            rotated_to: None,
            created_at: now,
            updated_at: now,
            last_used_at: None,
            usage_count: 0,
        };
        let view = new_record.view();
        let new_id = new_record.id;
        old.status = ApiKeyStatus::Rotating;
        old.grace_until = Some(grace_until);
        old.rotated_to = Some(new_id);
        old.updated_at = now;
        drop(old);
        hash_index().insert(new_record.key_hash.clone(), new_record.id);
        keys_store().insert(new_record.id, new_record);
        emit(
            apikey_events::ROTATED,
            serde_json::json!({
                "old_key_id": id,
                "new_key_id": new_id,
                "grace_until": grace_until,
            }),
        );
        Ok(ApiKeyCreatedView { key: view, full_key })
    })();
    match result {
        Ok(view) => json::success_response(view, 201),
        Err(error) => json::error_response(&error, debug),
    }
}

async fn revoke_key(ctx: web::Data<Arc<AppContext>>, id: web::Path<Uuid>) -> HttpResponse {
    let debug = ctx.is_debug();
    json::respond(revoke_api_key(id.into_inner()), debug)
}

async fn key_usage(ctx: web::Data<Arc<AppContext>>, id: web::Path<Uuid>) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let record = find_key(id.into_inner())?;
        Ok(serde_json::json!({
            "key_id": record.id,
            "name": record.name,
            "prefix": record.prefix,
            "status": record.status.as_str(),
            "usage_count": record.usage_count,
            "last_used_at": record.last_used_at,
            "rate_tier": record.rate_tier,
            "requests_per_minute": record.requests_per_minute,
            "created_at": record.created_at,
            "expires_at": record.expires_at,
        }))
    })();
    json::respond(result, debug)
}

async fn verify_key(
    ctx: web::Data<Arc<AppContext>>,
    body: web::Json<VerifyKeyRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        match authenticate_key(&body.key) {
            Ok(authenticated) => Ok(serde_json::json!({
                "valid": true,
                "key_id": authenticated.id,
                "owner_id": authenticated.owner_id,
                "scopes": authenticated.scopes,
                "rate_tier": authenticated.rate_tier,
                "requests_per_minute": authenticated.requests_per_minute,
            })),
            Err(error) => Ok(serde_json::json!({
                "valid": false,
                "reason": error.message,
            })),
        }
    })();
    json::respond(result, debug)
}

// ---------------------------------------------------------------------------
// Module definition.
// ---------------------------------------------------------------------------

pub struct ApiKeysModule;

impl Module for ApiKeysModule {
    fn id(&self) -> &'static str {
        "apikeys"
    }

    fn name(&self) -> &'static str {
        "API Keys"
    }

    fn description(&self) -> &'static str {
        "Enterprise API key management: scoped keys with SHA-256 hashed storage, expiry, rotation with a grace period, revocation, per-key rate-limit tiers, and usage tracking."
    }

    fn requires(&self) -> &[&'static str] {
        &["security", "api-manager"]
    }

    fn requires_libraries(&self) -> &[&'static str] {
        &["sha2", "rand"]
    }

    fn init(&self, ctx: &AppContext) -> UnifiedResult<()> {
        let _ = HOOK_BUS.set(Arc::clone(&ctx.hooks));
        Ok(())
    }

    fn configure_routes(&self, cfg: &mut actix_web::web::ServiceConfig) {
        cfg.service(
            web::scope("/api/v1/apikeys")
                .route("", web::get().to(list_keys))
                .route("", web::post().to(create_key))
                .route("/scopes", web::get().to(list_scopes))
                .route("/verify", web::post().to(verify_key))
                .route("/{id}", web::get().to(get_key))
                .route("/{id}", web::put().to(update_key))
                .route("/{id}/rotate", web::post().to(rotate_key))
                .route("/{id}/revoke", web::post().to(revoke_key))
                .route("/{id}/usage", web::get().to(key_usage)),
        );
    }

    fn route_info(&self) -> Vec<crate::core::modules::RouteInfo> {
        use crate::core::modules::RouteInfo;
        vec![
            RouteInfo { method: "GET", path: "/api/v1/apikeys", owner: "apikeys", description: "List API keys (paginated)" },
            RouteInfo { method: "POST", path: "/api/v1/apikeys", owner: "apikeys", description: "Create an API key; full key returned once" },
            RouteInfo { method: "GET", path: "/api/v1/apikeys/scopes", owner: "apikeys", description: "Scope catalog for registered modules" },
            RouteInfo { method: "GET", path: "/api/v1/apikeys/{id}", owner: "apikeys", description: "Get one API key (masked)" },
            RouteInfo { method: "PUT", path: "/api/v1/apikeys/{id}", owner: "apikeys", description: "Update an API key" },
            RouteInfo { method: "POST", path: "/api/v1/apikeys/{id}/rotate", owner: "apikeys", description: "Rotate an API key with a grace period" },
            RouteInfo { method: "POST", path: "/api/v1/apikeys/{id}/revoke", owner: "apikeys", description: "Revoke an API key" },
            RouteInfo { method: "GET", path: "/api/v1/apikeys/{id}/usage", owner: "apikeys", description: "Usage statistics for one API key" },
            RouteInfo { method: "POST", path: "/api/v1/apikeys/verify", owner: "apikeys", description: "Verify a presented API key" },
        ]
    }
}

pub fn register(registry: &ModuleRegistry) {
    registry.register(Arc::new(ApiKeysModule));
}
