// Cache layer.
//
// A small, adapter-based cache shared by every module through `AppContext`.
// Callers work with typed `CacheValue`s and TTLs through the `Cache`
// facade; the storage backend is chosen by configuration — the filesystem
// adapter by default, or Redis when `redis.enabled` is true. When Redis
// is enabled and `cache.failover_enabled` is true (the default), the
// Redis adapter is wrapped in `FailoverCache`: a Redis failure at
// runtime falls back to the filesystem adapter (logged, and reported
// by the cache status endpoint) instead of failing the request.
//
// Both adapters store the same JSON envelope, so an entry written by one
// backend can be read by the other:
//
//   {"key": "...", "expires_at": <unix seconds | null>, "value": {...}}
//
// The envelope carries the key so a hash collision or a stale file is
// detected instead of silently returning another entry's value.
//
// Key rules: a key is an opaque string of at most 512 characters, without
// control characters or surrounding whitespace. The filesystem adapter
// never uses the raw key as a path component — entries live under
// `sha256(key)`, sharded by the first two hex digits — so keys are safe
// against path traversal by construction. Redis keys are namespaced with
// a `unified:cache:` prefix, and `clear` only removes keys under that
// prefix (never `FLUSHDB`).
//
// Tenant isolation: cache keys are global by default, so a module that
// caches per-tenant data MUST use the tenant-scoped methods
// (`get_tenant` / `set_tenant` / `get_or_compute_tenant` /
// `invalidate_tenant`), which prefix the key with `tenant:{id}:`.
// Plain `get` / `set` reject keys with that prefix, so a tenant entry
// can never be read or overwritten through the unscoped API. The
// middleware `TenantContext` is the source of the tenant id on HTTP
// requests; the facade itself takes the id explicitly so background
// jobs can scope entries the same way.
//
// Stampede protection: `get_or_compute` serializes concurrent fills
// for the same key behind a per-key lock, so a hot miss is computed
// once instead of once per concurrent caller.
//
// Invalidation: entries are invalidated explicitly (`delete`,
// `delete_by_prefix`, `invalidate_tenant`). No module currently caches
// application data through this facade (verified by audit 2026-11-22:
// the only callers are the cache module itself), so there is no hidden
// stale-data path today; any module that starts caching MUST delete or
// invalidate its keys on every write path that mutates the cached
// data, or document the exact staleness window it accepts.
//
// Bounds: the filesystem adapter evicts its oldest entries once it
// holds more than `cache.max_entries` (0 means unlimited), checked
// every 128th write so the bound holds without a scan on every write.
//
// Public API note: the typed convenience methods (`Cache::get_text`,
// `Cache::get_json`, `Cache::set_json`, `Cache::get_value_tenant`)
// and the value predicates (`CacheValue::is_text`,
// `CacheValue::is_bytes`, `CacheValue::as_number`) have no callers
// inside this tree today — they are public API of the `unified-core`
// library for external modules, which reach the cache through the
// same facade. Keep them: deleting them would remove the ergonomic
// typed access external modules are expected to use. The sibling
// predicates (`is_number`, `is_boolean`, `is_object`) are already
// used by external crates.

use crate::config::{AppConfig, RedisSection};
use crate::error::{UnifiedError, UnifiedResult};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use sha2::Digest;
use std::collections::HashMap;
use std::future::Future;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

/// Maximum cache key length in characters. Tenant-scoped keys carry a
/// `tenant:{id}:` prefix on top of the caller's key, so the limit
/// leaves headroom for the prefix.
pub const MAX_KEY_LEN: usize = 512;
/// Prefix applied to every key stored in Redis.
pub const REDIS_KEY_PREFIX: &str = "unified:cache:";
/// Prefix marking a tenant-scoped cache key (see `tenant_key`).
pub const TENANT_KEY_PREFIX: &str = "tenant:";
/// Maximum tenant id length in characters.
pub const MAX_TENANT_ID_LEN: usize = 128;
/// Default maximum entries for the filesystem adapter (see
/// `CacheSection::max_entries`; 0 means unlimited).
pub const DEFAULT_MAX_FS_ENTRIES: u64 = 10_000;
/// The filesystem adapter checks its entry bound every Nth write.
const PRUNE_EVERY_N_SETS: u64 = 128;

/// Validate a cache key. Keys are opaque strings: non-empty, at most
/// `MAX_KEY_LEN` characters, with no control characters and no
/// surrounding whitespace.
pub fn validate_key(key: &str) -> UnifiedResult<()> {
    if key.is_empty() {
        return Err(UnifiedError::bad_request("Cache key must not be empty"));
    }
    if key.chars().count() > MAX_KEY_LEN {
        return Err(UnifiedError::bad_request(format!(
            "Cache key must be at most {MAX_KEY_LEN} characters"
        )));
    }
    if key.trim() != key || key.chars().any(|c| c.is_control()) {
        return Err(UnifiedError::bad_request(
            "Cache key must not contain control characters or surrounding whitespace",
        ));
    }
    Ok(())
}

/// Validate a tenant id: 1..=128 characters of letters, digits, `-`
/// and `_` only (the same shape as the middleware tenant ids).
pub fn validate_tenant_id(tenant_id: &str) -> UnifiedResult<()> {
    if tenant_id.is_empty() || tenant_id.len() > MAX_TENANT_ID_LEN {
        return Err(UnifiedError::bad_request(format!(
            "Tenant id must be 1..={MAX_TENANT_ID_LEN} characters"
        )));
    }
    if !tenant_id
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return Err(UnifiedError::bad_request(
            "Tenant id must contain only letters, digits, '-' or '_'",
        ));
    }
    Ok(())
}

/// The storage key for one tenant's entry: `tenant:{id}:{key}`.
/// Tenant entries are only reachable through the tenant-scoped
/// facade methods, so a tenant can never read another tenant's
/// entry — even with the same unscoped key.
pub fn tenant_key(tenant_id: &str, key: &str) -> UnifiedResult<String> {
    validate_tenant_id(tenant_id)?;
    validate_key(key)?;
    Ok(format!("{TENANT_KEY_PREFIX}{tenant_id}:{key}"))
}

/// Reject a tenant-scoped key on the unscoped API: tenant entries
/// are only reachable through the tenant-scoped methods.
fn reject_tenant_key(key: &str) -> UnifiedResult<()> {
    if key.starts_with(TENANT_KEY_PREFIX) {
        return Err(UnifiedError::bad_request(
            "Tenant-scoped cache keys must be accessed through the tenant-scoped methods",
        ));
    }
    Ok(())
}

/// A value stored in the cache.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum CacheValue {
    Text(String),
    Number(i64),
    Boolean(bool),
    Bytes(Vec<u8>),
    Object(serde_json::Value),
}

impl CacheValue {
    pub fn is_text(&self) -> bool {
        matches!(self, Self::Text(_))
    }

    pub fn is_number(&self) -> bool {
        matches!(self, Self::Number(_))
    }

    pub fn is_boolean(&self) -> bool {
        matches!(self, Self::Boolean(_))
    }

    pub fn is_bytes(&self) -> bool {
        matches!(self, Self::Bytes(_))
    }

    pub fn is_object(&self) -> bool {
        matches!(self, Self::Object(_))
    }

    /// Read the value as text. Numbers and booleans render as their
    /// plain text form; anything else is an error.
    pub fn as_text(&self) -> UnifiedResult<String> {
        match self {
            Self::Text(text) => Ok(text.clone()),
            Self::Number(number) => Ok(number.to_string()),
            Self::Boolean(flag) => Ok(flag.to_string()),
            Self::Bytes(bytes) => String::from_utf8(bytes.clone()).map_err(|_| {
                UnifiedError::bad_request("Cached bytes are not valid UTF-8 text")
            }),
            Self::Object(_) => Err(UnifiedError::bad_request(
                "Cached value is an object, not text",
            )),
        }
    }

    /// Read the value as a number. Booleans map to 0 / 1; text is parsed
    /// only when it is a plain integer.
    pub fn as_number(&self) -> UnifiedResult<i64> {
        match self {
            Self::Number(number) => Ok(*number),
            Self::Boolean(flag) => Ok(i64::from(*flag)),
            Self::Text(text) => text.trim().parse::<i64>().map_err(|_| {
                UnifiedError::bad_request("Cached text is not a number")
            }),
            Self::Bytes(_) | Self::Object(_) => Err(UnifiedError::bad_request(
                "Cached value is not a number",
            )),
        }
    }

    /// Read the value as a boolean. Numbers map to `false` for 0 and
    /// `true` otherwise; text parses as `true` / `false`.
    pub fn as_bool(&self) -> UnifiedResult<bool> {
        match self {
            Self::Boolean(flag) => Ok(*flag),
            Self::Number(number) => Ok(*number != 0),
            Self::Text(text) => text.trim().parse::<bool>().map_err(|_| {
                UnifiedError::bad_request("Cached text is not a boolean")
            }),
            Self::Bytes(_) | Self::Object(_) => Err(UnifiedError::bad_request(
                "Cached value is not a boolean",
            )),
        }
    }

    /// Read the value as raw bytes. Text is returned as UTF-8 bytes.
    pub fn as_bytes(&self) -> UnifiedResult<Vec<u8>> {
        match self {
            Self::Bytes(bytes) => Ok(bytes.clone()),
            Self::Text(text) => Ok(text.as_bytes().to_vec()),
            Self::Number(_) | Self::Boolean(_) | Self::Object(_) => Err(
                UnifiedError::bad_request("Cached value is not bytes"),
            ),
        }
    }

    /// Read the value as a deserializable JSON type. Objects deserialize
    /// directly; any other variant is first converted to its JSON form.
    pub fn as_json<T>(&self) -> UnifiedResult<T>
    where
        T: for<'de> Deserialize<'de>,
    {
        let value = match self {
            Self::Object(value) => value.clone(),
            Self::Text(text) => serde_json::Value::String(text.clone()),
            Self::Number(number) => serde_json::Value::from(*number),
            Self::Boolean(flag) => serde_json::Value::from(*flag),
            Self::Bytes(bytes) => serde_json::Value::String(
                String::from_utf8_lossy(bytes).into_owned(),
            ),
        };
        serde_json::from_value(value).map_err(|error| {
            UnifiedError::bad_request(format!("Cached value is not the expected type: {error}"))
        })
    }

    /// Build a value from any serializable type. JSON primitives keep
    /// their typed variant; arrays and objects are stored as `Object`.
    pub fn from_json<T: Serialize>(value: &T) -> UnifiedResult<Self> {
        let json = serde_json::to_value(value).map_err(|error| {
            UnifiedError::bad_request(format!("Value cannot be serialized to JSON: {error}"))
        })?;
        Ok(Self::from(json))
    }
}

impl From<String> for CacheValue {
    fn from(value: String) -> Self {
        Self::Text(value)
    }
}

impl From<&str> for CacheValue {
    fn from(value: &str) -> Self {
        Self::Text(value.to_string())
    }
}

impl From<i64> for CacheValue {
    fn from(value: i64) -> Self {
        Self::Number(value)
    }
}

impl From<bool> for CacheValue {
    fn from(value: bool) -> Self {
        Self::Boolean(value)
    }
}

impl From<Vec<u8>> for CacheValue {
    fn from(value: Vec<u8>) -> Self {
        Self::Bytes(value)
    }
}

impl From<serde_json::Value> for CacheValue {
    fn from(value: serde_json::Value) -> Self {
        match value {
            serde_json::Value::String(text) => Self::Text(text),
            serde_json::Value::Number(number) => number
                .as_i64()
                .map(Self::Number)
                .unwrap_or(Self::Object(serde_json::Value::Number(number))),
            serde_json::Value::Bool(flag) => Self::Boolean(flag),
            other => Self::Object(other),
        }
    }
}

/// One cache entry: a value plus its optional absolute expiry time.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CacheEntry {
    pub value: CacheValue,
    pub expires_at: Option<DateTime<Utc>>,
}

impl CacheEntry {
    pub fn new(value: CacheValue, expires_at: Option<DateTime<Utc>>) -> Self {
        Self { value, expires_at }
    }

    pub fn is_expired(&self) -> bool {
        self.expires_at
            .map(|expiry| Utc::now() > expiry)
            .unwrap_or(false)
    }

    /// Seconds until expiry, or `None` when the entry never expires.
    /// Returns 0 for an already-expired entry.
    pub fn ttl_remaining(&self) -> Option<u64> {
        self.expires_at.map(|expiry| {
            expiry
                .signed_duration_since(Utc::now())
                .num_seconds()
                .max(0) as u64
        })
    }
}

/// The on-disk / on-wire envelope shared by both adapters.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct CacheEnvelope {
    key: String,
    expires_at: Option<i64>,
    value: CacheValue,
}

impl CacheEnvelope {
    fn from_entry(key: &str, entry: &CacheEntry) -> Self {
        Self {
            key: key.to_string(),
            expires_at: entry.expires_at.map(|time| time.timestamp()),
            value: entry.value.clone(),
        }
    }

    fn into_entry(self, key: &str) -> UnifiedResult<CacheEntry> {
        if self.key != key {
            return Err(UnifiedError::internal(
                "Cache envelope key does not match the requested key",
            ));
        }
        Ok(CacheEntry {
            value: self.value,
            expires_at: self
                .expires_at
                .and_then(|timestamp| DateTime::from_timestamp(timestamp, 0)),
        })
    }
}

fn encode_envelope(key: &str, entry: &CacheEntry) -> UnifiedResult<String> {
    serde_json::to_string(&CacheEnvelope::from_entry(key, entry)).map_err(|error| {
        UnifiedError::internal(format!("Cannot serialize cache entry: {error}"))
    })
}

fn decode_envelope(raw: &str, key: &str) -> UnifiedResult<CacheEntry> {
    let envelope: CacheEnvelope = serde_json::from_str(raw).map_err(|error| {
        UnifiedError::internal(format!("Cannot parse cache entry: {error}"))
    })?;
    envelope.into_entry(key)
}

// ---------------------------------------------------------------------------
// Adapter trait.
// ---------------------------------------------------------------------------

/// A cache storage backend. Adapters are deliberately small: `get`, `set`,
/// `delete`, `clear`, and `has`. Expiry is enforced by the adapter on
/// read (an expired entry behaves as a miss), and the facade applies the
/// configured default TTL when the caller passes none.
#[async_trait]
pub trait CacheAdapter: Send + Sync + std::fmt::Debug {
    /// Short backend name, for example `"filesystem"` or `"redis"`.
    fn kind(&self) -> &'static str;

    /// Read an entry. Returns `Ok(None)` on a miss or an expired entry.
    async fn get(&self, key: &str) -> UnifiedResult<Option<CacheEntry>>;

    /// Store an entry. A `ttl` of `None` means no expiry; a zero TTL is
    /// also treated as no expiry.
    async fn set(&self, key: &str, value: CacheValue, ttl: Option<Duration>)
        -> UnifiedResult<()>;

    /// Delete an entry. Returns `true` when an entry was removed.
    async fn delete(&self, key: &str) -> UnifiedResult<bool>;

    /// Remove every entry owned by this cache.
    async fn clear(&self) -> UnifiedResult<()>;

    /// True when a live (non-expired) entry exists for `key`.
    async fn has(&self, key: &str) -> UnifiedResult<bool> {
        Ok(self.get(key).await?.is_some())
    }

    /// Delete every entry whose key starts with `prefix`. Returns
    /// the number of entries removed. The default implementation
    /// reports that prefix deletion is not supported; adapters
    /// override it. This is the invalidation primitive modules use
    /// when one write mutates a whole family of cached entries.
    async fn delete_by_prefix(&self, prefix: &str) -> UnifiedResult<u64> {
        let _ = prefix;
        Err(UnifiedError::bad_request(
            "This cache adapter does not support prefix invalidation",
        ))
    }

    /// Evict entries the adapter is allowed to drop (for example the
    /// filesystem adapter's oldest entries past its bound). Returns
    /// the number of entries removed. Adapters without an eviction
    /// policy answer 0.
    async fn prune(&self) -> UnifiedResult<u64> {
        Ok(0)
    }

    /// True when this adapter is currently serving from a fallback
    /// backend because its primary backend failed. Adapters without
    /// a fallback answer `false`.
    fn failover_active(&self) -> bool {
        false
    }
}

// ---------------------------------------------------------------------------
// Filesystem adapter.
// ---------------------------------------------------------------------------

/// Filesystem cache adapter.
///
/// Entries are JSON envelopes stored under `<dir>/<shard>/<sha256(key)>.json`,
/// where `<shard>` is the first two hex digits of the hash. Writes are
/// atomic: the envelope is written to a temporary file in the same shard
/// directory and then renamed over the target, so a concurrent reader
/// never observes a partially written entry. The adapter keeps at most
/// `max_entries` entries (0 means unlimited): every 128th write checks
/// the bound and evicts the oldest entries first, so the cache cannot
/// grow without limit.
#[derive(Debug)]
pub struct FilesystemCache {
    dir: PathBuf,
    max_entries: u64,
    sets_since_prune: AtomicU64,
}

impl FilesystemCache {
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self {
            dir: dir.into(),
            max_entries: DEFAULT_MAX_FS_ENTRIES,
            sets_since_prune: AtomicU64::new(0),
        }
    }

    /// Override the entry bound (0 means unlimited). The default is
    /// `DEFAULT_MAX_FS_ENTRIES`; `build_cache` sets it from
    /// `cache.max_entries`.
    pub fn with_max_entries(mut self, max_entries: u64) -> Self {
        self.max_entries = max_entries;
        self
    }

    pub fn dir(&self) -> &std::path::Path {
        &self.dir
    }

    /// The configured entry bound (0 means unlimited).
    pub fn max_entries(&self) -> u64 {
        self.max_entries
    }

    fn entry_path(&self, key: &str) -> PathBuf {
        let mut hasher = sha2::Sha256::new();
        hasher.update(key.as_bytes());
        let hex = hex_encode(&hasher.finalize());
        self.dir.join(&hex[..2]).join(format!("{hex}.json"))
    }

    /// Check the entry bound every `PRUNE_EVERY_N_SETS` writes and
    /// evict the oldest entries when it is exceeded. Best-effort:
    /// a prune failure is logged, never returned to the caller.
    async fn maybe_prune(&self) {
        if self.max_entries == 0 {
            return;
        }
        let count = self.sets_since_prune.fetch_add(1, Ordering::SeqCst) + 1;
        if count % PRUNE_EVERY_N_SETS != 0 {
            return;
        }
        if let Err(error) = self.prune().await {
            log::warn!("Filesystem cache prune failed: {}", error.message);
        }
    }
}

fn hex_encode(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for &byte in bytes {
        out.push(HEX[(byte >> 4) as usize] as char);
        out.push(HEX[(byte & 0x0f) as usize] as char);
    }
    out
}

#[async_trait]
impl CacheAdapter for FilesystemCache {
    fn kind(&self) -> &'static str {
        "filesystem"
    }

    async fn get(&self, key: &str) -> UnifiedResult<Option<CacheEntry>> {
        validate_key(key)?;
        let path = self.entry_path(key);
        let raw = match tokio::fs::read_to_string(&path).await {
            Ok(raw) => raw,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => {
                return Err(UnifiedError::internal(format!(
                    "Cannot read cache entry {key}: {error}"
                )));
            }
        };
        let entry = match decode_envelope(&raw, key) {
            Ok(entry) => entry,
            Err(_) => {
                // A corrupt envelope is treated as a miss and removed so
                // it cannot poison later reads.
                let _ = tokio::fs::remove_file(&path).await;
                return Ok(None);
            }
        };
        if entry.is_expired() {
            let _ = tokio::fs::remove_file(&path).await;
            return Ok(None);
        }
        Ok(Some(entry))
    }

    async fn set(
        &self,
        key: &str,
        value: CacheValue,
        ttl: Option<Duration>,
    ) -> UnifiedResult<()> {
        validate_key(key)?;
        let expires_at = ttl
            .filter(|ttl| !ttl.is_zero())
            .map(|ttl| Utc::now() + ttl);
        let entry = CacheEntry::new(value, expires_at);
        let raw = encode_envelope(key, &entry)?;
        let path = self.entry_path(key);
        let shard = path.parent().expect("entry path has a parent");
        tokio::fs::create_dir_all(shard).await.map_err(|error| {
            UnifiedError::internal(format!(
                "Cannot create cache directory {}: {error}",
                shard.display()
            ))
        })?;
        let temp_path = shard.join(format!(
            "{}.tmp-{}",
            path.file_name()
                .expect("entry path has a file name")
                .to_string_lossy(),
            uuid::Uuid::now_v7()
        ));
        tokio::fs::write(&temp_path, raw.as_bytes())
            .await
            .map_err(|error| {
                UnifiedError::internal(format!("Cannot write cache entry {key}: {error}"))
            })?;
        if let Err(error) = tokio::fs::rename(&temp_path, &path).await {
            let _ = tokio::fs::remove_file(&temp_path).await;
            return Err(UnifiedError::internal(format!(
                "Cannot store cache entry {key}: {error}"
            )));
        }
        self.maybe_prune().await;
        Ok(())
    }

    async fn delete(&self, key: &str) -> UnifiedResult<bool> {
        validate_key(key)?;
        let path = self.entry_path(key);
        match tokio::fs::remove_file(&path).await {
            Ok(()) => Ok(true),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(error) => Err(UnifiedError::internal(format!(
                "Cannot delete cache entry {key}: {error}"
            ))),
        }
    }

    async fn clear(&self) -> UnifiedResult<()> {
        let mut entries = match tokio::fs::read_dir(&self.dir).await {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(error) => {
                return Err(UnifiedError::internal(format!(
                    "Cannot list cache directory {}: {error}",
                    self.dir.display()
                )));
            }
        };
        while let Some(entry) = entries.next_entry().await.map_err(|error| {
            UnifiedError::internal(format!("Cannot list cache directory: {error}"))
        })? {
            let path = entry.path();
            let result = if path.is_dir() {
                tokio::fs::remove_dir_all(&path).await
            } else {
                tokio::fs::remove_file(&path).await
            };
            if let Err(error) = result {
                return Err(UnifiedError::internal(format!(
                    "Cannot clear cache entry {}: {error}",
                    path.display()
                )));
            }
        }
        Ok(())
    }

    async fn delete_by_prefix(&self, prefix: &str) -> UnifiedResult<u64> {
        // Keys are hashed into file names, so the prefix can only be
        // matched by decoding each envelope's stored key. Corrupt or
        // unreadable files are skipped, never treated as matches.
        let dir = self.dir.clone();
        let prefix = prefix.to_string();
        tokio::task::spawn_blocking(move || -> UnifiedResult<u64> {
            let mut removed: u64 = 0;
            if !dir.exists() {
                return Ok(0);
            }
            for entry in walkdir::WalkDir::new(&dir)
                .follow_links(false)
                .into_iter()
                .filter_map(|entry| entry.ok())
            {
                if !entry.file_type().is_file() {
                    continue;
                }
                let path = entry.path();
                if path.extension().map_or(true, |ext| ext != "json") {
                    continue;
                }
                let Ok(raw) = std::fs::read_to_string(path) else {
                    continue;
                };
                let Ok(envelope) = serde_json::from_str::<CacheEnvelope>(&raw) else {
                    continue;
                };
                if envelope.key.starts_with(&prefix) && std::fs::remove_file(path).is_ok()
                {
                    removed += 1;
                }
            }
            Ok(removed)
        })
        .await
        .map_err(|_| UnifiedError::internal("Cache prefix invalidation worker failed"))?
    }

    async fn prune(&self) -> UnifiedResult<u64> {
        let max_entries = self.max_entries;
        if max_entries == 0 {
            return Ok(0);
        }
        let dir = self.dir.clone();
        tokio::task::spawn_blocking(move || -> UnifiedResult<u64> {
            if !dir.exists() {
                return Ok(0);
            }
            let mut entries: Vec<(std::time::SystemTime, PathBuf)> = Vec::new();
            for entry in walkdir::WalkDir::new(&dir)
                .follow_links(false)
                .into_iter()
                .filter_map(|entry| entry.ok())
            {
                if !entry.file_type().is_file() {
                    continue;
                }
                let path = entry.path();
                if path.extension().map_or(true, |ext| ext != "json") {
                    continue;
                }
                let modified = entry
                    .metadata()
                    .ok()
                    .and_then(|metadata| metadata.modified().ok())
                    .unwrap_or(std::time::SystemTime::UNIX_EPOCH);
                entries.push((modified, path.to_path_buf()));
            }
            if (entries.len() as u64) <= max_entries {
                return Ok(0);
            }
            // Oldest first; entries past the bound are removed.
            entries.sort_by(|a, b| a.0.cmp(&b.0));
            let excess = entries.len() - max_entries as usize;
            let mut removed: u64 = 0;
            for (_, path) in entries.into_iter().take(excess) {
                if std::fs::remove_file(&path).is_ok() {
                    removed += 1;
                }
            }
            Ok(removed)
        })
        .await
        .map_err(|_| UnifiedError::internal("Cache prune worker failed"))?
    }
}

// ---------------------------------------------------------------------------
// Redis adapter.
// ---------------------------------------------------------------------------

/// Redis cache adapter.
///
/// Entries are the same JSON envelopes as the filesystem adapter, stored
/// under a `unified:cache:` prefix with a native Redis TTL (`SETEX`) when
/// the entry expires. The connection is established lazily on first use
/// and shared through a multiplexed connection; a failed connection
/// surfaces as an error instead of silently falling back, so a broken
/// Redis setup is visible rather than masked.
pub struct RedisCache {
    client: redis::Client,
    key_prefix: String,
    connection: Arc<tokio::sync::Mutex<Option<redis::aio::MultiplexedConnection>>>,
}

impl std::fmt::Debug for RedisCache {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RedisCache")
            .field("key_prefix", &self.key_prefix)
            .finish_non_exhaustive()
    }
}

impl RedisCache {
    /// Build the adapter from the Redis configuration section. This only
    /// parses the configuration and opens the client; no connection is
    /// made until the first operation.
    pub fn new(section: &RedisSection) -> UnifiedResult<Self> {
        let addr = if !section.unix_socket.is_empty() {
            redis::ConnectionAddr::Unix(PathBuf::from(&section.unix_socket))
        } else {
            redis::ConnectionAddr::Tcp(section.host.clone(), section.port)
        };
        // `ConnectionInfo` fields are private in redis 1.x, so build
        // it through the public setters instead of a struct literal.
        let mut redis_settings = redis::RedisConnectionInfo::default()
            .set_db(i64::from(section.database))
            .set_protocol(redis::ProtocolVersion::RESP2);
        if !section.username.is_empty() {
            redis_settings = redis_settings.set_username(section.username.clone());
        }
        if !section.password.is_empty() {
            redis_settings = redis_settings.set_password(section.password.clone());
        }
        let info = {
            use redis::IntoConnectionInfo;
            addr.into_connection_info()
                .map_err(|error| {
                    UnifiedError::internal(format!("Invalid Redis configuration: {error}"))
                })?
                .set_redis_settings(redis_settings)
        };
        let client = redis::Client::open(info).map_err(|error| {
            UnifiedError::internal(format!("Invalid Redis configuration: {error}"))
        })?;
        Ok(Self {
            client,
            key_prefix: REDIS_KEY_PREFIX.to_string(),
            connection: Arc::new(tokio::sync::Mutex::new(None)),
        })
    }

    fn namespaced_key(&self, key: &str) -> String {
        format!("{}{key}", self.key_prefix)
    }

    async fn connection(&self) -> UnifiedResult<redis::aio::MultiplexedConnection> {
        let mut guard = self.connection.lock().await;
        if let Some(connection) = guard.as_ref() {
            return Ok(connection.clone());
        }
        let connection = self
            .client
            .get_multiplexed_async_connection()
            .await
            .map_err(|error| {
                UnifiedError::internal(format!("Cannot connect to Redis: {error}"))
            })?;
        *guard = Some(connection.clone());
        Ok(connection)
    }
}

#[async_trait]
impl CacheAdapter for RedisCache {
    fn kind(&self) -> &'static str {
        "redis"
    }

    async fn get(&self, key: &str) -> UnifiedResult<Option<CacheEntry>> {
        use redis::AsyncCommands;
        validate_key(key)?;
        let mut connection = self.connection().await?;
        let namespaced = self.namespaced_key(key);
        let raw: Option<String> = connection.get(&namespaced).await.map_err(|error| {
            UnifiedError::internal(format!("Redis GET failed for {key}: {error}"))
        })?;
        let Some(raw) = raw else {
            return Ok(None);
        };
        let entry = match decode_envelope(&raw, key) {
            Ok(entry) => entry,
            Err(_) => {
                // A corrupt envelope is treated as a miss and removed.
                let _: i64 = connection.del(&namespaced).await.unwrap_or(0);
                return Ok(None);
            }
        };
        if entry.is_expired() {
            let _: i64 = connection.del(&namespaced).await.unwrap_or(0);
            return Ok(None);
        }
        Ok(Some(entry))
    }

    async fn set(
        &self,
        key: &str,
        value: CacheValue,
        ttl: Option<Duration>,
    ) -> UnifiedResult<()> {
        use redis::AsyncCommands;
        validate_key(key)?;
        let expires_at = ttl
            .filter(|ttl| !ttl.is_zero())
            .map(|ttl| Utc::now() + ttl);
        let entry = CacheEntry::new(value, expires_at);
        let raw = encode_envelope(key, &entry)?;
        let mut connection = self.connection().await?;
        let namespaced = self.namespaced_key(key);
        if let Some(ttl) = ttl.filter(|ttl| !ttl.is_zero()) {
            // Redis TTLs are whole seconds: round a sub-second
            // remainder up so the entry never expires early. The
            // envelope's own expiry is still enforced exactly on read.
            let secs = ttl
                .as_secs()
                .saturating_add(u64::from(ttl.subsec_nanos() > 0))
                .max(1);
            let _: () = connection
                .set_ex(&namespaced, raw, secs)
                .await
                .map_err(|error| {
                    UnifiedError::internal(format!("Redis SETEX failed for {key}: {error}"))
                })?;
        } else {
            let _: () = connection.set(&namespaced, raw).await.map_err(|error| {
                UnifiedError::internal(format!("Redis SET failed for {key}: {error}"))
            })?;
        }
        Ok(())
    }

    async fn delete(&self, key: &str) -> UnifiedResult<bool> {
        use redis::AsyncCommands;
        validate_key(key)?;
        let mut connection = self.connection().await?;
        let removed: i64 = connection
            .del(self.namespaced_key(key))
            .await
            .map_err(|error| {
                UnifiedError::internal(format!("Redis DEL failed for {key}: {error}"))
            })?;
        Ok(removed > 0)
    }

    async fn clear(&self) -> UnifiedResult<()> {
        use redis::AsyncCommands;
        let mut connection = self.connection().await?;
        let pattern = format!("{}*", self.key_prefix);
        let mut stream = connection
            .scan_match::<_, String>(pattern)
            .await
            .map_err(|error| {
                UnifiedError::internal(format!("Redis SCAN failed: {error}"))
            })?;
        let mut keys: Vec<String> = Vec::new();
        while let Some(key) = stream.next().await {
            keys.push(key.map_err(|error| {
                UnifiedError::internal(format!("Redis SCAN failed: {error}"))
            })?);
        }
        drop(stream);
        for batch in keys.chunks(500) {
            let _: i64 = connection.del(batch.to_vec()).await.map_err(|error| {
                UnifiedError::internal(format!("Redis DEL failed during clear: {error}"))
            })?;
        }
        Ok(())
    }

    async fn delete_by_prefix(&self, prefix: &str) -> UnifiedResult<u64> {
        use redis::AsyncCommands;
        let mut connection = self.connection().await?;
        // Escape SCAN glob characters in the prefix so the pattern
        // matches the literal prefix only.
        let escaped: String = prefix.chars().flat_map(|c| match c {
            '*' | '?' | '[' | ']' | '\\' => vec!['\\', c],
            other => vec![other],
        }).collect();
        let pattern = format!("{}{escaped}*", self.key_prefix);
        let mut stream = connection
            .scan_match::<_, String>(pattern)
            .await
            .map_err(|error| {
                UnifiedError::internal(format!("Redis SCAN failed: {error}"))
            })?;
        let mut keys: Vec<String> = Vec::new();
        while let Some(key) = stream.next().await {
            keys.push(key.map_err(|error| {
                UnifiedError::internal(format!("Redis SCAN failed: {error}"))
            })?);
        }
        drop(stream);
        let mut removed: u64 = 0;
        for batch in keys.chunks(500) {
            let deleted: i64 = connection.del(batch.to_vec()).await.map_err(|error| {
                UnifiedError::internal(format!(
                    "Redis DEL failed during prefix invalidation: {error}"
                ))
            })?;
            removed += deleted.max(0) as u64;
        }
        Ok(removed)
    }
}

// ---------------------------------------------------------------------------
// Failover adapter (Redis primary, filesystem fallback).
// ---------------------------------------------------------------------------

/// A cache adapter that serves from a fallback adapter when its
/// primary adapter fails at runtime. Reads fall back on a primary
/// error (a miss is not a failure: it answers `None` without
/// touching the fallback). Writes go to the primary and, on success,
/// are also written to the fallback best-effort so the fallback stays
/// warm; when the primary write fails, the write goes to the fallback
/// and succeeds there instead of failing the request. Every failover
/// is logged as a warning and `failover_active` reports whether the
/// most recent operation needed the fallback, so a dead primary is
/// visible in the cache status endpoint instead of silently masked.
pub struct FailoverCache {
    primary: Arc<dyn CacheAdapter>,
    fallback: Arc<dyn CacheAdapter>,
    primary_failed: AtomicBool,
}

impl std::fmt::Debug for FailoverCache {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FailoverCache")
            .field("primary", &self.primary.kind())
            .field("fallback", &self.fallback.kind())
            .field(
                "primary_failed",
                &self.primary_failed.load(Ordering::SeqCst),
            )
            .finish_non_exhaustive()
    }
}

impl FailoverCache {
    pub fn new(primary: Arc<dyn CacheAdapter>, fallback: Arc<dyn CacheAdapter>) -> Self {
        Self {
            primary,
            fallback,
            primary_failed: AtomicBool::new(false),
        }
    }

    fn note_primary_error(&self, operation: &str, error: &UnifiedError) {
        self.primary_failed.store(true, Ordering::SeqCst);
        log::warn!(
            "Cache primary ({}) failed on {operation} ({}); serving from the {} fallback",
            self.primary.kind(),
            error.message,
            self.fallback.kind()
        );
    }

    fn note_primary_ok(&self) {
        self.primary_failed.store(false, Ordering::SeqCst);
    }
}

#[async_trait]
impl CacheAdapter for FailoverCache {
    fn kind(&self) -> &'static str {
        self.primary.kind()
    }

    fn failover_active(&self) -> bool {
        self.primary_failed.load(Ordering::SeqCst)
    }

    async fn get(&self, key: &str) -> UnifiedResult<Option<CacheEntry>> {
        match self.primary.get(key).await {
            Ok(entry) => {
                self.note_primary_ok();
                Ok(entry)
            }
            Err(error) => {
                self.note_primary_error("get", &error);
                self.fallback.get(key).await
            }
        }
    }

    async fn set(
        &self,
        key: &str,
        value: CacheValue,
        ttl: Option<Duration>,
    ) -> UnifiedResult<()> {
        match self.primary.set(key, value.clone(), ttl).await {
            Ok(()) => {
                self.note_primary_ok();
                if let Err(error) = self.fallback.set(key, value, ttl).await {
                    log::warn!(
                        "Cache fallback ({}) warm write failed for {key}: {}",
                        self.fallback.kind(),
                        error.message
                    );
                }
                Ok(())
            }
            Err(error) => {
                self.note_primary_error("set", &error);
                self.fallback.set(key, value, ttl).await
            }
        }
    }

    async fn delete(&self, key: &str) -> UnifiedResult<bool> {
        match self.primary.delete(key).await {
            Ok(deleted) => {
                self.note_primary_ok();
                let _ = self.fallback.delete(key).await;
                Ok(deleted)
            }
            Err(error) => {
                self.note_primary_error("delete", &error);
                self.fallback.delete(key).await
            }
        }
    }

    async fn clear(&self) -> UnifiedResult<()> {
        match self.primary.clear().await {
            Ok(()) => {
                self.note_primary_ok();
                if let Err(error) = self.fallback.clear().await {
                    log::warn!(
                        "Cache fallback ({}) clear failed: {}",
                        self.fallback.kind(),
                        error.message
                    );
                }
                Ok(())
            }
            Err(error) => {
                self.note_primary_error("clear", &error);
                self.fallback.clear().await
            }
        }
    }

    async fn delete_by_prefix(&self, prefix: &str) -> UnifiedResult<u64> {
        match self.primary.delete_by_prefix(prefix).await {
            Ok(removed) => {
                self.note_primary_ok();
                let _ = self.fallback.delete_by_prefix(prefix).await;
                Ok(removed)
            }
            Err(error) => {
                self.note_primary_error("delete_by_prefix", &error);
                self.fallback.delete_by_prefix(prefix).await
            }
        }
    }

    async fn prune(&self) -> UnifiedResult<u64> {
        let primary = self.primary.prune().await.unwrap_or(0);
        let fallback = self.fallback.prune().await.unwrap_or(0);
        Ok(primary + fallback)
    }
}

// ---------------------------------------------------------------------------
// Facade.
// ---------------------------------------------------------------------------

/// The cache facade every module uses. It validates keys, applies the
/// configured default TTL when the caller passes none, and delegates
/// storage to the configured adapter.
///
/// Per-tenant data MUST go through the tenant-scoped methods
/// (`get_tenant` / `set_tenant` / `delete_tenant` /
/// `get_or_compute_tenant` / `invalidate_tenant`): the unscoped
/// methods reject `tenant:`-prefixed keys, so one tenant can never
/// read another tenant's cached entry.
#[derive(Debug)]
pub struct Cache {
    adapter: Arc<dyn CacheAdapter>,
    default_ttl: Option<Duration>,
    /// In-flight `get_or_compute` fills, one mutex per key (see
    /// `get_or_compute`). Entries are removed once no caller waits
    /// on them, so the map holds only keys being filled right now.
    inflight: Arc<tokio::sync::Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>>,
}

impl Cache {
    pub fn new(adapter: Arc<dyn CacheAdapter>, default_ttl: Option<Duration>) -> Self {
        Self {
            adapter,
            default_ttl,
            inflight: Arc::new(tokio::sync::Mutex::new(HashMap::new())),
        }
    }

    /// Short backend name of the active adapter.
    pub fn adapter_kind(&self) -> &'static str {
        self.adapter.kind()
    }

    /// True when the active adapter is currently serving from its
    /// fallback backend because the primary failed. Always `false`
    /// for adapters without a fallback.
    pub fn failover_active(&self) -> bool {
        self.adapter.failover_active()
    }

    /// The TTL applied when `set` is called without one.
    pub fn default_ttl(&self) -> Option<Duration> {
        self.default_ttl
    }

    pub async fn get(&self, key: &str) -> UnifiedResult<Option<CacheEntry>> {
        validate_key(key)?;
        reject_tenant_key(key)?;
        self.adapter.get(key).await
    }

    pub async fn get_value(&self, key: &str) -> UnifiedResult<Option<CacheValue>> {
        Ok(self.get(key).await?.map(|entry| entry.value))
    }

    pub async fn get_text(&self, key: &str) -> UnifiedResult<Option<String>> {
        match self.get_value(key).await? {
            Some(value) => Ok(Some(value.as_text()?)),
            None => Ok(None),
        }
    }

    pub async fn get_json<T>(&self, key: &str) -> UnifiedResult<Option<T>>
    where
        T: for<'de> Deserialize<'de>,
    {
        match self.get_value(key).await? {
            Some(value) => Ok(Some(value.as_json()?)),
            None => Ok(None),
        }
    }

    /// Store a value. `ttl` of `None` applies the configured default TTL.
    pub async fn set(
        &self,
        key: &str,
        value: CacheValue,
        ttl: Option<Duration>,
    ) -> UnifiedResult<()> {
        validate_key(key)?;
        reject_tenant_key(key)?;
        let ttl = ttl.or(self.default_ttl);
        self.adapter.set(key, value, ttl).await
    }

    /// Store a serializable value as JSON.
    pub async fn set_json<T: Serialize>(
        &self,
        key: &str,
        value: &T,
        ttl: Option<Duration>,
    ) -> UnifiedResult<()> {
        self.set(key, CacheValue::from_json(value)?, ttl).await
    }

    pub async fn delete(&self, key: &str) -> UnifiedResult<bool> {
        validate_key(key)?;
        reject_tenant_key(key)?;
        self.adapter.delete(key).await
    }

    pub async fn has(&self, key: &str) -> UnifiedResult<bool> {
        validate_key(key)?;
        reject_tenant_key(key)?;
        self.adapter.has(key).await
    }

    pub async fn clear(&self) -> UnifiedResult<()> {
        self.adapter.clear().await
    }

    /// Evict entries the adapter is allowed to drop (filesystem
    /// bound). Returns the number of entries removed.
    pub async fn prune(&self) -> UnifiedResult<u64> {
        self.adapter.prune().await
    }

    /// Delete every entry whose key starts with `prefix` (for
    /// example a module's key family after a write that mutates it).
    /// Returns the number of entries removed.
    pub async fn invalidate_prefix(&self, prefix: &str) -> UnifiedResult<u64> {
        if prefix.is_empty()
            || prefix.chars().count() > MAX_KEY_LEN
            || prefix.chars().any(|c| c.is_control())
        {
            return Err(UnifiedError::bad_request(
                "Cache invalidation prefix must be 1..=512 characters without control characters",
            ));
        }
        self.adapter.delete_by_prefix(prefix).await
    }

    // -- Tenant-scoped access. Keys are stored as
    // `tenant:{id}:{key}`; the unscoped methods above reject that
    // prefix, so isolation holds even if a caller guesses a key. --

    pub async fn get_tenant(
        &self,
        tenant_id: &str,
        key: &str,
    ) -> UnifiedResult<Option<CacheEntry>> {
        let scoped = tenant_key(tenant_id, key)?;
        self.adapter.get(&scoped).await
    }

    pub async fn get_value_tenant(
        &self,
        tenant_id: &str,
        key: &str,
    ) -> UnifiedResult<Option<CacheValue>> {
        Ok(self
            .get_tenant(tenant_id, key)
            .await?
            .map(|entry| entry.value))
    }

    pub async fn set_tenant(
        &self,
        tenant_id: &str,
        key: &str,
        value: CacheValue,
        ttl: Option<Duration>,
    ) -> UnifiedResult<()> {
        let scoped = tenant_key(tenant_id, key)?;
        let ttl = ttl.or(self.default_ttl);
        self.adapter.set(&scoped, value, ttl).await
    }

    pub async fn delete_tenant(
        &self,
        tenant_id: &str,
        key: &str,
    ) -> UnifiedResult<bool> {
        let scoped = tenant_key(tenant_id, key)?;
        self.adapter.delete(&scoped).await
    }

    pub async fn has_tenant(
        &self,
        tenant_id: &str,
        key: &str,
    ) -> UnifiedResult<bool> {
        Ok(self.get_tenant(tenant_id, key).await?.is_some())
    }

    /// Delete every cached entry belonging to one tenant.
    pub async fn invalidate_tenant(&self, tenant_id: &str) -> UnifiedResult<u64> {
        validate_tenant_id(tenant_id)?;
        self.adapter
            .delete_by_prefix(&format!("{TENANT_KEY_PREFIX}{tenant_id}:"))
            .await
    }

    // -- Stampede-protected fills. --

    /// Read a value, computing and storing it when missing.
    /// Concurrent callers for the same key share one computation:
    /// the first caller computes while the others wait on the
    /// per-key lock, then all read the stored value. The compute
    /// closure must not call `get_or_compute` for the same key (it
    /// would wait on its own lock forever).
    pub async fn get_or_compute<F, Fut>(
        &self,
        key: &str,
        ttl: Option<Duration>,
        compute: F,
    ) -> UnifiedResult<CacheValue>
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = UnifiedResult<CacheValue>>,
    {
        validate_key(key)?;
        reject_tenant_key(key)?;
        self.get_or_compute_scoped(key, ttl, compute).await
    }

    /// Tenant-scoped `get_or_compute` (see `get_or_compute`).
    pub async fn get_or_compute_tenant<F, Fut>(
        &self,
        tenant_id: &str,
        key: &str,
        ttl: Option<Duration>,
        compute: F,
    ) -> UnifiedResult<CacheValue>
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = UnifiedResult<CacheValue>>,
    {
        let scoped = tenant_key(tenant_id, key)?;
        self.get_or_compute_scoped(&scoped, ttl, compute).await
    }

    async fn get_or_compute_scoped<F, Fut>(
        &self,
        scoped_key: &str,
        ttl: Option<Duration>,
        compute: F,
    ) -> UnifiedResult<CacheValue>
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = UnifiedResult<CacheValue>>,
    {
        let slot = {
            let mut inflight = self.inflight.lock().await;
            inflight
                .entry(scoped_key.to_string())
                .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
                .clone()
        };
        let value = {
            let _guard = slot.lock().await;
            match self.adapter.get(scoped_key).await? {
                Some(entry) => entry.value,
                None => {
                    let value = compute().await?;
                    let ttl = ttl.or(self.default_ttl);
                    self.adapter.set(scoped_key, value.clone(), ttl).await?;
                    value
                }
            }
        };
        // Drop the map entry once nobody waits on this key, so the
        // map holds only keys being filled right now.
        {
            let mut inflight = self.inflight.lock().await;
            if Arc::strong_count(&slot) == 2 {
                inflight.remove(scoped_key);
            }
        }
        Ok(value)
    }
}

/// Build the application cache from the configuration. Redis is used when
/// `redis.enabled` is true; if the Redis client cannot even be built from
/// the configuration, a warning is logged and the filesystem adapter is
/// used instead, so a bad cache configuration never prevents startup.
/// When Redis is enabled and `cache.failover_enabled` is true (the
/// default), the Redis adapter is wrapped in `FailoverCache`, so a
/// Redis failure at runtime falls back to the filesystem adapter
/// instead of failing the request. Connection failures at runtime
/// still surface as errors when failover is disabled.
pub fn build_cache(config: &AppConfig) -> Arc<Cache> {
    let default_ttl = if config.cache.default_ttl_secs == 0 {
        None
    } else {
        Some(Duration::from_secs(config.cache.default_ttl_secs))
    };
    let filesystem: Arc<dyn CacheAdapter> = Arc::new(
        FilesystemCache::new(config.cache.dir.clone())
            .with_max_entries(config.cache.max_entries),
    );
    if config.redis.enabled {
        match RedisCache::new(&config.redis) {
            Ok(adapter) => {
                if config.cache.failover_enabled {
                    return Arc::new(Cache::new(
                        Arc::new(FailoverCache::new(Arc::new(adapter), filesystem)),
                        default_ttl,
                    ));
                }
                return Arc::new(Cache::new(Arc::new(adapter), default_ttl));
            }
            Err(error) => {
                log::warn!(
                    "Redis cache unavailable ({}); falling back to the filesystem cache",
                    error.message
                );
            }
        }
    }
    Arc::new(Cache::new(filesystem, default_ttl))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_encode_matches_reference_for_every_byte_value() {
        // Exhaustive oracle: the lookup-table encoding must agree
        // with plain per-byte formatting for all 256 byte values.
        // (`format!` is the reference here, in test code only.)
        for byte in 0..=u8::MAX {
            assert_eq!(hex_encode(&[byte]), format!("{byte:02x}"));
        }
        assert_eq!(hex_encode(&[0xde, 0xad, 0xbe, 0xef]), "deadbeef");
    }
}
