//! Licensing - In-memory stores, database loading helpers, and the hook context stash.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::{events, HookBus, HookContext};
use unified_core::json;
use unified_core::modules::{
    Module, ModuleRegistry, MODULE_ENTITLEMENT_DENIED_EVENT, MODULE_ENTITLEMENT_GRANTED_EVENT,
    RouteInfo,
};
use unified_core::pagination::{PageResult, PaginationParams};
use unified_core::db::models::{License, LicenseActivation, NewLicense, NewLicenseActivation};
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
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};
use uuid::Uuid;
use super::{keys::*, expiry::*, hooks::*, types::*, validation::*, handlers::*, products::*, helpers::*, product_handlers::*, verify::*, offline::*, distribution::*, releases::*, module::*};

// ---------------------------------------------------------------------------
// In-memory stores (used when no database pool is attached).
// ---------------------------------------------------------------------------

pub(crate) static LICENSES: OnceLock<DashMap<Uuid, License>> = OnceLock::new();

pub(crate) static ACTIVATIONS: OnceLock<DashMap<Uuid, LicenseActivation>> = OnceLock::new();

pub(crate) fn licenses_store() -> &'static DashMap<Uuid, License> {
    LICENSES.get_or_init(DashMap::new)
}

pub(crate) fn activations_store() -> &'static DashMap<Uuid, LicenseActivation> {
    ACTIVATIONS.get_or_init(DashMap::new)
}

pub(crate) fn db_pool(ctx: &AppContext) -> Option<unified_core::db::DbPool> {
    ctx.db.read().expect("db lock poisoned").clone()
}

pub(crate) fn db_load_licenses(pool: &unified_core::db::DbPool) -> UnifiedResult<Vec<License>> {
    use unified_core::db::schema::licenses::dsl as lic_dsl;
    let mut conn = pool.get().map_err(|error| {
        UnifiedError::internal(format!("Cannot get database connection: {error}"))
    })?;
    lic_dsl::licenses
        .select(License::as_select())
        .load(&mut conn)
        .map_err(|error| UnifiedError::internal(format!("Cannot load licenses: {error}")))
}

pub(crate) fn db_load_activations(
    pool: &unified_core::db::DbPool,
    license_id: Uuid,
) -> UnifiedResult<Vec<LicenseActivation>> {
    use unified_core::db::schema::license_activations::dsl as act_dsl;
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

pub(crate) fn activations_for(license_id: Uuid) -> Vec<LicenseActivation> {
    activations_store()
        .iter()
        .filter(|entry| entry.value().license_id == license_id)
        .map(|entry| entry.value().clone())
        .collect()
}

pub(crate) fn find_license_by_key(key: &str) -> Option<License> {
    licenses_store()
        .iter()
        .find(|entry| entry.value().license_key == key)
        .map(|entry| entry.value().clone())
}

pub(crate) fn db_find_license_by_key(pool: &unified_core::db::DbPool, key: &str) -> UnifiedResult<Option<License>> {
    use unified_core::db::schema::licenses::dsl as lic_dsl;
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
pub(crate) fn db_find_license_by_id(pool: &unified_core::db::DbPool, id: Uuid) -> UnifiedResult<Option<License>> {
    use unified_core::db::schema::licenses::dsl as lic_dsl;
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

pub(crate) static LICENSING_CTX: OnceLock<Arc<AppContext>> = OnceLock::new();

pub(crate) fn stash_ctx(ctx: &AppContext) -> Arc<AppContext> {
    Arc::new(AppContext {
        config: ctx.config.clone(),
        hooks: ctx.hooks.clone(),
        modules: ctx.modules.clone(),
        cache: ctx.cache.clone(),
        geo: ctx.geo.clone(),
        db: ctx.db.clone(),
        routes: ctx.routes.clone(),
        storage_registry: ctx.storage_registry.clone(),
        tls: ctx.tls.clone(),
        start_time: std::time::Instant::now(),
        paused: std::sync::atomic::AtomicBool::new(ctx.is_paused()),
        maintenance: std::sync::atomic::AtomicBool::new(ctx.is_maintenance()),
    })
}

pub(crate) fn hook_ctx() -> Option<Arc<AppContext>> {
    LICENSING_CTX.get().cloned()
}

pub(crate) static LICENSE_PRODUCTS: OnceLock<DashMap<Uuid, LicenseProduct>> = OnceLock::new();

pub(crate) static LICENSE_DISTRIBUTIONS: OnceLock<DashMap<Uuid, LicenseDistribution>> = OnceLock::new();

pub(crate) static LICENSE_RELEASES: OnceLock<DashMap<Uuid, LicenseRelease>> = OnceLock::new();

pub(crate) static LICENSE_EVENTS: OnceLock<DashMap<Uuid, LicenseEvent>> = OnceLock::new();

pub(crate) static LICENSE_EVENT_LAST_HASH: OnceLock<DashMap<Uuid, String>> = OnceLock::new();

pub(crate) static LICENSE_OFFLINE_TOKENS: OnceLock<DashMap<Uuid, LicenseOfflineToken>> = OnceLock::new();

pub(crate) static LICENSE_ACTIVATION_HISTORY: OnceLock<DashMap<Uuid, LicenseActivationHistory>> =
    OnceLock::new();

/// Release bytes kept in memory only when no storage target is
/// registered for the release (documented fallback).
pub(crate) static RELEASE_BLOBS: OnceLock<DashMap<Uuid, Vec<u8>>> = OnceLock::new();

/// Per-API-key verification rate-limit buckets: key id ->
/// (window start, calls in this window).
pub(crate) static VERIFY_BUCKETS: OnceLock<DashMap<String, (Instant, u32)>> = OnceLock::new();

/// Failed public verification attempts, keyed by normalized license
/// key (in-memory counters only, never returned).
pub(crate) static FAILED_VERIFICATIONS: OnceLock<DashMap<String, u64>> = OnceLock::new();

pub(crate) fn license_products_store() -> &'static DashMap<Uuid, LicenseProduct> {
    LICENSE_PRODUCTS.get_or_init(DashMap::new)
}

pub(crate) fn license_distributions_store() -> &'static DashMap<Uuid, LicenseDistribution> {
    LICENSE_DISTRIBUTIONS.get_or_init(DashMap::new)
}

pub(crate) fn license_releases_store() -> &'static DashMap<Uuid, LicenseRelease> {
    LICENSE_RELEASES.get_or_init(DashMap::new)
}

pub(crate) fn license_events_store() -> &'static DashMap<Uuid, LicenseEvent> {
    LICENSE_EVENTS.get_or_init(DashMap::new)
}

pub(crate) fn license_event_last_hash_store() -> &'static DashMap<Uuid, String> {
    LICENSE_EVENT_LAST_HASH.get_or_init(DashMap::new)
}

pub(crate) fn license_offline_tokens_store() -> &'static DashMap<Uuid, LicenseOfflineToken> {
    LICENSE_OFFLINE_TOKENS.get_or_init(DashMap::new)
}

pub(crate) fn license_activation_history_store() -> &'static DashMap<Uuid, LicenseActivationHistory> {
    LICENSE_ACTIVATION_HISTORY.get_or_init(DashMap::new)
}

pub(crate) fn release_blobs_store() -> &'static DashMap<Uuid, Vec<u8>> {
    RELEASE_BLOBS.get_or_init(DashMap::new)
}

pub(crate) fn verify_buckets_store() -> &'static DashMap<String, (Instant, u32)> {
    VERIFY_BUCKETS.get_or_init(DashMap::new)
}

pub(crate) fn failed_verifications_store() -> &'static DashMap<String, u64> {
    FAILED_VERIFICATIONS.get_or_init(DashMap::new)
}

// ---------------------------------------------------------------------------
// Per-license mutexes.
//
// Two independent lock maps (they must stay separate: the activation
// path calls the event append, so sharing one mutex would deadlock).
// ---------------------------------------------------------------------------

/// Per-license mutexes serialising the seat check-and-insert in
/// `validate_and_maybe_activate`: without one, two concurrent
/// activations can both pass the `activations.len() >=
/// max_activations` check and both insert, overshooting the seat
/// limit. This only serialises threads inside this process; a
/// database unique constraint on `(license_id, instance_id)` plus a
/// transactional seat check is the durable cross-process fix, but
/// that needs a migration.
pub(crate) static ACTIVATION_LOCKS: OnceLock<DashMap<Uuid, Arc<Mutex<()>>>> = OnceLock::new();

pub(crate) fn activation_lock_for(license_id: Uuid) -> Arc<Mutex<()>> {
    ACTIVATION_LOCKS
        .get_or_init(DashMap::new)
        .entry(license_id)
        .or_insert_with(|| Arc::new(Mutex::new(())))
        .clone()
}

/// Per-license mutexes serialising hash-chained event appends in
/// `append_license_event`: without one, two concurrent appends read
/// the same `prev_hash`, the chain forks, and `verify_event_chain`
/// fails. One mutex per license keeps unrelated licenses appending
/// in parallel.
pub(crate) static EVENT_LOCKS: OnceLock<DashMap<Uuid, Arc<Mutex<()>>>> = OnceLock::new();

pub(crate) fn event_lock_for(license_id: Uuid) -> Arc<Mutex<()>> {
    EVENT_LOCKS
        .get_or_init(DashMap::new)
        .entry(license_id)
        .or_insert_with(|| Arc::new(Mutex::new(())))
        .clone()
}
