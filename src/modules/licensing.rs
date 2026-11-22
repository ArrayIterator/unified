// Licensing module — license key management and validation.
//
// License keys use the format `XXXX-XXXX-XXXX-XXXX` over a 32-character
// alphabet (no ambiguous characters), with the final character acting as
// a checksum over the preceding characters. Validation is real: the key
// must exist, be `active`, not be expired, and have a free activation
// seat; a successful validation auto-registers the activation.
// Revocation emits the `license.revoked` hook event. The expiry scan
// (`check_license_expiry`, below) emits `license.expiring` for
// licenses entering a warning window; it runs on the
// `license.expiry.check_requested` hook and on
// `POST /api/v1/licensing/licenses/expiry-check`.
//
// Licensing as a service (batch 7): a Unified user can sell their own
// licensed products. Per-product settings (`LicenseProduct`) control
// the key format, seat defaults, validity, grace period, renewal
// reminders, offline-token allowance, and distribution channels.
// The public verification API (`POST /api/v1/licensing/public/verify`)
// is authenticated with an API key scoped to the product owner (see
// `crate::modules::apikeys`), never with the license key itself.
// Releases can be distributed through a git channel (signed webhook),
// the verification API itself, or file uploads stored through the
// shared storage abstraction (`StorageRegistry`) with a signed,
// expiring download link that requires a valid license. Every license
// mutation appends a hash-chained entry to the license event log
// (`license_events`), and each append emits `license.anchored` so an
// optional anchoring consumer (for example the Web3 module, which
// listens for `web3.anchor.requested`) can anchor the hash on-chain;
// licensing never calls the Web3 module directly.
//
// Trust model (read before relying on offline verification):
// - An offline token is a bearer credential: it can be replayed by
//   anyone who holds it until it expires, it is valid for exactly one
//   product, and revocation of the license or the token does NOT
//   propagate to verifications that already hold the token. The token
//   is HMAC-SHA256 signed with the server secret
//   (`store.download_signing_secret`, or a random per-process secret
//   when that is unset, in which case tokens stop verifying after a
//   restart). Offline verification performs no database lookup.
// - A git webhook secret is stored raw in memory only (it is needed
//   to verify the provider HMAC); it is never returned by any
//   endpoint and only its SHA-256 hash is kept for audit. After a
//   restart a distribution must be re-configured unless the
//   integrator adds encrypted persistence for the secret.
// - The new SaaS tables live in memory in this round (the same
//   pattern as the rest of this module); the migration ships the
//   PostgreSQL tables and the integrator wires them into
//   `src/db/schema.rs` / `src/db/models.rs` for durable persistence.
//
// SDK: FUTURE ONLY. No SDK ships in this round. The planned SDK wraps
// the verification API (`POST /api/v1/licensing/public/verify`) and the
// offline-token flow (`offline-token` issue + `offline-verify`), with
// language targets PHP, JavaScript/TypeScript, Python, and Rust.

use crate::core::context::AppContext;
use crate::core::error::{UnifiedError, UnifiedResult};
use crate::core::hooks::{events, HookBus, HookContext};
use crate::core::json;
use crate::core::modules::{
    Module, ModuleRegistry, MODULE_ENTITLEMENT_DENIED_EVENT, MODULE_ENTITLEMENT_GRANTED_EVENT,
    RouteInfo,
};
use crate::core::pagination::{PageResult, PaginationParams};
use crate::db::models::{License, LicenseActivation, NewLicense, NewLicenseActivation};
use crate::modules::apikeys::{
    authenticate_key, key_has_scope, note_used, AuthenticatedKey,
};
use actix_multipart::Multipart;
use actix_web::{http::header, web, HttpRequest, HttpResponse};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use diesel::prelude::*;
use futures_util::TryStreamExt;
use hmac::{Hmac, Mac};
use rand::Rng;
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};
use uuid::Uuid;

/// HMAC-SHA256, the same construction the VCS module uses for its
/// GitHub webhook signatures and the store module uses for its signed
/// download links.
type HmacSha256 = Hmac<Sha256>;

// ---------------------------------------------------------------------------
// Key generation: XXXX-XXXX-XXXX-XXXX with a checksum character.
// ---------------------------------------------------------------------------

/// Alphabet without ambiguous characters (no 0/O, 1/I/L).
const KEY_ALPHABET: &[u8] = b"ABCDEFGHJKLMNPQRSTUVWXYZ23456789";

/// Generate a new license key. The 16th character is a checksum: the
/// alphabet index of the sum of the indices of the first 15 characters
/// modulo the alphabet size.
pub fn generate_license_key() -> String {
    let mut rng = rand::rng();
    let mut indices: Vec<usize> = (0..15)
        .map(|_| rng.random_range(0..KEY_ALPHABET.len()))
        .collect();
    let checksum = indices.iter().sum::<usize>() % KEY_ALPHABET.len();
    indices.push(checksum);
    let chars: Vec<char> = indices
        .iter()
        .map(|index| KEY_ALPHABET[*index] as char)
        .collect();
    chars
        .chunks(4)
        .map(|chunk| chunk.iter().collect::<String>())
        .collect::<Vec<String>>()
        .join("-")
}

/// True when the key has the right shape and a matching checksum.
pub fn key_checksum_valid(key: &str) -> bool {
    let compact: String = key.chars().filter(|c| *c != '-').collect();
    if compact.len() != 16 {
        return false;
    }
    let mut indices = Vec::with_capacity(16);
    for ch in compact.chars() {
        match KEY_ALPHABET.iter().position(|byte| *byte as char == ch) {
            Some(index) => indices.push(index),
            None => return false,
        }
    }
    indices[..15].iter().sum::<usize>() % KEY_ALPHABET.len() == indices[15]
}

fn normalize_key(key: &str) -> String {
    key.trim().to_uppercase()
}

// ---------------------------------------------------------------------------
// Expiry scan: `license.expiring`.
// ---------------------------------------------------------------------------

/// Event emitted when a license enters an expiry warning window (or
/// has already expired). Payload: `{"license_id", "license_key",
/// "days_remaining", "threshold_days", "expired", "expires_at"}`.
/// There is no `license.expiring` constant in `crate::core::hooks`
/// (it only defines `license.issued` / `license.revoked`), so the
/// name lives here next to the code that emits it. The notifications
/// module does NOT subscribe to this event yet (verified by reading
/// its listener list: it subscribes to `license.issued` /
/// `activated` / `deactivated` / `suspended` / `revoked` only), so
/// the scan also requests a notification directly
/// (`notification.requested`, user `system`), the same pattern the
/// certificates module uses for `ssl.expiring` (which the
/// notifications module likewise does not subscribe to). If the
/// notifications module later subscribes to `license.expiring`
/// directly, drop the direct request to avoid duplicate alerts.
pub const LICENSE_EXPIRING: &str = "license.expiring";

/// Hook an operator or the scheduler can emit to run the expiry scan
/// now. The scheduler has no license job kind yet (its known kinds
/// are mail, geo, ssl, cleanup, backup, cluster, subscription
/// renewal, report, newsletter, digest); until one is added there,
/// the scan is reachable through this hook and through
/// `POST /api/v1/licensing/licenses/expiry-check`.
pub const LICENSE_EXPIRY_CHECK_REQUESTED: &str = "license.expiry.check_requested";

/// Warning windows in days before expiry. There is no expiry-window
/// setting in `AppConfig` (the `licenses` section only carries the
/// MaxMind key), so the windows are these documented defaults: a
/// license is alerted once per window it newly enters, and once when
/// it expires.
pub const EXPIRY_ALERT_WINDOW_DAYS: &[i64] = &[30, 14, 7];

/// Thresholds already alerted for, per license id, so the scan
/// alerts once per window instead of on every run. Mirrors the
/// certificates module's `alerted_thresholds` (`-1` = expired).
static EXPIRY_ALERTED: OnceLock<DashMap<Uuid, Vec<i64>>> = OnceLock::new();

fn expiry_alerted_store() -> &'static DashMap<Uuid, Vec<i64>> {
    EXPIRY_ALERTED.get_or_init(DashMap::new)
}

/// Renewal-reminder windows already sent, per license id (SaaS).
/// Kept apart from `EXPIRY_ALERTED` so a renewal reminder and an
/// expiry alert for the same window are tracked independently.
static REMINDER_ALERTED: OnceLock<DashMap<Uuid, Vec<i64>>> = OnceLock::new();

fn reminder_alerted_store() -> &'static DashMap<Uuid, Vec<i64>> {
    REMINDER_ALERTED.get_or_init(DashMap::new)
}

/// Licenses for which `license.grace.started` was already emitted
/// (SaaS): the grace-started event fires once per grace period.
static GRACE_ALERTED: OnceLock<DashMap<Uuid, bool>> = OnceLock::new();

fn grace_alerted_store() -> &'static DashMap<Uuid, bool> {
    GRACE_ALERTED.get_or_init(DashMap::new)
}

/// Run the expiry scan over every `active` license: for each warning
/// window (30/14/7 days) a license has newly entered, emit
/// `license.expiring` and request a notification; an already-expired
/// license alerts once the same way (`threshold_days: -1`,
/// `days_remaining` negative). Licenses with no expiry date and
/// non-`active` licenses are skipped. Returns a summary of what was
/// checked. The database rows are authoritative when a pool is
/// attached; otherwise the in-memory store is scanned.
pub fn check_license_expiry(ctx: &AppContext) -> UnifiedResult<Value> {
    let licenses: Vec<License> = match db_pool(ctx) {
        Some(pool) => db_load_licenses(&pool)?,
        None => licenses_store()
            .iter()
            .map(|entry| entry.value().clone())
            .collect(),
    };
    let now = Utc::now();
    let mut checked = 0u64;
    let mut expiring: Vec<Value> = Vec::new();
    let mut expired: Vec<Value> = Vec::new();
    for license in &licenses {
        if license.status != "active" {
            continue;
        }
        let Some(expires_at) = license.expires_at else {
            continue;
        };
        checked += 1;
        let days = (expires_at - now).num_days();
        let mut alerted = expiry_alerted_store()
            .get(&license.id)
            .map(|entry| entry.value().clone())
            .unwrap_or_default();
        let mut crossed: Vec<i64> = Vec::new();
        // Grace (SaaS): a license past expiry but inside its product
        // grace period is not expired yet — emit `license.grace.started`
        // once and skip the expired alert until grace ends.
        let (in_grace, days_in_grace) = grace_state(license);
        if days < 0 {
            if in_grace {
                if !grace_alerted_store().contains_key(&license.id) {
                    grace_alerted_store().insert(license.id, true);
                    ctx.hooks.emit(
                        LICENSE_GRACE_STARTED,
                        serde_json::json!({
                            "license_id": license.id,
                            "license_key": license.license_key,
                            "days_in_grace": days_in_grace,
                            "grace_period_days": grace_days_for(&license.product),
                            "expires_at": expires_at,
                        }),
                    );
                    ctx.hooks.emit(
                        "notification.requested",
                        serde_json::json!({
                            "user_id": "system",
                            "title": format!(
                                "License {} is in its grace period",
                                license.license_key
                            ),
                            "body": format!(
                                "License {} expired on {} and is now in its grace period (day {days_in_grace}); it still validates until grace ends.",
                                license.license_key,
                                expires_at.to_rfc3339()
                            ),
                            "kind": "license.grace.started",
                        }),
                    );
                }
            } else {
                if !alerted.contains(&-1) {
                    crossed.push(-1);
                }
                expired.push(serde_json::json!({
                    "license_id": license.id,
                    "license_key": license.license_key,
                    "days_remaining": days,
                    "expires_at": expires_at,
                }));
            }
        } else {
            for threshold in EXPIRY_ALERT_WINDOW_DAYS {
                if days <= *threshold && !alerted.contains(threshold) {
                    crossed.push(*threshold);
                }
            }
            if !crossed.is_empty() {
                expiring.push(serde_json::json!({
                    "license_id": license.id,
                    "license_key": license.license_key,
                    "days_remaining": days,
                    "expires_at": expires_at,
                    "threshold_days": &crossed,
                }));
            }
            // Renewal reminders (SaaS): per-product windows (default
            // 30/14/7/1 days), one `license.renewal.reminder` per
            // newly-entered window plus a direct notification.
            let reminder_windows = renewal_reminder_days_for(&license.product);
            let mut reminded = reminder_alerted_store()
                .get(&license.id)
                .map(|entry| entry.value().clone())
                .unwrap_or_default();
            for window in &reminder_windows {
                if days <= *window && !reminded.contains(window) {
                    reminded.push(*window);
                    ctx.hooks.emit(
                        LICENSE_RENEWAL_REMINDER,
                        serde_json::json!({
                            "license_id": license.id,
                            "license_key": license.license_key,
                            "product": license.product,
                            "days_remaining": days,
                            "threshold_days": window,
                            "expires_at": expires_at,
                        }),
                    );
                    ctx.hooks.emit(
                        "notification.requested",
                        serde_json::json!({
                            "user_id": "system",
                            "title": format!(
                                "Renewal reminder: license {} expires in {days} day(s)",
                                license.license_key
                            ),
                            "body": format!(
                                "License {} for product {} expires on {} (in {days} day(s)); renewal is due.",
                                license.license_key,
                                license.product,
                                expires_at.to_rfc3339()
                            ),
                            "kind": "license.renewal.reminder",
                        }),
                    );
                }
            }
            if !reminded.is_empty() {
                reminder_alerted_store().insert(license.id, reminded);
            }
        }
        for threshold in &crossed {
            alerted.push(*threshold);
            let expired_flag = *threshold == -1;
            ctx.hooks.emit(
                LICENSE_EXPIRING,
                serde_json::json!({
                    "license_id": license.id,
                    "license_key": license.license_key,
                    "days_remaining": days,
                    "threshold_days": threshold,
                    "expired": expired_flag,
                    "expires_at": expires_at,
                }),
            );
            // Direct notification, same pattern as the certificates
            // module's `ssl.expiring` alerts: the notifications
            // module does not subscribe to this event, so without
            // this request the alert would land nowhere.
            let (title, body) = if expired_flag {
                (
                    format!("License {} has expired", license.license_key),
                    format!(
                        "License {} expired {} day(s) ago; it can no longer be validated until it is renewed or replaced.",
                        license.license_key,
                        -days
                    ),
                )
            } else {
                (
                    format!(
                        "License {} expires in {days} day(s)",
                        license.license_key
                    ),
                    format!(
                        "License {} expires on {} (in {days} day(s)); renewal is due within the warning window.",
                        license.license_key,
                        expires_at.to_rfc3339()
                    ),
                )
            };
            ctx.hooks.emit(
                "notification.requested",
                serde_json::json!({
                    "user_id": "system",
                    "title": title,
                    "body": body,
                    "kind": "license.expiring",
                }),
            );
        }
        if !crossed.is_empty() {
            expiry_alerted_store().insert(license.id, alerted);
        }
    }
    Ok(serde_json::json!({
        "checked": checked,
        "expiring": expiring,
        "expired": expired,
    }))
}

/// Answer `license.expiry.check_requested` by running the expiry
/// scan and returning its summary as the hook result.
fn on_license_expiry_check_requested(ctx: &mut HookContext) {
    let Some(app_ctx) = hook_ctx() else {
        ctx.add_result(serde_json::json!({
            "checked": 0,
            "error": "licensing module is not initialised",
        }));
        return;
    };
    match check_license_expiry(&app_ctx) {
        Ok(summary) => ctx.add_result(summary),
        Err(error) => ctx.add_result(serde_json::json!({
            "checked": 0,
            "error": error.message,
        })),
    }
}

// ---------------------------------------------------------------------------
// In-memory stores (used when no database pool is attached).
// ---------------------------------------------------------------------------

static LICENSES: OnceLock<DashMap<Uuid, License>> = OnceLock::new();
static ACTIVATIONS: OnceLock<DashMap<Uuid, LicenseActivation>> = OnceLock::new();

fn licenses_store() -> &'static DashMap<Uuid, License> {
    LICENSES.get_or_init(DashMap::new)
}

fn activations_store() -> &'static DashMap<Uuid, LicenseActivation> {
    ACTIVATIONS.get_or_init(DashMap::new)
}

fn db_pool(ctx: &AppContext) -> Option<crate::db::DbPool> {
    ctx.db.read().expect("db lock poisoned").clone()
}

fn db_load_licenses(pool: &crate::db::DbPool) -> UnifiedResult<Vec<License>> {
    use crate::db::schema::licenses::dsl as lic_dsl;
    let mut conn = pool.get().map_err(|error| {
        UnifiedError::internal(format!("Cannot get database connection: {error}"))
    })?;
    lic_dsl::licenses
        .select(License::as_select())
        .load(&mut conn)
        .map_err(|error| UnifiedError::internal(format!("Cannot load licenses: {error}")))
}

fn db_load_activations(
    pool: &crate::db::DbPool,
    license_id: Uuid,
) -> UnifiedResult<Vec<LicenseActivation>> {
    use crate::db::schema::license_activations::dsl as act_dsl;
    let mut conn = pool.get().map_err(|error| {
        UnifiedError::internal(format!("Cannot get database connection: {error}"))
    })?;
    act_dsl::license_activations
        .filter(act_dsl::license_id.eq(license_id))
        .select(LicenseActivation::as_select())
        .load(&mut conn)
        .map_err(|error| {
            UnifiedError::internal(format!("Cannot load license activations: {error}"))
        })
}

fn activations_for(license_id: Uuid) -> Vec<LicenseActivation> {
    activations_store()
        .iter()
        .filter(|entry| entry.value().license_id == license_id)
        .map(|entry| entry.value().clone())
        .collect()
}

fn find_license_by_key(key: &str) -> Option<License> {
    licenses_store()
        .iter()
        .find(|entry| entry.value().license_key == key)
        .map(|entry| entry.value().clone())
}

fn db_find_license_by_key(pool: &crate::db::DbPool, key: &str) -> UnifiedResult<Option<License>> {
    use crate::db::schema::licenses::dsl as lic_dsl;
    let mut conn = pool.get().map_err(|error| {
        UnifiedError::internal(format!("Cannot get database connection: {error}"))
    })?;
    lic_dsl::licenses
        .filter(lic_dsl::license_key.eq(key))
        .select(License::as_select())
        .first(&mut conn)
        .optional()
        .map_err(|error| UnifiedError::internal(format!("Cannot load license: {error}")))
}

/// Load one license by id from the database, when a pool is attached.
fn db_find_license_by_id(pool: &crate::db::DbPool, id: Uuid) -> UnifiedResult<Option<License>> {
    use crate::db::schema::licenses::dsl as lic_dsl;
    let mut conn = pool.get().map_err(|error| {
        UnifiedError::internal(format!("Cannot get database connection: {error}"))
    })?;
    lic_dsl::licenses
        .filter(lic_dsl::id.eq(id))
        .select(License::as_select())
        .first(&mut conn)
        .optional()
        .map_err(|error| UnifiedError::internal(format!("Cannot load license: {error}")))
}

// ---------------------------------------------------------------------------
// Hook context: `init` stashes the shared application context (config,
// hook bus, module registry, database pool) so the synchronous hook
// handlers below can use the database and emit events. Hook handlers
// read it lazily, so registration order (hooks before `init`) does not
// matter. The stashed context shares the live pool/config locks; only
// the uptime and pause flags are snapshots.
// ---------------------------------------------------------------------------

static LICENSING_CTX: OnceLock<Arc<AppContext>> = OnceLock::new();

fn stash_ctx(ctx: &AppContext) -> Arc<AppContext> {
    Arc::new(AppContext {
        config: ctx.config.clone(),
        hooks: ctx.hooks.clone(),
        modules: ctx.modules.clone(),
        cache: ctx.cache.clone(),
        geo: ctx.geo.clone(),
        db: ctx.db.clone(),
        start_time: std::time::Instant::now(),
        paused: std::sync::atomic::AtomicBool::new(ctx.is_paused()),
        maintenance: std::sync::atomic::AtomicBool::new(ctx.is_maintenance()),
    })
}

fn hook_ctx() -> Option<Arc<AppContext>> {
    LICENSING_CTX.get().cloned()
}

/// Create and store one license with an already-chosen key, and emit
/// `license.issued`. Shared by the HTTP create handler and the
/// `order.created` hook handler.
fn issue_license(
    ctx: &AppContext,
    key: String,
    product: String,
    user_id: Option<Uuid>,
    company_id: Option<Uuid>,
    max_activations: i32,
    expires_at: Option<DateTime<Utc>>,
) -> UnifiedResult<License> {
    let duplicate = match db_pool(ctx) {
        Some(pool) => db_find_license_by_key(&pool, &key)?.is_some(),
        None => find_license_by_key(&key).is_some(),
    };
    if duplicate {
        return Err(UnifiedError::conflict(
            "A license with this key already exists",
        ));
    }
    let now = Utc::now();
    let license = License {
        id: Uuid::now_v7(),
        license_key: key,
        product,
        user_id,
        company_id,
        max_activations,
        status: "active".to_string(),
        expires_at,
        created_at: now,
        updated_at: now,
    };
    if let Some(pool) = db_pool(ctx) {
        use crate::db::schema::licenses::dsl as lic_dsl;
        let mut conn = pool.get().map_err(|error| {
            UnifiedError::internal(format!("Cannot get database connection: {error}"))
        })?;
        diesel::insert_into(lic_dsl::licenses)
            .values(&NewLicense {
                id: license.id,
                license_key: license.license_key.clone(),
                product: license.product.clone(),
                user_id: license.user_id,
                company_id: license.company_id,
                max_activations: license.max_activations,
                status: license.status.clone(),
                expires_at: license.expires_at,
            })
            .execute(&mut conn)
            .map_err(|error| UnifiedError::internal(format!("Cannot create license: {error}")))?;
    }
    licenses_store().insert(license.id, license.clone());
    ctx.hooks.emit(
        events::LICENSE_ISSUED,
        serde_json::json!({
            "license_id": license.id,
            "license_key": license.license_key,
            "product": license.product,
        }),
    );
    // Hash-chained event log (SaaS): issuing is the first entry of
    // the license chain.
    append_license_event(
        ctx,
        license.id,
        "issued",
        None,
        serde_json::json!({
            "license_key": license.license_key,
            "product": license.product,
            "max_activations": license.max_activations,
            "expires_at": license.expires_at,
        }),
    );
    Ok(license)
}

/// True when a license already exists for this product and owner
/// (in-memory store, plus the database when a pool is attached).
fn license_exists_for_product(
    ctx: &AppContext,
    product: &str,
    user_id: Option<Uuid>,
) -> bool {
    if licenses_store()
        .iter()
        .any(|entry| entry.value().product == product && entry.value().user_id == user_id)
    {
        return true;
    }
    if let Some(pool) = db_pool(ctx) {
        use crate::db::schema::licenses::dsl as lic_dsl;
        if let Ok(mut conn) = pool.get() {
            let mut query = lic_dsl::licenses
                .filter(lic_dsl::product.eq(product))
                .into_boxed();
            query = match user_id {
                Some(id) => query.filter(lic_dsl::user_id.eq(id)),
                None => query.filter(lic_dsl::user_id.is_null()),
            };
            if let Ok(count) = query
                .count()
                .get_result::<i64>(&mut conn)
            {
                return count > 0;
            }
        }
    }
    false
}

/// Answer `license.validate_requested` (from the Software module) with
/// the same validation the HTTP `/validate` endpoint performs: the key
/// must exist, pass its checksum, be `active`, not be expired, have a
/// free activation seat, and (when the payload names a product) belong
/// to that product.
fn on_license_validate_requested(ctx: &mut HookContext) {
    let Some(app_ctx) = hook_ctx() else {
        ctx.add_result(serde_json::json!({
            "valid": false,
            "message": "licensing module is not initialised",
        }));
        return;
    };
    let key = ctx
        .get("license_key")
        .and_then(|value| value.as_str())
        .unwrap_or("")
        .to_string();
    if key.is_empty() {
        ctx.add_result(serde_json::json!({
            "valid": false,
            "message": "license_key is required",
        }));
        return;
    }
    let instance_id = ctx
        .get("instance_id")
        .and_then(|value| value.as_str())
        .map(str::to_string);
    let hostname = ctx
        .get("hostname")
        .and_then(|value| value.as_str())
        .map(str::to_string);
    let outcome = match validate_and_maybe_activate(
        &app_ctx,
        &key,
        instance_id.as_deref(),
        hostname.as_deref(),
        true,
    ) {
        Ok(outcome) => outcome,
        Err(error) => {
            ctx.add_result(serde_json::json!({
                "valid": false,
                "message": error.message,
            }));
            return;
        }
    };
    let requested_product = ctx
        .get("product")
        .and_then(|value| value.as_str())
        .map(str::to_string);
    let product_matches = match (&requested_product, &outcome.license) {
        (Some(requested), Some(license)) => license.product == *requested,
        _ => true,
    };
    let mut result = serde_json::json!({
        "valid": outcome.valid && product_matches,
    });
    if let Some(license) = &outcome.license {
        result["license_id"] = serde_json::json!(license.id);
    }
    let message = if !outcome.valid {
        outcome.reason.clone()
    } else if !product_matches {
        Some("product_mismatch".to_string())
    } else {
        None
    };
    if let Some(message) = message {
        result["message"] = serde_json::json!(message);
    }
    ctx.add_result(result);
}

/// Auto-issue licenses for a new order (from the Store module). Only
/// items flagged `license_product` get a license, one per item, keyed by
/// the item name as the product; already-licensed product/owner pairs
/// are skipped so a repeated event does not issue duplicates. The seat
/// count comes from the item's `seats` field (times its quantity) and
/// the expiry from the item's `expires_at` field, both set by the store
/// from the purchased pricing tier and pricing model, so a software
/// purchase issues the right license: one-time purchases get the
/// updates-window expiry, subscriptions get one billing period.
fn on_order_created(ctx: &mut HookContext) {
    let Some(app_ctx) = hook_ctx() else {
        ctx.add_result(serde_json::json!({
            "issued": false,
            "error": "licensing module is not initialised",
        }));
        return;
    };
    let order_id = ctx
        .get("id")
        .and_then(|value| value.as_str())
        .map(str::to_string);
    let user_id = ctx
        .get("user_id")
        .and_then(|value| value.as_str())
        .and_then(|value| value.parse::<Uuid>().ok());
    let company_id = ctx
        .get("company_id")
        .and_then(|value| value.as_str())
        .and_then(|value| value.parse::<Uuid>().ok());
    let items = ctx
        .get("items")
        .and_then(|value| value.as_array())
        .cloned()
        .unwrap_or_default();
    for item in items {
        if item
            .get("license_product")
            .and_then(|value| value.as_bool())
            != Some(true)
        {
            continue;
        }
        let product = item
            .get("name")
            .and_then(|value| value.as_str())
            .unwrap_or("")
            .trim()
            .to_string();
        if product.is_empty() {
            continue;
        }
        if license_exists_for_product(&app_ctx, &product, user_id) {
            continue;
        }
        let quantity = item
            .get("quantity")
            .and_then(|value| value.as_u64())
            .unwrap_or(1)
            .max(1);
        let seats_per_unit = item
            .get("seats")
            .and_then(|value| value.as_i64())
            .unwrap_or(1)
            .max(1);
        let max_activations = (seats_per_unit * quantity as i64).min(i32::MAX as i64) as i32;
        let expires_at = item
            .get("expires_at")
            .and_then(|value| value.as_str())
            .and_then(|value| value.parse::<DateTime<Utc>>().ok());
        let software_id = item
            .get("software_id")
            .and_then(|value| value.as_str())
            .map(str::to_string);
        let software_slug = item
            .get("software_slug")
            .and_then(|value| value.as_str())
            .map(str::to_string);
        let download_limit = item
            .get("download_limit")
            .and_then(|value| value.as_u64());
        match issue_license(
            &app_ctx,
            generate_license_key(),
            product.clone(),
            user_id,
            company_id,
            max_activations,
            expires_at,
        ) {
            Ok(license) => {
                ctx.add_result(serde_json::json!({
                    "issued": true,
                    "order_id": order_id,
                    "license_id": license.id,
                    "license_key": license.license_key,
                    "product": product,
                    "seats": max_activations,
                    "expires_at": license.expires_at,
                    "software_id": software_id,
                    "software_slug": software_slug,
                    "download_limit": download_limit,
                }));
            }
            Err(error) => {
                ctx.add_result(serde_json::json!({
                    "issued": false,
                    "order_id": order_id,
                    "product": product,
                    "error": error.message,
                }));
            }
        }
    }
}

/// Revoke the real licenses of a refunded order (from the Store
/// module, `store.order.refunded`, payload
/// `{ "order_id": ..., "license_ids": [...] }`).
///
/// The store only suspends its own mirror of the issued licenses and
/// revokes its download entitlements, so without this listener a
/// refunded license key would stay valid. A refund is final, so the
/// license is revoked rather than suspended: a revoked key fails
/// validation with `revoked` and can never activate again. Licenses
/// are resolved from the in-memory store first and from the database
/// when a pool is attached and the license is not in memory; ids that
/// match nothing are reported in `not_found` instead of failing the
/// whole refund. Each revocation emits `license.revoked`.
fn on_order_refunded(ctx: &mut HookContext) {
    let Some(app_ctx) = hook_ctx() else {
        ctx.add_result(serde_json::json!({
            "revoked": [],
            "not_found": [],
            "error": "licensing module is not initialised",
        }));
        return;
    };
    let order_id = ctx
        .get("order_id")
        .and_then(|value| value.as_str())
        .map(str::to_string)
        .or_else(|| {
            ctx.get("order")
                .and_then(|value| value.get("id"))
                .and_then(|value| value.as_str())
                .map(str::to_string)
        });
    let license_ids: Vec<String> = ctx
        .get("license_ids")
        .and_then(|value| value.as_array())
        .map(|values| {
            values
                .iter()
                .filter_map(|value| value.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    let mut revoked: Vec<String> = Vec::new();
    let mut not_found: Vec<String> = Vec::new();
    for license_id in license_ids {
        let Ok(id) = license_id.parse::<Uuid>() else {
            not_found.push(license_id);
            continue;
        };
        let mut license = licenses_store()
            .get(&id)
            .map(|entry| entry.value().clone());
        if license.is_none() {
            if let Some(pool) = db_pool(&app_ctx) {
                license = db_find_license_by_id(&pool, id).ok().flatten();
            }
        }
        let Some(mut license) = license else {
            not_found.push(license_id);
            continue;
        };
        if license.status == "revoked" {
            revoked.push(license_id);
            continue;
        }
        license.status = "revoked".to_string();
        license.updated_at = Utc::now();
        if persist_license_status(&app_ctx, &license).is_err() {
            not_found.push(license_id);
            continue;
        }
        app_ctx.hooks.emit(
            events::LICENSE_REVOKED,
            serde_json::json!({
                "license_id": license.id,
                "license_key": license.license_key,
                "order_id": order_id,
                "reason": "order_refunded",
            }),
        );
        // Refund coverage for the SaaS tables: the revocation is
        // chained in the event log, every offline token of this
        // license is revoked server-side (so online checks fail; an
        // already-issued offline verification still validates until
        // its token expires — see the offline trust model), and the
        // activation history rows stay as an audit trail.
        append_license_event(
            &app_ctx,
            license.id,
            "revoked",
            None,
            serde_json::json!({
                "order_id": order_id,
                "reason": "order_refunded",
            }),
        );
        revoke_offline_tokens_for_license(license.id);
        revoked.push(license_id);
    }
    ctx.add_result(serde_json::json!({
        "revoked": revoked,
        "not_found": not_found,
        "order_id": order_id,
    }));
}

// ---------------------------------------------------------------------------
// Request / response types.
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct CreateLicenseRequest {
    product: String,
    #[serde(default)]
    license_key: Option<String>,
    #[serde(default)]
    user_id: Option<Uuid>,
    #[serde(default)]
    company_id: Option<Uuid>,
    #[serde(default = "default_max_activations")]
    max_activations: i32,
    #[serde(default)]
    expires_at: Option<DateTime<Utc>>,
}

fn default_max_activations() -> i32 {
    1
}

#[derive(Debug, Deserialize)]
struct UpdateLicenseRequest {
    #[serde(default)]
    product: Option<String>,
    #[serde(default)]
    max_activations: Option<i32>,
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    expires_at: Option<Option<DateTime<Utc>>>,
}

#[derive(Debug, Deserialize)]
struct ValidateRequest {
    key: String,
    #[serde(default)]
    instance_id: Option<String>,
    #[serde(default)]
    hostname: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ActivationRequest {
    key: String,
    instance_id: String,
    #[serde(default)]
    hostname: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
struct ValidationResult {
    valid: bool,
    reason: Option<String>,
    license: Option<License>,
    activations_used: usize,
    max_activations: i32,
    /// True when the license is past `expires_at` but still inside
    /// its product grace period (see `grace_state`).
    grace: bool,
    /// Whole days elapsed since `expires_at` while in grace (0 when
    /// not in grace).
    days_in_grace: i64,
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

fn validate_status(status: &str) -> UnifiedResult<()> {
    match status {
        "active" | "suspended" | "revoked" | "expired" => Ok(()),
        other => Err(UnifiedError::bad_request(format!(
            "Unknown license status \"{other}\" (expected active, suspended, revoked, or expired)"
        ))),
    }
}

// ---------------------------------------------------------------------------
// Shared validation / activation logic.
// ---------------------------------------------------------------------------

/// Validate a license key and, when an `instance_id` is supplied and the
/// license is valid, auto-register the activation (idempotent per
/// instance). Returns the validation outcome; a missing or invalid license
/// is a data result (`valid: false`), not an HTTP error.
fn validate_and_maybe_activate(
    ctx: &AppContext,
    key: &str,
    instance_id: Option<&str>,
    hostname: Option<&str>,
    auto_register: bool,
) -> UnifiedResult<ValidationResult> {
    let key = normalize_key(key);
    let license = match db_pool(ctx) {
        Some(pool) => db_find_license_by_key(&pool, &key)?,
        None => find_license_by_key(&key),
    };
    let Some(mut license) = license else {
        return Ok(ValidationResult {
            valid: false,
            reason: Some("not_found".to_string()),
            license: None,
            activations_used: 0,
            max_activations: 0,
            grace: false,
            days_in_grace: 0,
        });
    };
    // Grace state is computed once, before any status change: a
    // license inside its product grace period stays `active` below
    // instead of being marked `expired`, and validates with
    // `grace: true`.
    let (grace, days_in_grace) = grace_state(&license);
    // Keep an expired license honest even if its status was never updated.
    // A license inside its product grace period is NOT marked expired
    // here: it keeps validating (with `grace: true`) until grace ends.
    if license.status == "active" {
        if let Some(expires_at) = license.expires_at {
            if expires_at <= Utc::now() && !grace {
                license.status = "expired".to_string();
                persist_license_status(ctx, &license)?;
            }
        }
    }
    let mut activations = match db_pool(ctx) {
        Some(pool) => db_load_activations(&pool, license.id)?,
        None => activations_for(license.id),
    };
    let invalid = |reason: &str| ValidationResult {
        valid: false,
        reason: Some(reason.to_string()),
        license: Some(license.clone()),
        activations_used: activations.len(),
        max_activations: license.max_activations,
        grace: false,
        days_in_grace: 0,
    };
    // A product key carries its product prefix before the checksum
    // body (for example `UNF-XXXX-XXXX-XXXX-XXXX`); strip the known
    // prefix before the checksum check so product keys validate.
    if !license_key_checksum_valid(&key, &license.product) {
        return Ok(invalid("invalid_checksum"));
    }
    match license.status.as_str() {
        "suspended" => return Ok(invalid("suspended")),
        "revoked" => return Ok(invalid("revoked")),
        "expired" => return Ok(invalid("expired")),
        "active" => {}
        _ => return Ok(invalid("invalid_status")),
    }
    if let Some(expires_at) = license.expires_at {
        if expires_at <= Utc::now() && !grace {
            return Ok(invalid("expired"));
        }
    }
    if let Some(instance_id) = instance_id {
        if activations
            .iter()
            .any(|activation| activation.instance_id == instance_id)
        {
            return Ok(ValidationResult {
                valid: true,
                reason: None,
                license: Some(license),
                activations_used: activations.len(),
                max_activations: license.max_activations,
                grace,
                days_in_grace,
            });
        }
    }
    if activations.len() as i32 >= license.max_activations {
        return Ok(invalid("seats_exceeded"));
    }
    if auto_register {
        if let Some(instance_id) = instance_id {
            let activation = LicenseActivation {
                id: Uuid::now_v7(),
                license_id: license.id,
                instance_id: instance_id.to_string(),
                hostname: hostname.map(str::to_string),
                created_at: Utc::now(),
            };
            if let Some(pool) = db_pool(ctx) {
                use crate::db::schema::license_activations::dsl as act_dsl;
                let mut conn = pool.get().map_err(|error| {
                    UnifiedError::internal(format!("Cannot get database connection: {error}"))
                })?;
                diesel::insert_into(act_dsl::license_activations)
                    .values(&NewLicenseActivation {
                        id: activation.id,
                        license_id: activation.license_id,
                        instance_id: activation.instance_id.clone(),
                        hostname: activation.hostname.clone(),
                    })
                    .execute(&mut conn)
                    .map_err(|error| {
                        UnifiedError::internal(format!("Cannot record activation: {error}"))
                    })?;
            }
            activations_store().insert(activation.id, activation.clone());
            // Activation history + hash-chained event log (SaaS): every
            // activation is recorded with actor, instance, hostname,
            // and timestamp.
            record_activation_history(
                license.id,
                "activated",
                None,
                instance_id,
                hostname,
            );
            append_license_event(
                ctx,
                license.id,
                "activated",
                None,
                serde_json::json!({
                    "instance_id": instance_id,
                    "hostname": hostname,
                }),
            );
            activations.push(activation);
            ctx.hooks.emit(
                "license.activated",
                serde_json::json!({
                    "license_id": license.id,
                    "license_key": license.license_key,
                    "instance_id": instance_id,
                }),
            );
        }
    }
    Ok(ValidationResult {
        valid: true,
        reason: None,
        license: Some(license.clone()),
        activations_used: activations.len(),
        max_activations: license.max_activations,
        grace,
        days_in_grace,
    })
}

fn persist_license_status(ctx: &AppContext, license: &License) -> UnifiedResult<()> {
    if let Some(pool) = db_pool(ctx) {
        use crate::db::schema::licenses::dsl as lic_dsl;
        #[derive(AsChangeset)]
        #[diesel(table_name = crate::db::schema::licenses)]
        struct StatusChange {
            status: String,
        }
        let mut conn = pool.get().map_err(|error| {
            UnifiedError::internal(format!("Cannot get database connection: {error}"))
        })?;
        diesel::update(lic_dsl::licenses.filter(lic_dsl::id.eq(license.id)))
            .set(StatusChange {
                status: license.status.clone(),
            })
            .execute(&mut conn)
            .map_err(|error| {
                UnifiedError::internal(format!("Cannot update license status: {error}"))
            })?;
    }
    if let Some(mut entry) = licenses_store().get_mut(&license.id) {
        entry.status = license.status.clone();
        entry.updated_at = Utc::now();
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Module entitlements: a sellable module as a product.
// ---------------------------------------------------------------------------

/// True when `license` entitles `company_id` to use `module_id`.
///
/// A module entitlement is a license whose `product` names the
/// module: either the module id itself (for example
/// `product: "meetings"`) or the namespaced form `module:<id>` (for
/// example `product: "module:meetings"`). Licenses carry no separate
/// entitlements list — the `product` field is the entitlement
/// carrier (the same field `issue_license` fills from the purchased
/// item name on `order.created`, and the store's
/// `store.entitlement.check` fallback resolves through the product
/// name). A license counts only when it belongs to the company, is
/// `active`, and has not expired. This is a read-only check: it
/// never changes a license status (unlike validation, which marks an
/// expired license `expired`).
fn module_license_matches(license: &License, company_id: Uuid, module_id: &str) -> bool {
    if license.company_id != Some(company_id) || license.status != "active" {
        return false;
    }
    if license
        .expires_at
        .is_some_and(|expires_at| expires_at <= Utc::now())
    {
        return false;
    }
    license.product == module_id
        || license.product.strip_prefix("module:") == Some(module_id)
}

/// True when `company_id` holds an active license entitling it to
/// use `module_id` as a product (see `module_license_matches`).
/// The database rows are authoritative when a pool is attached;
/// otherwise the in-memory store is scanned, following the same
/// pattern as `check_license_expiry`. Fails closed: an unparseable
/// company id, an empty module id, or a database error means no
/// entitlement.
pub fn has_module_entitlement(ctx: &AppContext, company_id: &str, module_id: &str) -> bool {
    if module_id.trim().is_empty() {
        return false;
    }
    let Ok(company_id) = company_id.parse::<Uuid>() else {
        return false;
    };
    match db_pool(ctx) {
        Some(pool) => match db_load_licenses(&pool) {
            Ok(licenses) => licenses
                .iter()
                .any(|license| module_license_matches(license, company_id, module_id)),
            Err(_) => false,
        },
        None => licenses_store()
            .iter()
            .any(|entry| module_license_matches(entry.value(), company_id, module_id)),
    }
}

/// Extensions-Manager-facing gate for enabling a sellable module.
///
/// Exact rule, in order:
/// 1. Unknown module id: 404, no event.
/// 2. Module is not sellable (`available_as_product() == false`):
///    no entitlement is needed; succeeds silently with no event.
/// 3. Sellable core module (`is_core() == true`): covered by an
///    implicit owner entitlement — core modules ship with the
///    platform, so the company running this installation already
///    holds them. Emits `module.entitlement.granted` with
///    `reason: "owner_entitlement"`.
/// 4. Sellable non-core module: requires an active license for the
///    company (see `has_module_entitlement`). On success emits
///    `module.entitlement.granted` with `reason: "license"`; on
///    failure emits `module.entitlement.denied` with
///    `reason: "no_active_entitlement"` and returns 403.
///
/// The Extensions Manager must call this before enabling a module,
/// passing the company the enable is for: its enable path carries
/// no company id today, so the gate cannot be enforced inside
/// `enable_with_dependencies` until the caller resolves the
/// acting company first.
pub fn check_module_entitlement(
    ctx: &AppContext,
    company_id: &str,
    module_id: &str,
) -> UnifiedResult<()> {
    let module = ctx
        .modules
        .get(module_id)
        .ok_or_else(|| UnifiedError::not_found(format!("Module \"{module_id}\" not found")))?;
    if !module.available_as_product() {
        return Ok(());
    }
    if module.is_core() {
        ctx.hooks.emit(
            MODULE_ENTITLEMENT_GRANTED_EVENT,
            serde_json::json!({
                "id": module_id,
                "company_id": company_id,
                "reason": "owner_entitlement",
            }),
        );
        return Ok(());
    }
    if has_module_entitlement(ctx, company_id, module_id) {
        ctx.hooks.emit(
            MODULE_ENTITLEMENT_GRANTED_EVENT,
            serde_json::json!({
                "id": module_id,
                "company_id": company_id,
                "reason": "license",
            }),
        );
        return Ok(());
    }
    ctx.hooks.emit(
        MODULE_ENTITLEMENT_DENIED_EVENT,
        serde_json::json!({
            "id": module_id,
            "company_id": company_id,
            "reason": "no_active_entitlement",
        }),
    );
    Err(UnifiedError::forbidden(format!(
        "Module \"{module_id}\" requires an active license entitlement for this company"
    ))
    .with_metadata("module_id", serde_json::json!(module_id))
    .with_metadata("company_id", serde_json::json!(company_id)))
}

// ---------------------------------------------------------------------------
// Handlers.
// ---------------------------------------------------------------------------

async fn list_licenses(
    ctx: web::Data<Arc<AppContext>>,
    params: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let mut items: Vec<License> = match db_pool(&ctx) {
            Some(pool) => db_load_licenses(&pool)?,
            None => licenses_store().iter().map(|entry| entry.value().clone()).collect(),
        };
        if let Some(search) = &params.search {
            let needle = search.to_lowercase();
            items.retain(|row| {
                row.license_key.to_lowercase().contains(&needle)
                    || row.product.to_lowercase().contains(&needle)
            });
        }
        if let Some(status) = params.filters.get("status") {
            items.retain(|row| row.status == *status);
        }
        if let Some(product) = params.filters.get("product") {
            items.retain(|row| row.product == *product);
        }
        let sort = params.allowed_sort(&["product", "status", "created_at"], "created_at");
        match sort.as_str() {
            "product" => items.sort_by(|a, b| a.product.cmp(&b.product)),
            "status" => items.sort_by(|a, b| a.status.cmp(&b.status)),
            _ => items.sort_by(|a, b| a.created_at.cmp(&b.created_at)),
        }
        if params.normalized().order == "desc" {
            items.reverse();
        }
        Ok(paginate(items, &params).to_json())
    })();
    json::respond(result, debug)
}

async fn create_license(
    ctx: web::Data<Arc<AppContext>>,
    body: web::Json<CreateLicenseRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<License> = (|| {
        if body.product.trim().is_empty() {
            return Err(UnifiedError::bad_request("product is required"));
        }
        if body.max_activations < 1 {
            return Err(UnifiedError::bad_request(
                "max_activations must be at least 1",
            ));
        }
        let key = match &body.license_key {
            Some(custom) => {
                let key = normalize_key(custom);
                if key.len() != 19 || !key.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') {
                    return Err(UnifiedError::bad_request(
                        "license_key must use the format XXXX-XXXX-XXXX-XXXX",
                    ));
                }
                key
            }
            None => generate_license_key(),
        };
        issue_license(
            &ctx,
            key,
            body.product.trim().to_string(),
            body.user_id,
            body.company_id,
            body.max_activations,
            body.expires_at,
        )
    })();
    match result {
        Ok(license) => json::success_response(license, 201),
        Err(error) => json::error_response(&error, debug),
    }
}

async fn get_license(ctx: web::Data<Arc<AppContext>>, id: web::Path<Uuid>) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<License> = (|| {
        let id = id.into_inner();
        if let Some(pool) = db_pool(&ctx) {
            use crate::db::schema::licenses::dsl as lic_dsl;
            let mut conn = pool.get().map_err(|error| {
                UnifiedError::internal(format!("Cannot get database connection: {error}"))
            })?;
            if let Some(row) = lic_dsl::licenses
                .filter(lic_dsl::id.eq(id))
                .select(License::as_select())
                .first(&mut conn)
                .optional()
                .map_err(|error| {
                    UnifiedError::internal(format!("Cannot load license: {error}"))
                })?
            {
                return Ok(row);
            }
        }
        licenses_store()
            .get(&id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("License not found"))
    })();
    json::respond(result, debug)
}

async fn update_license(
    ctx: web::Data<Arc<AppContext>>,
    id: web::Path<Uuid>,
    body: web::Json<UpdateLicenseRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<License> = (|| {
        let id = id.into_inner();
        let mut license = licenses_store()
            .get(&id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("License not found"))?;
        if let Some(status) = &body.status {
            validate_status(status)?;
            license.status = status.clone();
        }
        if let Some(product) = &body.product {
            if product.trim().is_empty() {
                return Err(UnifiedError::bad_request("product must not be empty"));
            }
            license.product = product.trim().to_string();
        }
        if let Some(max) = body.max_activations {
            if max < 1 {
                return Err(UnifiedError::bad_request(
                    "max_activations must be at least 1",
                ));
            }
            license.max_activations = max;
        }
        if let Some(expires_at) = body.expires_at {
            license.expires_at = expires_at;
        }
        license.updated_at = Utc::now();
        if let Some(pool) = db_pool(&ctx) {
            use crate::db::schema::licenses::dsl as lic_dsl;
            #[derive(AsChangeset)]
            #[diesel(table_name = crate::db::schema::licenses)]
            struct LicenseChanges {
                product: Option<String>,
                max_activations: Option<i32>,
                status: Option<String>,
                expires_at: Option<Option<DateTime<Utc>>>,
            }
            let mut conn = pool.get().map_err(|error| {
                UnifiedError::internal(format!("Cannot get database connection: {error}"))
            })?;
            diesel::update(lic_dsl::licenses.filter(lic_dsl::id.eq(id)))
                .set(LicenseChanges {
                    product: body.product.clone(),
                    max_activations: body.max_activations,
                    status: body.status.clone(),
                    expires_at: body.expires_at,
                })
                .execute(&mut conn)
                .map_err(|error| {
                    UnifiedError::internal(format!("Cannot update license: {error}"))
                })?;
        }
        licenses_store().insert(id, license.clone());
        Ok(license)
    })();
    json::respond(result, debug)
}

async fn delete_license(ctx: web::Data<Arc<AppContext>>, id: web::Path<Uuid>) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let id = id.into_inner();
        if let Some(pool) = db_pool(&ctx) {
            use crate::db::schema::license_activations::dsl as act_dsl;
            use crate::db::schema::licenses::dsl as lic_dsl;
            let mut conn = pool.get().map_err(|error| {
                UnifiedError::internal(format!("Cannot get database connection: {error}"))
            })?;
            diesel::delete(act_dsl::license_activations.filter(act_dsl::license_id.eq(id)))
                .execute(&mut conn)
                .map_err(|error| {
                    UnifiedError::internal(format!("Cannot delete license activations: {error}"))
                })?;
            diesel::delete(lic_dsl::licenses.filter(lic_dsl::id.eq(id)))
                .execute(&mut conn)
                .map_err(|error| {
                    UnifiedError::internal(format!("Cannot delete license: {error}"))
                })?;
        }
        activations_store().retain(|_, activation| activation.license_id != id);
        let removed = licenses_store().remove(&id).is_some();
        if removed {
            Ok(serde_json::json!({ "deleted": true, "id": id }))
        } else {
            Err(UnifiedError::not_found("License not found"))
        }
    })();
    json::respond(result, debug)
}

async fn validate_handler(
    ctx: web::Data<Arc<AppContext>>,
    body: web::Json<ValidateRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result = validate_and_maybe_activate(
        &ctx,
        &body.key,
        body.instance_id.as_deref(),
        body.hostname.as_deref(),
        true,
    );
    json::respond(result, debug)
}

async fn activate_handler(
    ctx: web::Data<Arc<AppContext>>,
    body: web::Json<ActivationRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let outcome = validate_and_maybe_activate(
            &ctx,
            &body.key,
            Some(&body.instance_id),
            body.hostname.as_deref(),
            true,
        )?;
        if !outcome.valid {
            return Err(UnifiedError::unprocessable(format!(
                "License activation failed: {}",
                outcome.reason.unwrap_or_else(|| "invalid".to_string())
            ))
            .with_metadata("reason", Value::String(outcome.reason.unwrap_or_default())));
        }
        let license = outcome.license.clone().expect("validated license");
        let activations = match db_pool(&ctx) {
            Some(pool) => db_load_activations(&pool, license.id)?,
            None => activations_for(license.id),
        };
        let activation = activations
            .into_iter()
            .find(|activation| activation.instance_id == body.instance_id);
        Ok(serde_json::json!({
            "license": license,
            "activation": activation,
            "activations_used": outcome.activations_used,
            "max_activations": outcome.max_activations,
        }))
    })();
    json::respond(result, debug)
}

async fn deactivate_handler(
    ctx: web::Data<Arc<AppContext>>,
    body: web::Json<ActivationRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let key = normalize_key(&body.key);
        let license = match db_pool(&ctx) {
            Some(pool) => db_find_license_by_key(&pool, &key)?,
            None => find_license_by_key(&key),
        }
        .ok_or_else(|| UnifiedError::not_found("License not found"))?;
        let mut removed = false;
        if let Some(pool) = db_pool(&ctx) {
            use crate::db::schema::license_activations::dsl as act_dsl;
            let mut conn = pool.get().map_err(|error| {
                UnifiedError::internal(format!("Cannot get database connection: {error}"))
            })?;
            let count = diesel::delete(
                act_dsl::license_activations
                    .filter(act_dsl::license_id.eq(license.id))
                    .filter(act_dsl::instance_id.eq(&body.instance_id)),
            )
            .execute(&mut conn)
            .map_err(|error| {
                UnifiedError::internal(format!("Cannot deactivate license: {error}"))
            })?;
            removed = count > 0;
        }
        activations_store().retain(|_, activation| {
            !(activation.license_id == license.id && activation.instance_id == body.instance_id)
                || {
                    removed = true;
                    false
                }
        });
        // The retain closure above both removes and records the outcome;
        // re-check the in-memory store when no pool was present.
        if db_pool(&ctx).is_none() {
            removed = !activations_store()
                .iter()
                .any(|entry| {
                    entry.value().license_id == license.id
                        && entry.value().instance_id == body.instance_id
                })
                && removed;
        }
        if !removed {
            return Err(UnifiedError::not_found(
                "No activation exists for this instance",
            ));
        }
        ctx.hooks.emit(
            "license.deactivated",
            serde_json::json!({
                "license_id": license.id,
                "instance_id": body.instance_id,
            }),
        );
        // Activation history + hash-chained event log (SaaS).
        record_activation_history(
            license.id,
            "deactivated",
            None,
            &body.instance_id,
            None,
        );
        append_license_event(
            &ctx,
            license.id,
            "deactivated",
            None,
            serde_json::json!({ "instance_id": body.instance_id }),
        );
        let activations_used = match db_pool(&ctx) {
            Some(pool) => db_load_activations(&pool, license.id)?.len(),
            None => activations_for(license.id).len(),
        };
        Ok(serde_json::json!({
            "license": license,
            "instance_id": body.instance_id,
            "activations_used": activations_used,
            "max_activations": license.max_activations,
        }))
    })();
    json::respond(result, debug)
}

/// Run the expiry scan now and return its summary. This is the
/// operator trigger for the scan until the scheduler gains a
/// license job kind (see `LICENSE_EXPIRY_CHECK_REQUESTED`).
async fn expiry_check_handler(ctx: web::Data<Arc<AppContext>>) -> HttpResponse {
    let debug = ctx.is_debug();
    let result = check_license_expiry(&ctx);
    json::respond(result, debug)
}

async fn suspend_handler(ctx: web::Data<Arc<AppContext>>, id: web::Path<Uuid>) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<License> = (|| {
        let id = id.into_inner();
        let mut license = licenses_store()
            .get(&id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("License not found"))?;
        license.status = "suspended".to_string();
        license.updated_at = Utc::now();
        persist_license_status(&ctx, &license)?;
        ctx.hooks.emit(
            "license.suspended",
            serde_json::json!({ "license_id": license.id, "license_key": license.license_key }),
        );
        append_license_event(
            &ctx,
            license.id,
            "suspended",
            None,
            serde_json::json!({ "license_key": license.license_key }),
        );
        Ok(license)
    })();
    json::respond(result, debug)
}

async fn revoke_handler(ctx: web::Data<Arc<AppContext>>, id: web::Path<Uuid>) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<License> = (|| {
        let id = id.into_inner();
        let mut license = licenses_store()
            .get(&id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("License not found"))?;
        license.status = "revoked".to_string();
        license.updated_at = Utc::now();
        persist_license_status(&ctx, &license)?;
        ctx.hooks.emit(
            events::LICENSE_REVOKED,
            serde_json::json!({ "license_id": license.id, "license_key": license.license_key }),
        );
        append_license_event(
            &ctx,
            license.id,
            "revoked",
            None,
            serde_json::json!({ "license_key": license.license_key }),
        );
        revoke_offline_tokens_for_license(license.id);
        Ok(license)
    })();
    json::respond(result, debug)
}

async fn list_activations(
    ctx: web::Data<Arc<AppContext>>,
    id: web::Path<Uuid>,
    params: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let id = id.into_inner();
        let exists = match db_pool(&ctx) {
            Some(pool) => {
                use crate::db::schema::licenses::dsl as lic_dsl;
                let mut conn = pool.get().map_err(|error| {
                    UnifiedError::internal(format!("Cannot get database connection: {error}"))
                })?;
                lic_dsl::licenses
                    .filter(lic_dsl::id.eq(id))
                    .select(License::as_select())
                    .first(&mut conn)
                    .optional()
                    .map_err(|error| {
                        UnifiedError::internal(format!("Cannot load license: {error}"))
                    })?
                    .is_some()
            }
            None => licenses_store().contains_key(&id),
        };
        if !exists {
            return Err(UnifiedError::not_found("License not found"));
        }
        let mut items = match db_pool(&ctx) {
            Some(pool) => db_load_activations(&pool, id)?,
            None => activations_for(id),
        };
        if let Some(search) = &params.search {
            let needle = search.to_lowercase();
            items.retain(|row| {
                row.instance_id.to_lowercase().contains(&needle)
                    || row
                        .hostname
                        .as_deref()
                        .unwrap_or_default()
                        .to_lowercase()
                        .contains(&needle)
            });
        }
        items.sort_by(|a, b| a.created_at.cmp(&b.created_at));
        if params.normalized().order == "desc" {
            items.reverse();
        }
        Ok(paginate(items, &params).to_json())
    })();
    json::respond(result, debug)
}

// ---------------------------------------------------------------------------
// Licensing as a service (batch 7).
//
// New tables (migration 2026-11-22-000068_licensing_saas): see the
// migration for the PostgreSQL DDL. In this round the tables below
// live in memory (DashMap), the same pattern as the rest of this
// module; the integrator wires the migration tables into
// `src/db/schema.rs` / `src/db/models.rs` and the Diesel writers.
// No handler below touches `src/db/schema.rs` tables that do not
// exist yet, so nothing here can fail to compile on a missing table.
// ---------------------------------------------------------------------------

/// Emitted when a public verification call fails (invalid key, wrong
/// product, expired, suspended, revoked, or out of seats). Payload:
/// `{"license_key_masked", "product", "reason", "api_key_id"}`. The
/// full key is never emitted here: only the first four characters.
pub const LICENSE_VERIFY_FAILED: &str = "license.verify.failed";
/// Emitted by the expiry scan when a license newly enters a
/// per-product renewal-reminder window.
pub const LICENSE_RENEWAL_REMINDER: &str = "license.renewal.reminder";
/// Emitted once when a license enters its product grace period.
pub const LICENSE_GRACE_STARTED: &str = "license.grace.started";
/// Emitted when an upgrade / plan-change event adjusts a license.
pub const LICENSE_UPGRADED: &str = "license.upgraded";
/// Emitted on every hash-chained event-log append, carrying
/// `{"license_id", "event_id", "hash"}` so an anchoring consumer
/// (for example the Web3 module) can anchor the hash on-chain.
/// Licensing never calls the Web3 module directly.
pub const LICENSE_ANCHORED: &str = "license.anchored";
/// Emitted when a licensed release download link is issued.
pub const LICENSE_DOWNLOAD: &str = "license.download";
/// Emitted when an admin changes a license seat count.
pub const LICENSE_SEATS_CHANGED: &str = "license.seats.changed";
/// Emitted when a git provider webhook reports a release/tag.
pub const LICENSE_DISTRIBUTION_RELEASE: &str = "license.distribution.release";
/// Store/billing upgrade events licensing listens to. Neither event
/// is emitted by the store or billing modules yet at the time of
/// writing (verified by searching their sources); the handlers below
/// document the expected payload and apply as soon as the emitters
/// land. Until then upgrades are also reachable through the seats
/// and license-update endpoints.
pub const STORE_ORDER_UPGRADED: &str = "store.order.upgraded";
pub const BILLING_PLAN_CHANGED: &str = "billing.plan.changed";

/// Default renewal-reminder windows (days before expiry) when a
/// product does not configure its own.
pub const DEFAULT_RENEWAL_REMINDER_DAYS: &[i64] = &[30, 14, 7, 1];
/// Default offline-token allowance (days) when a product does not
/// configure its own.
pub const DEFAULT_OFFLINE_MAX_DAYS: i64 = 30;
/// Hard cap for any offline token, even when a product allows more.
pub const MAX_OFFLINE_TOKEN_DAYS: i64 = 90;
/// Maximum release upload size (100 MiB, the same cap as the storage
/// module's object upload).
pub const MAX_RELEASE_UPLOAD_BYTES: usize = 100 * 1024 * 1024;
/// Lifetime of a signed download link (1 hour, the same default as
/// the store module's signed download links).
pub const SIGNED_DOWNLOAD_TTL_SECS: i64 = 3600;
/// Default storage target name for releases when no explicit target
/// is given and a provider of that name is registered.
pub const DEFAULT_RELEASE_TARGET: &str = "licensing";

pub const KEY_FORMAT_STANDARD: &str = "standard";
pub const KEY_FORMAT_CUSTOM: &str = "custom";
pub const DISTRIBUTION_CHANNEL_GIT: &str = "git";
pub const DISTRIBUTION_CHANNEL_API: &str = "api";
pub const DISTRIBUTION_CHANNEL_FILE: &str = "file";

/// Honest trust note included in offline-token responses so no
/// consumer can mistake an offline token for a revocable credential.
const OFFLINE_TRUST_NOTE: &str = "Offline token trust model: this token is a bearer credential and can be replayed until expiry; it is valid for exactly one product; revoking the license or the token does not affect verifications that already hold the token; offline verification performs no database lookup.";

// ---------------------------------------------------------------------------
// SaaS records (in-memory in this round; see the section header).
// ---------------------------------------------------------------------------

/// Per-product licensing settings. `product` is the slug, unique.
/// `key_format` is `standard` (prefix + checksum body) or `custom`
/// (the generated key must also match `key_pattern`, a regex).
#[derive(Debug, Clone, Serialize)]
pub struct LicenseProduct {
    pub id: Uuid,
    pub product: String,
    pub display_name: Option<String>,
    /// Product owner (user id). When set, a public verification API
    /// key must belong to this owner; see `authenticate_public_key`.
    pub owner_id: Option<Uuid>,
    pub key_prefix: Option<String>,
    /// Number of 4-character segments in the key body (default 4).
    pub segments: i32,
    pub key_format: String,
    pub key_pattern: Option<String>,
    pub default_max_activations: i32,
    pub default_validity_days: Option<i64>,
    pub grace_period_days: i64,
    pub renewal_reminder_days: Vec<i64>,
    pub offline_max_days: i64,
    pub allowed_distribution_channels: Vec<String>,
    /// Endpoints the verification API may serve for this product
    /// (informational allowlist; empty means the verify endpoint).
    pub api_allowed_endpoints: Vec<String>,
    pub api_rate_limit_per_minute: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// One git distribution channel for a product. The raw webhook
/// secret is held in memory only (it is needed to verify the
/// provider HMAC) and is never serialized; only its SHA-256 hash is
/// kept for audit and for the database column.
#[derive(Debug, Clone, Serialize)]
pub struct LicenseDistribution {
    pub id: Uuid,
    pub product: String,
    /// `github`, `gitlab`, `bitbucket`, or `generic`.
    pub provider: String,
    /// Repository as `owner/repo`.
    pub repo: String,
    /// Branch or tag reference (default `main`).
    pub git_ref: String,
    #[serde(skip_serializing)]
    pub webhook_secret: Option<String>,
    pub webhook_secret_hash: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Public view of a distribution: the secret never leaves the server.
#[derive(Debug, Clone, Serialize)]
struct LicenseDistributionView {
    id: Uuid,
    product: String,
    provider: String,
    repo: String,
    git_ref: String,
    webhook_secret_configured: bool,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

impl From<&LicenseDistribution> for LicenseDistributionView {
    fn from(record: &LicenseDistribution) -> Self {
        Self {
            id: record.id,
            product: record.product.clone(),
            provider: record.provider.clone(),
            repo: record.repo.clone(),
            git_ref: record.git_ref.clone(),
            webhook_secret_configured: record.webhook_secret.is_some(),
            created_at: record.created_at,
            updated_at: record.updated_at,
        }
    }
}

/// One uploaded build/package attached to a licensed product.
/// `storage_key` is the object key in the storage target, or the
/// in-memory blob key when no storage target was available.
#[derive(Debug, Clone, Serialize)]
pub struct LicenseRelease {
    pub id: Uuid,
    pub product_id: Uuid,
    pub product: String,
    /// Semantic version (`1.2.3`, optionally with pre-release /
    /// build metadata).
    pub version: String,
    pub file_name: String,
    pub storage_target: Option<String>,
    pub storage_key: String,
    pub sha256_hex: String,
    pub size_bytes: i64,
    pub download_count: i64,
    pub created_at: DateTime<Utc>,
}

/// One hash-chained license event. `hash` is the SHA-256 hex of the
/// canonical payload `prev_hash + "\n" + license_id + "\n" + event +
/// "\n" + actor + "\n" + details_json`; `prev_hash` is the previous
/// entry's hash, or `GENESIS` for the first entry of a license.
#[derive(Debug, Clone, Serialize)]
pub struct LicenseEvent {
    pub id: Uuid,
    pub license_id: Uuid,
    pub event: String,
    pub actor: Option<String>,
    pub details: Value,
    pub prev_hash: String,
    pub hash: String,
    pub created_at: DateTime<Utc>,
}

/// One signed offline token record. The token itself is never stored
/// (only its id and expiry); revocation here only affects online
/// checks, never verifications that already hold the token.
#[derive(Debug, Clone, Serialize)]
pub struct LicenseOfflineToken {
    pub token_id: Uuid,
    pub license_id: Uuid,
    pub product: String,
    pub expires_at: DateTime<Utc>,
    pub issued_at: DateTime<Utc>,
    pub revoked: bool,
    pub created_at: DateTime<Utc>,
}

/// One activation-history row: every activate and deactivate, with
/// actor, instance, hostname, and timestamp.
#[derive(Debug, Clone, Serialize)]
pub struct LicenseActivationHistory {
    pub id: Uuid,
    pub license_id: Uuid,
    pub action: String,
    pub actor: Option<String>,
    pub instance_id: String,
    pub hostname: Option<String>,
    pub created_at: DateTime<Utc>,
}

static LICENSE_PRODUCTS: OnceLock<DashMap<Uuid, LicenseProduct>> = OnceLock::new();
static LICENSE_DISTRIBUTIONS: OnceLock<DashMap<Uuid, LicenseDistribution>> = OnceLock::new();
static LICENSE_RELEASES: OnceLock<DashMap<Uuid, LicenseRelease>> = OnceLock::new();
static LICENSE_EVENTS: OnceLock<DashMap<Uuid, LicenseEvent>> = OnceLock::new();
static LICENSE_EVENT_LAST_HASH: OnceLock<DashMap<Uuid, String>> = OnceLock::new();
static LICENSE_OFFLINE_TOKENS: OnceLock<DashMap<Uuid, LicenseOfflineToken>> = OnceLock::new();
static LICENSE_ACTIVATION_HISTORY: OnceLock<DashMap<Uuid, LicenseActivationHistory>> =
    OnceLock::new();
/// Release bytes kept in memory only when no storage target is
/// registered for the release (documented fallback).
static RELEASE_BLOBS: OnceLock<DashMap<Uuid, Vec<u8>>> = OnceLock::new();
/// Per-API-key verification rate-limit buckets: key id ->
/// (window start, calls in this window).
static VERIFY_BUCKETS: OnceLock<DashMap<String, (Instant, u32)>> = OnceLock::new();
/// Failed public verification attempts, keyed by normalized license
/// key (in-memory counters only, never returned).
static FAILED_VERIFICATIONS: OnceLock<DashMap<String, u64>> = OnceLock::new();

fn license_products_store() -> &'static DashMap<Uuid, LicenseProduct> {
    LICENSE_PRODUCTS.get_or_init(DashMap::new)
}

fn license_distributions_store() -> &'static DashMap<Uuid, LicenseDistribution> {
    LICENSE_DISTRIBUTIONS.get_or_init(DashMap::new)
}

fn license_releases_store() -> &'static DashMap<Uuid, LicenseRelease> {
    LICENSE_RELEASES.get_or_init(DashMap::new)
}

fn license_events_store() -> &'static DashMap<Uuid, LicenseEvent> {
    LICENSE_EVENTS.get_or_init(DashMap::new)
}

fn license_event_last_hash_store() -> &'static DashMap<Uuid, String> {
    LICENSE_EVENT_LAST_HASH.get_or_init(DashMap::new)
}

fn license_offline_tokens_store() -> &'static DashMap<Uuid, LicenseOfflineToken> {
    LICENSE_OFFLINE_TOKENS.get_or_init(DashMap::new)
}

fn license_activation_history_store() -> &'static DashMap<Uuid, LicenseActivationHistory> {
    LICENSE_ACTIVATION_HISTORY.get_or_init(DashMap::new)
}

fn release_blobs_store() -> &'static DashMap<Uuid, Vec<u8>> {
    RELEASE_BLOBS.get_or_init(DashMap::new)
}

fn verify_buckets_store() -> &'static DashMap<String, (Instant, u32)> {
    VERIFY_BUCKETS.get_or_init(DashMap::new)
}

fn failed_verifications_store() -> &'static DashMap<String, u64> {
    FAILED_VERIFICATIONS.get_or_init(DashMap::new)
}

// ---------------------------------------------------------------------------
// Small shared helpers (SaaS).
// ---------------------------------------------------------------------------

fn sha256_hex(data: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(data);
    hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// Constant-time comparison, the same construction the store module
/// uses for its download signatures.
fn constant_time_equal(expected: &str, actual: &str) -> bool {
    let expected = expected.as_bytes();
    let actual = actual.as_bytes();
    if expected.len() != actual.len() {
        return false;
    }
    expected
        .iter()
        .zip(actual.iter())
        .fold(0u8, |acc, (a, b)| acc | (a ^ b))
        == 0
}

/// HMAC signing secret: `store.download_signing_secret` when it is
/// configured (so signed tokens and download links survive restarts),
/// otherwise a random per-process secret generated once. The store
/// module documents the same fallback for its own signed links.
fn hmac_secret(ctx: &AppContext) -> Vec<u8> {
    let configured = ctx
        .config
        .read()
        .expect("config lock poisoned")
        .store
        .download_signing_secret
        .clone();
    if !configured.trim().is_empty() {
        return configured.into_bytes();
    }
    static FALLBACK_SECRET: OnceLock<Vec<u8>> = OnceLock::new();
    FALLBACK_SECRET
        .get_or_init(|| {
            log::warn!(
                "store.download_signing_secret is not set; licensing offline tokens and signed download links use a random per-process secret and stop verifying after a restart"
            );
            let mut rng = rand::rng();
            (0..32).map(|_| rng.random::<u8>()).collect()
        })
        .clone()
}

/// Load one license by id: in-memory store first, database when a
/// pool is attached and the license is not in memory.
fn load_license(ctx: &AppContext, id: Uuid) -> UnifiedResult<Option<License>> {
    if let Some(license) = licenses_store().get(&id) {
        return Ok(Some(license.value().clone()));
    }
    if let Some(pool) = db_pool(ctx) {
        return db_find_license_by_id(&pool, id);
    }
    Ok(None)
}

/// Persist a license's mutable fields (product, seats, status,
/// expiry) to the database when a pool is attached, and always to
/// the in-memory store.
fn persist_license(ctx: &AppContext, license: &License) -> UnifiedResult<()> {
    if let Some(pool) = db_pool(ctx) {
        use crate::db::schema::licenses::dsl as lic_dsl;
        #[derive(AsChangeset)]
        #[diesel(table_name = crate::db::schema::licenses)]
        struct LicenseUpdate {
            product: String,
            max_activations: i32,
            status: String,
            expires_at: Option<DateTime<Utc>>,
        }
        let mut conn = pool.get().map_err(|error| {
            UnifiedError::internal(format!("Cannot get database connection: {error}"))
        })?;
        diesel::update(lic_dsl::licenses.filter(lic_dsl::id.eq(license.id)))
            .set(LicenseUpdate {
                product: license.product.clone(),
                max_activations: license.max_activations,
                status: license.status.clone(),
                expires_at: license.expires_at,
            })
            .execute(&mut conn)
            .map_err(|error| {
                UnifiedError::internal(format!("Cannot update license: {error}"))
            })?;
    }
    licenses_store().insert(license.id, license.clone());
    Ok(())
}

/// Product settings for a slug (case-insensitive), if configured.
fn product_settings_for(product: &str) -> Option<LicenseProduct> {
    let needle = product.trim().to_lowercase();
    license_products_store()
        .iter()
        .find(|entry| entry.value().product.to_lowercase() == needle)
        .map(|entry| entry.value().clone())
}

/// Grace period (days) for a product; 0 when the product has no
/// settings or configures no grace.
fn grace_days_for(product: &str) -> i64 {
    product_settings_for(product)
        .map(|settings| settings.grace_period_days.max(0))
        .unwrap_or(0)
}

/// Renewal-reminder windows (days) for a product; the documented
/// defaults when the product has no settings.
fn renewal_reminder_days_for(product: &str) -> Vec<i64> {
    match product_settings_for(product) {
        Some(settings) if !settings.renewal_reminder_days.is_empty() => {
            settings.renewal_reminder_days
        }
        _ => DEFAULT_RENEWAL_REMINDER_DAYS.to_vec(),
    }
}

/// `(in_grace, days_in_grace)` for a license: true when the license
/// is past `expires_at` but the overrun is within the product grace
/// period. A license already marked `expired` is not revived by
/// grace: grace only applies while the status is still `active`.
fn grace_state(license: &License) -> (bool, i64) {
    let Some(expires_at) = license.expires_at else {
        return (false, 0);
    };
    let now = Utc::now();
    if expires_at > now {
        return (false, 0);
    }
    let grace_days = grace_days_for(&license.product);
    if grace_days <= 0 {
        return (false, 0);
    }
    let overdue = (now - expires_at).num_days();
    if overdue <= grace_days {
        (true, overdue)
    } else {
        (false, 0)
    }
}

/// Checksum validation for a key that may carry a product prefix:
/// strip the product's known prefix (for example `UNF-`) and check
/// the remaining 16-character body. Keys for products without
/// settings, or without a prefix, use the plain checksum check.
fn license_key_checksum_valid(key: &str, product: &str) -> bool {
    if let Some(settings) = product_settings_for(product) {
        if let Some(prefix) = &settings.key_prefix {
            let prefix = prefix.trim().to_uppercase();
            if !prefix.is_empty() {
                let prefixed = format!("{prefix}-");
                if let Some(rest) = key.strip_prefix(prefixed.as_str()) {
                    return key_checksum_valid(rest);
                }
            }
        }
    }
    key_checksum_valid(key)
}

/// Generate a key honouring a product's format: an optional prefix
/// plus `segments` 4-character groups whose final character is the
/// checksum over the body characters. A `custom` format must also
/// match the product regex; generation retries until a candidate
/// matches (bounded), then fails instead of issuing a bad key.
pub fn generate_product_key(product: &LicenseProduct) -> UnifiedResult<String> {
    let prefix = product
        .key_prefix
        .clone()
        .unwrap_or_default()
        .trim()
        .to_uppercase();
    let segments = product.segments.clamp(1, 8) as usize;
    let body_chars = segments * 4;
    let pattern = match &product.key_pattern {
        Some(pattern) => Some(
            Regex::new(pattern)
                .map_err(|error| UnifiedError::bad_request(format!("Invalid key pattern: {error}")))?,
        ),
        None => None,
    };
    for _ in 0..32 {
        let mut indices: Vec<usize> = (0..body_chars - 1)
            .map(|_| rand::rng().random_range(0..KEY_ALPHABET.len()))
            .collect();
        let checksum = indices.iter().sum::<usize>() % KEY_ALPHABET.len();
        indices.push(checksum);
        let chars: Vec<char> = indices
            .iter()
            .map(|index| KEY_ALPHABET[*index] as char)
            .collect();
        let body: String = chars
            .chunks(4)
            .map(|chunk| chunk.iter().collect::<String>())
            .collect::<Vec<String>>()
            .join("-");
        let key = if prefix.is_empty() {
            body
        } else {
            format!("{prefix}-{body}")
        };
        if product.key_format == KEY_FORMAT_CUSTOM {
            match &pattern {
                Some(regex) if regex.is_match(&key) => return Ok(key),
                Some(_) => continue,
                None => {
                    return Err(UnifiedError::bad_request(
                        "Custom key format requires a key_pattern",
                    ))
                }
            }
        }
        return Ok(key);
    }
    Err(UnifiedError::bad_request(
        "Custom key pattern could not be satisfied by generated keys",
    ))
}

/// Append one hash-chained entry to the license event log and emit
/// `license.anchored` with `{license_id, event_id, hash}`. The
/// chain is tamper-evident: any edit to an earlier entry breaks
/// every later hash (see `verify_event_chain`).
fn append_license_event(
    ctx: &AppContext,
    license_id: Uuid,
    event: &str,
    actor: Option<&str>,
    details: Value,
) -> LicenseEvent {
    let prev_hash = license_event_last_hash_store()
        .get(&license_id)
        .map(|entry| entry.value().clone())
        .unwrap_or_else(|| "GENESIS".to_string());
    let details_json = serde_json::to_string(&details).unwrap_or_default();
    let canonical = format!(
        "{prev_hash}\n{license_id}\n{event}\n{}\n{details_json}",
        actor.unwrap_or("")
    );
    let hash = sha256_hex(canonical.as_bytes());
    let entry = LicenseEvent {
        id: Uuid::now_v7(),
        license_id,
        event: event.to_string(),
        actor: actor.map(str::to_string),
        details,
        prev_hash: prev_hash.clone(),
        hash: hash.clone(),
        created_at: Utc::now(),
    };
    license_events_store().insert(entry.id, entry.clone());
    license_event_last_hash_store().insert(license_id, hash.clone());
    ctx.hooks.emit(
        LICENSE_ANCHORED,
        serde_json::json!({
            "license_id": license_id,
            "event_id": entry.id,
            "hash": hash,
        }),
    );
    entry
}

/// Recompute a license's event chain in creation order. Returns
/// `(valid, broken_at)`: `broken_at` is the first entry id whose
/// stored hash does not match its recomputed hash, or whose
/// `prev_hash` does not match the previous entry.
fn verify_event_chain(license_id: Uuid) -> (bool, Option<Uuid>) {
    let mut entries: Vec<LicenseEvent> = license_events_store()
        .iter()
        .filter(|entry| entry.value().license_id == license_id)
        .map(|entry| entry.value().clone())
        .collect();
    entries.sort_by(|a, b| a.created_at.cmp(&b.created_at).then(a.id.cmp(&b.id)));
    let mut prev_hash = "GENESIS".to_string();
    for entry in &entries {
        if entry.prev_hash != prev_hash {
            return (false, Some(entry.id));
        }
        let details_json = serde_json::to_string(&entry.details).unwrap_or_default();
        let canonical = format!(
            "{prev_hash}\n{}\n{}\n{}\n{details_json}",
            entry.license_id,
            entry.event,
            entry.actor.clone().unwrap_or_default()
        );
        if sha256_hex(canonical.as_bytes()) != entry.hash {
            return (false, Some(entry.id));
        }
        prev_hash = entry.hash.clone();
    }
    (true, None)
}

/// Record one activation-history row (activate or deactivate).
fn record_activation_history(
    license_id: Uuid,
    action: &str,
    actor: Option<&str>,
    instance_id: &str,
    hostname: Option<&str>,
) {
    let id = Uuid::now_v7();
    license_activation_history_store().insert(
        id,
        LicenseActivationHistory {
            id,
            license_id,
            action: action.to_string(),
            actor: actor.map(str::to_string),
            instance_id: instance_id.to_string(),
            hostname: hostname.map(str::to_string),
            created_at: Utc::now(),
        },
    );
}

/// Revoke every recorded offline token of a license (server-side
/// only; already-issued offline verifications are unaffected).
fn revoke_offline_tokens_for_license(license_id: Uuid) {
    for mut entry in license_offline_tokens_store().iter_mut() {
        if entry.value().license_id == license_id {
            entry.value_mut().revoked = true;
        }
    }
}

// ---------------------------------------------------------------------------
// Capability checks and public API-key authentication (SaaS).
// ---------------------------------------------------------------------------

/// Viewer identity for admin handlers, from `?viewer_role=` /
/// `?viewer_id=`. The known gap applies here exactly as in the
/// projects module: `viewer_id` / `viewer_role` arrive as trusted
/// query parameters until the auth extractor lands, so a caller can
/// claim any role the roles module will answer for. Capability
/// checks still fail closed when no role is supplied or the roles
/// module does not answer `role.check`.
#[derive(Debug, Deserialize)]
struct ViewerQuery {
    #[serde(default)]
    viewer_role: Option<String>,
    #[serde(default)]
    viewer_id: Option<String>,
}

/// Fail when no role was supplied or the roles module does not grant
/// `permission` for it (the projects module's `require_capability`
/// pattern: ask `role.check` on the hook bus, fail closed on silence).
fn require_licensing_capability(
    ctx: &AppContext,
    role: Option<&str>,
    permission: &str,
) -> UnifiedResult<()> {
    let denied = || {
        UnifiedError::forbidden(format!(
            "Role lacks required capability: {permission}"
        ))
    };
    let Some(role) = role.map(str::trim).filter(|value| !value.is_empty()) else {
        return Err(denied());
    };
    let outcome = ctx.hooks.emit(
        "role.check",
        serde_json::json!({
            "role": role,
            "permission": permission,
            "allowed": false,
        }),
    );
    if outcome
        .get("allowed")
        .and_then(|value| value.as_bool())
        .unwrap_or(false)
    {
        Ok(())
    } else {
        Err(denied())
    }
}

/// Read the presented API key from `Authorization: Bearer <key>` or
/// `X-Api-Key` (the MCP module's shape). The key value itself is
/// never logged and never stored.
fn presented_api_key(req: &HttpRequest) -> Option<String> {
    if let Some(header) = req.headers().get("authorization") {
        if let Ok(text) = header.to_str() {
            if let Some(key) = text.strip_prefix("Bearer ") {
                let key = key.trim();
                if !key.is_empty() {
                    return Some(key.to_string());
                }
            }
        }
    }
    req.headers()
        .get("x-api-key")
        .and_then(|header| header.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

/// Per-API-key per-minute rate limit for public verification calls.
/// The cap is the key's own tier limit (`requests_per_minute` from
/// the API Keys module); 0 means unlimited. Returns 429 with
/// `retry_after_secs` in metadata when the window is exhausted.
fn check_verify_rate(key_id: &str, limit: u32) -> UnifiedResult<()> {
    if limit == 0 {
        return Ok(());
    }
    let now = Instant::now();
    let mut bucket = verify_buckets_store()
        .entry(key_id.to_string())
        .or_insert((now, 0));
    if bucket.0.elapsed() >= Duration::from_secs(60) {
        *bucket = (now, 1);
        return Ok(());
    }
    if bucket.1 >= limit {
        let retry_after = 60u64.saturating_sub(bucket.0.elapsed().as_secs()).max(1);
        return Err(UnifiedError::new(
            "API key rate limit exceeded for license verification",
            429,
        )
        .with_metadata("retry_after_secs", serde_json::json!(retry_after)));
    }
    bucket.1 += 1;
    Ok(())
}

/// Authenticate a public licensing call: a valid, unrevoked,
/// unexpired API key scoped to `licensing:<action>` (or
/// `licensing:*` / `*`, via `key_has_scope`), within its per-minute
/// rate limit. The license key itself is never an authentication
/// credential here. On success the key usage is recorded.
fn authenticate_public_key(
    req: &HttpRequest,
    action: &str,
) -> UnifiedResult<AuthenticatedKey> {
    let presented = presented_api_key(req).ok_or_else(|| {
        UnifiedError::unauthorized(
            "Missing API key: send Authorization: Bearer <key> or X-Api-Key",
        )
    })?;
    let authenticated = authenticate_key(&presented)?;
    if !key_has_scope(&authenticated.scopes, "licensing", action) {
        return Err(UnifiedError::forbidden(format!(
            "API key is not scoped to licensing:{action}; grant scope \"licensing:{action}\", \"licensing:*\", or \"*\""
        )));
    }
    check_verify_rate(
        &authenticated.id.to_string(),
        authenticated.requests_per_minute,
    )?;
    note_used(authenticated.id);
    Ok(authenticated)
}

/// Mask a license key for audit payloads: first four characters
/// plus an ellipsis (the full key never goes into a failure event).
fn mask_license_key(key: &str) -> String {
    let key = key.trim();
    if key.len() <= 4 {
        "****".to_string()
    } else {
        format!("{}...", &key[..4])
    }
}

/// Count a failed public verification and emit
/// `license.verify.failed` (masked key only).
fn record_failed_verification(
    ctx: &AppContext,
    key: &str,
    product: &str,
    reason: &str,
    api_key_id: Option<Uuid>,
) {
    let normalized = normalize_key(key);
    let mut count = failed_verifications_store()
        .entry(normalized)
        .or_insert(0);
    *count += 1;
    let attempts = *count;
    drop(count);
    ctx.hooks.emit(
        LICENSE_VERIFY_FAILED,
        serde_json::json!({
            "license_key_masked": mask_license_key(key),
            "product": product,
            "reason": reason,
            "api_key_id": api_key_id,
            "failed_attempts_for_key": attempts,
        }),
    );
}

// ---------------------------------------------------------------------------
// SaaS request types.
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct CreateLicenseProductRequest {
    product: String,
    #[serde(default)]
    display_name: Option<String>,
    #[serde(default)]
    owner_id: Option<Uuid>,
    #[serde(default)]
    key_prefix: Option<String>,
    #[serde(default = "default_product_segments")]
    segments: i32,
    #[serde(default = "default_key_format")]
    key_format: String,
    #[serde(default)]
    key_pattern: Option<String>,
    #[serde(default = "default_product_max_activations")]
    default_max_activations: i32,
    #[serde(default)]
    default_validity_days: Option<i64>,
    #[serde(default)]
    grace_period_days: i64,
    #[serde(default)]
    renewal_reminder_days: Option<Vec<i64>>,
    #[serde(default = "default_offline_max_days_fn")]
    offline_max_days: i64,
    #[serde(default)]
    allowed_distribution_channels: Option<Vec<String>>,
    #[serde(default)]
    api_allowed_endpoints: Option<Vec<String>>,
    #[serde(default = "default_api_rate_limit_fn")]
    api_rate_limit_per_minute: i64,
}

fn default_product_segments() -> i32 {
    4
}

fn default_key_format() -> String {
    KEY_FORMAT_STANDARD.to_string()
}

fn default_product_max_activations() -> i32 {
    1
}

fn default_offline_max_days_fn() -> i64 {
    DEFAULT_OFFLINE_MAX_DAYS
}

fn default_api_rate_limit_fn() -> i64 {
    60
}

#[derive(Debug, Deserialize)]
struct UpdateLicenseProductRequest {
    #[serde(default)]
    display_name: Option<Option<String>>,
    #[serde(default)]
    owner_id: Option<Option<Uuid>>,
    #[serde(default)]
    key_prefix: Option<Option<String>>,
    #[serde(default)]
    segments: Option<i32>,
    #[serde(default)]
    key_format: Option<String>,
    #[serde(default)]
    key_pattern: Option<Option<String>>,
    #[serde(default)]
    default_max_activations: Option<i32>,
    #[serde(default)]
    default_validity_days: Option<Option<i64>>,
    #[serde(default)]
    grace_period_days: Option<i64>,
    #[serde(default)]
    renewal_reminder_days: Option<Vec<i64>>,
    #[serde(default)]
    offline_max_days: Option<i64>,
    #[serde(default)]
    allowed_distribution_channels: Option<Vec<String>>,
    #[serde(default)]
    api_allowed_endpoints: Option<Vec<String>>,
    #[serde(default)]
    api_rate_limit_per_minute: Option<i64>,
}

#[derive(Debug, Deserialize)]
struct PublicVerifyRequest {
    license_key: String,
    product: String,
    instance_id: String,
    #[serde(default)]
    hostname: Option<String>,
}

#[derive(Debug, Deserialize)]
struct UpdateSeatsRequest {
    max_activations: i32,
}

#[derive(Debug, Deserialize)]
struct IssueOfflineTokenRequest {
    #[serde(default)]
    max_days: Option<i64>,
}

#[derive(Debug, Deserialize)]
struct OfflineVerifyRequest {
    token: String,
    #[serde(default)]
    instance_id: Option<String>,
}

#[derive(Debug, Deserialize)]
struct CreateDistributionRequest {
    product: String,
    provider: String,
    repo: String,
    #[serde(default)]
    git_ref: Option<String>,
    #[serde(default)]
    webhook_secret: Option<String>,
}

#[derive(Debug, Deserialize)]
struct UpdateDistributionRequest {
    #[serde(default)]
    provider: Option<String>,
    #[serde(default)]
    repo: Option<String>,
    #[serde(default)]
    git_ref: Option<String>,
    #[serde(default)]
    webhook_secret: Option<Option<String>>,
}

#[derive(Debug, Deserialize)]
struct GitAccessQuery {
    license_key: String,
    instance_id: String,
    #[serde(default)]
    hostname: Option<String>,
}

#[derive(Debug, Deserialize)]
struct DownloadQuery {
    license_key: String,
    instance_id: String,
    #[serde(default)]
    hostname: Option<String>,
}

#[derive(Debug, Deserialize)]
struct SignedFileQuery {
    license_id: String,
    expires: i64,
    sig: String,
}

fn validate_product_slug(slug: &str) -> UnifiedResult<String> {
    let slug = slug.trim().to_lowercase();
    if slug.is_empty() {
        return Err(UnifiedError::bad_request("product slug is required"));
    }
    let valid = slug.len() <= 64
        && slug
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_')
        && slug
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_lowercase() || c.is_ascii_digit());
    if !valid {
        return Err(UnifiedError::bad_request(
            "product slug must be lowercase letters, digits, '-' or '_', up to 64 characters",
        ));
    }
    Ok(slug)
}

fn validate_key_format(format: &str, pattern: &Option<String>) -> UnifiedResult<()> {
    match format {
        KEY_FORMAT_STANDARD => Ok(()),
        KEY_FORMAT_CUSTOM => {
            let Some(pattern) = pattern else {
                return Err(UnifiedError::bad_request(
                    "Custom key format requires a key_pattern regex",
                ));
            };
            Regex::new(pattern).map_err(|error| {
                UnifiedError::bad_request(format!("Invalid key_pattern regex: {error}"))
            })?;
            Ok(())
        }
        other => Err(UnifiedError::bad_request(format!(
            "Unknown key_format \"{other}\" (expected standard or custom)"
        ))),
    }
}

fn validate_channels(channels: &[String]) -> UnifiedResult<()> {
    for channel in channels {
        match channel.as_str() {
            DISTRIBUTION_CHANNEL_GIT | DISTRIBUTION_CHANNEL_API | DISTRIBUTION_CHANNEL_FILE => {}
            other => {
                return Err(UnifiedError::bad_request(format!(
                    "Unknown distribution channel \"{other}\" (expected git, api, or file)"
                )))
            }
        }
    }
    Ok(())
}

fn validate_prefix(prefix: &Option<String>) -> UnifiedResult<Option<String>> {
    let Some(prefix) = prefix else {
        return Ok(None);
    };
    let prefix = prefix.trim().to_uppercase();
    if prefix.is_empty() {
        return Ok(None);
    }
    if prefix.len() > 8
        || !prefix
            .chars()
            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit())
    {
        return Err(UnifiedError::bad_request(
            "key_prefix must be 1-8 uppercase letters or digits",
        ));
    }
    Ok(Some(prefix))
}

// ---------------------------------------------------------------------------
// SaaS handlers: license products.
// ---------------------------------------------------------------------------

async fn list_license_products(
    ctx: web::Data<Arc<AppContext>>,
    params: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let viewer_role = params.filters.get("viewer_role").map(String::as_str);
        require_licensing_capability(&ctx, viewer_role, "licensing:view")?;
        let mut items: Vec<LicenseProduct> = license_products_store()
            .iter()
            .map(|entry| entry.value().clone())
            .collect();
        if let Some(search) = &params.search {
            let needle = search.to_lowercase();
            items.retain(|row| {
                row.product.to_lowercase().contains(&needle)
                    || row
                        .display_name
                        .as_deref()
                        .unwrap_or_default()
                        .to_lowercase()
                        .contains(&needle)
            });
        }
        let sort = params.allowed_sort(&["product", "created_at"], "created_at");
        match sort.as_str() {
            "product" => items.sort_by(|a, b| a.product.cmp(&b.product)),
            _ => items.sort_by(|a, b| a.created_at.cmp(&b.created_at)),
        }
        if params.normalized().order == "desc" {
            items.reverse();
        }
        Ok(paginate(items, &params).to_json())
    })();
    json::respond(result, debug)
}

async fn create_license_product(
    ctx: web::Data<Arc<AppContext>>,
    query: web::Query<ViewerQuery>,
    body: web::Json<CreateLicenseProductRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<LicenseProduct> = (|| {
        require_licensing_capability(&ctx, query.viewer_role.as_deref(), "licensing:manage")?;
        let product = validate_product_slug(&body.product)?;
        if product_settings_for(&product).is_some() {
            return Err(UnifiedError::conflict(format!(
                "A license product \"{product}\" already exists"
            )));
        }
        validate_key_format(&body.key_format, &body.key_pattern)?;
        let key_prefix = validate_prefix(&body.key_prefix)?;
        let segments = body.segments.clamp(1, 8);
        if body.default_max_activations < 1 {
            return Err(UnifiedError::bad_request(
                "default_max_activations must be at least 1",
            ));
        }
        if body.grace_period_days < 0 || body.grace_period_days > 365 {
            return Err(UnifiedError::bad_request(
                "grace_period_days must be between 0 and 365",
            ));
        }
        if body.offline_max_days < 1 || body.offline_max_days > MAX_OFFLINE_TOKEN_DAYS {
            return Err(UnifiedError::bad_request(format!(
                "offline_max_days must be between 1 and {MAX_OFFLINE_TOKEN_DAYS}"
            )));
        }
        let reminder_days = body
            .renewal_reminder_days
            .clone()
            .unwrap_or_else(|| DEFAULT_RENEWAL_REMINDER_DAYS.to_vec());
        for window in &reminder_days {
            if *window < 0 || *window > 365 {
                return Err(UnifiedError::bad_request(
                    "renewal_reminder_days entries must be between 0 and 365",
                ));
            }
        }
        let channels = body
            .allowed_distribution_channels
            .clone()
            .unwrap_or_else(|| {
                vec![
                    DISTRIBUTION_CHANNEL_GIT.to_string(),
                    DISTRIBUTION_CHANNEL_API.to_string(),
                    DISTRIBUTION_CHANNEL_FILE.to_string(),
                ]
            });
        validate_channels(&channels)?;
        if body.api_rate_limit_per_minute < 0 {
            return Err(UnifiedError::bad_request(
                "api_rate_limit_per_minute must not be negative",
            ));
        }
        let now = Utc::now();
        let record = LicenseProduct {
            id: Uuid::now_v7(),
            product,
            display_name: body.display_name.clone().map(|name| name.trim().to_string()),
            owner_id: body.owner_id,
            key_prefix,
            segments,
            key_format: body.key_format.trim().to_string(),
            key_pattern: body.key_pattern.clone(),
            default_max_activations: body.default_max_activations,
            default_validity_days: body.default_validity_days,
            grace_period_days: body.grace_period_days,
            renewal_reminder_days: reminder_days,
            offline_max_days: body.offline_max_days,
            allowed_distribution_channels: channels,
            api_allowed_endpoints: body.api_allowed_endpoints.clone().unwrap_or_default(),
            api_rate_limit_per_minute: body.api_rate_limit_per_minute,
            created_at: now,
            updated_at: now,
        };
        license_products_store().insert(record.id, record.clone());
        Ok(record)
    })();
    match result {
        Ok(record) => json::success_response(record, 201),
        Err(error) => json::error_response(&error, debug),
    }
}

async fn get_license_product(
    ctx: web::Data<Arc<AppContext>>,
    id: web::Path<Uuid>,
    query: web::Query<ViewerQuery>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<LicenseProduct> = (|| {
        require_licensing_capability(&ctx, query.viewer_role.as_deref(), "licensing:view")?;
        license_products_store()
            .get(&id.into_inner())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("License product not found"))
    })();
    json::respond(result, debug)
}

async fn update_license_product(
    ctx: web::Data<Arc<AppContext>>,
    id: web::Path<Uuid>,
    query: web::Query<ViewerQuery>,
    body: web::Json<UpdateLicenseProductRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<LicenseProduct> = (|| {
        require_licensing_capability(&ctx, query.viewer_role.as_deref(), "licensing:manage")?;
        let id = id.into_inner();
        let mut record = license_products_store()
            .get(&id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("License product not found"))?;
        // The slug is immutable: licenses, releases, and
        // distributions reference it, so renaming would orphan them.
        if let Some(name) = &body.display_name {
            record.display_name = name.clone().map(|value| value.trim().to_string());
        }
        if let Some(owner_id) = body.owner_id {
            record.owner_id = owner_id;
        }
        if let Some(prefix) = &body.key_prefix {
            record.key_prefix = validate_prefix(prefix)?;
        }
        if let Some(segments) = body.segments {
            record.segments = segments.clamp(1, 8);
        }
        if let Some(format) = &body.key_format {
            let pattern = body
                .key_pattern
                .clone()
                .unwrap_or_else(|| record.key_pattern.clone());
            validate_key_format(format, &pattern)?;
            record.key_format = format.trim().to_string();
        }
        if let Some(pattern) = &body.key_pattern {
            if let Some(pattern) = pattern {
                Regex::new(pattern).map_err(|error| {
                    UnifiedError::bad_request(format!("Invalid key_pattern regex: {error}"))
                })?;
            }
            record.key_pattern = pattern.clone();
        }
        if let Some(max) = body.default_max_activations {
            if max < 1 {
                return Err(UnifiedError::bad_request(
                    "default_max_activations must be at least 1",
                ));
            }
            record.default_max_activations = max;
        }
        if let Some(validity) = body.default_validity_days {
            record.default_validity_days = validity;
        }
        if let Some(grace) = body.grace_period_days {
            if grace < 0 || grace > 365 {
                return Err(UnifiedError::bad_request(
                    "grace_period_days must be between 0 and 365",
                ));
            }
            record.grace_period_days = grace;
        }
        if let Some(windows) = &body.renewal_reminder_days {
            for window in windows {
                if *window < 0 || *window > 365 {
                    return Err(UnifiedError::bad_request(
                        "renewal_reminder_days entries must be between 0 and 365",
                    ));
                }
            }
            record.renewal_reminder_days = windows.clone();
        }
        if let Some(days) = body.offline_max_days {
            if days < 1 || days > MAX_OFFLINE_TOKEN_DAYS {
                return Err(UnifiedError::bad_request(format!(
                    "offline_max_days must be between 1 and {MAX_OFFLINE_TOKEN_DAYS}"
                )));
            }
            record.offline_max_days = days;
        }
        if let Some(channels) = &body.allowed_distribution_channels {
            validate_channels(channels)?;
            record.allowed_distribution_channels = channels.clone();
        }
        if let Some(endpoints) = &body.api_allowed_endpoints {
            record.api_allowed_endpoints = endpoints.clone();
        }
        if let Some(limit) = body.api_rate_limit_per_minute {
            if limit < 0 {
                return Err(UnifiedError::bad_request(
                    "api_rate_limit_per_minute must not be negative",
                ));
            }
            record.api_rate_limit_per_minute = limit;
        }
        record.updated_at = Utc::now();
        license_products_store().insert(id, record.clone());
        Ok(record)
    })();
    json::respond(result, debug)
}

async fn delete_license_product(
    ctx: web::Data<Arc<AppContext>>,
    id: web::Path<Uuid>,
    query: web::Query<ViewerQuery>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        require_licensing_capability(&ctx, query.viewer_role.as_deref(), "licensing:manage")?;
        let id = id.into_inner();
        let record = license_products_store()
            .get(&id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("License product not found"))?;
        if licenses_store()
            .iter()
            .any(|entry| entry.value().product == record.product)
        {
            return Err(UnifiedError::conflict(format!(
                "License product \"{}\" still has licenses; revoke or move them first",
                record.product
            )));
        }
        license_products_store().remove(&id);
        Ok(serde_json::json!({ "deleted": true, "id": id }))
    })();
    json::respond(result, debug)
}

// ---------------------------------------------------------------------------
// SaaS handlers: public verification API.
// ---------------------------------------------------------------------------

/// Public license verification for the product owner's own
/// applications: `POST /api/v1/licensing/public/verify`.
///
/// Authentication is an API key (`Authorization: Bearer <key>` or
/// `X-Api-Key`) scoped to `licensing:read` (or `licensing:*` / `*`)
/// and, when the product configures an `owner_id`, belonging to that
/// owner. The license key in the body is the credential being
/// verified, never the authentication credential. Successful
/// verification with a new `instance_id` auto-registers the
/// activation (the same seat behaviour as `/validate`). Failed
/// attempts are counted and emit `license.verify.failed`.
/// Rate limit: the key's own tier limit per minute (429 past it).
async fn public_verify_handler(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    body: web::Json<PublicVerifyRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let authenticated = authenticate_public_key(&req, "read")?;
        let product = body.product.trim().to_string();
        if product.is_empty() || body.license_key.trim().is_empty() {
            return Err(UnifiedError::bad_request(
                "license_key and product are required",
            ));
        }
        if body.instance_id.trim().is_empty() {
            return Err(UnifiedError::bad_request("instance_id is required"));
        }
        if let Some(settings) = product_settings_for(&product) {
            if let Some(owner_id) = settings.owner_id {
                if authenticated.owner_id != Some(owner_id) {
                    return Err(UnifiedError::forbidden(
                        "API key does not belong to this product owner",
                    ));
                }
            }
        }
        let outcome = validate_and_maybe_activate(
            &ctx,
            &body.license_key,
            Some(body.instance_id.trim()),
            body.hostname.as_deref(),
            true,
        )?;
        let reason = match (&outcome.valid, &outcome.reason) {
            (true, _) => "valid".to_string(),
            (false, Some(reason)) => match reason.as_str() {
                "not_found" => "not_found".to_string(),
                "invalid_checksum" => "invalid".to_string(),
                "seats_exceeded" => "seats-exceeded".to_string(),
                other => other.to_string(),
            },
            (false, None) => "invalid".to_string(),
        };
        let Some(license) = &outcome.license else {
            record_failed_verification(
                &ctx,
                &body.license_key,
                &product,
                &reason,
                Some(authenticated.id),
            );
            return Ok(serde_json::json!({
                "valid": false,
                "reason": reason,
                "license_id": Value::Null,
                "product": product,
                "activations_used": 0,
                "max_activations": 0,
                "expires_at": Value::Null,
                "grace": false,
                "days_in_grace": 0,
                "offline": false,
            }));
        };
        if license.product != product {
            record_failed_verification(
                &ctx,
                &body.license_key,
                &product,
                "product_mismatch",
                Some(authenticated.id),
            );
            return Ok(serde_json::json!({
                "valid": false,
                "reason": "product_mismatch",
                "license_id": license.id,
                "product": license.product,
                "activations_used": outcome.activations_used,
                "max_activations": outcome.max_activations,
                "expires_at": license.expires_at,
                "grace": outcome.grace,
                "days_in_grace": outcome.days_in_grace,
                "offline": false,
            }));
        }
        if !outcome.valid {
            record_failed_verification(
                &ctx,
                &body.license_key,
                &product,
                &reason,
                Some(authenticated.id),
            );
        }
        Ok(serde_json::json!({
            "valid": outcome.valid,
            "reason": reason,
            "license_id": license.id,
            "product": license.product,
            "activations_used": outcome.activations_used,
            "max_activations": outcome.max_activations,
            "expires_at": license.expires_at,
            "grace": outcome.grace,
            "days_in_grace": outcome.days_in_grace,
            "offline": false,
        }))
    })();
    json::respond(result, debug)
}

// ---------------------------------------------------------------------------
// SaaS handlers: activation lifecycle.
// ---------------------------------------------------------------------------

async fn list_activation_history(
    ctx: web::Data<Arc<AppContext>>,
    id: web::Path<Uuid>,
    params: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        require_licensing_capability(
            &ctx,
            params
                .filters
                .get("viewer_role")
                .map(String::as_str),
            "licensing:view",
        )?;
        let id = id.into_inner();
        if load_license(&ctx, id)?.is_none() {
            return Err(UnifiedError::not_found("License not found"));
        }
        let mut items: Vec<LicenseActivationHistory> = license_activation_history_store()
            .iter()
            .filter(|entry| entry.value().license_id == id)
            .map(|entry| entry.value().clone())
            .collect();
        if let Some(search) = &params.search {
            let needle = search.to_lowercase();
            items.retain(|row| {
                row.instance_id.to_lowercase().contains(&needle)
                    || row.action.to_lowercase().contains(&needle)
            });
        }
        items.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        if params.normalized().order == "asc" {
            items.reverse();
        }
        Ok(paginate(items, &params).to_json())
    })();
    json::respond(result, debug)
}

async fn deactivate_activation(
    ctx: web::Data<Arc<AppContext>>,
    path: web::Path<(Uuid, Uuid)>,
    query: web::Query<ViewerQuery>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        require_licensing_capability(&ctx, query.viewer_role.as_deref(), "licensing:manage")?;
        let (license_id, activation_id) = path.into_inner();
        let actor = query.viewer_id.clone();
        let license = load_license(&ctx, license_id)?
            .ok_or_else(|| UnifiedError::not_found("License not found"))?;
        let activation = match db_pool(&ctx) {
            Some(pool) => db_load_activations(&pool, license_id)?
                .into_iter()
                .find(|row| row.id == activation_id),
            None => activations_store()
                .get(&activation_id)
                .map(|entry| entry.value().clone())
                .filter(|row| row.license_id == license_id),
        }
        .ok_or_else(|| UnifiedError::not_found("Activation not found for this license"))?;
        if let Some(pool) = db_pool(&ctx) {
            use crate::db::schema::license_activations::dsl as act_dsl;
            let mut conn = pool.get().map_err(|error| {
                UnifiedError::internal(format!("Cannot get database connection: {error}"))
            })?;
            diesel::delete(act_dsl::license_activations.filter(act_dsl::id.eq(activation_id)))
                .execute(&mut conn)
                .map_err(|error| {
                    UnifiedError::internal(format!("Cannot deactivate license: {error}"))
                })?;
        }
        activations_store().remove(&activation_id);
        record_activation_history(
            license_id,
            "deactivated",
            actor.as_deref(),
            &activation.instance_id,
            activation.hostname.as_deref(),
        );
        append_license_event(
            &ctx,
            license_id,
            "deactivated",
            actor.as_deref(),
            serde_json::json!({
                "instance_id": activation.instance_id,
                "activation_id": activation_id,
                "remote": true,
            }),
        );
        ctx.hooks.emit(
            "license.deactivated",
            serde_json::json!({
                "license_id": license_id,
                "instance_id": activation.instance_id,
                "activation_id": activation_id,
                "remote": true,
            }),
        );
        let activations_used = match db_pool(&ctx) {
            Some(pool) => db_load_activations(&pool, license_id)?.len(),
            None => activations_for(license_id).len(),
        };
        Ok(serde_json::json!({
            "license": license,
            "activation_id": activation_id,
            "instance_id": activation.instance_id,
            "deactivated": true,
            "activations_used": activations_used,
            "max_activations": license.max_activations,
        }))
    })();
    json::respond(result, debug)
}

async fn update_seats(
    ctx: web::Data<Arc<AppContext>>,
    id: web::Path<Uuid>,
    query: web::Query<ViewerQuery>,
    body: web::Json<UpdateSeatsRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<License> = (|| {
        require_licensing_capability(&ctx, query.viewer_role.as_deref(), "licensing:manage")?;
        let id = id.into_inner();
        let mut license = load_license(&ctx, id)?
            .ok_or_else(|| UnifiedError::not_found("License not found"))?;
        if body.max_activations < 1 {
            return Err(UnifiedError::bad_request(
                "max_activations must be at least 1",
            ));
        }
        let activations_used = match db_pool(&ctx) {
            Some(pool) => db_load_activations(&pool, id)?.len(),
            None => activations_for(id).len(),
        };
        if body.max_activations < activations_used as i32 {
            return Err(UnifiedError::conflict(format!(
                "max_activations ({}) is below the {} activation(s) already in use; deactivate an instance first",
                body.max_activations, activations_used
            )));
        }
        let previous = license.max_activations;
        license.max_activations = body.max_activations;
        license.updated_at = Utc::now();
        persist_license(&ctx, &license)?;
        append_license_event(
            &ctx,
            id,
            "seats_changed",
            query.viewer_id.as_deref(),
            serde_json::json!({
                "previous_max_activations": previous,
                "max_activations": license.max_activations,
                "activations_used": activations_used,
            }),
        );
        ctx.hooks.emit(
            LICENSE_SEATS_CHANGED,
            serde_json::json!({
                "license_id": id,
                "previous_max_activations": previous,
                "max_activations": license.max_activations,
            }),
        );
        Ok(license)
    })();
    json::respond(result, debug)
}

// ---------------------------------------------------------------------------
// SaaS handlers: hash-chained event log.
// ---------------------------------------------------------------------------

async fn list_license_events(
    ctx: web::Data<Arc<AppContext>>,
    id: web::Path<Uuid>,
    params: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        require_licensing_capability(
            &ctx,
            params
                .filters
                .get("viewer_role")
                .map(String::as_str),
            "licensing:view",
        )?;
        let id = id.into_inner();
        if load_license(&ctx, id)?.is_none() {
            return Err(UnifiedError::not_found("License not found"));
        }
        let mut items: Vec<LicenseEvent> = license_events_store()
            .iter()
            .filter(|entry| entry.value().license_id == id)
            .map(|entry| entry.value().clone())
            .collect();
        if let Some(search) = &params.search {
            let needle = search.to_lowercase();
            items.retain(|row| row.event.to_lowercase().contains(&needle));
        }
        items.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        if params.normalized().order == "asc" {
            items.reverse();
        }
        Ok(paginate(items, &params).to_json())
    })();
    json::respond(result, debug)
}

async fn verify_license_events(
    ctx: web::Data<Arc<AppContext>>,
    id: web::Path<Uuid>,
    query: web::Query<ViewerQuery>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        require_licensing_capability(&ctx, query.viewer_role.as_deref(), "licensing:view")?;
        let id = id.into_inner();
        if load_license(&ctx, id)?.is_none() {
            return Err(UnifiedError::not_found("License not found"));
        }
        let (chain_valid, broken_at) = verify_event_chain(id);
        let count = license_events_store()
            .iter()
            .filter(|entry| entry.value().license_id == id)
            .count();
        Ok(serde_json::json!({
            "license_id": id,
            "events": count,
            "chain_valid": chain_valid,
            "broken_at": broken_at,
        }))
    })();
    json::respond(result, debug)
}

// ---------------------------------------------------------------------------
// SaaS: offline tokens.
// ---------------------------------------------------------------------------

/// Signed offline-token payload. `token_id` is the server-side
/// record id; the token itself is never stored.
#[derive(Debug, Serialize, Deserialize)]
struct OfflineTokenPayload {
    license_id: Uuid,
    product: String,
    expires_at: DateTime<Utc>,
    issued_at: DateTime<Utc>,
    token_id: Uuid,
}

fn constant_time_equal_bytes(expected: &[u8], actual: &[u8]) -> bool {
    if expected.len() != actual.len() {
        return false;
    }
    expected
        .iter()
        .zip(actual.iter())
        .fold(0u8, |acc, (a, b)| acc | (a ^ b))
        == 0
}

/// Sign an offline-token payload:
/// `base64url(payload_json) + "." + base64url(HMAC-SHA256(payload_b64))`.
fn sign_offline_token(ctx: &AppContext, payload: &OfflineTokenPayload) -> UnifiedResult<String> {
    let payload_json = serde_json::to_string(payload)
        .map_err(|error| UnifiedError::internal(format!("Cannot encode offline token: {error}")))?;
    let payload_b64 = URL_SAFE_NO_PAD.encode(payload_json.as_bytes());
    let mut mac = HmacSha256::new_from_slice(&hmac_secret(ctx))
        .map_err(|_| UnifiedError::internal("Cannot initialise offline token signing"))?;
    mac.update(payload_b64.as_bytes());
    let signature_b64 = URL_SAFE_NO_PAD.encode(mac.finalize().into_bytes());
    Ok(format!("{payload_b64}.{signature_b64}"))
}

/// Verify an offline token's signature and expiry WITHOUT a
/// database lookup. A revoked recorded token is NOT rejected here:
/// revocation is checked by the caller against the in-memory
/// record, and only when a record exists (see the trust model).
fn verify_offline_token_signature(
    ctx: &AppContext,
    token: &str,
) -> UnifiedResult<OfflineTokenPayload> {
    let (payload_b64, signature_b64) = token.split_once('.').ok_or_else(|| {
        UnifiedError::bad_request("Offline token must look like <payload>.<signature>")
    })?;
    let mut mac = HmacSha256::new_from_slice(&hmac_secret(ctx))
        .map_err(|_| UnifiedError::internal("Cannot initialise offline token verification"))?;
    mac.update(payload_b64.as_bytes());
    let expected = mac.finalize().into_bytes();
    let presented = URL_SAFE_NO_PAD
        .decode(signature_b64)
        .map_err(|_| UnifiedError::unauthorized("Offline token signature is not valid base64"))?;
    if !constant_time_equal_bytes(&expected, &presented) {
        return Err(UnifiedError::unauthorized(
            "Offline token signature verification failed",
        ));
    }
    let payload_json = URL_SAFE_NO_PAD
        .decode(payload_b64)
        .map_err(|_| UnifiedError::bad_request("Offline token payload is not valid base64"))?;
    let payload: OfflineTokenPayload = serde_json::from_slice(&payload_json)
        .map_err(|_| UnifiedError::bad_request("Offline token payload is not valid JSON"))?;
    if payload.expires_at <= Utc::now() {
        return Err(UnifiedError::unprocessable("Offline token has expired"));
    }
    Ok(payload)
}

async fn issue_offline_token(
    ctx: web::Data<Arc<AppContext>>,
    id: web::Path<Uuid>,
    query: web::Query<ViewerQuery>,
    body: web::Json<IssueOfflineTokenRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        require_licensing_capability(&ctx, query.viewer_role.as_deref(), "licensing:manage")?;
        let id = id.into_inner();
        let license = load_license(&ctx, id)?
            .ok_or_else(|| UnifiedError::not_found("License not found"))?;
        if license.status != "active" {
            return Err(UnifiedError::unprocessable(format!(
                "Offline tokens can only be issued for active licenses (status is {})",
                license.status
            )));
        }
        let product_max = product_settings_for(&license.product)
            .map(|settings| settings.offline_max_days)
            .unwrap_or(DEFAULT_OFFLINE_MAX_DAYS)
            .clamp(1, MAX_OFFLINE_TOKEN_DAYS);
        let requested = body.max_days.unwrap_or(product_max);
        if requested < 1 {
            return Err(UnifiedError::bad_request("max_days must be at least 1"));
        }
        let effective_days = requested.min(product_max).min(MAX_OFFLINE_TOKEN_DAYS);
        let now = Utc::now();
        let payload = OfflineTokenPayload {
            license_id: id,
            product: license.product.clone(),
            expires_at: now + chrono::Duration::days(effective_days),
            issued_at: now,
            token_id: Uuid::now_v7(),
        };
        let token = sign_offline_token(&ctx, &payload)?;
        license_offline_tokens_store().insert(
            payload.token_id,
            LicenseOfflineToken {
                token_id: payload.token_id,
                license_id: id,
                product: license.product.clone(),
                expires_at: payload.expires_at,
                issued_at: now,
                revoked: false,
                created_at: now,
            },
        );
        append_license_event(
            &ctx,
            id,
            "offline_token_issued",
            query.viewer_id.as_deref(),
            serde_json::json!({
                "token_id": payload.token_id,
                "expires_at": payload.expires_at,
                "max_days": effective_days,
            }),
        );
        Ok(serde_json::json!({
            "token": token,
            "token_id": payload.token_id,
            "license_id": id,
            "product": license.product,
            "expires_at": payload.expires_at,
            "max_days": effective_days,
            "offline": true,
            "trust": OFFLINE_TRUST_NOTE,
        }))
    })();
    json::respond(result, debug)
}

/// Verify a signed offline token without a database lookup.
/// Returns the public-verify shape with `"offline": true`;
/// activation counts are null because no lookup happens, and the
/// trust note is included in every response.
async fn offline_verify_handler(
    ctx: web::Data<Arc<AppContext>>,
    body: web::Json<OfflineVerifyRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        if body.token.trim().is_empty() {
            return Err(UnifiedError::bad_request("token is required"));
        }
        let payload = match verify_offline_token_signature(&ctx, body.token.trim()) {
            Ok(payload) => payload,
            Err(error) => {
                return Ok(serde_json::json!({
                    "valid": false,
                    "reason": "invalid",
                    "license_id": Value::Null,
                    "product": Value::Null,
                    "activations_used": Value::Null,
                    "max_activations": Value::Null,
                    "expires_at": Value::Null,
                    "grace": false,
                    "days_in_grace": 0,
                    "offline": true,
                    "error": error.message,
                    "trust": OFFLINE_TRUST_NOTE,
                }));
            }
        };
        // Revocation only applies when this process still holds the
        // token record; a token issued before a restart has no record
        // and verifies on signature + expiry alone (trust model).
        let revoked = license_offline_tokens_store()
            .get(&payload.token_id)
            .map(|entry| entry.value().revoked)
            .unwrap_or(false);
        if revoked {
            return Ok(serde_json::json!({
                "valid": false,
                "reason": "revoked",
                "license_id": payload.license_id,
                "product": payload.product,
                "activations_used": Value::Null,
                "max_activations": Value::Null,
                "expires_at": payload.expires_at,
                "grace": false,
                "days_in_grace": 0,
                "offline": true,
                "token_id": payload.token_id,
                "trust": OFFLINE_TRUST_NOTE,
            }));
        }
        let _ = &body.instance_id;
        Ok(serde_json::json!({
            "valid": true,
            "reason": "valid",
            "license_id": payload.license_id,
            "product": payload.product,
            "activations_used": Value::Null,
            "max_activations": Value::Null,
            "expires_at": payload.expires_at,
            "grace": false,
            "days_in_grace": 0,
            "offline": true,
            "token_id": payload.token_id,
            "trust": OFFLINE_TRUST_NOTE,
        }))
    })();
    json::respond(result, debug)
}

async fn list_offline_tokens(
    ctx: web::Data<Arc<AppContext>>,
    id: web::Path<Uuid>,
    params: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        require_licensing_capability(
            &ctx,
            params
                .filters
                .get("viewer_role")
                .map(String::as_str),
            "licensing:view",
        )?;
        let id = id.into_inner();
        if load_license(&ctx, id)?.is_none() {
            return Err(UnifiedError::not_found("License not found"));
        }
        let mut items: Vec<LicenseOfflineToken> = license_offline_tokens_store()
            .iter()
            .filter(|entry| entry.value().license_id == id)
            .map(|entry| entry.value().clone())
            .collect();
        items.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        if params.normalized().order == "asc" {
            items.reverse();
        }
        Ok(paginate(items, &params).to_json())
    })();
    json::respond(result, debug)
}

async fn revoke_offline_token(
    ctx: web::Data<Arc<AppContext>>,
    path: web::Path<(Uuid, Uuid)>,
    query: web::Query<ViewerQuery>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        require_licensing_capability(&ctx, query.viewer_role.as_deref(), "licensing:manage")?;
        let (license_id, token_id) = path.into_inner();
        let Some(mut record) = license_offline_tokens_store().get_mut(&token_id) else {
            return Err(UnifiedError::not_found("Offline token not found"));
        };
        if record.license_id != license_id {
            return Err(UnifiedError::not_found(
                "Offline token not found for this license",
            ));
        }
        record.revoked = true;
        append_license_event(
            &ctx,
            license_id,
            "offline_token_revoked",
            query.viewer_id.as_deref(),
            serde_json::json!({ "token_id": token_id }),
        );
        Ok(serde_json::json!({
            "token_id": token_id,
            "license_id": license_id,
            "revoked": true,
        }))
    })();
    json::respond(result, debug)
}

// ---------------------------------------------------------------------------
// SaaS handlers: distribution channels (git).
// ---------------------------------------------------------------------------

fn validate_provider(provider: &str) -> UnifiedResult<String> {
    match provider.trim().to_lowercase().as_str() {
        "github" | "gitlab" | "bitbucket" | "generic" => {
            Ok(provider.trim().to_lowercase())
        }
        other => Err(UnifiedError::bad_request(format!(
            "Unknown provider \"{other}\" (expected github, gitlab, bitbucket, or generic)"
        ))),
    }
}

fn validate_repo(repo: &str) -> UnifiedResult<String> {
    let repo = repo.trim().to_string();
    let parts: Vec<&str> = repo.split('/').collect();
    let valid = parts.len() >= 2
        && parts.iter().all(|part| {
            !part.is_empty()
                && part
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.')
        });
    if !valid {
        return Err(UnifiedError::bad_request(
            "repo must look like \"owner/repo\" (letters, digits, '-', '_' or '.')",
        ));
    }
    Ok(repo)
}

fn distribution_view(record: &LicenseDistribution) -> LicenseDistributionView {
    LicenseDistributionView::from(record)
}

async fn list_distributions(
    ctx: web::Data<Arc<AppContext>>,
    params: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        require_licensing_capability(
            &ctx,
            params
                .filters
                .get("viewer_role")
                .map(String::as_str),
            "licensing:view",
        )?;
        let mut items: Vec<LicenseDistributionView> = license_distributions_store()
            .iter()
            .map(|entry| distribution_view(entry.value()))
            .collect();
        if let Some(product) = params.filters.get("product") {
            items.retain(|row| row.product == *product);
        }
        if let Some(search) = &params.search {
            let needle = search.to_lowercase();
            items.retain(|row| {
                row.product.to_lowercase().contains(&needle)
                    || row.repo.to_lowercase().contains(&needle)
            });
        }
        items.sort_by(|a, b| a.created_at.cmp(&b.created_at));
        if params.normalized().order == "desc" {
            items.reverse();
        }
        Ok(paginate(items, &params).to_json())
    })();
    json::respond(result, debug)
}

async fn create_distribution(
    ctx: web::Data<Arc<AppContext>>,
    query: web::Query<ViewerQuery>,
    body: web::Json<CreateDistributionRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<LicenseDistributionView> = (|| {
        require_licensing_capability(&ctx, query.viewer_role.as_deref(), "licensing:manage")?;
        let product = validate_product_slug(&body.product)?;
        let Some(settings) = product_settings_for(&product) else {
            return Err(UnifiedError::bad_request(format!(
                "Unknown license product \"{product}\": create the product first"
            )));
        };
        if !settings
            .allowed_distribution_channels
            .iter()
            .any(|channel| channel == DISTRIBUTION_CHANNEL_GIT)
        {
            return Err(UnifiedError::bad_request(format!(
                "License product \"{product}\" does not allow the git distribution channel"
            )));
        }
        let provider = validate_provider(&body.provider)?;
        let repo = validate_repo(&body.repo)?;
        let git_ref = body
            .git_ref
            .clone()
            .unwrap_or_else(|| "main".to_string())
            .trim()
            .to_string();
        if git_ref.is_empty() {
            return Err(UnifiedError::bad_request("git_ref must not be empty"));
        }
        if let Some(secret) = &body.webhook_secret {
            if secret.len() < 16 {
                return Err(UnifiedError::bad_request(
                    "webhook_secret must be at least 16 characters",
                ));
            }
        }
        let now = Utc::now();
        let record = LicenseDistribution {
            id: Uuid::now_v7(),
            product,
            provider,
            repo,
            git_ref,
            webhook_secret_hash: body
                .webhook_secret
                .as_ref()
                .map(|secret| sha256_hex(secret.as_bytes())),
            webhook_secret: body.webhook_secret.clone(),
            created_at: now,
            updated_at: now,
        };
        license_distributions_store().insert(record.id, record.clone());
        Ok(distribution_view(&record))
    })();
    match result {
        Ok(view) => json::success_response(view, 201),
        Err(error) => json::error_response(&error, debug),
    }
}

async fn get_distribution(
    ctx: web::Data<Arc<AppContext>>,
    id: web::Path<Uuid>,
    query: web::Query<ViewerQuery>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<LicenseDistributionView> = (|| {
        require_licensing_capability(&ctx, query.viewer_role.as_deref(), "licensing:view")?;
        license_distributions_store()
            .get(&id.into_inner())
            .map(|entry| distribution_view(entry.value()))
            .ok_or_else(|| UnifiedError::not_found("Distribution not found"))
    })();
    json::respond(result, debug)
}

async fn update_distribution(
    ctx: web::Data<Arc<AppContext>>,
    id: web::Path<Uuid>,
    query: web::Query<ViewerQuery>,
    body: web::Json<UpdateDistributionRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<LicenseDistributionView> = (|| {
        require_licensing_capability(&ctx, query.viewer_role.as_deref(), "licensing:manage")?;
        let id = id.into_inner();
        let mut record = license_distributions_store()
            .get(&id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Distribution not found"))?;
        if let Some(provider) = &body.provider {
            record.provider = validate_provider(provider)?;
        }
        if let Some(repo) = &body.repo {
            record.repo = validate_repo(repo)?;
        }
        if let Some(git_ref) = &body.git_ref {
            if git_ref.trim().is_empty() {
                return Err(UnifiedError::bad_request("git_ref must not be empty"));
            }
            record.git_ref = git_ref.trim().to_string();
        }
        if let Some(secret) = &body.webhook_secret {
            match secret {
                Some(secret) => {
                    if secret.len() < 16 {
                        return Err(UnifiedError::bad_request(
                            "webhook_secret must be at least 16 characters",
                        ));
                    }
                    record.webhook_secret_hash = Some(sha256_hex(secret.as_bytes()));
                    record.webhook_secret = Some(secret.clone());
                }
                None => {
                    record.webhook_secret_hash = None;
                    record.webhook_secret = None;
                }
            }
        }
        record.updated_at = Utc::now();
        license_distributions_store().insert(id, record.clone());
        Ok(distribution_view(&record))
    })();
    json::respond(result, debug)
}

async fn delete_distribution(
    ctx: web::Data<Arc<AppContext>>,
    id: web::Path<Uuid>,
    query: web::Query<ViewerQuery>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        require_licensing_capability(&ctx, query.viewer_role.as_deref(), "licensing:manage")?;
        let id = id.into_inner();
        if license_distributions_store().remove(&id).is_none() {
            return Err(UnifiedError::not_found("Distribution not found"));
        }
        Ok(serde_json::json!({ "deleted": true, "id": id }))
    })();
    json::respond(result, debug)
}

/// Verify a git provider webhook signature against the stored
/// secret, following the VCS module's verification rules
/// (`src/modules/vcs.rs`, `verify_webhook_signature`): GitHub uses
/// HMAC-SHA256 in `X-Hub-Signature-256` (`sha256=<hex>` over the raw
/// body); GitLab compares the shared secret in `X-Gitlab-Token`;
/// Bitbucket — which provides no signature scheme — compares the
/// shared secret in the custom `X-Webhook-Secret` header; `generic`
/// uses the same shared-secret header. The secret itself never
/// appears in any message.
fn verify_git_webhook_signature(
    provider: &str,
    req: &HttpRequest,
    body: &[u8],
    secret: &str,
) -> UnifiedResult<()> {
    let header_value = |name: &str| {
        req.headers()
            .get(name)
            .and_then(|value| value.to_str().ok())
            .unwrap_or("")
            .to_string()
    };
    match provider {
        "github" => {
            let provided = header_value("x-hub-signature-256");
            let provided = provided.strip_prefix("sha256=").ok_or_else(|| {
                UnifiedError::unauthorized("Webhook signature must use the sha256 scheme")
            })?;
            let mut mac = HmacSha256::new_from_slice(secret.as_bytes())
                .map_err(|_| UnifiedError::internal("Cannot initialise webhook verification"))?;
            mac.update(body);
            let expected: String = mac
                .finalize()
                .into_bytes()
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect();
            if !constant_time_equal(&expected, &provided.to_ascii_lowercase()) {
                return Err(UnifiedError::unauthorized(
                    "Webhook signature verification failed",
                ));
            }
            Ok(())
        }
        "gitlab" => {
            if !constant_time_equal(secret, &header_value("x-gitlab-token")) {
                return Err(UnifiedError::unauthorized(
                    "Webhook signature verification failed",
                ));
            }
            Ok(())
        }
        _ => {
            if !constant_time_equal(secret, &header_value("x-webhook-secret")) {
                return Err(UnifiedError::unauthorized(
                    "Webhook signature verification failed",
                ));
            }
            Ok(())
        }
    }
}

/// Detect the git provider from the webhook event headers (the same
/// headers the VCS module reads).
fn git_provider_from_headers(req: &HttpRequest) -> &'static str {
    if req.headers().contains_key("x-github-event") {
        "github"
    } else if req.headers().contains_key("x-gitlab-event") {
        "gitlab"
    } else if req.headers().contains_key("x-event-key") {
        "bitbucket"
    } else {
        "generic"
    }
}

fn git_event_from_headers(req: &HttpRequest, provider: &str) -> String {
    let header_name = match provider {
        "github" => "x-github-event",
        "gitlab" => "x-gitlab-event",
        "bitbucket" => "x-event-key",
        _ => "x-webhook-event",
    };
    req.headers()
        .get(header_name)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("")
        .to_string()
}

/// Confirm a download is licensed for a git-distributed product:
/// `GET /api/v1/licensing/distribute/git/{product}/access?license_key=...&instance_id=...`.
///
/// API-key authenticated (`licensing:read`). The license is checked
/// WITHOUT consuming a seat (the release download endpoint is the
/// one that activates). On success the response carries a signed,
/// expiring download link for the product's latest release, when a
/// release exists.
async fn git_access_handler(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    product: web::Path<String>,
    query: web::Query<GitAccessQuery>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let authenticated = authenticate_public_key(&req, "read")?;
        let product = product.into_inner().trim().to_lowercase();
        if let Some(settings) = product_settings_for(&product) {
            if let Some(owner_id) = settings.owner_id {
                if authenticated.owner_id != Some(owner_id) {
                    return Err(UnifiedError::forbidden(
                        "API key does not belong to this product owner",
                    ));
                }
            }
            if !settings
                .allowed_distribution_channels
                .iter()
                .any(|channel| channel == DISTRIBUTION_CHANNEL_GIT)
            {
                return Ok(serde_json::json!({
                    "allowed": false,
                    "reason": "channel_not_allowed",
                    "product": product,
                    "download_url": Value::Null,
                }));
            }
        }
        let outcome = validate_and_maybe_activate(
            &ctx,
            &query.license_key,
            Some(query.instance_id.trim()),
            query.hostname.as_deref(),
            false,
        )?;
        let reason = match (&outcome.valid, &outcome.reason) {
            (true, _) => "valid".to_string(),
            (false, Some(reason)) => match reason.as_str() {
                "not_found" => "not_found".to_string(),
                "invalid_checksum" => "invalid".to_string(),
                "seats_exceeded" => "seats-exceeded".to_string(),
                other => other.to_string(),
            },
            (false, None) => "invalid".to_string(),
        };
        let Some(license) = &outcome.license else {
            return Ok(serde_json::json!({
                "allowed": false,
                "reason": reason,
                "product": product,
                "download_url": Value::Null,
            }));
        };
        if license.product != product || !outcome.valid {
            let reason = if license.product != product {
                "product_mismatch".to_string()
            } else {
                reason
            };
            return Ok(serde_json::json!({
                "allowed": false,
                "reason": reason,
                "license_id": license.id,
                "product": license.product,
                "download_url": Value::Null,
            }));
        }
        let mut releases: Vec<LicenseRelease> = license_releases_store()
            .iter()
            .filter(|entry| entry.value().product == product)
            .map(|entry| entry.value().clone())
            .collect();
        releases.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        let (download_url, download_expires_at, release_id, release_version) =
            match releases.first() {
                Some(release) => {
                    let (url, expires_at) =
                        build_signed_download_url(&ctx, release.id, license.id);
                    (
                        serde_json::json!(url),
                        serde_json::json!(expires_at),
                        serde_json::json!(release.id),
                        serde_json::json!(release.version),
                    )
                }
                None => (
                    Value::Null,
                    Value::Null,
                    Value::Null,
                    Value::Null,
                ),
            };
        Ok(serde_json::json!({
            "allowed": true,
            "reason": "valid",
            "license_id": license.id,
            "product": license.product,
            "release_id": release_id,
            "release_version": release_version,
            "download_url": download_url,
            "download_expires_at": download_expires_at,
            "grace": outcome.grace,
            "days_in_grace": outcome.days_in_grace,
        }))
    })();
    json::respond(result, debug)
}

/// Git provider webhook receiver:
/// `POST /api/v1/licensing/distribute/git/{product}/webhook`.
/// Authenticated by the provider signature against the stored
/// distribution secret (no API key: the webhook comes from the
/// provider). Records the release event and emits
/// `license.distribution.release`; it never serves downloads.
async fn git_webhook_handler(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    product: web::Path<String>,
    body: web::Bytes,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let product = product.into_inner().trim().to_lowercase();
        let provider = git_provider_from_headers(&req);
        let distribution = license_distributions_store()
            .iter()
            .find(|entry| {
                entry.value().product == product && entry.value().provider == provider
            })
            .map(|entry| entry.value().clone())
            .ok_or_else(|| {
                UnifiedError::not_found(format!(
                    "No {provider} distribution configured for product \"{product}\""
                ))
            })?;
        let secret = distribution.webhook_secret.clone().ok_or_else(|| {
            UnifiedError::unauthorized(
                "No webhook secret configured for this distribution",
            )
        })?;
        verify_git_webhook_signature(provider, &req, &body, &secret)?;
        let payload: Value = serde_json::from_slice(&body).map_err(|_| {
            UnifiedError::bad_request("Webhook body is not valid JSON")
        })?;
        let event = git_event_from_headers(&req, provider);
        let tag = match provider {
            "github" => payload
                .get("release")
                .and_then(|release| release.get("tag_name"))
                .and_then(|value| value.as_str())
                .map(str::to_string)
                .or_else(|| {
                    payload
                        .get("ref")
                        .and_then(|value| value.as_str())
                        .map(|value| value.trim_start_matches("refs/tags/").to_string())
                }),
            "gitlab" => payload
                .get("ref")
                .and_then(|value| value.as_str())
                .map(|value| value.trim_start_matches("refs/tags/").to_string()),
            _ => payload
                .get("tag")
                .and_then(|value| value.as_str())
                .map(str::to_string)
                .or_else(|| {
                    payload
                        .get("version")
                        .and_then(|value| value.as_str())
                        .map(str::to_string)
                }),
        };
        ctx.hooks.emit(
            LICENSE_DISTRIBUTION_RELEASE,
            serde_json::json!({
                "product": product,
                "provider": provider,
                "repo": distribution.repo,
                "git_ref": distribution.git_ref,
                "event": event,
                "tag": tag,
            }),
        );
        Ok(serde_json::json!({
            "received": true,
            "product": product,
            "provider": provider,
            "event": event,
            "tag": tag,
        }))
    })();
    json::respond(result, debug)
}

// ---------------------------------------------------------------------------
// SaaS handlers: file-upload releases.
// ---------------------------------------------------------------------------

fn sanitize_release_file_name(raw: &str) -> UnifiedResult<String> {
    let name = raw
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(raw)
        .trim()
        .to_string();
    if name.is_empty() || name == "." || name == ".." || name.len() > 255 {
        return Err(UnifiedError::bad_request(
            "Uploaded file name is invalid",
        ));
    }
    if !name
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.')
    {
        return Err(UnifiedError::bad_request(
            "Uploaded file name may only contain letters, digits, '-', '_' or '.'",
        ));
    }
    Ok(name)
}

fn valid_semver(version: &str) -> bool {
    let Ok(regex) = Regex::new(
        r"^\d+\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?(?:\+[0-9A-Za-z.-]+)?$",
    ) else {
        return false;
    };
    regex.is_match(version)
}

/// Build a signed, expiring download link for a release, bound to
/// one license: `/distribute/releases/{id}/file?license_id=...&
/// expires=...&sig=...`. The signature is HMAC-SHA256 over
/// `licensing-download|{release_id}|{license_id}|{expires}`.
fn build_signed_download_url(
    ctx: &AppContext,
    release_id: Uuid,
    license_id: Uuid,
) -> (String, DateTime<Utc>) {
    let expires_at = Utc::now() + chrono::Duration::seconds(SIGNED_DOWNLOAD_TTL_SECS);
    let expires = expires_at.timestamp();
    let message = format!("licensing-download|{release_id}|{license_id}|{expires}");
    let mut mac = HmacSha256::new_from_slice(&hmac_secret(ctx))
        .expect("HMAC accepts any key length");
    mac.update(message.as_bytes());
    let sig: String = mac
        .finalize()
        .into_bytes()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    (
        format!(
            "/api/v1/licensing/distribute/releases/{release_id}/file?license_id={license_id}&expires={expires}&sig={sig}"
        ),
        expires_at,
    )
}

/// Upload a build/package for a licensed product (multipart:
/// `version` text field, `file` binary field, optional
/// `storage_target` text field). The bytes go to the named storage
/// target through the shared `StorageRegistry` when one is
/// registered; when no target is registered the bytes are kept in
/// memory and the response says so (`stored_via: "memory"`). The
/// SHA-256 of the upload is recorded and the release is versioned
/// with a validated semver string.
async fn create_release(
    ctx: web::Data<Arc<AppContext>>,
    product_id: web::Path<Uuid>,
    query: web::Query<ViewerQuery>,
    mut payload: Multipart,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| async {
        require_licensing_capability(&ctx, query.viewer_role.as_deref(), "licensing:manage")?;
        let product_id = product_id.into_inner();
        let product = license_products_store()
            .get(&product_id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("License product not found"))?;
        if !product
            .allowed_distribution_channels
            .iter()
            .any(|channel| channel == DISTRIBUTION_CHANNEL_FILE)
        {
            return Err(UnifiedError::bad_request(format!(
                "License product \"{}\" does not allow the file distribution channel",
                product.product
            )));
        }
        let mut version = String::new();
        let mut file_name: Option<String> = None;
        let mut storage_target: Option<String> = None;
        let mut bytes: Vec<u8> = Vec::new();
        while let Some(mut field) = payload
            .try_next()
            .await
            .map_err(|error| UnifiedError::bad_request(format!("Invalid multipart data: {error}")))?
        {
            let name = field
                .content_disposition()
                .get_name()
                .unwrap_or("")
                .to_string();
            match name.as_str() {
                "version" => {
                    while let Some(chunk) = field
                        .try_next()
                        .await
                        .map_err(|error| {
                            UnifiedError::bad_request(format!("Invalid multipart data: {error}"))
                        })?
                    {
                        version.push_str(&String::from_utf8_lossy(&chunk));
                    }
                }
                "storage_target" => {
                    let mut value = String::new();
                    while let Some(chunk) = field
                        .try_next()
                        .await
                        .map_err(|error| {
                            UnifiedError::bad_request(format!("Invalid multipart data: {error}"))
                        })?
                    {
                        value.push_str(&String::from_utf8_lossy(&chunk));
                    }
                    let value = value.trim().to_string();
                    if !value.is_empty() {
                        storage_target = Some(value);
                    }
                }
                "file" => {
                    let raw_name = field
                        .content_disposition()
                        .get_filename()
                        .unwrap_or("upload")
                        .to_string();
                    file_name = Some(sanitize_release_file_name(&raw_name)?);
                    while let Some(chunk) = field
                        .try_next()
                        .await
                        .map_err(|error| {
                            UnifiedError::bad_request(format!("Invalid multipart data: {error}"))
                        })?
                    {
                        if bytes.len() + chunk.len() > MAX_RELEASE_UPLOAD_BYTES {
                            return Err(UnifiedError::new(
                                "Upload exceeds the 100 MB size limit",
                                413,
                            ));
                        }
                        bytes.extend_from_slice(&chunk);
                    }
                }
                _ => {
                    while let Some(_chunk) = field.try_next().await.map_err(|error| {
                        UnifiedError::bad_request(format!("Invalid multipart data: {error}"))
                    })?
                    {}
                }
            }
        }
        let version = version.trim().to_string();
        if version.is_empty() || !valid_semver(&version) {
            return Err(UnifiedError::bad_request(
                "version is required and must be a semantic version (for example 1.2.3)",
            ));
        }
        let file_name = file_name
            .ok_or_else(|| UnifiedError::bad_request("Multipart field \"file\" is required"))?;
        if bytes.is_empty() {
            return Err(UnifiedError::bad_request(
                "Uploaded file must not be empty",
            ));
        }
        if license_releases_store()
            .iter()
            .any(|entry| {
                entry.value().product_id == product_id && entry.value().version == version
            })
        {
            return Err(UnifiedError::conflict(format!(
                "Release version \"{version}\" already exists for this product"
            )));
        }
        let sha256_hex = sha256_hex(&bytes);
        let size_bytes = bytes.len() as i64;
        let storage_key = format!(
            "licensing-releases/{}/{}/{}",
            product.product, version, file_name
        );
        let target_name = storage_target
            .clone()
            .unwrap_or_else(|| DEFAULT_RELEASE_TARGET.to_string());
        let release_id = Uuid::now_v7();
        let (stored_target, stored_via) = match ctx.storage_registry.get(&target_name) {
            Some(provider) => {
                provider
                    .upload(&storage_key, bytes)
                    .await
                    .map_err(|error| {
                        UnifiedError::internal(format!("Cannot store release: {error}"))
                    })?;
                (Some(target_name), "provider")
            }
            None => {
                release_blobs_store().insert(release_id, bytes);
                (None, "memory")
            }
        };
        let release = LicenseRelease {
            id: release_id,
            product_id,
            product: product.product.clone(),
            version,
            file_name,
            storage_target: stored_target,
            storage_key,
            sha256_hex,
            size_bytes,
            download_count: 0,
            created_at: Utc::now(),
        };
        license_releases_store().insert(release.id, release.clone());
        Ok(serde_json::json!({
            "release": release,
            "stored_via": stored_via,
        }))
    })()
    .await;
    match result {
        Ok(view) => json::success_response(view, 201),
        Err(error) => json::error_response(&error, debug),
    }
}

async fn list_releases(
    ctx: web::Data<Arc<AppContext>>,
    product_id: web::Path<Uuid>,
    params: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        require_licensing_capability(
            &ctx,
            params
                .filters
                .get("viewer_role")
                .map(String::as_str),
            "licensing:view",
        )?;
        let product_id = product_id.into_inner();
        if !license_products_store().contains_key(&product_id) {
            return Err(UnifiedError::not_found("License product not found"));
        }
        let mut items: Vec<LicenseRelease> = license_releases_store()
            .iter()
            .filter(|entry| entry.value().product_id == product_id)
            .map(|entry| entry.value().clone())
            .collect();
        if let Some(search) = &params.search {
            let needle = search.to_lowercase();
            items.retain(|row| {
                row.version.to_lowercase().contains(&needle)
                    || row.file_name.to_lowercase().contains(&needle)
            });
        }
        items.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        if params.normalized().order == "asc" {
            items.reverse();
        }
        Ok(paginate(items, &params).to_json())
    })();
    json::respond(result, debug)
}

/// Issue a signed download link for a release. The caller proves
/// the license (`license_key` + `instance_id`); a successful call
/// activates the instance (consumes a seat for a new instance) and
/// logs the download. The link itself is served by
/// `download_release_file` and expires after one hour.
async fn download_release(
    ctx: web::Data<Arc<AppContext>>,
    release_id: web::Path<Uuid>,
    query: web::Query<DownloadQuery>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let release_id = release_id.into_inner();
        let release = license_releases_store()
            .get(&release_id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Release not found"))?;
        let outcome = validate_and_maybe_activate(
            &ctx,
            &query.license_key,
            Some(query.instance_id.trim()),
            query.hostname.as_deref(),
            true,
        )?;
        let Some(license) = &outcome.license else {
            return Err(UnifiedError::unprocessable(format!(
                "License download failed: {}",
                outcome.reason.clone().unwrap_or_else(|| "invalid".to_string())
            ))
            .with_metadata(
                "reason",
                Value::String(outcome.reason.clone().unwrap_or_default()),
            ));
        };
        if license.product != release.product {
            return Err(UnifiedError::forbidden(
                "License product does not match this release",
            )
            .with_metadata("reason", Value::String("product_mismatch".to_string())));
        }
        if !outcome.valid {
            return Err(UnifiedError::unprocessable(format!(
                "License download failed: {}",
                outcome.reason.clone().unwrap_or_else(|| "invalid".to_string())
            ))
            .with_metadata(
                "reason",
                Value::String(outcome.reason.clone().unwrap_or_default()),
            ));
        }
        if let Some(mut entry) = license_releases_store().get_mut(&release_id) {
            entry.download_count += 1;
        }
        let (download_url, expires_at) =
            build_signed_download_url(&ctx, release_id, license.id);
        append_license_event(
            &ctx,
            license.id,
            "downloaded",
            None,
            serde_json::json!({
                "release_id": release_id,
                "version": release.version,
                "instance_id": query.instance_id,
            }),
        );
        ctx.hooks.emit(
            LICENSE_DOWNLOAD,
            serde_json::json!({
                "license_id": license.id,
                "release_id": release_id,
                "version": release.version,
                "instance_id": query.instance_id,
            }),
        );
        Ok(serde_json::json!({
            "download_url": download_url,
            "expires_at": expires_at,
            "release_id": release_id,
            "version": release.version,
            "file_name": release.file_name,
            "size_bytes": release.size_bytes,
            "sha256_hex": release.sha256_hex,
            "license_id": license.id,
        }))
    })();
    json::respond(result, debug)
}

/// Serve a release through a signed link issued by
/// `download_release` (or `git_access_handler`). The HMAC signature
/// is the credential here: no license key and no API key are needed,
/// and the link expires after one hour. When the release lives in a
/// storage target the caller is redirected to that provider's signed
/// URL; otherwise the bytes kept in memory are served directly.
async fn download_release_file(
    ctx: web::Data<Arc<AppContext>>,
    release_id: web::Path<Uuid>,
    query: web::Query<SignedFileQuery>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let release_id = release_id.into_inner();
    let release = match license_releases_store()
        .get(&release_id)
        .map(|entry| entry.value().clone())
    {
        Some(release) => release,
        None => {
            return json::error_response(&UnifiedError::not_found("Release not found"), debug);
        }
    };
    let Ok(license_id) = query.license_id.parse::<Uuid>() else {
        return json::error_response(
            &UnifiedError::bad_request("license_id must be a UUID"),
            debug,
        );
    };
    if query.expires <= Utc::now().timestamp() {
        return json::error_response(
            &UnifiedError::unauthorized("Signed download link has expired"),
            debug,
        );
    }
    let message = format!("licensing-download|{release_id}|{license_id}|{}", query.expires);
    let mut mac = match HmacSha256::new_from_slice(&hmac_secret(&ctx)) {
        Ok(mac) => mac,
        Err(_) => {
            return json::error_response(
                &UnifiedError::internal("Cannot initialise download verification"),
                debug,
            );
        }
    };
    mac.update(message.as_bytes());
    let expected: String = mac
        .finalize()
        .into_bytes()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    if !constant_time_equal(&expected, &query.sig.to_ascii_lowercase()) {
        return json::error_response(
            &UnifiedError::unauthorized("Signed download link verification failed"),
            debug,
        );
    }
    if let Some(target) = &release.storage_target {
        if let Some(provider) = ctx.storage_registry.get(target) {
            return match provider.signed_url(&release.storage_key, 3600).await {
                Ok(url) => HttpResponse::Found()
                    .insert_header((header::LOCATION, url))
                    .finish(),
                Err(error) => json::error_response(&error, debug),
            };
        }
    }
    match release_blobs_store().get(&release_id) {
        Some(bytes) => {
            let file_name = release.file_name.replace('"', "");
            HttpResponse::Ok()
                .content_type("application/octet-stream")
                .insert_header((
                    header::CONTENT_DISPOSITION,
                    format!("attachment; filename=\"{file_name}\""),
                ))
                .body(bytes.value().clone())
        }
        None => json::error_response(
            &UnifiedError::new(
                "Release bytes are no longer available in memory (the server restarted after an in-memory upload); re-upload the release",
                410,
            ),
            debug,
        ),
    }
}

// ---------------------------------------------------------------------------
// SaaS hook handler: upgrades (store / billing plan changes).
// ---------------------------------------------------------------------------

/// Handle `store.order.upgraded` and `billing.plan.changed` by
/// adjusting the named license(s): extend the expiry by
/// `extend_days` (from the later of now / current expiry), set an
/// absolute `expires_at`, change `max_activations` (`seats` or
/// `new_max_activations`), or move the license to `new_product`.
/// Emits `license.upgraded` and chains an `upgraded` event per
/// license; expiry/grace/reminder alert state is cleared so the
/// scan re-alerts against the new expiry.
fn on_order_upgraded(ctx: &mut HookContext) {
    let Some(app_ctx) = hook_ctx() else {
        ctx.add_result(serde_json::json!({
            "upgraded": [],
            "not_found": [],
            "error": "licensing module is not initialised",
        }));
        return;
    };
    let order_id = ctx
        .get("order_id")
        .and_then(|value| value.as_str())
        .map(str::to_string);
    let mut license_ids: Vec<String> = ctx
        .get("license_ids")
        .and_then(|value| value.as_array())
        .map(|values| {
            values
                .iter()
                .filter_map(|value| value.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    if license_ids.is_empty() {
        if let Some(id) = ctx
            .get("license_id")
            .and_then(|value| value.as_str())
            .map(str::to_string)
        {
            license_ids.push(id);
        }
    }
    let extend_days = ctx
        .get("extend_days")
        .and_then(|value| value.as_i64())
        .or_else(|| {
            ctx.get("extend_validity_days")
                .and_then(|value| value.as_i64())
        });
    let new_max = ctx
        .get("new_max_activations")
        .and_then(|value| value.as_i64())
        .or_else(|| ctx.get("seats").and_then(|value| value.as_i64()));
    let new_product = ctx
        .get("new_product")
        .and_then(|value| value.as_str())
        .map(str::to_string);
    let new_expires_at = ctx
        .get("expires_at")
        .and_then(|value| value.as_str())
        .and_then(|value| value.parse::<DateTime<Utc>>().ok());
    let mut upgraded: Vec<String> = Vec::new();
    let mut not_found: Vec<String> = Vec::new();
    for license_id in license_ids {
        let Ok(id) = license_id.parse::<Uuid>() else {
            not_found.push(license_id);
            continue;
        };
        let Some(mut license) = load_license(&app_ctx, id).ok().flatten() else {
            not_found.push(license_id);
            continue;
        };
        let now = Utc::now();
        if let Some(days) = extend_days {
            if days > 0 {
                let base = license.expires_at.filter(|exp| *exp > now).unwrap_or(now);
                license.expires_at = Some(base + chrono::Duration::days(days));
            }
        }
        if let Some(expires_at) = new_expires_at {
            license.expires_at = Some(expires_at);
        }
        if let Some(max) = new_max {
            if max >= 1 {
                license.max_activations = (max.min(i32::MAX as i64)) as i32;
            }
        }
        if let Some(product) = &new_product {
            if !product.trim().is_empty() {
                license.product = product.trim().to_string();
            }
        }
        // An upgraded license is usable again: a revoked license
        // stays revoked (a refund is final), but an expired license
        // with a new expiry returns to active.
        if license.status == "expired" && license.expires_at.is_some_and(|exp| exp > now) {
            license.status = "active".to_string();
        }
        license.updated_at = now;
        if persist_license(&app_ctx, &license).is_err() {
            not_found.push(license_id);
            continue;
        }
        expiry_alerted_store().remove(&id);
        reminder_alerted_store().remove(&id);
        grace_alerted_store().remove(&id);
        append_license_event(
            &app_ctx,
            id,
            "upgraded",
            None,
            serde_json::json!({
                "order_id": order_id,
                "extend_days": extend_days,
                "expires_at": license.expires_at,
                "max_activations": license.max_activations,
                "product": license.product,
            }),
        );
        app_ctx.hooks.emit(
            LICENSE_UPGRADED,
            serde_json::json!({
                "license_id": id,
                "order_id": order_id,
                "expires_at": license.expires_at,
                "max_activations": license.max_activations,
                "product": license.product,
            }),
        );
        upgraded.push(license_id);
    }
    ctx.add_result(serde_json::json!({
        "upgraded": upgraded,
        "not_found": not_found,
        "order_id": order_id,
    }));
}

// ---------------------------------------------------------------------------
// Module definition.
// ---------------------------------------------------------------------------

pub struct LicensingModule;

impl Module for LicensingModule {
    fn id(&self) -> &'static str {
        "licensing"
    }

    fn name(&self) -> &'static str {
        "Licensing"
    }

    fn description(&self) -> &'static str {
        "License key management: generation, validation, activation seats, suspension, revocation, and expiry alerts. Licensing as a service: per-product key formats and settings, API-key authenticated verification, activation history and seat management, grace periods and renewal reminders, upgrades, offline tokens, git/API/file distribution channels, and a hash-chained license event log."
    }

    fn init(&self, ctx: &AppContext) -> UnifiedResult<()> {
        let _ = LICENSING_CTX.set(stash_ctx(ctx));
        Ok(())
    }

    fn register_hooks(&self, bus: &HookBus) {
        bus.register(
            "license.validate_requested",
            "licensing",
            Arc::new(on_license_validate_requested),
        );
        bus.register(
            LICENSE_EXPIRY_CHECK_REQUESTED,
            "licensing",
            Arc::new(on_license_expiry_check_requested),
        );
        bus.register("order.created", "licensing", Arc::new(on_order_created));
        bus.register(
            "store.order.refunded",
            "licensing",
            Arc::new(on_order_refunded),
        );
        bus.register(
            STORE_ORDER_UPGRADED,
            "licensing",
            Arc::new(on_order_upgraded),
        );
        bus.register(
            BILLING_PLAN_CHANGED,
            "licensing",
            Arc::new(on_order_upgraded),
        );
    }

    fn route_info(&self) -> Vec<RouteInfo> {
        vec![
            RouteInfo { method: "GET", path: "/api/v1/licensing/licenses", owner: "licensing", description: "List licenses" },
            RouteInfo { method: "POST", path: "/api/v1/licensing/licenses", owner: "licensing", description: "Create license" },
            RouteInfo { method: "POST", path: "/api/v1/licensing/validate", owner: "licensing", description: "Validate handler" },
            RouteInfo { method: "POST", path: "/api/v1/licensing/activate", owner: "licensing", description: "Activate handler" },
            RouteInfo { method: "POST", path: "/api/v1/licensing/deactivate", owner: "licensing", description: "Deactivate handler" },
            RouteInfo { method: "GET", path: "/api/v1/licensing/licenses/{id}", owner: "licensing", description: "Get license" },
            RouteInfo { method: "PUT", path: "/api/v1/licensing/licenses/{id}", owner: "licensing", description: "Update license" },
            RouteInfo { method: "DELETE", path: "/api/v1/licensing/licenses/{id}", owner: "licensing", description: "Delete license" },
            RouteInfo { method: "POST", path: "/api/v1/licensing/licenses/{id}/suspend", owner: "licensing", description: "Suspend handler" },
            RouteInfo { method: "POST", path: "/api/v1/licensing/licenses/expiry-check", owner: "licensing", description: "Run the license expiry scan now" },
            RouteInfo { method: "POST", path: "/api/v1/licensing/licenses/{id}/revoke", owner: "licensing", description: "Revoke handler" },
            RouteInfo { method: "GET", path: "/api/v1/licensing/licenses/{id}/activations", owner: "licensing", description: "List activations" },
            // Licensing as a service (batch 7).
            RouteInfo { method: "GET", path: "/api/v1/licensing/license-products", owner: "licensing", description: "List license products (admin, licensing:view)" },
            RouteInfo { method: "POST", path: "/api/v1/licensing/license-products", owner: "licensing", description: "Create license product (admin, licensing:manage)" },
            RouteInfo { method: "GET", path: "/api/v1/licensing/license-products/{id}", owner: "licensing", description: "Get license product (admin, licensing:view)" },
            RouteInfo { method: "PUT", path: "/api/v1/licensing/license-products/{id}", owner: "licensing", description: "Update license product (admin, licensing:manage)" },
            RouteInfo { method: "DELETE", path: "/api/v1/licensing/license-products/{id}", owner: "licensing", description: "Delete license product (admin, licensing:manage)" },
            RouteInfo { method: "POST", path: "/api/v1/licensing/public/verify", owner: "licensing", description: "Public license verification (API key, licensing:read)" },
            RouteInfo { method: "GET", path: "/api/v1/licensing/licenses/{id}/activation-history", owner: "licensing", description: "List activation history (admin, licensing:view)" },
            RouteInfo { method: "POST", path: "/api/v1/licensing/licenses/{id}/activations/{activation_id}/deactivate", owner: "licensing", description: "Remotely deactivate one activation (admin, licensing:manage)" },
            RouteInfo { method: "POST", path: "/api/v1/licensing/licenses/{id}/seats", owner: "licensing", description: "Change license seat count (admin, licensing:manage)" },
            RouteInfo { method: "GET", path: "/api/v1/licensing/licenses/{id}/events", owner: "licensing", description: "List hash-chained license events (admin, licensing:view)" },
            RouteInfo { method: "GET", path: "/api/v1/licensing/licenses/{id}/events/verify", owner: "licensing", description: "Verify the hash-chained license event log (admin, licensing:view)" },
            RouteInfo { method: "POST", path: "/api/v1/licensing/licenses/{id}/offline-token", owner: "licensing", description: "Issue a signed offline token (admin, licensing:manage)" },
            RouteInfo { method: "POST", path: "/api/v1/licensing/offline-verify", owner: "licensing", description: "Verify a signed offline token without a database lookup (token is the credential)" },
            RouteInfo { method: "GET", path: "/api/v1/licensing/licenses/{id}/offline-tokens", owner: "licensing", description: "List offline tokens (admin, licensing:view)" },
            RouteInfo { method: "POST", path: "/api/v1/licensing/licenses/{id}/offline-tokens/{token_id}/revoke", owner: "licensing", description: "Revoke an offline token server-side (admin, licensing:manage)" },
            RouteInfo { method: "GET", path: "/api/v1/licensing/distributions", owner: "licensing", description: "List git distributions (admin, licensing:view)" },
            RouteInfo { method: "POST", path: "/api/v1/licensing/distributions", owner: "licensing", description: "Create git distribution (admin, licensing:manage)" },
            RouteInfo { method: "GET", path: "/api/v1/licensing/distributions/{id}", owner: "licensing", description: "Get git distribution (admin, licensing:view)" },
            RouteInfo { method: "PUT", path: "/api/v1/licensing/distributions/{id}", owner: "licensing", description: "Update git distribution (admin, licensing:manage)" },
            RouteInfo { method: "DELETE", path: "/api/v1/licensing/distributions/{id}", owner: "licensing", description: "Delete git distribution (admin, licensing:manage)" },
            RouteInfo { method: "GET", path: "/api/v1/licensing/distribute/git/{product}/access", owner: "licensing", description: "Confirm a download is licensed for a git-distributed product (API key, licensing:read)" },
            RouteInfo { method: "POST", path: "/api/v1/licensing/distribute/git/{product}/webhook", owner: "licensing", description: "Git provider webhook receiver (provider HMAC signature)" },
            RouteInfo { method: "POST", path: "/api/v1/licensing/products/{product_id}/releases", owner: "licensing", description: "Upload a release for a licensed product (admin, licensing:manage)" },
            RouteInfo { method: "GET", path: "/api/v1/licensing/products/{product_id}/releases", owner: "licensing", description: "List releases for a licensed product (admin, licensing:view)" },
            RouteInfo { method: "GET", path: "/api/v1/licensing/distribute/releases/{release_id}/download", owner: "licensing", description: "Issue a signed download link for a release (valid license required)" },
            RouteInfo { method: "GET", path: "/api/v1/licensing/distribute/releases/{release_id}/file", owner: "licensing", description: "Serve a release through a signed download link (HMAC token)" },
        ]
    }

    fn configure_routes(&self, cfg: &mut actix_web::web::ServiceConfig) {
        cfg.service(
            web::scope("/api/v1/licensing")
                .route("/licenses", web::get().to(list_licenses))
                .route("/licenses", web::post().to(create_license))
                .route("/validate", web::post().to(validate_handler))
                .route("/activate", web::post().to(activate_handler))
                .route("/deactivate", web::post().to(deactivate_handler))
                .route("/licenses/{id}", web::get().to(get_license))
                .route("/licenses/{id}", web::put().to(update_license))
                .route("/licenses/{id}", web::delete().to(delete_license))
                .route("/licenses/{id}/suspend", web::post().to(suspend_handler))
                .route("/licenses/{id}/revoke", web::post().to(revoke_handler))
                .route("/licenses/expiry-check", web::post().to(expiry_check_handler))
                .route(
                    "/licenses/{id}/activations",
                    web::get().to(list_activations),
                )
                .route("/license-products", web::get().to(list_license_products))
                .route(
                    "/license-products",
                    web::post().to(create_license_product),
                )
                .route(
                    "/license-products/{id}",
                    web::get().to(get_license_product),
                )
                .route(
                    "/license-products/{id}",
                    web::put().to(update_license_product),
                )
                .route(
                    "/license-products/{id}",
                    web::delete().to(delete_license_product),
                )
                .route("/public/verify", web::post().to(public_verify_handler))
                .route(
                    "/licenses/{id}/activation-history",
                    web::get().to(list_activation_history),
                )
                .route(
                    "/licenses/{id}/activations/{activation_id}/deactivate",
                    web::post().to(deactivate_activation),
                )
                .route("/licenses/{id}/seats", web::post().to(update_seats))
                .route(
                    "/licenses/{id}/events",
                    web::get().to(list_license_events),
                )
                .route(
                    "/licenses/{id}/events/verify",
                    web::get().to(verify_license_events),
                )
                .route(
                    "/licenses/{id}/offline-token",
                    web::post().to(issue_offline_token),
                )
                .route("/offline-verify", web::post().to(offline_verify_handler))
                .route(
                    "/licenses/{id}/offline-tokens",
                    web::get().to(list_offline_tokens),
                )
                .route(
                    "/licenses/{id}/offline-tokens/{token_id}/revoke",
                    web::post().to(revoke_offline_token),
                )
                .route("/distributions", web::get().to(list_distributions))
                .route("/distributions", web::post().to(create_distribution))
                .route(
                    "/distributions/{id}",
                    web::get().to(get_distribution),
                )
                .route(
                    "/distributions/{id}",
                    web::put().to(update_distribution),
                )
                .route(
                    "/distributions/{id}",
                    web::delete().to(delete_distribution),
                )
                .route(
                    "/distribute/git/{product}/access",
                    web::get().to(git_access_handler),
                )
                .route(
                    "/distribute/git/{product}/webhook",
                    web::post().to(git_webhook_handler),
                )
                .route(
                    "/products/{product_id}/releases",
                    web::post().to(create_release),
                )
                .route(
                    "/products/{product_id}/releases",
                    web::get().to(list_releases),
                )
                .route(
                    "/distribute/releases/{release_id}/download",
                    web::get().to(download_release),
                )
                .route(
                    "/distribute/releases/{release_id}/file",
                    web::get().to(download_release_file),
                ),
        );
    }
}

pub fn register(registry: &ModuleRegistry) {
    registry.register(Arc::new(LicensingModule));
}
