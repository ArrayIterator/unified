// Cache layer.
//
// A small, adapter-based cache shared by every module through `AppContext`.
// Callers work with typed `CacheValue`s and TTLs through the `Cache`
// facade; the storage backend is chosen by configuration — the filesystem
// adapter by default, or Redis when `redis.enabled` is true.
//
// Both adapters store the same JSON envelope, so an entry written by one
// backend can be read by the other:
//
//   {"key": "...", "expires_at": <unix seconds | null>, "value": {...}}
//
// The envelope carries the key so a hash collision or a stale file is
// detected instead of silently returning another entry's value.
//
// Key rules: a key is an opaque string of at most 250 characters, without
// control characters or surrounding whitespace. The filesystem adapter
// never uses the raw key as a path component — entries live under
// `sha256(key)`, sharded by the first two hex digits — so keys are safe
// against path traversal by construction. Redis keys are namespaced with
// a `unified:cache:` prefix, and `clear` only removes keys under that
// prefix (never `FLUSHDB`).

use crate::core::config::{AppConfig, RedisSection};
use crate::core::error::{UnifiedError, UnifiedResult};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use sha2::Digest;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

/// Maximum cache key length in characters.
pub const MAX_KEY_LEN: usize = 250;
/// Prefix applied to every key stored in Redis.
pub const REDIS_KEY_PREFIX: &str = "unified:cache:";

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
/// never observes a partially written entry.
#[derive(Debug)]
pub struct FilesystemCache {
    dir: PathBuf,
}

impl FilesystemCache {
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self { dir: dir.into() }
    }

    pub fn dir(&self) -> &std::path::Path {
        &self.dir
    }

    fn entry_path(&self, key: &str) -> PathBuf {
        let mut hasher = sha2::Sha256::new();
        hasher.update(key.as_bytes());
        let hex = hex_encode(&hasher.finalize());
        self.dir.join(&hex[..2]).join(format!("{hex}.json"))
    }
}

fn hex_encode(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
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
        let info = redis::ConnectionInfo {
            addr,
            redis: redis::RedisConnectionInfo {
                db: i64::from(section.database),
                username: if section.username.is_empty() {
                    None
                } else {
                    Some(section.username.clone())
                },
                password: if section.password.is_empty() {
                    None
                } else {
                    Some(section.password.clone())
                },
                protocol: redis::ProtocolVersion::RESP2,
            },
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
            let _: () = connection
                .set_ex(&namespaced, raw, ttl.as_secs())
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
            keys.push(key);
        }
        drop(stream);
        for batch in keys.chunks(500) {
            let _: i64 = connection.del(batch.to_vec()).await.map_err(|error| {
                UnifiedError::internal(format!("Redis DEL failed during clear: {error}"))
            })?;
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Facade.
// ---------------------------------------------------------------------------

/// The cache facade every module uses. It validates keys, applies the
/// configured default TTL when the caller passes none, and delegates
/// storage to the configured adapter.
#[derive(Debug)]
pub struct Cache {
    adapter: Arc<dyn CacheAdapter>,
    default_ttl: Option<Duration>,
}

impl Cache {
    pub fn new(adapter: Arc<dyn CacheAdapter>, default_ttl: Option<Duration>) -> Self {
        Self {
            adapter,
            default_ttl,
        }
    }

    /// Short backend name of the active adapter.
    pub fn adapter_kind(&self) -> &'static str {
        self.adapter.kind()
    }

    /// The TTL applied when `set` is called without one.
    pub fn default_ttl(&self) -> Option<Duration> {
        self.default_ttl
    }

    pub async fn get(&self, key: &str) -> UnifiedResult<Option<CacheEntry>> {
        validate_key(key)?;
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
        self.adapter.delete(key).await
    }

    pub async fn has(&self, key: &str) -> UnifiedResult<bool> {
        validate_key(key)?;
        self.adapter.has(key).await
    }

    pub async fn clear(&self) -> UnifiedResult<()> {
        self.adapter.clear().await
    }
}

/// Build the application cache from the configuration. Redis is used when
/// `redis.enabled` is true; if the Redis client cannot even be built from
/// the configuration, a warning is logged and the filesystem adapter is
/// used instead, so a bad cache configuration never prevents startup.
/// Connection failures at runtime still surface as errors.
pub fn build_cache(config: &AppConfig) -> Arc<Cache> {
    let default_ttl = if config.cache.default_ttl_secs == 0 {
        None
    } else {
        Some(Duration::from_secs(config.cache.default_ttl_secs))
    };
    if config.redis.enabled {
        match RedisCache::new(&config.redis) {
            Ok(adapter) => {
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
    Arc::new(Cache::new(
        Arc::new(FilesystemCache::new(config.cache.dir.clone())),
        default_ttl,
    ))
}
