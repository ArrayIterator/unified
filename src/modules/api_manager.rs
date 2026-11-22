// API Manager module.
//
// The public / client API surface, separate from the core API: a registry
// of client-facing endpoints with payload validation rules, a version
// overview, a simple per-endpoint per-minute rate limiter, and API key
// management. The key endpoints under `/api/v1/api-manager/keys` are a
// thin adapter over the API Keys module (`crate::modules::apikeys`),
// which is the single source of truth for key storage, hashing, and
// verification; this module keeps no key store of its own.
//
// Versioned public API (see the "Version registry" section below):
//
//   - `ApiVersion` records describe the public API versions (v1, v2,
//     v3, ...). Each version has its own settings: enabled/disabled,
//     deprecated flag plus sunset date, per-version rate limit, auth
//     requirement, and notes. A version can be created blank or by
//     copying the settings of an existing version.
//   - The global public API prefix (default `"v"`, producing `/v1/...`,
//     `/v2/...`) is a setting on this module's state. It governs the
//     PUBLIC versioned surface only. The internal module routes stay
//     mounted at `/api/v1/<module>` by default; changing the prefix
//     does not move them. The prefix setting carries an explicit
//     `migrate_internal` flag: only when an admin enables it does the
//     deployment intend to remount the internal routes under the new
//     prefix, which requires the server factory to mount them there
//     (see the remount snippet in the coordinator report -- the server
//     factory is not edited by this module, and an in-memory setting
//     alone cannot remount routes that Actix already registered).
//   - Every endpoint declares version support: all versions, a
//     minimum version, a maximum version, a range, or an explicit
//     list of versions. `resolve_endpoint` answers, for a given
//     method + path + version, which endpoint matches or precisely
//     why it does not (unknown version, disabled version, endpoint
//     not in that version with `available_in` metadata).
//   - Built-in endpoints are collected at startup from every
//     module's `route_info()` data via the module registry catalog.
//     A module declares which versions expose its routes through
//     `Module::route_version_support` (default: all versions); the
//     collector reads that declaration per owning module, so a
//     built-in route registers with real version support instead of
//     everything defaulting to `All`. The API Manager is the single
//     source of truth for what the public API exposes per version.
//   - Custom endpoints are checked against the built-in registry at
//     creation time: a method + path collision in any chosen version
//     is rejected with 422 and emits `api.endpoint.conflict`.
//
// Note on execution: `POST /endpoints/{id}/execute` validates the payload
// against the endpoint rules and returns the validation result plus an
// echo. Dispatching the request into a registered core route handler is
// out of scope for this milestone.
//
// Access control (default deny): public access is governed by
// `ApiAccessRule` rows (per endpoint template and version, with a
// per-endpoint public-access flag) plus the API Keys module's
// per-key scopes and per-minute limits. Nothing on the public
// surface is reachable without an explicit allow rule: a request to
// a public or key-presented internal route that matches no allow
// rule is rejected with 403 before any handler runs. Enforcement
// lives in `PublicApiAccessMiddleware` (registered through the
// module middleware chain, priority 850) and, for the public
// gateway, inside `resolve_public` itself; the two never overlap
// (the gateway's own `/api/v1/api-manager/` prefix is exempt from
// the middleware), so a request is checked exactly once. Every
// access-control change is written to an `ApiAccessAuditEntry` and,
// when a database pool is attached, persisted to the migration
// 000004 tables (`api_access_rules`, `api_access_audit`); the same
// tables are loaded back into memory at startup, so rules and the
// audit trail survive restarts. Key lifecycle changes
// (`apikey.created` / `revoked` / `rotated`, emitted by the API Keys
// module) are audited here too, through hook listeners registered
// in `register_hooks`.

use crate::core::config::{ApiSection, AppConfig};
use crate::core::context::AppContext;
use crate::core::error::{UnifiedError, UnifiedResult};
use crate::core::hardening::client_ip;
use crate::core::hooks::{HookBus, HookContext};
use crate::core::json as api_json;
use crate::core::middleware::{Middleware, MiddlewareResult, NextFn};
use crate::core::modules::{Module, ModuleRegistry, RouteInfo, RouteVersionSupport};
use crate::core::pagination::{PageResult, PaginationParams};
use crate::db::DbPool;
use crate::modules::apikeys::apikey_events;
use actix_web::dev::ServiceRequest;
use actix_web::{web, HttpRequest, HttpResponse};
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use diesel::prelude::*;
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, RwLock};
use std::time::{SystemTime, UNIX_EPOCH};
use uuid::Uuid;

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EndpointRule {
    pub field: String,
    pub required: bool,
    #[serde(rename = "type")]
    pub field_type: String,
    pub min_length: Option<usize>,
    pub max_length: Option<usize>,
    pub min_value: Option<f64>,
    pub max_value: Option<f64>,
    pub pattern: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApiEndpoint {
    pub id: Uuid,
    pub method: String,
    pub path: String,
    /// Lowest version in `version_support`, kept for backward
    /// compatibility with clients that read a single version.
    pub version: String,
    pub description: String,
    pub rate_limit: u32,
    pub enabled: bool,
    pub rules: Vec<EndpointRule>,
    pub execution_count: u64,
    /// Id of the module that owns this endpoint. Custom endpoints
    /// created through this module use `"custom"`.
    #[serde(default = "default_owner_module")]
    pub owner_module: String,
    /// Whether callers must authenticate (API key) for this endpoint.
    #[serde(default = "default_true")]
    pub auth_required: bool,
    /// `"custom"` for admin-created endpoints, `"builtin"` for
    /// endpoints imported from the module route catalog.
    #[serde(default = "default_endpoint_source")]
    pub source: String,
    /// Which API versions expose this endpoint.
    #[serde(default)]
    pub version_support: VersionSupport,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

fn default_owner_module() -> String {
    "custom".to_string()
}

fn default_true() -> bool {
    true
}

fn default_endpoint_source() -> String {
    "custom".to_string()
}

#[derive(Debug, Deserialize)]
pub struct CreateEndpointInput {
    pub method: String,
    pub path: String,
    /// Legacy single version (for example `"v1"`). Optional now:
    /// prefer `versions` or `version_support`. When only this field
    /// is given it becomes a one-version list.
    pub version: Option<String>,
    /// Explicit multi-select of versions, for example `["v1", "v2"]`.
    /// Each entry must be an existing registry version id.
    pub versions: Option<Vec<String>>,
    /// Structured version support. Takes precedence over `versions`
    /// and `version`, which are ignored when this is present.
    pub version_support: Option<VersionSupport>,
    pub description: Option<String>,
    pub rate_limit: Option<u32>,
    pub enabled: Option<bool>,
    pub rules: Option<Vec<EndpointRule>>,
    /// Owner module label for the new endpoint. Defaults to
    /// `"custom"`; built-in owners are never accepted here because
    /// built-in endpoints come from the route catalog, not this form.
    pub owner_module: Option<String>,
    pub auth_required: Option<bool>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateEndpointInput {
    pub method: Option<String>,
    pub path: Option<String>,
    pub version: Option<String>,
    pub versions: Option<Vec<String>>,
    pub version_support: Option<VersionSupport>,
    pub description: Option<String>,
    pub rate_limit: Option<u32>,
    pub enabled: Option<bool>,
    pub rules: Option<Vec<EndpointRule>>,
    pub owner_module: Option<String>,
    pub auth_required: Option<bool>,
}

#[derive(Debug, Deserialize)]
pub struct CreateKeyInput {
    pub name: String,
    pub scopes: Option<Vec<String>>,
    pub expires_in_days: Option<u32>,
}

// ---------------------------------------------------------------------------
// Version registry types
// ---------------------------------------------------------------------------

/// How an endpoint declares the API versions it is available in.
///
/// Precedence when resolving a request: an explicit list beats a
/// range, a range beats a bare minimum/maximum, and `All` matches
/// every registered version. A `Min`/`Max`/`Range` bound is
/// inclusive. `List` entries are kept sorted by version number.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum VersionSupport {
    All,
    Min { min_version: String },
    Max { max_version: String },
    Range { min_version: String, max_version: String },
    List { versions: Vec<String> },
}

impl Default for VersionSupport {
    fn default() -> Self {
        VersionSupport::All
    }
}

impl VersionSupport {
    /// True when `version` is covered by this support declaration.
    /// Unknown version ids never match, not even under `All`.
    pub fn supports(&self, version: &str, known_versions: &[String]) -> bool {
        let Some(number) = parse_version_number(version) else {
            return false;
        };
        if !known_versions.iter().any(|known| known == version) {
            return false;
        }
        match self {
            VersionSupport::All => true,
            VersionSupport::Min { min_version } => {
                parse_version_number(min_version).is_some_and(|min| number >= min)
            }
            VersionSupport::Max { max_version } => {
                parse_version_number(max_version).is_some_and(|max| number <= max)
            }
            VersionSupport::Range { min_version, max_version } => {
                let min_ok = parse_version_number(min_version).is_some_and(|min| number >= min);
                let max_ok = parse_version_number(max_version).is_some_and(|max| number <= max);
                min_ok && max_ok
            }
            VersionSupport::List { versions } => versions.iter().any(|entry| entry == version),
        }
    }

    /// Every registered version this support covers, sorted by
    /// version number. Used for `available_in` metadata.
    pub fn available_in(&self, known_versions: &[String]) -> Vec<String> {
        let mut covered: Vec<String> = known_versions
            .iter()
            .filter(|version| self.supports(version, known_versions))
            .cloned()
            .collect();
        sort_version_ids(&mut covered);
        covered
    }

    /// The lowest covered version, if any.
    pub fn lowest_version(&self, known_versions: &[String]) -> Option<String> {
        self.available_in(known_versions).into_iter().next()
    }
}

impl From<&RouteVersionSupport> for VersionSupport {
    /// Convert a module's declared route version support into the
    /// registry's `VersionSupport`. `All` stays `All`; the bounded
    /// variants map one to one, with `&'static str` bounds and
    /// version lists copied into owned strings.
    fn from(support: &RouteVersionSupport) -> Self {
        match support {
            RouteVersionSupport::All => VersionSupport::All,
            RouteVersionSupport::Min { min_version } => VersionSupport::Min {
                min_version: min_version.to_string(),
            },
            RouteVersionSupport::Max { max_version } => VersionSupport::Max {
                max_version: max_version.to_string(),
            },
            RouteVersionSupport::Range {
                min_version,
                max_version,
            } => VersionSupport::Range {
                min_version: min_version.to_string(),
                max_version: max_version.to_string(),
            },
            RouteVersionSupport::List { versions } => VersionSupport::List {
                versions: versions.iter().map(|version| version.to_string()).collect(),
            },
        }
    }
}

/// One public API version with its own settings.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApiVersion {
    /// Registry id, `v` followed by a positive number (`v1`, `v10`).
    pub id: String,
    /// Numeric part of the id, for ordering.
    pub number: u32,
    /// Disabled versions resolve to 404 with a clear message.
    pub enabled: bool,
    /// Deprecated versions still resolve, but callers are warned
    /// and the sunset date tells them when the version goes away.
    pub deprecated: bool,
    pub sunset_date: Option<DateTime<Utc>>,
    /// Per-version rate limit, requests per minute. An endpoint
    /// without its own limit falls back to this value.
    pub rate_limit_per_minute: u32,
    /// Whether callers must authenticate for this version by default.
    pub auth_required: bool,
    pub notes: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Deserialize)]
pub struct CreateApiVersionInput {
    /// Version id (`v1`, `v2`, ...). `version` is accepted as an
    /// alias; at least one of the two is required.
    pub id: Option<String>,
    pub version: Option<String>,
    /// Copy the settings of this existing version as a starting
    /// point. When omitted the version starts blank (defaults).
    pub copy_from: Option<String>,
    pub enabled: Option<bool>,
    pub deprecated: Option<bool>,
    pub sunset_date: Option<DateTime<Utc>>,
    pub rate_limit_per_minute: Option<u32>,
    pub auth_required: Option<bool>,
    pub notes: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateApiVersionInput {
    pub enabled: Option<bool>,
    pub deprecated: Option<bool>,
    pub sunset_date: Option<DateTime<Utc>>,
    pub rate_limit_per_minute: Option<u32>,
    pub auth_required: Option<bool>,
    pub notes: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct DeprecateVersionInput {
    /// Required: the date after which the version is removed.
    /// Must be in the future.
    pub sunset_date: DateTime<Utc>,
    pub notes: Option<String>,
}

/// Global public API prefix setting.
///
/// `prefix` is the segment placed before the version in public
/// URLs: `"v"` produces `/v1/...`, `"api/v2"` produces
/// `/api/v2/v1/...` only when combined with a version id -- in
/// practice an admin sets either a bare segment such as `"v"` or a
/// full leading path such as `"api"`. `migrate_internal` records
/// the admin's explicit choice to also remount the internal
/// `/api/v1/<module>` routes under the new prefix; it is a flag
/// only -- the remount itself happens in the server factory on the
/// next startup (see the module header comment).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApiPrefixSettings {
    pub prefix: String,
    pub migrate_internal: bool,
    pub updated_at: DateTime<Utc>,
}

impl Default for ApiPrefixSettings {
    fn default() -> Self {
        Self {
            prefix: "v".to_string(),
            migrate_internal: false,
            updated_at: Utc::now(),
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct UpdatePrefixInput {
    pub prefix: String,
    pub migrate_internal: Option<bool>,
}

/// One built-in endpoint collected from a module's `route_info()`.
///
/// The owning module declares its version support through
/// `Module::route_version_support` (read by `collect_from_catalog`
/// per owner); a module that declares nothing registers with
/// `VersionSupport::All`. `rate_limit` is `None` here, which means
/// "fall back to the owning version's rate limit".
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegisteredEndpoint {
    /// Stable key: `"<METHOD> <path template>"`.
    pub id: String,
    pub method: String,
    /// Path template as declared, for example `/api/v1/vcs/providers/{provider}`.
    pub path: String,
    pub owner_module: String,
    pub description: String,
    pub auth_required: bool,
    pub rate_limit: Option<u32>,
    pub source: String,
    pub version_support: VersionSupport,
    pub registered_at: DateTime<Utc>,
}

/// One row of the per-version endpoint registry: a single endpoint
/// as exposed in a single version.
#[derive(Debug, Clone, Serialize)]
pub struct RegistryRow {
    /// `"<METHOD> <path template> @ <version>"`.
    pub id: String,
    pub method: String,
    pub path: String,
    /// Public URL for this version under the current prefix.
    pub public_path: String,
    pub version: String,
    pub version_enabled: bool,
    pub version_deprecated: bool,
    pub owner_module: String,
    pub description: String,
    pub auth_required: bool,
    /// Effective rate limit: the endpoint's own limit, or the
    /// owning version's per-version limit when the endpoint has none.
    pub rate_limit: u32,
    pub source: String,
    pub version_support: VersionSupport,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub custom_endpoint_id: Option<Uuid>,
}

/// Query for resolving or inspecting one endpoint.
#[derive(Debug, Deserialize)]
pub struct ResolveQuery {
    pub method: Option<String>,
    pub version: Option<String>,
    pub path: Option<String>,
}

/// The version-choice shape the admin UI creation form consumes.
#[derive(Debug, Clone, Serialize)]
pub struct VersionChoice {
    pub id: String,
    pub label: String,
    pub enabled: bool,
    pub deprecated: bool,
    pub sunset_date: Option<DateTime<Utc>>,
}

// ---------------------------------------------------------------------------
// State
// ---------------------------------------------------------------------------

#[derive(Clone)]
pub struct ApiManagerState {
    pub endpoints: Arc<DashMap<Uuid, ApiEndpoint>>,
    /// Per-endpoint per-minute execution counters: key is
    /// `<endpoint-id>:<unix-minute>`, value is the request count.
    pub rate_counters: Arc<DashMap<String, u64>>,
    /// Version registry, keyed by version id (`v1`, `v2`, ...).
    pub versions: Arc<DashMap<String, ApiVersion>>,
    /// Global public API prefix setting.
    pub prefix: Arc<RwLock<ApiPrefixSettings>>,
    /// Built-in endpoints collected from `route_info()`, keyed by
    /// `"<METHOD> <path template>"`.
    pub registered: Arc<DashMap<String, RegisteredEndpoint>>,
    /// True once the route catalog has been collected into
    /// `registered` (set by `init`, or lazily by the first registry
    /// read when `init` did not run).
    pub registry_collected: Arc<AtomicBool>,
    /// Access-control rules, keyed by `"<METHOD> <path template>@<version|*>"`.
    /// See `ApiAccessRule`.
    pub access_rules: Arc<DashMap<String, ApiAccessRule>>,
    /// Audit trail of access-control changes (bounded; see
    /// `record_access_audit`). Newest entries are at the end.
    pub access_audit: Arc<RwLock<Vec<ApiAccessAuditEntry>>>,
    /// Database pool attached by `init` (if any), so access rules
    /// and audit entries can be persisted to the migration 000004
    /// tables (`api_access_rules`, `api_access_audit`). `None`
    /// means no pool was attached: the in-memory state stays
    /// authoritative and every persistence call is a no-op.
    pub db: Arc<RwLock<Option<DbPool>>>,
}

impl Default for ApiManagerState {
    fn default() -> Self {
        Self {
            endpoints: Arc::new(DashMap::new()),
            rate_counters: Arc::new(DashMap::new()),
            versions: Arc::new(DashMap::new()),
            prefix: Arc::new(RwLock::new(ApiPrefixSettings::default())),
            registered: Arc::new(DashMap::new()),
            registry_collected: Arc::new(AtomicBool::new(false)),
            access_rules: Arc::new(DashMap::new()),
            access_audit: Arc::new(RwLock::new(Vec::new())),
            db: Arc::new(RwLock::new(None)),
        }
    }
}

impl ApiManagerState {
    /// All registered version ids, sorted by version number.
    pub fn version_ids(&self) -> Vec<String> {
        let mut ids: Vec<String> = self.versions.iter().map(|entry| entry.key().clone()).collect();
        sort_version_ids(&mut ids);
        ids
    }

    /// Seed the default versions when the registry is still empty.
    /// Idempotent: a non-empty registry is left untouched, so admin
    /// edits survive restarts of the collection step. Uses the
    /// `ApiSection` defaults (prefix `"v"`, default version `"v1"`,
    /// 1000 requests per minute, auth required); `init` seeds from
    /// the real config through `apply_config` instead.
    pub fn ensure_seeded_versions(&self) {
        self.ensure_seeded_versions_from_config(&ApiSection::default());
    }

    /// Seed the default versions from an `ApiSection`: v1 and v2
    /// enabled, v3 reserved and disabled unless it is the configured
    /// default version, every version using the configured default
    /// rate limit and auth requirement. Idempotent: a non-empty
    /// registry is left untouched.
    pub fn ensure_seeded_versions_from_config(&self, api: &ApiSection) {
        if !self.versions.is_empty() {
            return;
        }
        let rate_limit_per_minute = if api.default_rate_limit_per_minute == 0 {
            1000
        } else {
            api.default_rate_limit_per_minute
        };
        let now = Utc::now();
        for (id, number, notes) in [
            (
                "v1",
                1u32,
                "Initial public API version. Built-in endpoints are available in all versions unless a version rule restricts them.",
            ),
            (
                "v2",
                2u32,
                "Second public API version. Built-in endpoints are available in all versions unless a version rule restricts them.",
            ),
            (
                "v3",
                3u32,
                "Reserved for the next API generation. Enable it when its endpoints are ready.",
            ),
        ] {
            // v1 and v2 are enabled by default; v3 only when the
            // config names it as the default version.
            let enabled = number <= 2 || api.default_version == id;
            self.versions.insert(
                id.to_string(),
                ApiVersion {
                    id: id.to_string(),
                    number,
                    enabled,
                    deprecated: false,
                    sunset_date: None,
                    rate_limit_per_minute,
                    auth_required: api.default_auth_required,
                    notes: notes.to_string(),
                    created_at: now,
                    updated_at: now,
                },
            );
        }
    }

    /// Seed the prefix and version defaults from `AppConfig.api`
    /// (the file-backed config section): prefix (with its
    /// `migrate_internal` flag), default version, per-version rate
    /// limit and auth requirement. Runs once at startup from `init`;
    /// the runtime copies stay authoritative afterwards, so admin
    /// `PUT /prefix` and version edits are not overwritten until
    /// the next restart. An invalid configured prefix is ignored
    /// (the current prefix is kept) rather than failing startup.
    pub fn apply_config(&self, config: &AppConfig) {
        if let Ok(prefix) = validate_prefix(&config.api.prefix) {
            if let Ok(mut settings) = self.prefix.write() {
                settings.prefix = prefix;
                settings.migrate_internal = config.api.migrate_internal;
                settings.updated_at = Utc::now();
            }
        }
        self.ensure_seeded_versions_from_config(&config.api);
    }

    /// Attach the database pool (if any) so rule and audit writes
    /// can persist. Called once from `init`.
    pub fn set_db_pool(&self, pool: Option<DbPool>) {
        if let Ok(mut guard) = self.db.write() {
            *guard = pool;
        }
    }

    /// The attached database pool, if `init` attached one.
    fn db_pool(&self) -> Option<DbPool> {
        self.db.read().ok().and_then(|guard| guard.clone())
    }

    /// Collect built-in endpoints from the module route catalog.
    /// Each route registers with the version support its owning
    /// module declares through `Module::route_version_support`
    /// (looked up in `registry` by owner id); routes whose owner
    /// declares nothing register with `VersionSupport::All`. Routes
    /// already collected are left untouched.
    pub fn collect_from_catalog(&self, catalog: &[RouteInfo], registry: &ModuleRegistry) {
        let now = Utc::now();
        for info in catalog {
            let key = format!("{} {}", info.method.to_uppercase(), info.path);
            let version_support =
                VersionSupport::from(&registry.route_version_support_of(info.owner));
            self.registered.entry(key.clone()).or_insert_with(|| RegisteredEndpoint {
                id: key,
                method: info.method.to_uppercase(),
                path: info.path.to_string(),
                owner_module: info.owner.to_string(),
                description: info.description.to_string(),
                auth_required: true,
                rate_limit: None,
                source: "builtin".to_string(),
                version_support,
                registered_at: now,
            });
        }
        self.registry_collected.store(true, Ordering::SeqCst);
    }

    /// Collect from the catalog only when `init` has not done so
    /// yet. Handlers call this so the registry works even when the
    /// module was constructed without running `init`.
    pub fn ensure_collected(&self, catalog: &[RouteInfo], registry: &ModuleRegistry) {
        if !self.registry_collected.load(Ordering::SeqCst) {
            self.collect_from_catalog(catalog, registry);
        }
    }

    /// Public URL of a version-relative path under the current
    /// prefix: prefix `"v"` and version `"v1"` turn
    /// `"/vcs/providers"` into `"/v1/vcs/providers"`.
    pub fn public_url(&self, version: &str, relative_path: &str) -> String {
        let prefix = self
            .prefix
            .read()
            .map(|settings| settings.prefix.clone())
            .unwrap_or_else(|_| "v".to_string());
        let relative = relative_path.strip_prefix('/').unwrap_or(relative_path);
        format!("/{prefix}/{version}/{relative}")
    }

    /// Turn a stored full path (`/api/v1/vcs/providers`) into the
    /// version-relative path (`/vcs/providers`) used under the
    /// public prefix. Paths outside `/api/v1/` are returned as-is.
    pub fn relative_path(full_path: &str) -> String {
        full_path
            .strip_prefix("/api/v1")
            .map(str::to_string)
            .unwrap_or_else(|| full_path.to_string())
    }

    /// Find a built-in endpoint whose method and path template
    /// collide with the requested method + path in any version the
    /// requested support covers. Returns the conflicting endpoint
    /// and the overlapping versions.
    pub fn builtin_conflict(
        &self,
        method: &str,
        path: &str,
        support: &VersionSupport,
    ) -> Option<(RegisteredEndpoint, Vec<String>)> {
        let known = self.version_ids();
        let template = normalize_path_template(path);
        for entry in self.registered.iter() {
            let candidate = entry.value();
            if candidate.method != method {
                continue;
            }
            if normalize_path_template(&candidate.path) != template {
                continue;
            }
            let overlapping: Vec<String> = support
                .available_in(&known)
                .into_iter()
                .filter(|version| candidate.version_support.supports(version, &known))
                .collect();
            if !overlapping.is_empty() {
                return Some((candidate.clone(), overlapping));
            }
        }
        None
    }

    /// Find a custom endpoint whose method and path template
    /// collide with the requested method + path in any version the
    /// requested support covers, excluding `exclude_id`.
    pub fn custom_conflict(
        &self,
        method: &str,
        path: &str,
        support: &VersionSupport,
        exclude_id: Option<Uuid>,
    ) -> Option<(ApiEndpoint, Vec<String>)> {
        let known = self.version_ids();
        let template = normalize_path_template(path);
        for entry in self.endpoints.iter() {
            let candidate = entry.value();
            if Some(candidate.id) == exclude_id {
                continue;
            }
            if candidate.method != method {
                continue;
            }
            if normalize_path_template(&candidate.path) != template {
                continue;
            }
            let overlapping: Vec<String> = support
                .available_in(&known)
                .into_iter()
                .filter(|version| candidate.version_support.supports(version, &known))
                .collect();
            if !overlapping.is_empty() {
                return Some((candidate.clone(), overlapping));
            }
        }
        None
    }

    /// Expand every registered and custom endpoint into one
    /// `RegistryRow` per version it covers.
    pub fn registry_rows(&self) -> Vec<RegistryRow> {
        let known = self.version_ids();
        let mut rows: Vec<RegistryRow> = Vec::new();
        for entry in self.registered.iter() {
            let endpoint = entry.value();
            for version_id in endpoint.version_support.available_in(&known) {
                let Some(version) = self.versions.get(&version_id) else {
                    continue;
                };
                rows.push(RegistryRow {
                    id: format!("{} {} @ {}", endpoint.method, endpoint.path, version_id),
                    method: endpoint.method.clone(),
                    path: endpoint.path.clone(),
                    public_path: self
                        .public_url(&version_id, &Self::relative_path(&endpoint.path)),
                    version: version_id,
                    version_enabled: version.enabled,
                    version_deprecated: version.deprecated,
                    owner_module: endpoint.owner_module.clone(),
                    description: endpoint.description.clone(),
                    auth_required: endpoint.auth_required,
                    rate_limit: endpoint
                        .rate_limit
                        .unwrap_or(version.rate_limit_per_minute),
                    source: endpoint.source.clone(),
                    version_support: endpoint.version_support.clone(),
                    custom_endpoint_id: None,
                });
            }
        }
        for entry in self.endpoints.iter() {
            let endpoint = entry.value();
            for version_id in endpoint.version_support.available_in(&known) {
                let Some(version) = self.versions.get(&version_id) else {
                    continue;
                };
                rows.push(RegistryRow {
                    id: format!("{} {} @ {}", endpoint.method, endpoint.path, version_id),
                    method: endpoint.method.clone(),
                    path: endpoint.path.clone(),
                    public_path: self
                        .public_url(&version_id, &Self::relative_path(&endpoint.path)),
                    version: version_id,
                    version_enabled: version.enabled,
                    version_deprecated: version.deprecated,
                    owner_module: endpoint.owner_module.clone(),
                    description: endpoint.description.clone(),
                    auth_required: endpoint.auth_required,
                    rate_limit: endpoint.rate_limit,
                    source: endpoint.source.clone(),
                    version_support: endpoint.version_support.clone(),
                    custom_endpoint_id: Some(endpoint.id),
                });
            }
        }
        rows.sort_by(|a, b| {
            parse_version_number(&a.version)
                .unwrap_or(0)
                .cmp(&parse_version_number(&b.version).unwrap_or(0))
                .then(a.path.cmp(&b.path))
                .then(a.method.cmp(&b.method))
        });
        rows
    }

    /// Resolve a method + version + version-relative path to the
    /// matching endpoint, or a precise miss reason. `path` must
    /// already be version-relative (see `strip_public_prefix`):
    /// for example `"/vcs/providers"` for version `v1`.
    pub fn resolve_endpoint(
        &self,
        method: &str,
        version: &str,
        path: &str,
    ) -> Result<ResolvedEndpoint, UnifiedError> {
        let known = self.version_ids();
        let version_record = self.versions.get(version).map(|entry| entry.value().clone());
        let Some(version_record) = version_record else {
            return Err(UnifiedError::not_found(format!(
                "API version \"{version}\" does not exist"
            ))
            .with_metadata("reason", json!("unknown_version"))
            .with_metadata("version", json!(version))
            .with_metadata("available_versions", json!(known)));
        };
        if !version_record.enabled {
            return Err(UnifiedError::not_found(format!(
                "API version \"{version}\" is disabled"
            ))
            .with_metadata("reason", json!("version_disabled"))
            .with_metadata("version", json!(version))
            .with_metadata("enabled", json!(false)));
        }
        // Every candidate whose path template matches, regardless of
        // method or version support, so the miss reason can be exact.
        let mut path_matches: Vec<RegistryRow> = self
            .registry_rows()
            .into_iter()
            .filter(|row| path_template_matches(&row.path, path))
            .collect();
        // Registry rows store full internal paths; a request path is
        // version-relative, so also match against the relative form.
        if path_matches.is_empty() {
            let relative = Self::relative_path(path);
            path_matches = self
                .registry_rows()
                .into_iter()
                .filter(|row| {
                    path_template_matches(&row.path, &relative)
                        || path_template_matches(&Self::relative_path(&row.path), &relative)
                        || path_template_matches(&Self::relative_path(&row.path), path)
                })
                .collect();
        }
        if path_matches.is_empty() {
            return Err(UnifiedError::not_found(format!(
                "No endpoint matches {method} {path} in API version \"{version}\""
            ))
            .with_metadata("reason", json!("no_such_path"))
            .with_metadata("version", json!(version))
            .with_metadata("method", json!(method))
            .with_metadata("path", json!(path)));
        }
        let method_matches: Vec<&RegistryRow> = path_matches
            .iter()
            .filter(|row| row.method == method || row.method == "*")
            .collect();
        if method_matches.is_empty() {
            let mut available_methods: Vec<String> = path_matches
                .iter()
                .map(|row| row.method.clone())
                .collect();
            available_methods.sort();
            available_methods.dedup();
            return Err(UnifiedError::not_found(format!(
                "No endpoint matches {method} {path} in API version \"{version}\""
            ))
            .with_metadata("reason", json!("method_not_allowed"))
            .with_metadata("version", json!(version))
            .with_metadata("method", json!(method))
            .with_metadata("path", json!(path))
            .with_metadata("available_methods", json!(available_methods)));
        }
        let in_version: Vec<&&RegistryRow> = method_matches
            .iter()
            .filter(|row| row.version == version)
            .collect();
        let Some(matched) = in_version.into_iter().next() else {
            let mut available_in: Vec<String> = method_matches
                .iter()
                .map(|row| row.version.clone())
                .collect();
            available_in.sort_by_key(|entry| parse_version_number(entry).unwrap_or(0));
            available_in.dedup();
            return Err(UnifiedError::not_found(format!(
                "Endpoint {method} {path} is not available in API version \"{version}\""
            ))
            .with_metadata("reason", json!("not_in_version"))
            .with_metadata("version", json!(version))
            .with_metadata("method", json!(method))
            .with_metadata("path", json!(path))
            .with_metadata("available_in", json!(available_in)));
        };
        let matched = (*matched).clone();
        let warning = if version_record.deprecated {
            Some(format!(
                "API version \"{version}\" is deprecated and will be removed on {}",
                version_record
                    .sunset_date
                    .map(|date| date.to_rfc3339())
                    .unwrap_or_else(|| "an announced sunset date".to_string())
            ))
        } else {
            None
        };
        Ok(ResolvedEndpoint {
            endpoint: matched,
            version: version_record,
            deprecated_warning: warning,
        })
    }
}

/// A successful endpoint resolution.
#[derive(Debug, Clone, Serialize)]
pub struct ResolvedEndpoint {
    pub endpoint: RegistryRow,
    pub version: ApiVersion,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub deprecated_warning: Option<String>,
}

// ---------------------------------------------------------------------------
// Public access control (default deny)
// ---------------------------------------------------------------------------

/// Maximum number of access-audit entries kept in memory. The
/// migration (000004, `api_access_audit`) persists the same rows;
/// this bound only caps the in-memory trail.
const MAX_ACCESS_AUDIT_ENTRIES: usize = 1000;

/// Header carrying a presented API key (`X-Api-Key`, the established
/// convention across the codebase).
const API_KEY_HEADER: &str = "x-api-key";

/// One admin-managed rule governing public access to a single
/// endpoint template, optionally restricted to one API version
/// (`version: None` means every version). Persisted by migration
/// 000004 as `api_access_rules`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApiAccessRule {
    pub id: Uuid,
    pub method: String,
    /// Full internal path template, for example
    /// `/api/v1/store/products/{id}`.
    pub path: String,
    /// `None` means the rule applies to every version.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    /// `false` denies the endpoint outright, even with a valid key.
    pub allowed: bool,
    /// When `true` the endpoint may be called without a key (still
    /// subject to the allow rule and the anonymous rate limit).
    /// When `false` a valid, in-scope key is required.
    pub public_access: bool,
    /// Who made the last change (admin identity or `"admin"`).
    pub actor: String,
    pub reason: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// One entry of the access-control audit trail. Persisted by
/// migration 000004 as `api_access_audit`. Actions: `rule_created`,
/// `rule_updated`, `rule_deleted`, `key_scopes_updated`,
/// `key_rate_limit_updated`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApiAccessAuditEntry {
    pub id: Uuid,
    pub actor: String,
    pub action: String,
    /// What changed, for example `GET /api/v1/store/products@v1`
    /// for a rule or the key id for a key change.
    pub target: String,
    pub detail: Value,
    pub created_at: DateTime<Utc>,
}

/// Input for creating an access rule. Creating a rule is itself the
/// explicit allow: omitting `allowed` allows the endpoint.
#[derive(Debug, Deserialize)]
pub struct CreateAccessRuleInput {
    pub method: String,
    pub path: String,
    #[serde(default)]
    pub version: Option<String>,
    #[serde(default = "default_true")]
    pub allowed: bool,
    #[serde(default)]
    pub public_access: bool,
    #[serde(default)]
    pub actor: Option<String>,
    #[serde(default)]
    pub reason: Option<String>,
}

/// Input for updating an access rule. Only the fields present are changed.
#[derive(Debug, Deserialize)]
pub struct UpdateAccessRuleInput {
    #[serde(default)]
    pub allowed: Option<bool>,
    #[serde(default)]
    pub public_access: Option<bool>,
    #[serde(default)]
    pub actor: Option<String>,
    #[serde(default)]
    pub reason: Option<String>,
}

/// Input for updating one key's access: scopes and/or rate limit.
/// An omitted field is left unchanged.
#[derive(Debug, Deserialize)]
pub struct UpdateKeyAccessInput {
    #[serde(default)]
    pub scopes: Option<Vec<String>>,
    #[serde(default)]
    pub rate_tier: Option<String>,
    #[serde(default)]
    pub requests_per_minute: Option<u32>,
    #[serde(default)]
    pub actor: Option<String>,
}

/// One row of `api_access_rules`, read with a raw query (the table
/// has no Diesel schema entry, so the typed DSL cannot address it).
/// `detail` is read as text because Diesel here has no
/// `serde_json` feature, so a `Value` cannot be loaded from a
/// `jsonb` column directly; the text is parsed afterwards.
#[derive(Debug, QueryableByName)]
struct AccessRuleRow {
    #[diesel(sql_type = diesel::sql_types::Uuid)]
    id: Uuid,
    #[diesel(sql_type = diesel::sql_types::Text)]
    method: String,
    #[diesel(sql_type = diesel::sql_types::Text)]
    path: String,
    #[diesel(sql_type = diesel::sql_types::Nullable<diesel::sql_types::Text>)]
    version: Option<String>,
    #[diesel(sql_type = diesel::sql_types::Bool)]
    allowed: bool,
    #[diesel(sql_type = diesel::sql_types::Bool)]
    public_access: bool,
    #[diesel(sql_type = diesel::sql_types::Text)]
    actor: String,
    #[diesel(sql_type = diesel::sql_types::Nullable<diesel::sql_types::Text>)]
    reason: Option<String>,
    #[diesel(sql_type = diesel::sql_types::Timestamptz)]
    created_at: DateTime<Utc>,
    #[diesel(sql_type = diesel::sql_types::Timestamptz)]
    updated_at: DateTime<Utc>,
}

/// One row of `api_access_audit`, read with a raw query. `detail`
/// is the `jsonb` column read as text and parsed afterwards.
#[derive(Debug, QueryableByName)]
struct AccessAuditRow {
    #[diesel(sql_type = diesel::sql_types::Uuid)]
    id: Uuid,
    #[diesel(sql_type = diesel::sql_types::Text)]
    actor: String,
    #[diesel(sql_type = diesel::sql_types::Text)]
    action: String,
    #[diesel(sql_type = diesel::sql_types::Text)]
    target: String,
    #[diesel(sql_type = diesel::sql_types::Text)]
    detail: String,
    #[diesel(sql_type = diesel::sql_types::Timestamptz)]
    created_at: DateTime<Utc>,
}

impl ApiManagerState {
    /// Map key for a rule: `"<METHOD> <path template>@<version|*>"`.
    pub fn access_rule_key(method: &str, path: &str, version: Option<&str>) -> String {
        format!(
            "{} {}@{}",
            method,
            normalize_path_template(path),
            version.unwrap_or("*")
        )
    }

    /// The rule governing one resolved endpoint: the exact-version
    /// rule first, then the all-versions (`*`) rule.
    pub fn find_access_rule(&self, row: &RegistryRow) -> Option<ApiAccessRule> {
        let exact = Self::access_rule_key(&row.method, &row.path, Some(&row.version));
        if let Some(rule) = self.access_rules.get(&exact) {
            return Some(rule.value().clone());
        }
        let wildcard = Self::access_rule_key(&row.method, &row.path, None);
        self.access_rules
            .get(&wildcard)
            .map(|entry| entry.value().clone())
    }

    /// Append one audit entry, dropping the oldest entries past the cap.
    pub fn record_access_audit(&self, actor: &str, action: &str, target: &str, detail: Value) {
        let entry = ApiAccessAuditEntry {
            id: Uuid::now_v7(),
            actor: actor.to_string(),
            action: action.to_string(),
            target: target.to_string(),
            detail,
            created_at: Utc::now(),
        };
        if let Ok(mut audit) = self.access_audit.write() {
            audit.push(entry.clone());
            if audit.len() > MAX_ACCESS_AUDIT_ENTRIES {
                let excess = audit.len() - MAX_ACCESS_AUDIT_ENTRIES;
                audit.drain(..excess);
            }
        }
        // Persist best-effort (bounded: one row per entry); see
        // `persist_access_audit_entry`.
        self.persist_access_audit_entry(&entry);
    }

    // ------------------------------------------------------------------
    // Persistence of the access rules and audit trail (migration
    // 000004: `api_access_rules`, `api_access_audit`).
    //
    // The tables have no Diesel schema entries (`src/db/schema.rs`
    // does not list them), so the typed DSL cannot address them;
    // these methods use raw `diesel::sql_query` instead, the same
    // fallback the dashboards module uses for its permission load.
    // All writes are best-effort: a missing pool, a missing table
    // (migration not run yet), or any database error is swallowed,
    // because the in-memory state stays authoritative and must never
    // fail an admin request. A failed write is therefore retried by
    // the next create/update/delete of the same row.
    // ------------------------------------------------------------------

    /// Load access rules and the audit trail from the database into
    /// the in-memory state. Each store loads only while it is still
    /// empty, so a reload never clobbers runtime edits. Returns
    /// (rules loaded, audit entries loaded). Fails when no pool is
    /// attached or the tables are missing; `init` treats that as
    /// best-effort and keeps the in-memory state authoritative.
    pub fn load_access_from_db(&self) -> UnifiedResult<(usize, usize)> {
        let Some(pool) = self.db_pool() else {
            return Err(UnifiedError::internal("no database pool attached"));
        };
        let mut conn = pool.get().map_err(|error| {
            UnifiedError::internal(format!("Cannot get database connection: {error}"))
        })?;
        let mut loaded_rules = 0usize;
        if self.access_rules.is_empty() {
            let rows = diesel::sql_query(
                "SELECT id, method, path, version, allowed, public_access, actor, reason, created_at, updated_at \
                 FROM api_access_rules ORDER BY created_at ASC",
            )
            .load::<AccessRuleRow>(&mut conn)
            .map_err(|error| {
                UnifiedError::internal(format!("Cannot load API access rules: {error}"))
            })?;
            for row in rows {
                let rule = ApiAccessRule {
                    id: row.id,
                    method: row.method,
                    path: row.path,
                    version: row.version,
                    allowed: row.allowed,
                    public_access: row.public_access,
                    actor: row.actor,
                    reason: row.reason,
                    created_at: row.created_at,
                    updated_at: row.updated_at,
                };
                let key = Self::access_rule_key(&rule.method, &rule.path, rule.version.as_deref());
                self.access_rules.insert(key, rule);
                loaded_rules += 1;
            }
        }
        let mut loaded_audit = 0usize;
        if self.access_audit.read().map(|audit| audit.is_empty()).unwrap_or(false) {
            let rows = diesel::sql_query(
                "SELECT id, actor, action, target, detail::text AS detail, created_at \
                 FROM api_access_audit ORDER BY created_at ASC LIMIT $1",
            )
            .bind::<diesel::sql_types::BigInt, _>(MAX_ACCESS_AUDIT_ENTRIES as i64)
            .load::<AccessAuditRow>(&mut conn)
            .map_err(|error| {
                UnifiedError::internal(format!("Cannot load API access audit: {error}"))
            })?;
            if let Ok(mut audit) = self.access_audit.write() {
                for row in rows {
                    audit.push(ApiAccessAuditEntry {
                        id: row.id,
                        actor: row.actor,
                        action: row.action,
                        target: row.target,
                        detail: serde_json::from_str(&row.detail).unwrap_or(Value::Null),
                        created_at: row.created_at,
                    });
                    loaded_audit += 1;
                }
            }
        }
        Ok((loaded_rules, loaded_audit))
    }

    /// Persist one access rule to `api_access_rules` (insert, or
    /// update the row with the same method/path/version). Best
    /// effort; never fails the caller (see the section header).
    pub fn persist_access_rule(&self, rule: &ApiAccessRule) {
        let Some(pool) = self.db_pool() else {
            return;
        };
        let Ok(mut conn) = pool.get() else {
            return;
        };
        let updated = diesel::sql_query(
            "UPDATE api_access_rules SET allowed = $1, public_access = $2, actor = $3, reason = $4, updated_at = $5 \
             WHERE method = $6 AND path = $7 AND version IS NOT DISTINCT FROM $8",
        )
        .bind::<diesel::sql_types::Bool, _>(rule.allowed)
        .bind::<diesel::sql_types::Bool, _>(rule.public_access)
        .bind::<diesel::sql_types::Text, _>(&rule.actor)
        .bind::<diesel::sql_types::Nullable<diesel::sql_types::Text>, _>(rule.reason.clone())
        .bind::<diesel::sql_types::Timestamptz, _>(rule.updated_at)
        .bind::<diesel::sql_types::Text, _>(&rule.method)
        .bind::<diesel::sql_types::Text, _>(&rule.path)
        .bind::<diesel::sql_types::Nullable<diesel::sql_types::Text>, _>(rule.version.clone())
        .execute(&mut conn);
        match updated {
            Ok(0) => {
                let _ = diesel::sql_query(
                    "INSERT INTO api_access_rules (id, method, path, version, allowed, public_access, actor, reason, created_at, updated_at) \
                     VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)",
                )
                .bind::<diesel::sql_types::Uuid, _>(rule.id)
                .bind::<diesel::sql_types::Text, _>(&rule.method)
                .bind::<diesel::sql_types::Text, _>(&rule.path)
                .bind::<diesel::sql_types::Nullable<diesel::sql_types::Text>, _>(rule.version.clone())
                .bind::<diesel::sql_types::Bool, _>(rule.allowed)
                .bind::<diesel::sql_types::Bool, _>(rule.public_access)
                .bind::<diesel::sql_types::Text, _>(&rule.actor)
                .bind::<diesel::sql_types::Nullable<diesel::sql_types::Text>, _>(rule.reason.clone())
                .bind::<diesel::sql_types::Timestamptz, _>(rule.created_at)
                .bind::<diesel::sql_types::Timestamptz, _>(rule.updated_at)
                .execute(&mut conn);
            }
            Ok(_) | Err(_) => {}
        }
    }

    /// Delete one access rule from `api_access_rules` (matched by
    /// method/path/version, `NULL` version included). Best effort;
    /// never fails the caller (see the section header).
    pub fn delete_access_rule_from_db(&self, rule: &ApiAccessRule) {
        let Some(pool) = self.db_pool() else {
            return;
        };
        let Ok(mut conn) = pool.get() else {
            return;
        };
        let _ = diesel::sql_query(
            "DELETE FROM api_access_rules WHERE method = $1 AND path = $2 AND version IS NOT DISTINCT FROM $3",
        )
        .bind::<diesel::sql_types::Text, _>(&rule.method)
        .bind::<diesel::sql_types::Text, _>(&rule.path)
        .bind::<diesel::sql_types::Nullable<diesel::sql_types::Text>, _>(rule.version.clone())
        .execute(&mut conn);
    }

    /// Persist one audit entry to `api_access_audit` (append-only).
    /// Best effort; never fails the caller (see the section header).
    /// The action CHECK on the table allows only the seven
    /// access-control actions; every `record_access_audit` caller
    /// uses one of them.
    pub fn persist_access_audit_entry(&self, entry: &ApiAccessAuditEntry) {
        let Some(pool) = self.db_pool() else {
            return;
        };
        let Ok(mut conn) = pool.get() else {
            return;
        };
        let _ = diesel::sql_query(
            "INSERT INTO api_access_audit (id, actor, action, target, detail, created_at, updated_at) \
             VALUES ($1, $2, $3, $4, $5::jsonb, $6, $7)",
        )
        .bind::<diesel::sql_types::Uuid, _>(entry.id)
        .bind::<diesel::sql_types::Text, _>(&entry.actor)
        .bind::<diesel::sql_types::Text, _>(&entry.action)
        .bind::<diesel::sql_types::Text, _>(&entry.target)
        .bind::<diesel::sql_types::Text, _>(entry.detail.to_string())
        .bind::<diesel::sql_types::Timestamptz, _>(entry.created_at)
        .bind::<diesel::sql_types::Timestamptz, _>(entry.created_at)
        .execute(&mut conn);
    }

    /// Fixed-window per-minute check for public-API access, reusing
    /// the module's `rate_counters` map. Returns 429 once the
    /// subject has spent its allowance for this endpoint and minute.
    /// Shares the map's existing caveat: counters are never evicted
    /// (see the `rate_counters` field docs).
    fn check_access_rate_limit(
        &self,
        subject: &str,
        row: &RegistryRow,
        limit: u32,
    ) -> Result<(), UnifiedError> {
        let minute = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|elapsed| elapsed.as_secs() / 60)
            .unwrap_or(0);
        let counter_key = format!(
            "access:{subject}:{} {}@{}:{minute}",
            row.method, row.path, row.version
        );
        let mut count = self.rate_counters.entry(counter_key).or_insert(0);
        *count += 1;
        if *count > u64::from(limit) {
            return Err(UnifiedError::new(
                format!(
                    "Rate limit exceeded for {} {} ({} requests per minute)",
                    row.method, row.path, limit
                ),
                429,
            )
            .with_metadata("reason", json!("rate_limited"))
            .with_metadata("limit_per_minute", json!(limit))
            .with_metadata("retry_after_seconds", json!(60)));
        }
        Ok(())
    }

    /// The 403 returned when a request has no allow rule or hits an
    /// explicit deny rule.
    fn access_denied(row: &RegistryRow, reason: &str) -> UnifiedError {
        UnifiedError::forbidden(format!(
            "Public access to {} {} is not allowed",
            row.method, row.path
        ))
        .with_metadata("reason", json!(reason))
        .with_metadata("method", json!(row.method.clone()))
        .with_metadata("path", json!(row.path.clone()))
        .with_metadata("version", json!(row.version.clone()))
    }

    /// Enforce public access for one request. Returns the
    /// authenticated key when one was presented and accepted, or
    /// `None` for an anonymous call to a public endpoint -- and
    /// also `None` when the request does not match any registry
    /// endpoint at all, so normal routing can return its own 404.
    /// Callers must run this before any handler logic: it rejects
    /// with 401/403/429, in this order, when the endpoint has no
    /// allow rule (default deny), the key is missing, invalid, or
    /// out of scope, or the per-minute allowance is spent. The
    /// scope check runs before the rate-limit spend, so a denied
    /// key never consumes quota.
    ///
    /// `version` comes from the public prefix (`/v1/...`); internal
    /// `/api/v1/...` paths resolve as version `v1`. A key is
    /// required unless the governing rule marks the endpoint public
    /// (`public_access` is the explicit per-endpoint override of the
    /// endpoint's `auth_required` flag, which is true for every
    /// built-in endpoint). Note the version-level
    /// `ApiVersion.auth_required` is deliberately NOT consulted
    /// here: it is a registry default, not a per-endpoint decision,
    /// and honouring it would make the per-endpoint public flag
    /// meaningless. Anonymous calls are rate-limited per client IP
    /// against the endpoint's effective limit.
    pub fn check_public_access(
        &self,
        method: &str,
        raw_path: &str,
        presented_key: Option<&str>,
        client_ip: Option<String>,
    ) -> Result<Option<crate::modules::apikeys::AuthenticatedKey>, UnifiedError> {
        self.ensure_seeded_versions();
        let (version, relative_path) = strip_public_prefix(self, raw_path);
        let version = version.unwrap_or_else(|| "v1".to_string());
        // Not a registry endpoint (unknown path, disabled version,
        // method not allowed, not in this version): leave the
        // decision to normal routing, which returns its own 404.
        let resolved = match self.resolve_endpoint(method, &version, &relative_path) {
            Ok(resolved) => resolved,
            Err(_) => return Ok(None),
        };
        let row = &resolved.endpoint;
        // Default deny: no rule, or an explicit deny rule, rejects
        // the request before any handler logic runs.
        let Some(rule) = self.find_access_rule(row) else {
            return Err(Self::access_denied(row, "public_access_not_allowed"));
        };
        if !rule.allowed {
            return Err(Self::access_denied(row, "public_access_not_allowed"));
        }
        let action = if method == "GET" || method == "HEAD" {
            "read"
        } else {
            "write"
        };
        let needs_key = !rule.public_access && row.auth_required;
        let key = match presented_key.map(str::trim).filter(|key| !key.is_empty()) {
            Some(key) => Some(crate::modules::apikeys::authenticate_key(key)?),
            None => {
                if needs_key {
                    return Err(UnifiedError::unauthorized("API key required")
                        .with_metadata("reason", json!("api_key_required"))
                        .with_metadata("method", json!(method))
                        .with_metadata("path", json!(row.path.clone()))
                        .with_metadata("version", json!(row.version.clone())));
                }
                None
            }
        };
        if let Some(authenticated) = &key {
            if !crate::modules::apikeys::key_has_scope(
                &authenticated.scopes,
                &row.owner_module,
                action,
            ) {
                return Err(UnifiedError::forbidden(format!(
                    "API key lacks scope \"{}:{action}\" for this endpoint",
                    row.owner_module
                ))
                .with_metadata("reason", json!("scope_denied"))
                .with_metadata("module", json!(row.owner_module.clone()))
                .with_metadata("action", json!(action))
                .with_metadata("method", json!(method))
                .with_metadata("path", json!(row.path.clone()))
                .with_metadata("version", json!(row.version.clone())));
            }
        }
        // Rate limit: a presented key spends its own per-minute
        // limit; an anonymous call spends the endpoint's effective
        // limit (its own limit, or the owning version's).
        let (subject, limit) = match &key {
            Some(authenticated) => (
                format!("key:{}", authenticated.id),
                authenticated.requests_per_minute,
            ),
            None => (
                client_ip
                    .map(|ip| format!("ip:{ip}"))
                    .unwrap_or_else(|| "anon".to_string()),
                row.rate_limit,
            ),
        };
        self.check_access_rate_limit(&subject, row, limit)?;
        if let Some(authenticated) = &key {
            crate::modules::apikeys::note_used(authenticated.id);
        }
        Ok(key)
    }
}

/// Request-path guard enforcing the access rules.
///
/// Runs at priority 850: after the IP filter (900, so blocked IPs
/// never reach the key check) and before the generic rate limiter
/// (800, so denied requests do not consume quota). It applies to
/// the public versioned surface (`/{prefix}/{version}/...`) and to
/// internal `/api/v1/...` routes called with an `X-Api-Key` header.
/// Requests without a key to internal routes pass through
/// untouched, so session/JWT admin traffic is unaffected, and the
/// API Manager's own `/api/v1/api-manager/` prefix is exempt: its
/// public gateway enforces inside `resolve_public`, and its admin
/// routes are session-protected. The guard and the gateway check
/// never overlap, so a request is checked exactly once.
#[derive(Clone)]
pub struct PublicApiAccessMiddleware {
    state: ApiManagerState,
}

impl std::fmt::Debug for PublicApiAccessMiddleware {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "PublicApiAccessMiddleware")
    }
}

impl PublicApiAccessMiddleware {
    pub fn new(state: ApiManagerState) -> Self {
        Self { state }
    }
}

impl Middleware for PublicApiAccessMiddleware {
    fn priority(&self) -> isize {
        850
    }

    fn handle(&self, req: ServiceRequest, next: NextFn) -> MiddlewareResult {
        let state = self.state.clone();
        let path = req.path().to_string();
        let method = req.method().to_string();
        let presented_key = req
            .headers()
            .get(API_KEY_HEADER)
            .and_then(|value| value.to_str().ok())
            .map(str::to_string);
        let prefix = state
            .prefix
            .read()
            .map(|settings| settings.prefix.clone())
            .unwrap_or_else(|_| "v".to_string());
        let public_root = format!("/{prefix}/");
        let is_public_surface = path.starts_with(&public_root);
        let is_keyed_internal = path.starts_with("/api/v1/")
            && presented_key.is_some()
            && !path.starts_with("/api/v1/api-manager/");
        if !is_public_surface && !is_keyed_internal {
            return Box::pin(async move { next(req).await });
        }
        let ip = client_ip(&req).map(|ip| ip.to_string());
        match state.check_public_access(&method, &path, presented_key.as_deref(), ip) {
            Ok(_) => Box::pin(async move { next(req).await }),
            Err(error) => {
                let response = api_json::error_response(&error, false);
                Box::pin(async move { Ok(req.into_response(response)) })
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Validation helpers
// ---------------------------------------------------------------------------

const HTTP_METHODS: &[&str] = &["GET", "POST", "PUT", "PATCH", "DELETE"];
const RULE_TYPES: &[&str] = &["string", "integer", "number", "boolean", "array", "object"];

fn validate_method(method: &str) -> Result<String, UnifiedError> {
    let method = method.trim().to_uppercase();
    if !HTTP_METHODS.contains(&method.as_str()) {
        return Err(UnifiedError::bad_request(format!(
            "method must be one of {}",
            HTTP_METHODS.join(", ")
        )));
    }
    Ok(method)
}

fn validate_path(path: &str) -> Result<String, UnifiedError> {
    let path = path.trim();
    if !path.starts_with('/') {
        return Err(UnifiedError::bad_request("path must start with '/'"));
    }
    if path.contains(char::is_whitespace) {
        return Err(UnifiedError::bad_request("path must not contain whitespace"));
    }
    if path.chars().count() > 500 {
        return Err(UnifiedError::bad_request(
            "path must be at most 500 characters",
        ));
    }
    Ok(path.to_string())
}

fn validate_version(version: &str) -> Result<String, UnifiedError> {
    let version = version.trim();
    // Accept `v1`, `v1.2`, or a plain semver string such as `1.2.3`.
    let version_pattern =
        Regex::new(r"^v\d+(\.\d+)*$|^\d+\.\d+\.\d+$").expect("static regex is valid");
    if !version_pattern.is_match(version) {
        return Err(UnifiedError::bad_request(
            "version must look like \"v1\", \"v1.2\", or a semver string such as \"1.2.3\"",
        ));
    }
    Ok(version.to_string())
}

/// Numeric part of a registry version id (`v10` -> 10). Returns
/// `None` for anything that is not `v` followed by a positive
/// number. Legacy endpoint versions such as `v1.2` or `1.2.3` are
/// not registry ids and return `None` here on purpose.
fn parse_version_number(version: &str) -> Option<u32> {
    let number = version.strip_prefix('v')?;
    if number.is_empty() || !number.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    let parsed: u32 = number.parse().ok()?;
    if parsed == 0 {
        None
    } else {
        Some(parsed)
    }
}

/// Sort version ids (`v1`, `v2`, `v10`) by version number, so `v10`
/// sorts after `v2` instead of before it.
fn sort_version_ids(ids: &mut Vec<String>) {
    ids.sort_by_key(|id| parse_version_number(id).unwrap_or(0));
}

/// Validate a version-registry id: strictly `v` plus a positive
/// number (`v1`, `v2`, `v10`). This is stricter than the legacy
/// endpoint `version` field above, which also accepts `v1.2` and
/// semver strings for backward compatibility.
fn validate_registry_version_id(version: &str) -> Result<String, UnifiedError> {
    let version = version.trim();
    if parse_version_number(version).is_none() {
        return Err(UnifiedError::bad_request(
            "version id must look like \"v1\", \"v2\", or \"v10\" (\"v\" followed by a positive number)",
        ));
    }
    Ok(version.to_string())
}

/// Validate the global public API prefix: one or more segments of
/// letters, digits, `-` or `_`, joined by `/`, with no leading or
/// trailing slash and no whitespace. Examples: `"v"`, `"api"`,
/// `"api/v2"`. An empty prefix is rejected: it would produce
/// ambiguous `//v1/...` URLs.
fn validate_prefix(prefix: &str) -> Result<String, UnifiedError> {
    let prefix = prefix.trim();
    if prefix.is_empty() {
        return Err(UnifiedError::bad_request("prefix must not be empty"));
    }
    if prefix.starts_with('/') || prefix.ends_with('/') {
        return Err(UnifiedError::bad_request(
            "prefix must not start or end with '/'",
        ));
    }
    if prefix.contains(char::is_whitespace) {
        return Err(UnifiedError::bad_request("prefix must not contain whitespace"));
    }
    if prefix.chars().count() > 100 {
        return Err(UnifiedError::bad_request(
            "prefix must be at most 100 characters",
        ));
    }
    let segment_pattern = Regex::new(r"^[A-Za-z0-9_-]+$").expect("static regex is valid");
    for segment in prefix.split('/') {
        if !segment_pattern.is_match(segment) {
            return Err(UnifiedError::bad_request(format!(
                "prefix segment \"{segment}\" is invalid: use only letters, digits, '-' or '_'"
            )));
        }
    }
    Ok(prefix.to_string())
}

/// Normalize a path template for comparison: `{id}` placeholders
/// become `{}`, and a trailing slash (other than the root) is
/// dropped, so `/vcs/providers/{provider}/` and
/// `/vcs/providers/{id}` compare equal.
fn normalize_path_template(path: &str) -> String {
    let trimmed = path.trim();
    let trimmed = trimmed.strip_suffix('/').filter(|_| trimmed.len() > 1).unwrap_or(trimmed);
    trimmed
        .split('/')
        .map(|segment| {
            if segment.starts_with('{') && segment.ends_with('}') {
                "{}"
            } else {
                segment
            }
        })
        .collect::<Vec<&str>>()
        .join("/")
}

/// True when a concrete request path matches a path template:
/// `{placeholder}` segments match any single segment.
fn path_template_matches(template: &str, path: &str) -> bool {
    let template_segments: Vec<&str> = template.split('/').collect();
    let path_segments: Vec<&str> = path.split('/').collect();
    if template_segments.len() != path_segments.len() {
        return false;
    }
    template_segments.iter().zip(path_segments.iter()).all(|(template, actual)| {
        (template.starts_with('{') && template.ends_with('}')) || template == actual
    })
}

/// Strip a public or internal prefix from a request path and return
/// the version-relative path the resolver works with, plus the
/// version it carried when one was present. Accepts:
/// `/{prefix}/{version}/...` (public), `/api/v1/...` (internal),
/// and bare version-relative paths (`/...`).
fn strip_public_prefix(state: &ApiManagerState, raw_path: &str) -> (Option<String>, String) {
    let path = raw_path.trim();
    let prefix = state
        .prefix
        .read()
        .map(|settings| settings.prefix.clone())
        .unwrap_or_else(|_| "v".to_string());
    let public_root = format!("/{prefix}/");
    if let Some(rest) = path.strip_prefix(&public_root) {
        let mut parts = rest.splitn(2, '/');
        let version = parts.next().unwrap_or("");
        let tail = parts.next().unwrap_or("");
        if parse_version_number(version).is_some() {
            return (Some(version.to_string()), format!("/{tail}"));
        }
    }
    if let Some(rest) = path.strip_prefix("/api/v1") {
        return (None, rest.to_string());
    }
    (None, path.to_string())
}

/// Validate a `VersionSupport` declaration against the registry:
/// every referenced version must be a strict id that exists in the
/// registry, lists must be non-empty, and ranges must be ordered.
/// Lists are normalized (de-duplicated, sorted by version number).
fn validate_version_support(
    state: &ApiManagerState,
    support: VersionSupport,
) -> Result<VersionSupport, UnifiedError> {
    let known = state.version_ids();
    let require_known = |version: &str| -> Result<String, UnifiedError> {
        let version = validate_registry_version_id(version)?;
        if !known.iter().any(|entry| entry == &version) {
            return Err(UnifiedError::bad_request(format!(
                "version \"{version}\" does not exist in the API version registry"
            ))
            .with_metadata("unknown_version", json!(version))
            .with_metadata("available_versions", json!(known)));
        }
        Ok(version)
    };
    match support {
        VersionSupport::All => Ok(VersionSupport::All),
        VersionSupport::Min { min_version } => Ok(VersionSupport::Min {
            min_version: require_known(&min_version)?,
        }),
        VersionSupport::Max { max_version } => Ok(VersionSupport::Max {
            max_version: require_known(&max_version)?,
        }),
        VersionSupport::Range { min_version, max_version } => {
            let min_version = require_known(&min_version)?;
            let max_version = require_known(&max_version)?;
            let min_number = parse_version_number(&min_version).unwrap_or(0);
            let max_number = parse_version_number(&max_version).unwrap_or(0);
            if min_number > max_number {
                return Err(UnifiedError::bad_request(
                    "min_version must not be newer than max_version",
                ));
            }
            Ok(VersionSupport::Range { min_version, max_version })
        }
        VersionSupport::List { versions } => {
            if versions.is_empty() {
                return Err(UnifiedError::bad_request(
                    "versions must contain at least one version",
                ));
            }
            if versions.len() > 50 {
                return Err(UnifiedError::bad_request(
                    "versions must contain at most 50 versions",
                ));
            }
            let mut normalized: Vec<String> = Vec::new();
            for version in &versions {
                let version = require_known(version)?;
                if !normalized.contains(&version) {
                    normalized.push(version);
                }
            }
            sort_version_ids(&mut normalized);
            Ok(VersionSupport::List { versions: normalized })
        }
    }
}

/// Resolve the version support for a create/update endpoint input.
/// Precedence: `version_support` > `versions` > legacy `version`.
/// The legacy `version` field keeps its old validation; when it is
/// a strict registry id it must also exist in the registry, while a
/// legacy `v1.2` / semver value is stored as-is for backward
/// compatibility and is not required to exist in the registry.
fn resolve_input_support(
    state: &ApiManagerState,
    version: &Option<String>,
    versions: &Option<Vec<String>>,
    version_support: &Option<VersionSupport>,
) -> Result<VersionSupport, UnifiedError> {
    if let Some(support) = version_support {
        return validate_version_support(state, support.clone());
    }
    if let Some(versions) = versions {
        return validate_version_support(
            state,
            VersionSupport::List { versions: versions.clone() },
        );
    }
    match version {
        Some(version) => {
            let version = validate_version(version)?;
            if parse_version_number(&version).is_some() {
                let known = state.version_ids();
                if !known.iter().any(|entry| entry == &version) {
                    return Err(UnifiedError::bad_request(format!(
                        "version \"{version}\" does not exist in the API version registry"
                    ))
                    .with_metadata("unknown_version", json!(version))
                    .with_metadata("available_versions", json!(known)));
                }
            }
            Ok(VersionSupport::List { versions: vec![version] })
        }
        None => Err(UnifiedError::bad_request(
            "provide one of: version, versions, or version_support",
        )),
    }
}

/// Build the 422 collision error for a custom endpoint that
/// collides with a built-in endpoint. The conflicting endpoint and
/// its owner module are named, and the overlapping versions are in
/// the metadata.
fn builtin_collision_error(
    method: &str,
    path: &str,
    conflicting: &RegisteredEndpoint,
    overlapping: &[String],
) -> UnifiedError {
    UnifiedError::unprocessable(format!(
        "endpoint {method} {path} already exists as a built-in endpoint owned by module \"{}\" ({} {})",
        conflicting.owner_module, conflicting.method, conflicting.path
    ))
    .with_metadata(
        "conflicting_endpoint",
        json!(format!("{} {}", conflicting.method, conflicting.path)),
    )
    .with_metadata("owner_module", json!(conflicting.owner_module))
    .with_metadata("versions", json!(overlapping))
}

/// Emit `api.endpoint.conflict` so the admin UI and audit log can
/// show a rejected custom-endpoint creation.
fn emit_endpoint_conflict(
    ctx: &AppContext,
    method: &str,
    path: &str,
    conflicting: &RegisteredEndpoint,
    overlapping: &[String],
) {
    ctx.hooks.emit(
        "api.endpoint.conflict",
        json!({
            "method": method,
            "path": path,
            "conflicting_endpoint": format!("{} {}", conflicting.method, conflicting.path),
            "owner_module": conflicting.owner_module,
            "versions": overlapping,
        }),
    );
}

fn validate_rules(rules: &[EndpointRule]) -> Result<(), UnifiedError> {
    let field_pattern =
        Regex::new(r"^[a-zA-Z0-9_]+(\.[a-zA-Z0-9_]+)*$").expect("static regex is valid");
    for rule in rules {
        if !field_pattern.is_match(&rule.field) {
            return Err(UnifiedError::bad_request(format!(
                "rule field \"{}\" is not a valid dotted field name",
                rule.field
            )));
        }
        if !RULE_TYPES.contains(&rule.field_type.as_str()) {
            return Err(UnifiedError::bad_request(format!(
                "rule field \"{}\": type must be one of {}",
                rule.field,
                RULE_TYPES.join(", ")
            )));
        }
        if let (Some(min), Some(max)) = (rule.min_length, rule.max_length) {
            if min > max {
                return Err(UnifiedError::bad_request(format!(
                    "rule field \"{}\": min_length must not exceed max_length",
                    rule.field
                )));
            }
        }
        if let (Some(min), Some(max)) = (rule.min_value, rule.max_value) {
            if min > max {
                return Err(UnifiedError::bad_request(format!(
                    "rule field \"{}\": min_value must not exceed max_value",
                    rule.field
                )));
            }
        }
        if let Some(pattern) = &rule.pattern {
            Regex::new(pattern).map_err(|_| {
                UnifiedError::bad_request(format!(
                    "rule field \"{}\": pattern is not a valid regular expression",
                    rule.field
                ))
            })?;
        }
    }
    Ok(())
}

/// Look a dotted field up in a JSON object payload.
fn lookup_field<'a>(payload: &'a Value, field: &str) -> Option<&'a Value> {
    let mut current = payload;
    for segment in field.split('.') {
        current = current.get(segment)?;
    }
    Some(current)
}

fn type_matches(value: &Value, field_type: &str) -> bool {
    match field_type {
        "string" => value.is_string(),
        "integer" => value.as_i64().is_some() || value.as_u64().is_some(),
        "number" => value.is_number(),
        "boolean" => value.is_boolean(),
        "array" => value.is_array(),
        "object" => value.is_object(),
        _ => false,
    }
}

/// Validate a payload against the endpoint rules. Returns the list of
/// problems; an empty list means the payload is valid.
fn validate_payload(payload: &Value, rules: &[EndpointRule]) -> Vec<String> {
    let mut errors: Vec<String> = Vec::new();
    if !payload.is_object() {
        errors.push("payload must be a JSON object".to_string());
        return errors;
    }
    for rule in rules {
        let value = lookup_field(payload, &rule.field);
        let Some(value) = value else {
            if rule.required {
                errors.push(format!("field \"{}\" is required", rule.field));
            }
            continue;
        };
        if value.is_null() {
            if rule.required {
                errors.push(format!("field \"{}\" must not be null", rule.field));
            }
            continue;
        }
        if !type_matches(value, &rule.field_type) {
            errors.push(format!(
                "field \"{}\" must be of type {}",
                rule.field, rule.field_type
            ));
            continue;
        }
        if let Some(text) = value.as_str() {
            let len = text.chars().count();
            if let Some(min) = rule.min_length {
                if len < min {
                    errors.push(format!(
                        "field \"{}\" must be at least {min} characters long",
                        rule.field
                    ));
                }
            }
            if let Some(max) = rule.max_length {
                if len > max {
                    errors.push(format!(
                        "field \"{}\" must be at most {max} characters long",
                        rule.field
                    ));
                }
            }
            if let Some(pattern) = &rule.pattern {
                if let Ok(regex) = Regex::new(pattern) {
                    if !regex.is_match(text) {
                        errors.push(format!(
                            "field \"{}\" does not match the required pattern",
                            rule.field
                        ));
                    }
                }
            }
        }
        if let Some(number) = value.as_f64() {
            if let Some(min) = rule.min_value {
                if number < min {
                    errors.push(format!("field \"{}\" must be at least {min}", rule.field));
                }
            }
            if let Some(max) = rule.max_value {
                if number > max {
                    errors.push(format!("field \"{}\" must be at most {max}", rule.field));
                }
            }
        }
    }
    errors
}

// ---------------------------------------------------------------------------
// Endpoint handlers
// ---------------------------------------------------------------------------

async fn list_endpoints(
    state: web::Data<ApiManagerState>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let params = query.into_inner();
    let search = params.search.clone();
    let version_filter = params.filters.get("version").cloned();
    let method_filter = params.filters.get("method").cloned();
    let enabled_filter = params.filters.get("enabled").cloned();
    let owner_filter = params.filters.get("owner_module").cloned();
    let source_filter = params.filters.get("source").cloned();
    let auth_filter = params.filters.get("auth_required").cloned();
    let mut items: Vec<ApiEndpoint> = state
        .endpoints
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|endpoint| {
            if let Some(version) = &version_filter {
                // A version matches when the endpoint's version
                // support covers it, or when its legacy single
                // version field equals it (legacy `v1.2` records).
                let known = state.version_ids();
                if !endpoint.version_support.supports(version, &known)
                    && endpoint.version != *version
                {
                    return false;
                }
            }
            if let Some(method) = &method_filter {
                if endpoint.method != method.to_uppercase() {
                    return false;
                }
            }
            if let Some(enabled) = &enabled_filter {
                let wanted = enabled == "true";
                if endpoint.enabled != wanted {
                    return false;
                }
            }
            if let Some(owner) = &owner_filter {
                if endpoint.owner_module != *owner {
                    return false;
                }
            }
            if let Some(source) = &source_filter {
                if endpoint.source != *source {
                    return false;
                }
            }
            if let Some(auth_required) = &auth_filter {
                let wanted = auth_required == "true";
                if endpoint.auth_required != wanted {
                    return false;
                }
            }
            if let Some(needle) = &search {
                let needle = needle.to_lowercase();
                return endpoint.path.to_lowercase().contains(&needle)
                    || endpoint.description.to_lowercase().contains(&needle)
                    || endpoint.version.to_lowercase().contains(&needle);
            }
            true
        })
        .collect();
    let sort = params.allowed_sort(&["created_at", "path", "version"], "created_at");
    let descending = params.normalized().order == "desc";
    items.sort_by(|a, b| {
        let ordering = match sort.as_str() {
            "path" => a.path.cmp(&b.path),
            "version" => a.version.cmp(&b.version),
            _ => a.created_at.cmp(&b.created_at),
        };
        if descending {
            ordering.reverse()
        } else {
            ordering
        }
    });
    let pagination = params.normalized();
    let total = items.len() as u64;
    let start = ((pagination.page - 1) * pagination.per_page) as usize;
    let page_items: Vec<ApiEndpoint> = items
        .into_iter()
        .skip(start)
        .take(pagination.per_page as usize)
        .collect();
    api_json::success_response(PageResult::new(page_items, total, &pagination).to_json(), 200)
}

async fn create_endpoint(
    state: web::Data<ApiManagerState>,
    ctx: web::Data<Arc<AppContext>>,
    input: web::Json<CreateEndpointInput>,
) -> HttpResponse {
    state.ensure_seeded_versions();
    let input = input.into_inner();
    let outcome = (|| -> Result<ApiEndpoint, UnifiedError> {
        let method = validate_method(&input.method)?;
        let path = validate_path(&input.path)?;
        let support = resolve_input_support(
            &state,
            &input.version,
            &input.versions,
            &input.version_support,
        )?;
        let rules = input.rules.unwrap_or_default();
        validate_rules(&rules)?;
        let rate_limit = input.rate_limit.unwrap_or(60);
        if rate_limit == 0 || rate_limit > 100_000 {
            return Err(UnifiedError::bad_request(
                "rate_limit must be between 1 and 100000 requests per minute",
            ));
        }
        let owner_module = match input.owner_module {
            Some(owner) => {
                let owner = owner.trim().to_string();
                if owner.is_empty() {
                    return Err(UnifiedError::bad_request("owner_module must not be empty"));
                }
                if owner == "builtin" {
                    return Err(UnifiedError::bad_request(
                        "owner_module \"builtin\" is reserved for endpoints imported from the module route catalog",
                    ));
                }
                owner
            }
            None => "custom".to_string(),
        };
        // Collision check 1: a built-in / core / module endpoint
        // already exists with the same method + path in any chosen
        // version. This is rejected with 422 and emits
        // `api.endpoint.conflict`.
        if let Some((conflicting, overlapping)) =
            state.builtin_conflict(&method, &path, &support)
        {
            emit_endpoint_conflict(&ctx, &method, &path, &conflicting, &overlapping);
            return Err(builtin_collision_error(
                &method,
                &path,
                &conflicting,
                &overlapping,
            ));
        }
        // Collision check 2: another custom endpoint already covers
        // the same method + path in an overlapping version. The
        // original 409 behaviour is kept for custom-vs-custom
        // duplicates.
        if let Some((existing, overlapping)) =
            state.custom_conflict(&method, &path, &support, None)
        {
            let _ = existing;
            return Err(UnifiedError::conflict(format!(
                "an endpoint for {method} {path} already exists in version(s) {}",
                overlapping.join(", ")
            )));
        }
        let now = Utc::now();
        let known = state.version_ids();
        let version = support
            .lowest_version(&known)
            .unwrap_or_else(|| "v1".to_string());
        let endpoint = ApiEndpoint {
            id: Uuid::now_v7(),
            method,
            path,
            version,
            description: input.description.unwrap_or_default(),
            rate_limit,
            enabled: input.enabled.unwrap_or(true),
            rules,
            execution_count: 0,
            owner_module: owner_module.clone(),
            auth_required: input.auth_required.unwrap_or(true),
            source: "custom".to_string(),
            version_support: support.clone(),
            created_at: now,
            updated_at: now,
        };
        state.endpoints.insert(endpoint.id, endpoint.clone());
        ctx.hooks.emit(
            "api.endpoint.created",
            json!({
                "endpoint_id": endpoint.id.to_string(),
                "method": endpoint.method,
                "path": endpoint.path,
                "versions": support.available_in(&known),
                "owner_module": owner_module,
            }),
        );
        Ok(endpoint)
    })();
    match outcome {
        Ok(endpoint) => api_json::success_response(endpoint, 201),
        Err(error) => api_json::error_response(&error, ctx.is_debug()),
    }
}

async fn get_endpoint(state: web::Data<ApiManagerState>, id: web::Path<Uuid>) -> HttpResponse {
    match state.endpoints.get(&id.into_inner()) {
        Some(endpoint) => api_json::success_response(endpoint.value().clone(), 200),
        None => api_json::error_response(&UnifiedError::not_found("endpoint not found"), false),
    }
}

async fn update_endpoint(
    state: web::Data<ApiManagerState>,
    ctx: web::Data<Arc<AppContext>>,
    id: web::Path<Uuid>,
    input: web::Json<UpdateEndpointInput>,
) -> HttpResponse {
    state.ensure_seeded_versions();
    let id = id.into_inner();
    let input = input.into_inner();
    let outcome = (|| -> Result<ApiEndpoint, UnifiedError> {
        let mut endpoint = state
            .endpoints
            .get(&id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("endpoint not found"))?;
        if let Some(method) = &input.method {
            endpoint.method = validate_method(method)?;
        }
        if let Some(path) = &input.path {
            endpoint.path = validate_path(path)?;
        }
        if input.version.is_some() || input.versions.is_some() || input.version_support.is_some()
        {
            endpoint.version_support = resolve_input_support(
                &state,
                &input.version,
                &input.versions,
                &input.version_support,
            )?;
            let known = state.version_ids();
            endpoint.version = endpoint
                .version_support
                .lowest_version(&known)
                .unwrap_or_else(|| endpoint.version.clone());
        }
        if let Some(rules) = &input.rules {
            validate_rules(rules)?;
            endpoint.rules = rules.clone();
        }
        if let Some(rate_limit) = input.rate_limit {
            if rate_limit == 0 || rate_limit > 100_000 {
                return Err(UnifiedError::bad_request(
                    "rate_limit must be between 1 and 100000 requests per minute",
                ));
            }
            endpoint.rate_limit = rate_limit;
        }
        if let Some(description) = input.description {
            endpoint.description = description;
        }
        if let Some(enabled) = input.enabled {
            endpoint.enabled = enabled;
        }
        if let Some(owner_module) = input.owner_module {
            let owner_module = owner_module.trim().to_string();
            if owner_module.is_empty() {
                return Err(UnifiedError::bad_request("owner_module must not be empty"));
            }
            if owner_module == "builtin" {
                return Err(UnifiedError::bad_request(
                    "owner_module \"builtin\" is reserved for endpoints imported from the module route catalog",
                ));
            }
            endpoint.owner_module = owner_module;
        }
        if let Some(auth_required) = input.auth_required {
            endpoint.auth_required = auth_required;
        }
        // Re-run both collision checks against the updated method +
        // path + version support, excluding this endpoint itself.
        if let Some((conflicting, overlapping)) = state.builtin_conflict(
            &endpoint.method,
            &endpoint.path,
            &endpoint.version_support,
        ) {
            emit_endpoint_conflict(
                &ctx,
                &endpoint.method,
                &endpoint.path,
                &conflicting,
                &overlapping,
            );
            return Err(builtin_collision_error(
                &endpoint.method,
                &endpoint.path,
                &conflicting,
                &overlapping,
            ));
        }
        if let Some((_, overlapping)) = state.custom_conflict(
            &endpoint.method,
            &endpoint.path,
            &endpoint.version_support,
            Some(id),
        ) {
            return Err(UnifiedError::conflict(format!(
                "an endpoint for {} {} already exists in version(s) {}",
                endpoint.method,
                endpoint.path,
                overlapping.join(", ")
            )));
        }
        endpoint.updated_at = Utc::now();
        state.endpoints.insert(id, endpoint.clone());
        Ok(endpoint)
    })();
    match outcome {
        Ok(endpoint) => api_json::success_response(endpoint, 200),
        Err(error) => api_json::error_response(&error, ctx.is_debug()),
    }
}

async fn delete_endpoint(state: web::Data<ApiManagerState>, id: web::Path<Uuid>) -> HttpResponse {
    let id = id.into_inner();
    match state.endpoints.remove(&id) {
        Some((_, endpoint)) => api_json::success_response(
            json!({ "id": endpoint.id, "deleted": true }),
            200,
        ),
        None => api_json::error_response(&UnifiedError::not_found("endpoint not found"), false),
    }
}

async fn execute_endpoint(
    state: web::Data<ApiManagerState>,
    ctx: web::Data<Arc<AppContext>>,
    id: web::Path<Uuid>,
    payload: web::Json<Value>,
) -> HttpResponse {
    let id = id.into_inner();
    let payload = payload.into_inner();
    let Some(mut endpoint) = state.endpoints.get(&id).map(|entry| entry.value().clone()) else {
        return api_json::error_response(&UnifiedError::not_found("endpoint not found"), false);
    };
    if !endpoint.enabled {
        return api_json::error_response(
            &UnifiedError::forbidden("endpoint is disabled"),
            ctx.is_debug(),
        );
    }
    // Per-endpoint per-minute rate limit.
    let minute = Utc::now().timestamp() / 60;
    let counter_key = format!("{id}:{minute}");
    let count = {
        let mut counter = state.rate_counters.entry(counter_key).or_insert(0);
        *counter += 1;
        *counter
    };
    if count > u64::from(endpoint.rate_limit) {
        return api_json::error_response(
            &UnifiedError::new(
                format!(
                    "rate limit exceeded: at most {} requests per minute for this endpoint",
                    endpoint.rate_limit
                ),
                429,
            )
            .with_metadata("endpoint_id", json!(id.to_string()))
            .with_metadata("rate_limit", json!(endpoint.rate_limit)),
            ctx.is_debug(),
        );
    }

    let errors = validate_payload(&payload, &endpoint.rules);
    let valid = errors.is_empty();
    endpoint.execution_count += 1;
    endpoint.updated_at = Utc::now();
    state.endpoints.insert(id, endpoint.clone());
    ctx.hooks.emit(
        "api-manager.endpoint.executed",
        json!({
            "endpoint_id": id.to_string(),
            "method": endpoint.method,
            "path": endpoint.path,
            "version": endpoint.version,
            "valid": valid,
            "error_count": errors.len(),
        }),
    );
    api_json::success_response(
        json!({
            "endpoint": {
                "id": endpoint.id,
                "method": endpoint.method,
                "path": endpoint.path,
                "version": endpoint.version,
            },
            "valid": valid,
            "errors": errors,
            "echo": payload,
            "dispatch": "validation_only",
            "note": "Payload validation only. Dispatching the request into a registered core route handler is out of scope for this milestone.",
        }),
        200,
    )
}

async fn list_versions(state: web::Data<ApiManagerState>) -> HttpResponse {
    let mut grouped: std::collections::BTreeMap<String, Vec<Value>> =
        std::collections::BTreeMap::new();
    for entry in state.endpoints.iter() {
        let endpoint = entry.value();
        grouped
            .entry(endpoint.version.clone())
            .or_default()
            .push(json!({
                "id": endpoint.id,
                "method": endpoint.method,
                "path": endpoint.path,
                "description": endpoint.description,
                "enabled": endpoint.enabled,
                "rate_limit": endpoint.rate_limit,
            }));
    }
    let versions: Vec<Value> = grouped
        .into_iter()
        .map(|(version, endpoints)| {
            let count = endpoints.len();
            json!({
                "version": version,
                "count": count,
                "endpoints": endpoints,
            })
        })
        .collect();
    api_json::success_response(json!({ "versions": versions }), 200)
}

// ---------------------------------------------------------------------------
// Version registry handlers
// ---------------------------------------------------------------------------

async fn list_api_versions(
    state: web::Data<ApiManagerState>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    state.ensure_seeded_versions();
    let params = query.into_inner();
    let search = params.search.clone();
    let enabled_filter = params.filters.get("enabled").cloned();
    let deprecated_filter = params.filters.get("deprecated").cloned();
    let mut items: Vec<ApiVersion> = state
        .versions
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|version| {
            if let Some(enabled) = &enabled_filter {
                if version.enabled != (enabled == "true") {
                    return false;
                }
            }
            if let Some(deprecated) = &deprecated_filter {
                if version.deprecated != (deprecated == "true") {
                    return false;
                }
            }
            if let Some(needle) = &search {
                let needle = needle.to_lowercase();
                return version.id.to_lowercase().contains(&needle)
                    || version.notes.to_lowercase().contains(&needle);
            }
            true
        })
        .collect();
    items.sort_by_key(|version| version.number);
    let pagination = params.normalized();
    let total = items.len() as u64;
    let start = ((pagination.page - 1) * pagination.per_page) as usize;
    let page_items: Vec<ApiVersion> = items
        .into_iter()
        .skip(start)
        .take(pagination.per_page as usize)
        .collect();
    api_json::success_response(PageResult::new(page_items, total, &pagination).to_json(), 200)
}

async fn create_api_version(
    state: web::Data<ApiManagerState>,
    ctx: web::Data<Arc<AppContext>>,
    input: web::Json<CreateApiVersionInput>,
) -> HttpResponse {
    state.ensure_seeded_versions();
    let input = input.into_inner();
    let outcome = (|| -> Result<ApiVersion, UnifiedError> {
        let raw_id = input
            .id
            .clone()
            .or(input.version.clone())
            .ok_or_else(|| UnifiedError::bad_request("provide an id such as \"v1\""))?;
        let id = validate_registry_version_id(&raw_id)?;
        if state.versions.contains_key(&id) {
            return Err(UnifiedError::conflict(format!(
                "API version \"{id}\" already exists"
            )));
        }
        let now = Utc::now();
        // Start from the copied version's settings when requested,
        // otherwise from the blank defaults below.
        let mut version = ApiVersion {
            id: id.clone(),
            number: parse_version_number(&id).unwrap_or(0),
            enabled: true,
            deprecated: false,
            sunset_date: None,
            rate_limit_per_minute: 1000,
            auth_required: true,
            notes: String::new(),
            created_at: now,
            updated_at: now,
        };
        if let Some(copy_from) = &input.copy_from {
            let source = state
                .versions
                .get(copy_from.trim())
                .map(|entry| entry.value().clone())
                .ok_or_else(|| {
                    UnifiedError::not_found(format!(
                        "API version \"{copy_from}\" does not exist, so its settings cannot be copied"
                    ))
                })?;
            version.enabled = source.enabled;
            version.deprecated = source.deprecated;
            version.sunset_date = source.sunset_date;
            version.rate_limit_per_minute = source.rate_limit_per_minute;
            version.auth_required = source.auth_required;
            version.notes = format!("Copied from version \"{}\". {}", source.id, source.notes);
        }
        if let Some(enabled) = input.enabled {
            version.enabled = enabled;
        }
        if let Some(deprecated) = input.deprecated {
            version.deprecated = deprecated;
        }
        if let Some(sunset_date) = input.sunset_date {
            if sunset_date <= now {
                return Err(UnifiedError::bad_request(
                    "sunset_date must be in the future",
                ));
            }
            version.sunset_date = Some(sunset_date);
        }
        if version.deprecated && version.sunset_date.is_none() {
            return Err(UnifiedError::bad_request(
                "sunset_date is required when creating a deprecated version",
            ));
        }
        if let Some(rate_limit) = input.rate_limit_per_minute {
            if rate_limit == 0 || rate_limit > 1_000_000 {
                return Err(UnifiedError::bad_request(
                    "rate_limit_per_minute must be between 1 and 1000000",
                ));
            }
            version.rate_limit_per_minute = rate_limit;
        }
        if let Some(auth_required) = input.auth_required {
            version.auth_required = auth_required;
        }
        if let Some(notes) = input.notes {
            if notes.chars().count() > 2000 {
                return Err(UnifiedError::bad_request(
                    "notes must be at most 2000 characters",
                ));
            }
            version.notes = notes;
        }
        state.versions.insert(id.clone(), version.clone());
        ctx.hooks.emit(
            "api.version.created",
            json!({
                "version": id,
                "copy_from": input.copy_from,
                "enabled": version.enabled,
                "rate_limit_per_minute": version.rate_limit_per_minute,
                "auth_required": version.auth_required,
            }),
        );
        Ok(version)
    })();
    match outcome {
        Ok(version) => api_json::success_response(version, 201),
        Err(error) => api_json::error_response(&error, ctx.is_debug()),
    }
}

async fn get_api_version(
    state: web::Data<ApiManagerState>,
    version: web::Path<String>,
) -> HttpResponse {
    state.ensure_seeded_versions();
    let version = version.into_inner();
    match state.versions.get(&version) {
        Some(entry) => api_json::success_response(entry.value().clone(), 200),
        None => api_json::error_response(
            &UnifiedError::not_found(format!("API version \"{version}\" does not exist")),
            false,
        ),
    }
}

async fn update_api_version(
    state: web::Data<ApiManagerState>,
    ctx: web::Data<Arc<AppContext>>,
    version: web::Path<String>,
    input: web::Json<UpdateApiVersionInput>,
) -> HttpResponse {
    state.ensure_seeded_versions();
    let version_id = version.into_inner();
    let input = input.into_inner();
    let outcome = (|| -> Result<ApiVersion, UnifiedError> {
        let mut version = state
            .versions
            .get(&version_id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| {
                UnifiedError::not_found(format!("API version \"{version_id}\" does not exist"))
            })?;
        let was_deprecated = version.deprecated;
        if let Some(enabled) = input.enabled {
            version.enabled = enabled;
        }
        if let Some(deprecated) = input.deprecated {
            version.deprecated = deprecated;
        }
        if let Some(sunset_date) = input.sunset_date {
            if sunset_date <= Utc::now() {
                return Err(UnifiedError::bad_request(
                    "sunset_date must be in the future",
                ));
            }
            version.sunset_date = Some(sunset_date);
        }
        if version.deprecated && version.sunset_date.is_none() {
            return Err(UnifiedError::bad_request(
                "sunset_date is required when deprecating a version",
            ));
        }
        if let Some(rate_limit) = input.rate_limit_per_minute {
            if rate_limit == 0 || rate_limit > 1_000_000 {
                return Err(UnifiedError::bad_request(
                    "rate_limit_per_minute must be between 1 and 1000000",
                ));
            }
            version.rate_limit_per_minute = rate_limit;
        }
        if let Some(auth_required) = input.auth_required {
            version.auth_required = auth_required;
        }
        if let Some(notes) = input.notes {
            if notes.chars().count() > 2000 {
                return Err(UnifiedError::bad_request(
                    "notes must be at most 2000 characters",
                ));
            }
            version.notes = notes;
        }
        version.updated_at = Utc::now();
        state.versions.insert(version_id.clone(), version.clone());
        if version.deprecated && !was_deprecated {
            ctx.hooks.emit(
                "api.version.deprecated",
                json!({
                    "version": version_id,
                    "sunset_date": version.sunset_date.map(|date| date.to_rfc3339()),
                }),
            );
        }
        Ok(version)
    })();
    match outcome {
        Ok(version) => api_json::success_response(version, 200),
        Err(error) => api_json::error_response(&error, ctx.is_debug()),
    }
}

async fn deprecate_api_version(
    state: web::Data<ApiManagerState>,
    ctx: web::Data<Arc<AppContext>>,
    version: web::Path<String>,
    input: web::Json<DeprecateVersionInput>,
) -> HttpResponse {
    state.ensure_seeded_versions();
    let version_id = version.into_inner();
    let input = input.into_inner();
    let outcome = (|| -> Result<ApiVersion, UnifiedError> {
        let mut version = state
            .versions
            .get(&version_id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| {
                UnifiedError::not_found(format!("API version \"{version_id}\" does not exist"))
            })?;
        if version.deprecated {
            return Err(UnifiedError::conflict(format!(
                "API version \"{version_id}\" is already deprecated"
            )));
        }
        if input.sunset_date <= Utc::now() {
            return Err(UnifiedError::bad_request(
                "sunset_date must be in the future",
            ));
        }
        version.deprecated = true;
        version.sunset_date = Some(input.sunset_date);
        if let Some(notes) = input.notes {
            if notes.chars().count() > 2000 {
                return Err(UnifiedError::bad_request(
                    "notes must be at most 2000 characters",
                ));
            }
            version.notes = notes;
        }
        version.updated_at = Utc::now();
        state.versions.insert(version_id.clone(), version.clone());
        ctx.hooks.emit(
            "api.version.deprecated",
            json!({
                "version": version_id,
                "sunset_date": input.sunset_date.to_rfc3339(),
            }),
        );
        Ok(version)
    })();
    match outcome {
        Ok(version) => api_json::success_response(version, 200),
        Err(error) => api_json::error_response(&error, ctx.is_debug()),
    }
}

async fn delete_api_version(
    state: web::Data<ApiManagerState>,
    version: web::Path<String>,
) -> HttpResponse {
    state.ensure_seeded_versions();
    let version_id = version.into_inner();
    let known = state.version_ids();
    // Refuse only when a custom endpoint still covers this version.
    // Built-in endpoints declare "all versions", so they adapt to
    // the remaining versions automatically and never block deletion.
    let referencing: Vec<String> = state
        .endpoints
        .iter()
        .filter(|entry| {
            entry
                .value()
                .version_support
                .supports(&version_id, &known)
        })
        .map(|entry| format!("{} {}", entry.value().method, entry.value().path))
        .collect();
    if !referencing.is_empty() {
        return api_json::error_response(
            &UnifiedError::conflict(format!(
                "API version \"{version_id}\" cannot be deleted: custom endpoints still cover it"
            ))
            .with_metadata("referencing_endpoints", json!(referencing)),
            false,
        );
    }
    match state.versions.remove(&version_id) {
        Some((_, version)) => api_json::success_response(
            json!({ "id": version.id, "deleted": true }),
            200,
        ),
        None => api_json::error_response(
            &UnifiedError::not_found(format!("API version \"{version_id}\" does not exist")),
            false,
        ),
    }
}

// ---------------------------------------------------------------------------
// Global prefix handlers
// ---------------------------------------------------------------------------

async fn get_prefix(state: web::Data<ApiManagerState>) -> HttpResponse {
    state.ensure_seeded_versions();
    let settings = state
        .prefix
        .read()
        .map(|settings| settings.clone())
        .unwrap_or_default();
    let migrate_internal = settings.migrate_internal;
    api_json::success_response(
        json!({
            "prefix": settings.prefix,
            "migrate_internal": migrate_internal,
            "updated_at": settings.updated_at,
            "example_public_url": state.public_url("v1", "/example"),
            "internal_routes_unchanged": !migrate_internal,
            "note": if migrate_internal {
                "migrate_internal is enabled: on the next startup the server factory must remount the internal /api/v1/<module> routes under the new prefix (see the remount snippet in the coordinator report). This setting alone does not move routes that Actix already registered."
            } else {
                "The prefix governs the public versioned surface only. Internal module routes stay mounted at /api/v1/<module>."
            },
        }),
        200,
    )
}

async fn update_prefix(
    state: web::Data<ApiManagerState>,
    input: web::Json<UpdatePrefixInput>,
) -> HttpResponse {
    state.ensure_seeded_versions();
    let input = input.into_inner();
    let outcome = (|| -> Result<ApiPrefixSettings, UnifiedError> {
        let prefix = validate_prefix(&input.prefix)?;
        let mut settings = state
            .prefix
            .read()
            .map(|settings| settings.clone())
            .unwrap_or_default();
        settings.prefix = prefix;
        if let Some(migrate_internal) = input.migrate_internal {
            settings.migrate_internal = migrate_internal;
        }
        settings.updated_at = Utc::now();
        *state
            .prefix
            .write()
            .map_err(|_| UnifiedError::internal("prefix setting lock is poisoned"))? =
            settings.clone();
        Ok(settings)
    })();
    match outcome {
        Ok(settings) => api_json::success_response(
            json!({
                "prefix": settings.prefix,
                "migrate_internal": settings.migrate_internal,
                "updated_at": settings.updated_at,
                "example_public_url": state.public_url("v1", "/example"),
                "internal_routes_unchanged": !settings.migrate_internal,
                "note": if settings.migrate_internal {
                    "migrate_internal is enabled: on the next startup the server factory must remount the internal /api/v1/<module> routes under the new prefix. This setting alone does not move routes that Actix already registered."
                } else {
                    "The prefix governs the public versioned surface only. Internal module routes stay mounted at /api/v1/<module>."
                },
            }),
            200,
        ),
        Err(error) => api_json::error_response(&error, false),
    }
}

// ---------------------------------------------------------------------------
// Version choices, endpoint registry, resolver
// ---------------------------------------------------------------------------

/// `GET /version-choices`: the shape the admin UI creation form
/// consumes when an admin picks which versions a custom endpoint
/// covers: one existing version, all versions, or a multi-select.
async fn get_version_choices(state: web::Data<ApiManagerState>) -> HttpResponse {
    state.ensure_seeded_versions();
    let versions: Vec<VersionChoice> = state
        .versions
        .iter()
        .map(|entry| {
            let version = entry.value();
            VersionChoice {
                id: version.id.clone(),
                label: version.id.clone(),
                enabled: version.enabled,
                deprecated: version.deprecated,
                sunset_date: version.sunset_date,
            }
        })
        .collect();
    let mut versions = versions;
    versions.sort_by_key(|choice| parse_version_number(&choice.id).unwrap_or(0));
    let default_version = versions
        .iter()
        .find(|choice| choice.enabled && !choice.deprecated)
        .map(|choice| choice.id.clone());
    api_json::success_response(
        json!({
            "modes": [
                { "id": "single", "label": "One version", "description": "The endpoint is available in exactly one version." },
                { "id": "all", "label": "All versions", "description": "The endpoint is available in every registered version." },
                { "id": "multi", "label": "Multiple versions", "description": "Pick two or more versions from the list." },
            ],
            "versions": versions,
            "default_mode": "single",
            "default_version": default_version,
        }),
        200,
    )
}

async fn list_registry(
    state: web::Data<ApiManagerState>,
    ctx: web::Data<Arc<AppContext>>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    state.ensure_seeded_versions();
    state.ensure_collected(&ctx.modules.route_catalog(), &ctx.modules);
    let params = query.into_inner();
    let search = params.search.clone();
    let module_filter = params.filters.get("module").cloned();
    let method_filter = params.filters.get("method").cloned();
    let version_filter = params.filters.get("version").cloned();
    let auth_filter = params.filters.get("auth_required").cloned();
    let source_filter = params.filters.get("source").cloned();
    let mut rows: Vec<RegistryRow> = state
        .registry_rows()
        .into_iter()
        .filter(|row| {
            if let Some(module) = &module_filter {
                if row.owner_module != *module {
                    return false;
                }
            }
            if let Some(method) = &method_filter {
                if row.method != method.to_uppercase() {
                    return false;
                }
            }
            if let Some(version) = &version_filter {
                if row.version != *version {
                    return false;
                }
            }
            if let Some(auth_required) = &auth_filter {
                if row.auth_required != (auth_required == "true") {
                    return false;
                }
            }
            if let Some(source) = &source_filter {
                if row.source != *source {
                    return false;
                }
            }
            if let Some(needle) = &search {
                let needle = needle.to_lowercase();
                return row.path.to_lowercase().contains(&needle)
                    || row.description.to_lowercase().contains(&needle)
                    || row.owner_module.to_lowercase().contains(&needle);
            }
            true
        })
        .collect();
    let sort = params.allowed_sort(&["path", "version", "method"], "path");
    let descending = params.normalized().order == "desc";
    rows.sort_by(|a, b| {
        let ordering = match sort.as_str() {
            "version" => parse_version_number(&a.version)
                .unwrap_or(0)
                .cmp(&parse_version_number(&b.version).unwrap_or(0))
                .then(a.path.cmp(&b.path)),
            "method" => a.method.cmp(&b.method).then(a.path.cmp(&b.path)),
            _ => a.path.cmp(&b.path).then(a.method.cmp(&b.method)),
        };
        if descending {
            ordering.reverse()
        } else {
            ordering
        }
    });
    // Per-version endpoint counts for the grouped overview.
    let mut grouped: BTreeMap<String, u64> = BTreeMap::new();
    for row in state.registry_rows() {
        *grouped.entry(row.version).or_insert(0) += 1;
    }
    let versions: Vec<Value> = grouped
        .into_iter()
        .map(|(version, count)| {
            let record = state.versions.get(&version);
            json!({
                "version": version,
                "count": count,
                "enabled": record.as_ref().map(|entry| entry.enabled).unwrap_or(false),
                "deprecated": record.as_ref().map(|entry| entry.deprecated).unwrap_or(false),
            })
        })
        .collect();
    let pagination = params.normalized();
    let total = rows.len() as u64;
    let start = ((pagination.page - 1) * pagination.per_page) as usize;
    let page_items: Vec<RegistryRow> = rows
        .into_iter()
        .skip(start)
        .take(pagination.per_page as usize)
        .collect();
    let page = PageResult::new(page_items, total, &pagination);
    api_json::success_response(
        json!({
            "items": page.items,
            "pagination": page.pagination,
            "versions": versions,
        }),
        200,
    )
}

async fn get_registry_detail(
    state: web::Data<ApiManagerState>,
    ctx: web::Data<Arc<AppContext>>,
    query: web::Query<ResolveQuery>,
) -> HttpResponse {
    state.ensure_seeded_versions();
    state.ensure_collected(&ctx.modules.route_catalog(), &ctx.modules);
    let query = query.into_inner();
    let method = match query.method {
        Some(method) => match validate_method(&method) {
            Ok(method) => method,
            Err(error) => return api_json::error_response(&error, ctx.is_debug()),
        },
        None => {
            return api_json::error_response(
                &UnifiedError::bad_request("query parameter \"method\" is required"),
                ctx.is_debug(),
            );
        }
    };
    let path = match query.path {
        Some(path) => match validate_path(&path) {
            Ok(path) => path,
            Err(error) => return api_json::error_response(&error, ctx.is_debug()),
        },
        None => {
            return api_json::error_response(
                &UnifiedError::bad_request("query parameter \"path\" is required"),
                ctx.is_debug(),
            );
        }
    };
    let known = state.version_ids();
    let rows: Vec<RegistryRow> = state
        .registry_rows()
        .into_iter()
        .filter(|row| {
            row.method == method
                && (normalize_path_template(&row.path) == normalize_path_template(&path)
                    || normalize_path_template(&ApiManagerState::relative_path(&row.path))
                        == normalize_path_template(&path)
                    || normalize_path_template(&row.path)
                        == normalize_path_template(&ApiManagerState::relative_path(&path)))
        })
        .collect();
    if rows.is_empty() {
        return api_json::error_response(
            &UnifiedError::not_found(format!("No endpoint matches {method} {path}"))
                .with_metadata("method", json!(method))
                .with_metadata("path", json!(path)),
            ctx.is_debug(),
        );
    }
    let available_in: Vec<String> = rows.iter().map(|row| row.version.clone()).collect();
    let first = rows[0].clone();
    let detail = if let Some(version) = query.version {
        match rows.into_iter().find(|row| row.version == version) {
            Some(row) => row,
            None => {
                return api_json::error_response(
                    &UnifiedError::not_found(format!(
                        "Endpoint {method} {path} is not available in API version \"{version}\""
                    ))
                    .with_metadata("available_in", json!(available_in))
                    .with_metadata("version", json!(version)),
                    ctx.is_debug(),
                );
            }
        }
    } else {
        first
    };
    let _ = known;
    let version_support = detail.version_support.clone();
    let owner_module = detail.owner_module.clone();
    let auth_required = detail.auth_required;
    let rate_limit = detail.rate_limit;
    api_json::success_response(
        json!({
            "endpoint": detail,
            "available_in": available_in,
            "version_support": version_support,
            "owner_module": owner_module,
            "auth_required": auth_required,
            "rate_limit": rate_limit,
        }),
        200,
    )
}

async fn resolve_handler(
    state: web::Data<ApiManagerState>,
    ctx: web::Data<Arc<AppContext>>,
    query: web::Query<ResolveQuery>,
) -> HttpResponse {
    state.ensure_seeded_versions();
    state.ensure_collected(&ctx.modules.route_catalog(), &ctx.modules);
    let query = query.into_inner();
    let method = match query.method {
        Some(method) => match validate_method(&method) {
            Ok(method) => method,
            Err(error) => return api_json::error_response(&error, ctx.is_debug()),
        },
        None => "GET".to_string(),
    };
    let version = match query.version {
        Some(version) => version.trim().to_string(),
        None => {
            return api_json::error_response(
                &UnifiedError::bad_request("query parameter \"version\" is required"),
                ctx.is_debug(),
            );
        }
    };
    let raw_path = match query.path {
        Some(path) => path,
        None => {
            return api_json::error_response(
                &UnifiedError::bad_request("query parameter \"path\" is required"),
                ctx.is_debug(),
            );
        }
    };
    let (carried_version, relative_path) = strip_public_prefix(&state, &raw_path);
    let version = carried_version.unwrap_or(version);
    match state.resolve_endpoint(&method, &version, &relative_path) {
        Ok(resolved) => api_json::success_response(
            json!({
                "matched": true,
                "method": method,
                "version": version,
                "path": relative_path,
                "public_path": resolved.endpoint.public_path,
                "endpoint": resolved.endpoint,
                "deprecated_warning": resolved.deprecated_warning,
            }),
            200,
        ),
        Err(error) => api_json::error_response(&error, ctx.is_debug()),
    }
}

/// Public versioned gateway: resolves any request under
/// `/public/{version}/{path...}` against the registry and reports
/// the match. Resolution only -- dispatching into the matched core
/// route handler is out of scope for this milestone, exactly like
/// `POST /endpoints/{id}/execute`.
async fn resolve_public(
    state: web::Data<ApiManagerState>,
    ctx: web::Data<Arc<AppContext>>,
    request: HttpRequest,
) -> HttpResponse {
    state.ensure_seeded_versions();
    state.ensure_collected(&ctx.modules.route_catalog(), &ctx.modules);
    let version = request
        .match_info()
        .get("version")
        .unwrap_or("")
        .to_string();
    let method = request.method().to_string();
    let tail = request
        .match_info()
        .get("path")
        .unwrap_or("")
        .to_string();
    let relative_path = format!("/{tail}");
    match state.resolve_endpoint(&method, &version, &relative_path) {
        Ok(resolved) => {
            // The public gateway enforces access control itself (the
            // middleware exempts this module's prefix): a resolved
            // endpoint with no allow rule, or a missing, invalid, or
            // out-of-scope key, is rejected here, before the
            // resolution is returned. The check runs against the
            // public path (`/{prefix}/{version}/{tail}`), not the
            // gateway's own mount path.
            let presented_key = request
                .headers()
                .get(API_KEY_HEADER)
                .and_then(|value| value.to_str().ok());
            let client_ip = request.peer_addr().map(|addr| addr.ip().to_string());
            let public_path = state.public_url(&version, &relative_path);
            if let Err(error) =
                state.check_public_access(&method, &public_path, presented_key, client_ip)
            {
                return api_json::error_response(&error, ctx.is_debug());
            }
            api_json::success_response(
            json!({
                "matched": true,
                "method": method,
                "version": version,
                "path": relative_path,
                "public_path": resolved.endpoint.public_path,
                "endpoint": resolved.endpoint,
                "deprecated_warning": resolved.deprecated_warning,
                "dispatch": "resolution_only",
                "note": "Resolution only. Dispatching the request into the matched core route handler is out of scope for this milestone.",
            }),
            200,
        )
        }
        Err(error) => api_json::error_response(&error, ctx.is_debug()),
    }
}

// ---------------------------------------------------------------------------
// API key handlers.
//
// These endpoints are a thin adapter over the API Keys module
// (`crate::modules::apikeys`), the single source of truth for key
// storage, hashing, and verification. No key material is stored
// here; the handlers only translate this module's request and
// response shapes to and from the shared key service. The
// `api-manager` endpoints keep their historic field names (`key`
// for the full value shown once, `revoked` as a boolean) and gain
// the shared store's `status`, `rate_tier`,
// `requests_per_minute`, and `usage_count` fields.
async fn create_key(
    ctx: web::Data<Arc<AppContext>>,
    input: web::Json<CreateKeyInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let input = input.into_inner();
    // Validation (name length, scope shape, expiry range) lives in
    // the shared service; scopes are validated against the
    // registered module ids, and an omitted scope list defaults to
    // `["*"]` there.
    match crate::modules::apikeys::create_api_key(
        crate::modules::apikeys::CreateApiKeyParams {
            name: input.name.trim().to_string(),
            owner_id: None,
            scopes: input.scopes,
            rate_tier: None,
            requests_per_minute: None,
            expires_in_days: input.expires_in_days,
        },
        &ctx.modules.ids(),
    ) {
        Ok(created) => {
            let key = created.key;
            // The full key is returned exactly once, at creation time.
            api_json::success_response(
                json!({
                    "id": key.id,
                    "name": key.name,
                    "key": created.full_key,
                    "prefix": key.prefix,
                    "scopes": key.scopes,
                    "status": key.status,
                    "rate_tier": key.rate_tier,
                    "requests_per_minute": key.requests_per_minute,
                    "created_at": key.created_at,
                    "expires_at": key.expires_at,
                    "revoked": key.status == "revoked",
                }),
                201,
            )
        }
        Err(error) => api_json::error_response(&error, debug),
    }
}

async fn list_keys(query: web::Query<PaginationParams>) -> HttpResponse {
    let params = query.into_inner();
    let search = params.search.clone();
    let revoked_filter = params.filters.get("revoked").cloned();
    let mut items: Vec<crate::modules::apikeys::ApiKeyView> =
        crate::modules::apikeys::all_key_views()
            .into_iter()
            .filter(|key| {
                if let Some(revoked) = &revoked_filter {
                    let wanted = revoked == "true";
                    if (key.status == "revoked") != wanted {
                        return false;
                    }
                }
                if let Some(needle) = &search {
                    let needle = needle.to_lowercase();
                    return key.name.to_lowercase().contains(&needle)
                        || key.prefix.to_lowercase().contains(&needle);
                }
                true
            })
            .collect();
    items.sort_by(|a, b| b.created_at.cmp(&a.created_at));
    let items: Vec<Value> = items
        .into_iter()
        .map(|key| {
            json!({
                "id": key.id,
                "name": key.name,
                "prefix": key.prefix,
                "scopes": key.scopes,
                "status": key.status,
                "rate_tier": key.rate_tier,
                "requests_per_minute": key.requests_per_minute,
                "created_at": key.created_at,
                "expires_at": key.expires_at,
                "last_used_at": key.last_used_at,
                "usage_count": key.usage_count,
                "revoked": key.status == "revoked",
            })
        })
        .collect();
    let pagination = params.normalized();
    let total = items.len() as u64;
    let start = ((pagination.page - 1) * pagination.per_page) as usize;
    let page_items: Vec<Value> = items
        .into_iter()
        .skip(start)
        .take(pagination.per_page as usize)
        .collect();
    api_json::success_response(PageResult::new(page_items, total, &pagination).to_json(), 200)
}

async fn revoke_key(id: web::Path<Uuid>) -> HttpResponse {
    let id = id.into_inner();
    match crate::modules::apikeys::revoke_api_key(id) {
        Ok(view) => api_json::success_response(
            json!({ "id": view.id, "revoked": true }),
            200,
        ),
        Err(error) => api_json::error_response(&error, false),
    }
}

/// Audit one API-key lifecycle event emitted by the API Keys
/// module. The payload carries no actor, so the key owner (when
/// known) is recorded as the actor and `system` otherwise. The
/// entry is persisted through `record_access_audit` like every
/// other access-control change.
fn on_apikey_created(state: &ApiManagerState, ctx: &mut HookContext) {
    let key_id = ctx
        .get("key_id")
        .and_then(|value| value.as_str())
        .unwrap_or("")
        .to_string();
    let actor = ctx
        .get("owner_id")
        .and_then(|value| value.as_str())
        .filter(|text| !text.is_empty())
        .unwrap_or("system")
        .to_string();
    state.record_access_audit(
        &actor,
        "key_created",
        &key_id,
        json!({
            "name": ctx.get("name"),
            "scopes": ctx.get("scopes"),
            "rate_tier": ctx.get("rate_tier"),
            "expires_at": ctx.get("expires_at"),
        }),
    );
    ctx.add_result(json!({"audited": true, "action": "key_created", "target": key_id}));
}

/// Audit an `apikey.revoked` event (manual revocation, or a rotation
/// grace period expiring) as a `key_revoked` audit entry.
fn on_apikey_revoked(state: &ApiManagerState, ctx: &mut HookContext) {
    let key_id = ctx
        .get("key_id")
        .and_then(|value| value.as_str())
        .unwrap_or("")
        .to_string();
    state.record_access_audit(
        "system",
        "key_revoked",
        &key_id,
        json!({ "reason": ctx.get("reason") }),
    );
    ctx.add_result(json!({"audited": true, "action": "key_revoked", "target": key_id}));
}

/// Audit an `apikey.rotated` event: rotation creates a replacement
/// key, so the new key is audited as `key_created` with the old key
/// recorded in the detail. (The rotation path emits only
/// `apikey.rotated`, not `apikey.created`, so there is no double
/// audit for the same key.)
fn on_apikey_rotated(state: &ApiManagerState, ctx: &mut HookContext) {
    let new_key_id = ctx
        .get("new_key_id")
        .and_then(|value| value.as_str())
        .unwrap_or("")
        .to_string();
    state.record_access_audit(
        "system",
        "key_created",
        &new_key_id,
        json!({
            "rotated_from": ctx.get("old_key_id"),
            "grace_until": ctx.get("grace_until"),
        }),
    );
    ctx.add_result(json!({"audited": true, "action": "key_created", "target": new_key_id}));
}

// ---------------------------------------------------------------------------
// Access-control handlers.
//
// Rules and the audit trail live on `ApiManagerState` (in memory)
// and are persisted to the migration 000004 tables
// (`api_access_rules`, `api_access_audit`) whenever a database pool
// is attached; see `load_access_from_db` / `persist_access_rule` /
// `persist_access_audit_entry`. Per-key scopes and rate limits live
// in the API Keys module; these handlers read them through its
// views and write them through its `update_api_key_access` service
// function, so this module never touches key records directly. Key
// material is never returned: only the key id, name, scopes, tier,
// and limit.
// ---------------------------------------------------------------------------

async fn list_access_rules(state: web::Data<ApiManagerState>) -> HttpResponse {
    let mut rules: Vec<ApiAccessRule> = state
        .access_rules
        .iter()
        .map(|entry| entry.value().clone())
        .collect();
    rules.sort_by(|a, b| {
        a.method
            .cmp(&b.method)
            .then(a.path.cmp(&b.path))
            .then(a.version.cmp(&b.version))
    });
    api_json::success_response(json!({ "rules": rules }), 200)
}

async fn create_access_rule(
    state: web::Data<ApiManagerState>,
    input: web::Json<CreateAccessRuleInput>,
) -> HttpResponse {
    let debug = false;
    let input = input.into_inner();
    let result: Result<ApiAccessRule, UnifiedError> = (|| {
        let method = validate_method(&input.method)?;
        let path = validate_path(&input.path)?;
        let version = match &input.version {
            Some(version) => {
                let version = validate_version(version)?;
                if !state.versions.contains_key(&version) {
                    return Err(UnifiedError::bad_request(format!(
                        "unknown API version \"{version}\""
                    )));
                }
                Some(version)
            }
            None => None,
        };
        let key = ApiManagerState::access_rule_key(&method, &path, version.as_deref());
        if state.access_rules.contains_key(&key) {
            return Err(UnifiedError::conflict(format!(
                "an access rule already exists for {method} {path}{}",
                version
                    .as_ref()
                    .map(|version| format!("@{version}"))
                    .unwrap_or_default()
            )));
        }
        let actor = input
            .actor
            .clone()
            .unwrap_or_else(|| "admin".to_string());
        let now = Utc::now();
        let rule = ApiAccessRule {
            id: Uuid::now_v7(),
            method,
            path,
            version,
            allowed: input.allowed,
            public_access: input.public_access,
            actor: actor.clone(),
            reason: input.reason.clone(),
            created_at: now,
            updated_at: now,
        };
        state.access_rules.insert(key, rule.clone());
        state.persist_access_rule(&rule);
        state.record_access_audit(
            &actor,
            "rule_created",
            &format!("{} {}@{}", rule.method, rule.path, rule.version.as_deref().unwrap_or("*")),
            json!({
                "rule_id": rule.id,
                "allowed": rule.allowed,
                "public_access": rule.public_access,
                "reason": rule.reason,
            }),
        );
        Ok(rule)
    })();
    match result {
        Ok(rule) => api_json::success_response(json!({ "rule": rule }), 201),
        Err(error) => api_json::error_response(&error, debug),
    }
}

async fn update_access_rule(
    state: web::Data<ApiManagerState>,
    id: web::Path<Uuid>,
    input: web::Json<UpdateAccessRuleInput>,
) -> HttpResponse {
    let debug = false;
    let id = id.into_inner();
    let input = input.into_inner();
    let result: Result<ApiAccessRule, UnifiedError> = (|| {
        let key = state
            .access_rules
            .iter()
            .find(|entry| entry.value().id == id)
            .map(|entry| entry.key().clone())
            .ok_or_else(|| UnifiedError::not_found("access rule not found"))?;
        let mut rule = state
            .access_rules
            .get(&key)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("access rule not found"))?;
        let mut changed: Vec<&str> = Vec::new();
        if let Some(allowed) = input.allowed {
            if rule.allowed != allowed {
                rule.allowed = allowed;
                changed.push("allowed");
            }
        }
        if let Some(public_access) = input.public_access {
            if rule.public_access != public_access {
                rule.public_access = public_access;
                changed.push("public_access");
            }
        }
        if let Some(actor) = &input.actor {
            rule.actor = actor.clone();
        }
        if let Some(reason) = &input.reason {
            rule.reason = Some(reason.clone());
        }
        rule.updated_at = Utc::now();
        let actor = rule.actor.clone();
        state.access_rules.insert(key, rule.clone());
        state.persist_access_rule(&rule);
        state.record_access_audit(
            &actor,
            "rule_updated",
            &format!("{} {}@{}", rule.method, rule.path, rule.version.as_deref().unwrap_or("*")),
            json!({
                "rule_id": rule.id,
                "changed": changed,
                "allowed": rule.allowed,
                "public_access": rule.public_access,
                "reason": rule.reason,
            }),
        );
        Ok(rule)
    })();
    match result {
        Ok(rule) => api_json::success_response(json!({ "rule": rule }), 200),
        Err(error) => api_json::error_response(&error, debug),
    }
}

async fn delete_access_rule(
    state: web::Data<ApiManagerState>,
    id: web::Path<Uuid>,
) -> HttpResponse {
    let debug = false;
    let id = id.into_inner();
    let result: Result<ApiAccessRule, UnifiedError> = (|| {
        let key = state
            .access_rules
            .iter()
            .find(|entry| entry.value().id == id)
            .map(|entry| entry.key().clone())
            .ok_or_else(|| UnifiedError::not_found("access rule not found"))?;
        let (_, rule) = state
            .access_rules
            .remove(&key)
            .ok_or_else(|| UnifiedError::not_found("access rule not found"))?;
        state.delete_access_rule_from_db(&rule);
        state.record_access_audit(
            &rule.actor,
            "rule_deleted",
            &format!("{} {}@{}", rule.method, rule.path, rule.version.as_deref().unwrap_or("*")),
            json!({ "rule_id": rule.id }),
        );
        Ok(rule)
    })();
    match result {
        Ok(rule) => api_json::success_response(json!({ "deleted": true, "id": rule.id }), 200),
        Err(error) => api_json::error_response(&error, debug),
    }
}

async fn list_access_audit(
    state: web::Data<ApiManagerState>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let params = query.into_inner();
    let mut entries: Vec<ApiAccessAuditEntry> = state
        .access_audit
        .read()
        .map(|audit| audit.clone())
        .unwrap_or_default();
    entries.sort_by(|a, b| b.created_at.cmp(&a.created_at));
    let pagination = params.normalized();
    let total = entries.len() as u64;
    let start = ((pagination.page - 1) * pagination.per_page) as usize;
    let page_items: Vec<ApiAccessAuditEntry> = entries
        .into_iter()
        .skip(start)
        .take(pagination.per_page as usize)
        .collect();
    api_json::success_response(PageResult::new(page_items, total, &pagination).to_json(), 200)
}

async fn get_key_access(id: web::Path<Uuid>) -> HttpResponse {
    let debug = false;
    let id = id.into_inner();
    let result: Result<Value, UnifiedError> = (|| {
        let view = crate::modules::apikeys::all_key_views()
            .into_iter()
            .find(|key| key.id == id)
            .ok_or_else(|| UnifiedError::not_found("API key not found"))?;
        Ok(json!({
            "id": view.id,
            "name": view.name,
            "scopes": view.scopes,
            "rate_tier": view.rate_tier,
            "requests_per_minute": view.requests_per_minute,
            "status": view.status,
            "usage_count": view.usage_count,
        }))
    })();
    match result {
        Ok(value) => api_json::success_response(value, 200),
        Err(error) => api_json::error_response(&error, debug),
    }
}

async fn update_key_access(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ApiManagerState>,
    id: web::Path<Uuid>,
    input: web::Json<UpdateKeyAccessInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let id = id.into_inner();
    let input = input.into_inner();
    let actor = input.actor.clone().unwrap_or_else(|| "admin".to_string());
    let scopes_changed = input.scopes.is_some();
    let limit_changed = input.rate_tier.is_some() || input.requests_per_minute.is_some();
    let result = crate::modules::apikeys::update_api_key_access(
        id,
        input.scopes.clone(),
        input.rate_tier.clone(),
        input.requests_per_minute,
        &ctx.modules.ids(),
    );
    match result {
        Ok(view) => {
            if scopes_changed {
                state.record_access_audit(
                    &actor,
                    "key_scopes_updated",
                    &id.to_string(),
                    json!({ "scopes": view.scopes }),
                );
            }
            if limit_changed {
                state.record_access_audit(
                    &actor,
                    "key_rate_limit_updated",
                    &id.to_string(),
                    json!({
                        "rate_tier": view.rate_tier,
                        "requests_per_minute": view.requests_per_minute,
                    }),
                );
            }
            api_json::success_response(
                json!({
                    "id": view.id,
                    "name": view.name,
                    "scopes": view.scopes,
                    "rate_tier": view.rate_tier,
                    "requests_per_minute": view.requests_per_minute,
                    "status": view.status,
                    "usage_count": view.usage_count,
                }),
                200,
            )
        }
        Err(error) => api_json::error_response(&error, debug),
    }
}

// ---------------------------------------------------------------------------
// Module
// ---------------------------------------------------------------------------

pub struct ApiManagerModule {
    state: ApiManagerState,
}

impl ApiManagerModule {
    pub fn new() -> Self {
        Self {
            state: ApiManagerState::default(),
        }
    }
}

impl Default for ApiManagerModule {
    fn default() -> Self {
        Self::new()
    }
}

impl Module for ApiManagerModule {
    fn id(&self) -> &'static str {
        "api-manager"
    }

    fn name(&self) -> &'static str {
        "API Manager"
    }

    fn description(&self) -> &'static str {
        "Public client API registry with payload validation, rate limits, and API keys. Owns the versioned public API: version registry with per-version settings, global API prefix, per-version endpoint registry collected from every module's route information, and collision-checked custom endpoints. Enforces default-deny public access: per-endpoint per-version allow rules, per-key scopes and rate limits, and an audit trail of access-control changes."
    }

    fn register_middleware(&self, registry: &mut crate::core::middleware::MiddlewareManager) {
        registry.register(PublicApiAccessMiddleware::new(self.state.clone()));
    }

    fn register_hooks(&self, bus: &HookBus) {
        // Key administration is audited here, not in the API Keys
        // module: the audit trail (and its database table) lives on
        // this state, and the keys module already emits
        // `apikey.created` / `apikey.revoked` / `apikey.rotated`
        // for every lifecycle change. `apikey.used` is deliberately
        // not audited: it fires on every authenticated request.
        let state = self.state.clone();
        bus.register(
            apikey_events::CREATED,
            "api-manager",
            std::sync::Arc::new(move |ctx: &mut HookContext| {
                on_apikey_created(&state, ctx);
            }),
        );
        let state = self.state.clone();
        bus.register(
            apikey_events::REVOKED,
            "api-manager",
            std::sync::Arc::new(move |ctx: &mut HookContext| {
                on_apikey_revoked(&state, ctx);
            }),
        );
        let state = self.state.clone();
        bus.register(
            apikey_events::ROTATED,
            "api-manager",
            std::sync::Arc::new(move |ctx: &mut HookContext| {
                on_apikey_rotated(&state, ctx);
            }),
        );
    }

    fn init(&self, ctx: &AppContext) -> crate::core::error::UnifiedResult<()> {
        // Seed the prefix and default versions from the config file
        // (`AppConfig.api`) and collect every module's routes into
        // the built-in endpoint registry. Both steps are idempotent,
        // so a second init never overwrites admin edits.
        let config = ctx.config.read().expect("config lock poisoned").clone();
        self.state.apply_config(&config);
        self.state.collect_from_catalog(&ctx.modules.route_catalog(), &ctx.modules);
        self.state.set_db_pool(ctx.db.read().expect("db lock poisoned").clone());
        // Best-effort reload of the access rules and audit trail
        // from migration 000004 (`api_access_rules`,
        // `api_access_audit`): the tables exist only once that
        // migration has run, and the in-memory state stays
        // authoritative until then.
        let _ = self.state.load_access_from_db();
        Ok(())
    }

    fn configure_routes(&self, cfg: &mut web::ServiceConfig) {
        let state = web::Data::new(self.state.clone());
        cfg.service(
            web::scope("/api/v1/api-manager")
                .app_data(state)
                .route("/endpoints", web::get().to(list_endpoints))
                .route("/endpoints", web::post().to(create_endpoint))
                .route("/endpoints/{id}", web::get().to(get_endpoint))
                .route("/endpoints/{id}", web::put().to(update_endpoint))
                .route("/endpoints/{id}", web::delete().to(delete_endpoint))
                .route("/endpoints/{id}/execute", web::post().to(execute_endpoint))
                .route("/versions", web::get().to(list_versions))
                .route("/api-versions", web::get().to(list_api_versions))
                .route("/api-versions", web::post().to(create_api_version))
                .route("/api-versions/{version}", web::get().to(get_api_version))
                .route("/api-versions/{version}", web::put().to(update_api_version))
                .route("/api-versions/{version}", web::delete().to(delete_api_version))
                .route(
                    "/api-versions/{version}/deprecate",
                    web::post().to(deprecate_api_version),
                )
                .route("/prefix", web::get().to(get_prefix))
                .route("/prefix", web::put().to(update_prefix))
                .route("/version-choices", web::get().to(get_version_choices))
                .route("/registry", web::get().to(list_registry))
                .route("/registry/detail", web::get().to(get_registry_detail))
                .route("/resolve", web::get().to(resolve_handler))
                .route("/public/{version}/{path:.*}", web::route().to(resolve_public))
                .route("/keys", web::post().to(create_key))
                .route("/keys", web::get().to(list_keys))
                .route("/keys/{id}", web::delete().to(revoke_key))
                .route("/keys/{id}/access", web::get().to(get_key_access))
                .route("/keys/{id}/access", web::put().to(update_key_access))
                .route("/access/rules", web::get().to(list_access_rules))
                .route("/access/rules", web::post().to(create_access_rule))
                .route("/access/rules/{id}", web::put().to(update_access_rule))
                .route("/access/rules/{id}", web::delete().to(delete_access_rule))
                .route("/access/audit", web::get().to(list_access_audit)),
        );
    }

    fn route_info(&self) -> Vec<RouteInfo> {
        vec![
            RouteInfo { method: "GET", path: "/api/v1/api-manager/endpoints", owner: "api-manager", description: "List custom endpoints (paginated, filter by version, method, enabled, owner, source, auth)." },
            RouteInfo { method: "POST", path: "/api/v1/api-manager/endpoints", owner: "api-manager", description: "Create a custom endpoint; rejected with 422 when it collides with a built-in endpoint." },
            RouteInfo { method: "GET", path: "/api/v1/api-manager/endpoints/{id}", owner: "api-manager", description: "Get one custom endpoint." },
            RouteInfo { method: "PUT", path: "/api/v1/api-manager/endpoints/{id}", owner: "api-manager", description: "Update a custom endpoint, including its version support." },
            RouteInfo { method: "DELETE", path: "/api/v1/api-manager/endpoints/{id}", owner: "api-manager", description: "Delete a custom endpoint." },
            RouteInfo { method: "POST", path: "/api/v1/api-manager/endpoints/{id}/execute", owner: "api-manager", description: "Validate a payload against an endpoint's rules (validation only)." },
            RouteInfo { method: "GET", path: "/api/v1/api-manager/versions", owner: "api-manager", description: "Legacy grouped overview of custom endpoints by version string." },
            RouteInfo { method: "GET", path: "/api/v1/api-manager/api-versions", owner: "api-manager", description: "List API versions with per-version settings (paginated, filter by enabled, deprecated)." },
            RouteInfo { method: "POST", path: "/api/v1/api-manager/api-versions", owner: "api-manager", description: "Create an API version, blank or by copying an existing version's settings." },
            RouteInfo { method: "GET", path: "/api/v1/api-manager/api-versions/{version}", owner: "api-manager", description: "Get one API version with its settings." },
            RouteInfo { method: "PUT", path: "/api/v1/api-manager/api-versions/{version}", owner: "api-manager", description: "Update an API version's settings." },
            RouteInfo { method: "DELETE", path: "/api/v1/api-manager/api-versions/{version}", owner: "api-manager", description: "Delete an API version (refused while custom endpoints cover it)." },
            RouteInfo { method: "POST", path: "/api/v1/api-manager/api-versions/{version}/deprecate", owner: "api-manager", description: "Deprecate an API version with a sunset date." },
            RouteInfo { method: "GET", path: "/api/v1/api-manager/prefix", owner: "api-manager", description: "Get the global public API prefix setting." },
            RouteInfo { method: "PUT", path: "/api/v1/api-manager/prefix", owner: "api-manager", description: "Update the global public API prefix setting." },
            RouteInfo { method: "GET", path: "/api/v1/api-manager/version-choices", owner: "api-manager", description: "Version choices for the custom-endpoint creation form (single, all, multi-select)." },
            RouteInfo { method: "GET", path: "/api/v1/api-manager/registry", owner: "api-manager", description: "List endpoints grouped by version (paginated, filter by module, method, version, auth)." },
            RouteInfo { method: "GET", path: "/api/v1/api-manager/registry/detail", owner: "api-manager", description: "Per-endpoint detail: version support, owner module, auth, rate limit." },
            RouteInfo { method: "GET", path: "/api/v1/api-manager/resolve", owner: "api-manager", description: "Resolve a method + version + path to its endpoint or a precise miss reason." },
            RouteInfo { method: "*", path: "/api/v1/api-manager/public/{version}/{path:.*}", owner: "api-manager", description: "Public versioned gateway: resolve a request against the per-version registry." },
            RouteInfo { method: "POST", path: "/api/v1/api-manager/keys", owner: "api-manager", description: "Create an API key (full key shown exactly once)." },
            RouteInfo { method: "GET", path: "/api/v1/api-manager/keys", owner: "api-manager", description: "List API keys (paginated, hashes never returned)." },
            RouteInfo { method: "DELETE", path: "/api/v1/api-manager/keys/{id}", owner: "api-manager", description: "Revoke an API key." },
            RouteInfo { method: "GET", path: "/api/v1/api-manager/keys/{id}/access", owner: "api-manager", description: "Get one API key's access: scopes, rate tier, and per-minute limit." },
            RouteInfo { method: "PUT", path: "/api/v1/api-manager/keys/{id}/access", owner: "api-manager", description: "Update one API key's scopes, rate tier, and per-minute limit." },
            RouteInfo { method: "GET", path: "/api/v1/api-manager/access/rules", owner: "api-manager", description: "List public-access rules (per endpoint and version)." },
            RouteInfo { method: "POST", path: "/api/v1/api-manager/access/rules", owner: "api-manager", description: "Create a public-access rule; rejected with 409 when the rule already exists." },
            RouteInfo { method: "PUT", path: "/api/v1/api-manager/access/rules/{id}", owner: "api-manager", description: "Update a public-access rule (allow, public access, actor, reason)." },
            RouteInfo { method: "DELETE", path: "/api/v1/api-manager/access/rules/{id}", owner: "api-manager", description: "Delete a public-access rule." },
            RouteInfo { method: "GET", path: "/api/v1/api-manager/access/audit", owner: "api-manager", description: "List the access-control audit trail (newest first, paginated)." },
        ]
    }
}

pub fn register(registry: &ModuleRegistry) {
    registry.register(Arc::new(ApiManagerModule::new()));
}
