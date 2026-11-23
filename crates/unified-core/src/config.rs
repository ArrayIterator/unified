// YAML configuration.
//
// The configuration file is loaded at startup and on reload. Before a reload
// applies a new configuration, `validate()` must run and report zero errors
// — a failing configuration never replaces the running one. The CLI exposes
// the same check as `unified -t` (default config) and
// `unified -t -f /path/to/config.yaml`.

use crate::error::{UnifiedError, UnifiedResult};
use crate::hardening::SecuritySettings;
use crate::routes::RoutesSection;
use crate::storage::StorageSection;
use chrono::NaiveDate;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use uuid::Uuid;

/// Environment variable that overrides the default config path.
pub const CONFIG_ENV: &str = "UNIFIED_CONFIG_FILE";
/// Default config file name, resolved relative to the working directory.
pub const DEFAULT_CONFIG_FILE: &str = "config.yaml";

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct AppConfig {
    pub app: AppSection,
    pub cache: CacheSection,
    pub redis: RedisSection,
    pub database: DatabaseSection,
    pub licenses: LicenseSection,
    pub geo: GeoSection,
    pub acme: AcmeSection,
    pub store: StoreSection,
    /// Security hardening settings (brute-force protection, rate
    /// limiting, anti-DDoS, IP lists, response headers). See
    /// `crate::hardening::SecuritySettings`.
    pub security: SecuritySettings,
    /// Public API settings: global prefix and versioning defaults.
    pub api: ApiSection,
    /// Named page-route overrides (`login`, `dashboard`, ...). See
    /// `crate::routes::RoutesSection`.
    pub routes: RoutesSection,
    /// Storage targets registered at startup. See
    /// `crate::storage::StorageSection`.
    pub storage: StorageSection,
    /// Load balancer defaults (health checks, strategy, limits).
    pub load_balancer: LoadBalancerSection,
    /// Pingora gateway listener and runtime settings.
    pub gateway: GatewaySection,
    /// System-records retention.
    pub records: RecordsSection,
    /// Documents module settings (chunked-upload spool directory
    /// and the in-RAM chunk cap). See `DocumentsConfig`.
    pub documents: DocumentsConfig,
    /// Runtime asset pipeline settings (JavaScript and CSS
    /// minification). See `AssetsConfig`.
    pub assets: AssetsConfig,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            app: AppSection::default(),
            cache: CacheSection::default(),
            redis: RedisSection::default(),
            database: DatabaseSection::default(),
            licenses: LicenseSection::default(),
            geo: GeoSection::default(),
            acme: AcmeSection::default(),
            store: StoreSection::default(),
            security: SecuritySettings::default(),
            api: ApiSection::default(),
            routes: RoutesSection::default(),
            storage: StorageSection::default(),
            load_balancer: LoadBalancerSection::default(),
            gateway: GatewaySection::default(),
            records: RecordsSection::default(),
            documents: DocumentsConfig::default(),
            assets: AssetsConfig::default(),
        }
    }
}

/// Documents module settings, from the `documents:` section of the
/// config file. Both values are optional: `None` keeps the built-in
/// defaults, and the admin settings page can override them at
/// runtime (the config file wins again on the next restart).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct DocumentsConfig {
    /// Temp files directory for chunked-upload part spool files.
    /// `None` (default) means `<storage>/temps/uploads` under
    /// the local storage root. Session directories hang under it by
    /// UTC date: `<root>/YYYY/MM/DD/<session-id>`.
    pub spool_dir: Option<String>,
    /// Maximum chunk data held in RAM in bytes. `None` (default)
    /// means 100 MiB.
    pub max_chunk_ram_bytes: Option<u64>,
    /// Extension -> content-kind overrides for the extension
    /// registry (`documents`, `media`, or `others`; keys are
    /// lowercased). Merged over the shipped defaults in
    /// `src/modules/documents/extension_kinds.yaml`; `None`
    /// (default) means the defaults alone.
    pub extensions: Option<HashMap<String, String>>,
}

impl DocumentsConfig {
    /// Minimum `max_chunk_ram_bytes` (100 MiB).
    pub const MAX_CHUNK_RAM_MIN: u64 = 100 * 1024 * 1024;
    /// Maximum `max_chunk_ram_bytes` (8 GiB).
    pub const MAX_CHUNK_RAM_MAX: u64 = 8 * 1024 * 1024 * 1024;
    /// Default `max_chunk_ram_bytes` (100 MiB).
    pub const MAX_CHUNK_RAM_DEFAULT: u64 = Self::MAX_CHUNK_RAM_MIN;
}

impl Default for DocumentsConfig {
    fn default() -> Self {
        Self {
            spool_dir: None,
            max_chunk_ram_bytes: None,
            extensions: None,
        }
    }
}

/// Runtime asset pipeline settings, from the `assets:` section of
/// the config file. The pipeline transpiles the dashboard
/// TypeScript sources to JavaScript at startup and serves the
/// result from RAM; stylesheets are read from `assets/css/`.
/// Takes effect at startup only, like the TypeScript compilation
/// itself: changing it requires a restart.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct AssetsConfig {
    /// Minify the JavaScript and CSS served by the runtime asset
    /// pipeline. `None` (default) means automatic: minify unless
    /// the application runs in development mode
    /// (`app.mode == "development"`). `Some(true)` always minifies,
    /// `Some(false)` never minifies. Debug builds serve the
    /// readable originals; release builds serve the minified
    /// versions.
    pub minify: Option<bool>,
}

impl Default for AssetsConfig {
    fn default() -> Self {
        Self { minify: None }
    }
}

/// Placeholder secrets shipped in `config.example.yaml`. They must
/// never sign a real token: `AppConfig::validate` rejects them at
/// startup, and the Auth Manager's signing key rejects them a
/// second time at issue time. The Auth Manager imports these
/// constants instead of defining its own copies, so the two layers
/// can never drift apart.
pub const PLACEHOLDER_SECRET_KEY: &str = "change-this-to-a-random-secret-string";
pub const PLACEHOLDER_SALT_KEY: &str = "change-this-to-a-random-salt-string";

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct AppSection {
    /// Application mode: `development` or `production`.
    pub mode: String,
    /// URL path of the dashboard, for example `dashboard`.
    pub dashboard_path: String,
    /// Cookie name used to store the session id.
    pub session_name: String,
    /// CSRF token lifetime in minutes (5..=86400).
    pub csrf_duration: u32,
    /// Secret key used to sign session tokens and sensitive data.
    pub secret_key: String,
    /// Salt key used together with the secret key.
    pub salt_key: String,
    /// Sunset date (`YYYY-MM-DD`) for legacy unpeppered API-key
    /// hashes: on or after this date a key stored under the plain
    /// SHA-256 digest stops authenticating and its owner must
    /// rotate it. Empty means the built-in default
    /// (`2027-03-01`); see `apikeys::LEGACY_API_KEY_HASH_SUNSET_DEFAULT`.
    pub apikey_legacy_hash_sunset: String,
    /// System user the daemon runs as (empty means auto-detect).
    pub user: String,
    /// TCP listen addresses, for example `127.0.0.1:8080`.
    pub tcp: Vec<String>,
    /// TLS configuration.
    pub ssl: SslSection,
    /// Worker count, 0 means auto-detect from CPU cores.
    pub workers: usize,
    /// Operation timeout in seconds (1..=3600).
    pub operation_timeout: u64,
    /// Request timeout in seconds (0 disables it).
    pub request_timeout: u64,
    /// Disconnect timeout in seconds.
    pub disconnect_timeout: u64,
    /// Maximum number of concurrent connections.
    pub max_connections: usize,
    /// Per-worker maximum concurrent TLS connections (0 means auto).
    pub max_connections_rate: usize,
    /// Listen backlog size.
    pub backlog: u32,
    /// Maximum number of open file descriptors.
    pub rlimit_nofile: u64,
    /// Keep-alive timeout in seconds (0 disables it).
    pub keep_alive: u64,
    /// Enable automatic memory cleanup.
    pub auto_clean_memory: bool,
    /// Memory threshold that triggers cleanup, for example `384M`.
    pub auto_clean_memory_size: String,
    /// Cleanup interval, for example `1H`.
    pub auto_clean_memory_interval: String,
}

impl Default for AppSection {
    fn default() -> Self {
        Self {
            mode: "production".to_string(),
            dashboard_path: "dashboard".to_string(),
            session_name: "vault".to_string(),
            csrf_duration: 30,
            secret_key: String::new(),
            salt_key: String::new(),
            apikey_legacy_hash_sunset: String::new(),
            user: String::new(),
            tcp: vec!["127.0.0.1:8080".to_string()],
            ssl: SslSection::default(),
            workers: 0,
            operation_timeout: 10,
            request_timeout: 10,
            disconnect_timeout: 5,
            max_connections: 25_000,
            max_connections_rate: 0,
            backlog: 4096,
            rlimit_nofile: 65_535,
            keep_alive: 75,
            auto_clean_memory: true,
            auto_clean_memory_size: "384M".to_string(),
            auto_clean_memory_interval: "1H".to_string(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct SslSection {
    /// Automatically create a certificate when no certificate matches.
    pub auto_create: bool,
    /// TLS listen addresses, for example `127.0.0.1:8443`.
    pub listen: Vec<String>,
    /// Path to the default certificate file.
    pub cert: String,
    /// Path to the default private key file.
    pub key: String,
    /// TLS session cache size in megabytes (2..=256).
    pub ssl_session_cache: u32,
    /// Per-domain / per-IP certificates.
    pub domains: HashMap<String, DomainSsl>,
}

impl Default for SslSection {
    fn default() -> Self {
        Self {
            auto_create: false,
            listen: vec!["127.0.0.1:8443".to_string()],
            cert: String::new(),
            key: String::new(),
            ssl_session_cache: 32,
            domains: HashMap::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct DomainSsl {
    pub cert: String,
    pub key: String,
}

impl Default for DomainSsl {
    fn default() -> Self {
        Self {
            cert: String::new(),
            key: String::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct CacheSection {
    /// Directory for the filesystem cache adapter, used when Redis is
    /// disabled. Relative paths resolve against the working directory.
    pub dir: String,
    /// Default time-to-live for cache entries in seconds. 0 means entries
    /// never expire unless a per-entry TTL is given.
    pub default_ttl_secs: u64,
    /// Maximum entries kept by the filesystem cache adapter. 0 means
    /// unlimited. When the limit is exceeded the oldest entries are
    /// evicted (see `FilesystemCache::prune`). Configurable default:
    /// 10000.
    pub max_entries: u64,
    /// When true (the default) and Redis is enabled, a Redis failure
    /// at runtime falls back to the filesystem adapter instead of
    /// failing the request. The fallback is logged and reported by
    /// the cache status endpoint.
    pub failover_enabled: bool,
}

impl Default for CacheSection {
    fn default() -> Self {
        Self {
            dir: "data/cache".to_string(),
            default_ttl_secs: 3600,
            max_entries: 10_000,
            failover_enabled: true,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct RedisSection {
    /// Enable the Redis cache adapter. When false, the filesystem cache
    /// adapter is used instead.
    pub enabled: bool,
    pub unix_socket: String,
    pub host: String,
    pub port: u16,
    pub username: String,
    pub password: String,
    pub database: u8,
    pub max_pools: u32,
    pub minimum_idle_connections: u32,
    pub idle_timeout: u64,
    pub connect_timeout: u64,
}

impl Default for RedisSection {
    fn default() -> Self {
        Self {
            enabled: false,
            unix_socket: String::new(),
            host: "localhost".to_string(),
            port: 6379,
            username: String::new(),
            password: String::new(),
            database: 0,
            max_pools: 10,
            minimum_idle_connections: 1,
            idle_timeout: 600,
            connect_timeout: 5,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct DatabaseSection {
    /// Driver: `postgresql` (default), `mysql`, or `sqlite`.
    pub driver: String,
    /// Unix socket path (empty means TCP).
    pub socket: Option<String>,
    pub host: String,
    pub port: u16,
    pub username: String,
    pub password: String,
    pub database: String,
    pub max_connections: u32,
    pub min_connections: u32,
    /// Maximum connection lifetime in seconds.
    pub max_lifetime: u64,
    /// Statement timeout in seconds.
    pub statement_timeout: u64,
    /// Idle connection timeout in seconds.
    pub idle_timeout: u64,
    /// Connection acquisition timeout in seconds.
    pub acquire_timeout: u64,
    pub log_enable: bool,
    /// Log level: `debug`, `info`, `warn`, `error`, `off`.
    pub log_level: String,
    /// Slow query log level.
    pub log_slow_level: String,
    /// Slow query threshold in seconds.
    pub log_threshold: u64,
    /// Collation (MySQL only).
    pub collation: String,
}

impl Default for DatabaseSection {
    fn default() -> Self {
        Self {
            driver: "postgresql".to_string(),
            socket: None,
            host: "localhost".to_string(),
            port: 5432,
            username: String::new(),
            password: String::new(),
            database: String::new(),
            max_connections: 50,
            min_connections: 1,
            max_lifetime: 1800,
            statement_timeout: 5,
            idle_timeout: 600,
            acquire_timeout: 10,
            log_enable: false,
            log_level: "warn".to_string(),
            log_slow_level: "error".to_string(),
            log_threshold: 1,
            collation: "utf8mb4_general_ci".to_string(),
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct LicenseSection {
    /// MaxMind license key used to download GeoLite databases.
    pub maxmind: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct GeoSection {
    /// Path to the GeoLite / MaxMind database file.
    pub database_path: String,
    /// Download URL for the database. Empty means the built-in MaxMind
    /// download URL; a custom URL may be configured here.
    pub download_url: String,
    /// Automatically update the database on a schedule.
    pub auto_update: bool,
    /// Update interval in hours.
    pub update_interval_hours: u64,
    /// When true, settings-defined IP range overrides win over the
    /// GeoLite database reading (default behaviour).
    pub settings_override: bool,
}

impl Default for GeoSection {
    fn default() -> Self {
        Self {
            database_path: "data/geo/GeoLite2-City.mmdb".to_string(),
            download_url: String::new(),
            auto_update: false,
            update_interval_hours: 168,
            settings_override: true,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct AcmeSection {
    /// Enable Let's Encrypt ACME certificate issuance and renewal.
    pub enabled: bool,
    /// ACME directory URL, for example the Let's Encrypt production URL.
    pub directory_url: String,
    /// Contact addresses for the ACME account, for example
    /// `mailto:admin@example.com`.
    pub contacts: Vec<String>,
}

impl Default for AcmeSection {
    fn default() -> Self {
        Self {
            enabled: false,
            directory_url: "https://acme-v02.api.letsencrypt.org/directory".to_string(),
            contacts: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct StoreSection {
    /// Secret key that signs expiring download links (HMAC-SHA256).
    /// When set, signed links survive restarts because the same key is
    /// used on every start. When empty, the store generates a random
    /// per-process secret and logs a warning; links signed before a
    /// restart then stop verifying. Use a long random string (at
    /// least 32 characters). Changing the value invalidates links
    /// signed with the previous value and requires a restart to take
    /// effect.
    pub download_signing_secret: String,
}

/// Public API settings: the global prefix and the versioning
/// defaults. Mirrors the runtime shapes in the API Manager module
/// (`ApiPrefixSettings` for the prefix, `ApiVersion` seed defaults
/// for the per-version settings: v1 and v2 enabled, v3 reserved and
/// disabled, 1000 requests per minute, authentication required).
/// The module keeps its own runtime copy; these values are the
/// file-backed defaults it seeds from.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ApiSection {
    /// Segment placed before the version in public URLs: `"v"`
    /// produces `/v1/...`, `/v2/...`. One or more segments of
    /// letters, digits, `-` or `_`, joined by `/`, with no leading or
    /// trailing slash and no whitespace (for example `"v"`, `"api"`,
    /// `"api/v2"`).
    pub prefix: String,
    /// When true, the deployment intends to also remount the internal
    /// `/api/v1/<module>` routes under the new prefix on the next
    /// startup. A flag only: the remount itself happens in the server
    /// factory, and an in-memory setting alone cannot move routes
    /// Actix already registered.
    pub migrate_internal: bool,
    /// Version served when a caller asks for no version explicitly
    /// (`v` followed by a positive number, for example `"v1"`).
    pub default_version: String,
    /// Default per-version rate limit, requests per minute. An
    /// endpoint without its own limit falls back to its version's
    /// limit, which defaults to this value.
    pub default_rate_limit_per_minute: u32,
    /// Whether callers must authenticate for a version by default.
    pub default_auth_required: bool,
}

impl Default for ApiSection {
    fn default() -> Self {
        Self {
            prefix: "v".to_string(),
            migrate_internal: false,
            default_version: "v1".to_string(),
            default_rate_limit_per_minute: 1000,
            default_auth_required: true,
        }
    }
}

/// Load balancer defaults. Every field has a default; balancers also
/// carry per-balancer overrides.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct LoadBalancerSection {
    /// Default health-check interval in seconds (5..=3600).
    pub health_check_interval_secs: u64,
    /// Default health-check timeout in seconds (1..=60).
    pub health_check_timeout_secs: u64,
    /// Default health-check path for HTTP checks.
    pub health_check_path: String,
    /// Default strategy: round_robin | least_connections |
    /// weighted | ip_hash.
    pub default_strategy: String,
    /// Maximum backends per balancer (1..=1000).
    pub max_backends_per_balancer: usize,
}

impl Default for LoadBalancerSection {
    fn default() -> Self {
        Self {
            health_check_interval_secs: 30,
            health_check_timeout_secs: 5,
            health_check_path: "/health".to_string(),
            default_strategy: "round_robin".to_string(),
            max_backends_per_balancer: 100,
        }
    }
}

/// Pingora gateway settings. This is the durable YAML section; the
/// gateway module keeps its own runtime copy (`GatewayConfig`) with
/// the same fields and defaults, and reads this section at startup.
/// The gateway only listens when `enabled` is true and the binary
/// was built with the `gateway` Cargo feature.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct GatewaySection {
    /// Master switch. The proxy listener only starts when this is
    /// true and the `gateway` feature is compiled in.
    pub enabled: bool,
    /// Plain-HTTP listen addresses, for example `0.0.0.0:8081`.
    pub listen: Vec<String>,
    /// TLS listen addresses, for example `0.0.0.0:8444`. Empty means
    /// no TLS listener is bound.
    pub tls_listen: Vec<String>,
    /// Default upstream used when no routing rule matches: a load
    /// balancer id (UUID) or a plain `host:port` address. `None`
    /// means unmatched requests are rejected.
    pub default_upstream: Option<String>,
    /// Request-log ring buffer capacity (1..=10000).
    pub request_log_capacity: usize,
    /// Health-check sweep interval in seconds (5..=3600).
    pub health_check_interval_secs: u64,
    /// Upstream connect timeout in seconds (1..=60).
    pub connect_timeout_secs: u64,
}

impl Default for GatewaySection {
    fn default() -> Self {
        Self {
            enabled: false,
            listen: vec!["127.0.0.1:8081".to_string()],
            tls_listen: vec!["127.0.0.1:8444".to_string()],
            default_upstream: None,
            request_log_capacity: 500,
            health_check_interval_secs: 30,
            connect_timeout_secs: 5,
        }
    }
}

impl GatewaySection {
    /// Validate the section. Returns a list of problems; an empty
    /// list means the section is valid. Mirrors
    /// `GatewayConfig::validate` in the gateway module, except the
    /// default-upstream check is done locally: the gateway validates
    /// it through the load balancer's host parser, which the config
    /// module must not depend on.
    pub fn validate(&self) -> Vec<String> {
        let mut problems = Vec::new();
        if self.listen.is_empty() && self.tls_listen.is_empty() {
            problems.push(
                "gateway.listen and gateway.tls_listen must not both be empty".to_string(),
            );
        }
        for address in self.listen.iter().chain(self.tls_listen.iter()) {
            if let Err(message) = validate_listen_address(address, "gateway") {
                problems.push(message);
            }
        }
        if !(1..=10_000).contains(&self.request_log_capacity) {
            problems.push(format!(
                "gateway.request_log_capacity must be between 1 and 10000, got {}",
                self.request_log_capacity
            ));
        }
        if !(5..=3600).contains(&self.health_check_interval_secs) {
            problems.push(format!(
                "gateway.health_check_interval_secs must be between 5 and 3600, got {}",
                self.health_check_interval_secs
            ));
        }
        if !(1..=60).contains(&self.connect_timeout_secs) {
            problems.push(format!(
                "gateway.connect_timeout_secs must be between 1 and 60, got {}",
                self.connect_timeout_secs
            ));
        }
        if let Some(upstream) = &self.default_upstream {
            let trimmed = upstream.trim();
            if trimmed.is_empty() {
                problems.push("gateway.default_upstream must not be empty".to_string());
            } else if !valid_gateway_upstream(trimmed) {
                problems.push(format!(
                    "gateway.default_upstream must be a load balancer UUID or a \"host:port\" address, got \"{trimmed}\""
                ));
            }
        }
        problems
    }
}

/// System-records retention: records older than this many days are
/// removed only by the explicit, confirmed retention purge.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct RecordsSection {
    /// Default retention in days (at least 1).
    pub retention_days: u32,
}

impl Default for RecordsSection {
    fn default() -> Self {
        Self { retention_days: 365 }
    }
}

impl AppConfig {
    /// Resolve the default config path: `$UNIFIED_CONFIG_FILE` when set,
    /// otherwise `config.yaml` in the working directory.
    pub fn default_path() -> PathBuf {
        if let Ok(path) = std::env::var(CONFIG_ENV) {
            if !path.trim().is_empty() {
                return PathBuf::from(path);
            }
        }
        PathBuf::from(DEFAULT_CONFIG_FILE)
    }

    /// Load a configuration file from disk.
    pub fn load(path: &Path) -> UnifiedResult<Self> {
        let text = std::fs::read_to_string(path).map_err(|error| {
            UnifiedError::new(
                format!("Cannot read config file {}: {error}", path.display()),
                500,
            )
        })?;
        let config: AppConfig = serde_yaml::from_str(&text)?;
        Ok(config)
    }

    /// Load the default configuration file.
    pub fn load_default() -> UnifiedResult<Self> {
        Self::load(&Self::default_path())
    }

    /// Validate the configuration. Returns a list of problems; an empty
    /// list means the configuration is valid. A reload must never apply a
    /// configuration that returns a non-empty list.
    pub fn validate(&self) -> Vec<String> {
        let mut problems = Vec::new();
        let app = &self.app;

        if app.mode != "production" && app.mode != "development" {
            problems.push(format!(
                "app.mode must be \"production\" or \"development\", got \"{}\"",
                app.mode
            ));
        }
        if app.dashboard_path.trim().is_empty()
            || !app
                .dashboard_path
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        {
            problems.push(
                "app.dashboard_path must be non-empty and contain only letters, digits, '-' or '_'"
                    .to_string(),
            );
        }
        if app.session_name.trim().is_empty()
            || !app
                .session_name
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        {
            problems.push(
                "app.session_name must be non-empty and contain only letters, digits, '-' or '_'"
                    .to_string(),
            );
        }
        if !(5..=86_400).contains(&app.csrf_duration) {
            problems.push(format!(
                "app.csrf_duration must be between 5 and 86400 minutes, got {}",
                app.csrf_duration
            ));
        }
        // Signing keys are mandatory in every mode, not only in
        // production: an unset, short, or placeholder key makes
        // token signing fail closed at issue time, so the
        // configuration is rejected here instead — startup and
        // reload refuse an invalid config outright. Development
        // gets no exception: a development server minting tokens
        // under an ephemeral or placeholder secret is a
        // token-forgery hole the moment its config is copied to
        // production. The checks use the trimmed values, matching
        // the Auth Manager's `signing_key`.
        let secret_key = app.secret_key.trim();
        let salt_key = app.salt_key.trim();
        if secret_key.is_empty() {
            problems.push("app.secret_key must be set".to_string());
        } else if secret_key.len() < 32 {
            problems.push("app.secret_key must be at least 32 characters".to_string());
        } else if secret_key == PLACEHOLDER_SECRET_KEY {
            problems.push(
                "app.secret_key still holds the placeholder value from config.example.yaml; generate a real random key"
                    .to_string(),
            );
        }
        if salt_key.is_empty() {
            problems.push("app.salt_key must be set".to_string());
        } else if salt_key.len() < 16 {
            problems.push("app.salt_key must be at least 16 characters".to_string());
        } else if salt_key == PLACEHOLDER_SALT_KEY {
            problems.push(
                "app.salt_key still holds the placeholder value from config.example.yaml; generate a real random key"
                    .to_string(),
            );
        }
        if !app.apikey_legacy_hash_sunset.trim().is_empty()
            && NaiveDate::parse_from_str(app.apikey_legacy_hash_sunset.trim(), "%Y-%m-%d").is_err()
        {
            problems.push(
                "app.apikey_legacy_hash_sunset must be a YYYY-MM-DD date or empty (empty means the built-in default)"
                    .to_string(),
            );
        }
        if app.secret_key == app.salt_key && !app.secret_key.is_empty() {
            problems.push("app.secret_key and app.salt_key must differ".to_string());
        }
        for address in &app.tcp {
            if let Err(message) = validate_listen_address(address, "app.tcp") {
                problems.push(message);
            }
        }
        if app.tcp.is_empty() {
            problems.push("app.tcp must contain at least one listen address".to_string());
        }
        for address in &app.ssl.listen {
            if let Err(message) = validate_listen_address(address, "app.ssl.listen") {
                problems.push(message);
            }
        }
        if !(2..=256).contains(&app.ssl.ssl_session_cache) {
            problems.push(format!(
                "app.ssl.ssl_session_cache must be between 2 and 256, got {}",
                app.ssl.ssl_session_cache
            ));
        }
        if (app.ssl.cert.is_empty()) != (app.ssl.key.is_empty()) {
            problems.push(
                "app.ssl.cert and app.ssl.key must be set together or both left empty".to_string(),
            );
        }
        for (domain, entry) in &app.ssl.domains {
            if (entry.cert.is_empty()) != (entry.key.is_empty()) {
                problems.push(format!(
                    "app.ssl.domains[{domain}]: cert and key must be set together or both left empty"
                ));
            }
        }
        if app.operation_timeout == 0 || app.operation_timeout > 3600 {
            problems.push(format!(
                "app.operation_timeout must be between 1 and 3600, got {}",
                app.operation_timeout
            ));
        }
        if app.request_timeout > 3600 {
            problems.push(format!(
                "app.request_timeout must be between 0 and 3600, got {}",
                app.request_timeout
            ));
        }
        if app.max_connections < 128 {
            problems.push(format!(
                "app.max_connections must be at least 128, got {}",
                app.max_connections
            ));
        }
        if app.backlog < 128 {
            problems.push(format!(
                "app.backlog must be at least 128, got {}",
                app.backlog
            ));
        }
        if app.rlimit_nofile < 4096 {
            problems.push(format!(
                "app.rlimit_nofile must be at least 4096, got {}",
                app.rlimit_nofile
            ));
        }

        let db = &self.database;
        match db.driver.as_str() {
            "postgresql" | "mysql" | "sqlite" => {}
            other => problems.push(format!(
                "database.driver must be \"postgresql\", \"mysql\" or \"sqlite\", got \"{other}\""
            )),
        }
        if db.driver == "postgresql" && db.port == 0 {
            problems.push("database.port must be set for PostgreSQL".to_string());
        }
        if db.max_connections == 0 {
            problems.push("database.max_connections must be at least 1".to_string());
        }
        if db.min_connections > db.max_connections {
            problems.push(
                "database.min_connections must not exceed database.max_connections".to_string(),
            );
        }
        if db.acquire_timeout == 0 {
            problems.push("database.acquire_timeout must be at least 1".to_string());
        }
        for level in [&db.log_level, &db.log_slow_level] {
            if !matches!(
                level.as_str(),
                "debug" | "info" | "warn" | "error" | "off"
            ) {
                problems.push(format!(
                    "database log level must be debug, info, warn, error or off, got \"{level}\""
                ));
            }
        }
        if self.cache.dir.trim().is_empty() {
            problems.push("cache.dir must not be empty".to_string());
        }
        if self.redis.enabled {
            if self.redis.port == 0 && self.redis.unix_socket.is_empty() {
                problems.push(
                    "redis.port must be set when no unix_socket is configured".to_string(),
                );
            }
            if self.redis.max_pools == 0 {
                problems.push("redis.max_pools must be at least 1".to_string());
            }
        }
        if self.geo.update_interval_hours == 0 {
            problems.push("geo.update_interval_hours must be at least 1".to_string());
        }
        if !self.store.download_signing_secret.is_empty()
            && self.store.download_signing_secret.len() < 32
        {
            problems.push(
                "store.download_signing_secret must be at least 32 characters when set"
                    .to_string(),
            );
        }
        if self.acme.enabled {
            if self.acme.directory_url.trim().is_empty()
                || !(self.acme.directory_url.starts_with("http://")
                    || self.acme.directory_url.starts_with("https://"))
            {
                problems.push(
                    "acme.directory_url must be a valid http(s) URL when acme.enabled is true"
                        .to_string(),
                );
            }
            if self.acme.contacts.is_empty() {
                problems.push(
                    "acme.contacts must contain at least one contact when acme.enabled is true"
                        .to_string(),
                );
            }
        }
        problems.extend(self.security.validate());
        if let Err(message) = validate_api_prefix(&self.api.prefix) {
            problems.push(format!("api.prefix: {message}"));
        }
        if !self
            .api
            .default_version
            .strip_prefix('v')
            .is_some_and(|number| !number.is_empty() && number.parse::<u32>().is_ok_and(|n| n > 0))
        {
            problems.push(format!(
                "api.default_version must look like \"v1\", \"v2\" or \"v10\" (\"v\" followed by a positive number), got \"{}\"",
                self.api.default_version
            ));
        }
        if self.api.default_rate_limit_per_minute == 0 {
            problems.push(
                "api.default_rate_limit_per_minute must be at least 1".to_string(),
            );
        }
        problems.extend(self.routes.validate());
        for target in &self.storage.targets {
            if let Err(error) = target.validate() {
                problems.push(format!(
                    "storage.targets[\"{}\"]: {}",
                    target.name, error.message
                ));
            }
        }
        if let Some(default_target) = &self.storage.default_target {
            if !default_target.trim().is_empty()
                && !self
                    .storage
                    .targets
                    .iter()
                    .any(|target| target.name == *default_target)
            {
                problems.push(format!(
                    "storage.default_target \"{default_target}\" does not match any configured storage target"
                ));
            }
        }
        if let Some(spool_dir) = &self.documents.spool_dir {
            if spool_dir.trim().is_empty() {
                problems.push("documents.spool_dir must not be empty".to_string());
            } else if spool_dir.chars().any(|c| c == '\0' || c.is_control()) {
                problems.push("documents.spool_dir contains invalid characters".to_string());
            }
        }
        if let Some(max_chunk_ram_bytes) = self.documents.max_chunk_ram_bytes {
            if !(DocumentsConfig::MAX_CHUNK_RAM_MIN..=DocumentsConfig::MAX_CHUNK_RAM_MAX)
                .contains(&max_chunk_ram_bytes)
            {
                problems.push(format!(
                    "documents.max_chunk_ram_bytes must be between {} and {}, got {}",
                    DocumentsConfig::MAX_CHUNK_RAM_MIN,
                    DocumentsConfig::MAX_CHUNK_RAM_MAX,
                    max_chunk_ram_bytes
                ));
            }
        }
        if let Some(extensions) = &self.documents.extensions {
            for (ext, kind) in extensions {
                let ext = ext.to_ascii_lowercase();
                if ext.is_empty()
                    || ext.len() > 10
                    || !ext
                        .bytes()
                        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
                {
                    problems.push(format!(
                        "documents.extensions key \"{ext}\" must match [a-z0-9]{{1,10}}"
                    ));
                }
                if !matches!(
                    kind.to_ascii_lowercase().as_str(),
                    "documents" | "media" | "others"
                ) {
                    problems.push(format!(
                        "documents.extensions[\"{ext}\"] must be documents, media, or others, got \"{kind}\""
                    ));
                }
            }
        }
        let lb = &self.load_balancer;
        if !(5..=3600).contains(&lb.health_check_interval_secs) {
            problems.push(format!(
                "load_balancer.health_check_interval_secs must be between 5 and 3600, got {}",
                lb.health_check_interval_secs
            ));
        }
        if !(1..=60).contains(&lb.health_check_timeout_secs) {
            problems.push(format!(
                "load_balancer.health_check_timeout_secs must be between 1 and 60, got {}",
                lb.health_check_timeout_secs
            ));
        }
        if !lb.health_check_path.starts_with('/') {
            problems.push(
                "load_balancer.health_check_path must start with '/'".to_string(),
            );
        }
        if !matches!(
            lb.default_strategy.as_str(),
            "round_robin" | "least_connections" | "weighted" | "ip_hash"
        ) {
            problems.push(format!(
                "load_balancer.default_strategy must be round_robin, least_connections, weighted or ip_hash, got \"{}\"",
                lb.default_strategy
            ));
        }
        if !(1..=1000).contains(&lb.max_backends_per_balancer) {
            problems.push(format!(
                "load_balancer.max_backends_per_balancer must be between 1 and 1000, got {}",
                lb.max_backends_per_balancer
            ));
        }
        problems.extend(self.gateway.validate());
        if self.records.retention_days == 0 {
            problems.push("records.retention_days must be at least 1".to_string());
        }

        problems
    }

    /// Load a configuration file and validate it. This is the shared
    /// implementation behind `unified -t` and `unified -t -f <path>`.
    /// Returns the list of validation problems (empty means valid).
    pub fn test(path: &Path) -> UnifiedResult<Vec<String>> {
        let config = Self::load(path)?;
        Ok(config.validate())
    }

    /// True when the application runs in development mode.
    pub fn is_debug(&self) -> bool {
        self.app.mode == "development"
    }

    /// Build a Diesel connection URL for the configured database.
    pub fn database_url(&self) -> UnifiedResult<String> {
        let db = &self.database;
        match db.driver.as_str() {
            "postgresql" => Ok(format!(
                "postgres://{}:{}@{}:{}/{}",
                db.username, db.password, db.host, db.port, db.database
            )),
            "mysql" => Ok(format!(
                "mysql://{}:{}@{}:{}/{}",
                db.username, db.password, db.host, db.port, db.database
            )),
            "sqlite" => Ok(db.database.clone()),
            other => Err(UnifiedError::bad_request(format!(
                "Unsupported database driver: {other}"
            ))),
        }
    }
}

/// Validate the global public API prefix: one or more segments of
/// letters, digits, `-` or `_`, joined by `/`, with no leading or
/// trailing slash and no whitespace. Examples: `"v"`, `"api"`,
/// `"api/v2"`. An empty prefix is rejected: it would produce
/// ambiguous `//v1/...` URLs. Mirrors the API Manager module's
/// `validate_prefix` (private there, so the check is repeated here).
fn validate_api_prefix(prefix: &str) -> Result<(), String> {
    let prefix = prefix.trim();
    if prefix.is_empty() {
        return Err("prefix must not be empty".to_string());
    }
    if prefix.starts_with('/') || prefix.ends_with('/') {
        return Err("prefix must not start or end with '/'".to_string());
    }
    if prefix.contains(char::is_whitespace) {
        return Err("prefix must not contain whitespace".to_string());
    }
    if prefix.chars().count() > 100 {
        return Err("prefix must be at most 100 characters".to_string());
    }
    for segment in prefix.split('/') {
        if segment.is_empty()
            || !segment
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        {
            return Err(format!(
                "prefix segment \"{segment}\" is invalid: use only letters, digits, '-' or '_'"
            ));
        }
    }
    Ok(())
}

/// True when a gateway default upstream is a load balancer UUID or a
/// `host:port` address.
fn valid_gateway_upstream(value: &str) -> bool {
    if Uuid::parse_str(value).is_ok() {
        return true;
    }
    let Some((host, port)) = value.rsplit_once(':') else {
        return false;
    };
    !host.trim().is_empty()
        && !host.contains(char::is_whitespace)
        && !host.contains('/')
        && port.parse::<u16>().is_ok_and(|p| p != 0)
}

/// Validate one listen address (`host:port`). Only localhost names and IP
/// addresses are accepted as hosts.
fn validate_listen_address(address: &str, field: &str) -> Result<(), String> {
    match address.parse::<SocketAddr>() {
        Ok(addr) => {
            if addr.port() == 0 {
                return Err(format!("{field}: port must not be 0 in \"{address}\""));
            }
            Ok(())
        }
        Err(_) => {
            // Accept `localhost:port` as well.
            if let Some((host, port)) = address.rsplit_once(':') {
                if host == "localhost" && port.parse::<u16>().is_ok_and(|p| p != 0) {
                    return Ok(());
                }
            }
            Err(format!(
                "{field}: \"{address}\" is not a valid listen address (expected host:port)"
            ))
        }
    }
}
