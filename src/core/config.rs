// YAML configuration.
//
// The configuration file is loaded at startup and on reload. Before a reload
// applies a new configuration, `validate()` must run and report zero errors
// — a failing configuration never replaces the running one. The CLI exposes
// the same check as `unified -t` (default config) and
// `unified -t -f /path/to/config.yaml`.

use crate::core::error::{UnifiedError, UnifiedResult};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};

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
        }
    }
}

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
}

impl Default for CacheSection {
    fn default() -> Self {
        Self {
            dir: "data/cache".to_string(),
            default_ttl_secs: 3600,
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
        if app.mode == "production" && app.secret_key.len() < 32 {
            problems.push(
                "app.secret_key must be at least 32 characters in production mode".to_string(),
            );
        }
        if app.mode == "production" && app.salt_key.len() < 16 {
            problems.push(
                "app.salt_key must be at least 16 characters in production mode".to_string(),
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
