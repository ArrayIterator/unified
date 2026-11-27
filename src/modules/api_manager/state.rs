// Api Manager - In-memory API manager state and registry seeding.

use unified_core::config::{ApiSection, AppConfig};
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hardening::client_ip;
use unified_core::modules::{ModuleRegistry, RouteInfo};
use unified_core::db::DbPool;
use chrono::Utc;
use dashmap::DashMap;
use diesel::prelude::*;
use serde_json::{json, Value};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, RwLock};
use std::time::{SystemTime, UNIX_EPOCH};
use uuid::Uuid;

use super::{AccessAuditRow, AccessRuleRow, ApiAccessAuditEntry, ApiAccessRule, ApiEndpoint,
    ApiPrefixSettings, ApiVersion, DEFAULT_ACCESS_RULES, MAX_ACCESS_AUDIT_ENTRIES,
    MAX_RATE_COUNTER_ENTRIES, RegisteredEndpoint, RegistryRow, ResolvedEndpoint, VersionSupport,
    normalize_path_template, parse_version_number, path_template_matches, sort_version_ids,
    strip_public_prefix, validate_prefix};

// State

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

    /// Seed the access rules from `DEFAULT_ACCESS_RULES`.
    /// Idempotent: a rule whose key already exists (seeded earlier,
    /// loaded from the database by `load_access_from_db`, or created
    /// by an admin) is left untouched, so admin edits survive
    /// restarts. Rules are inserted into the in-memory map only;
    /// persistence to `api_access_rules` happens through the normal
    /// `persist_access_rule` path when a rule is later edited, and
    /// the seed rows are re-created from this constant on every
    /// restart until then. Callers should run this after
    /// `load_access_from_db` so database rows win over seeds.
    pub fn ensure_seeded_access_rules(&self) {
        let now = Utc::now();
        for (method, path, version, public_access, source) in DEFAULT_ACCESS_RULES {
            let key = Self::access_rule_key(method, path, Some(version));
            if self.access_rules.contains_key(&key) {
                continue;
            }
            let rule = ApiAccessRule {
                id: Uuid::now_v7(),
                method: method.to_string(),
                path: path.to_string(),
                version: Some(version.to_string()),
                allowed: true,
                public_access: *public_access,
                actor: "seed".to_string(),
                reason: Some(format!("Seeded access rule ({source})")),
                created_at: now,
                updated_at: now,
            };
            self.access_rules.insert(key, rule);
        }
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

    // Persistence of the access rules and audit trail (migration
    // 000004: `api_access_rules`, `api_access_audit`).
    //
    // The tables have no Diesel schema entries (crates/unified-core/src/db/schema.rs`
    // does not list them), so the typed DSL cannot address them;
    // these methods use raw `diesel::sql_query` instead, the same
    // fallback the dashboards module uses for its permission load.
    // All writes are best-effort: a missing pool, a missing table
    // (migration not run yet), or any database error is swallowed,
    // because the in-memory state stays authoritative and must never
    // fail an admin request. A failed write is therefore retried by
    // the next create/update/delete of the same row.

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

    /// Drop rate-counter entries from finished minutes once the map
    /// grows past `MAX_RATE_COUNTER_ENTRIES`. Counter keys end in
    /// `:<unix-minute>`; an entry is kept when its minute is the
    /// current minute or the one before it, so an in-flight window
    /// is never pruned mid-minute.
    pub(crate) fn prune_rate_counters(&self, current_minute: u64) {
        if self.rate_counters.len() <= MAX_RATE_COUNTER_ENTRIES {
            return;
        }
        self.rate_counters.retain(|key, _| {
            key.rsplit(':')
                .next()
                .and_then(|minute| minute.parse::<u64>().ok())
                .is_some_and(|minute| minute + 1 >= current_minute)
        });
    }

    /// Fixed-window per-minute check for public-API access, reusing
    /// the module's `rate_counters` map. Returns 429 once the
    /// subject has spent its allowance for this endpoint and minute.
    /// The 429 metadata uses the same keys as the hardening rate
    /// limiter (`retry_after_secs`, `limit`, `remaining`,
    /// `reset_in_secs`), so every 429 in the application carries
    /// one shape; `limit_per_minute` is kept as an alias for the
    /// previous metadata key.
    fn check_access_rate_limit(
        &self,
        subject: &str,
        row: &RegistryRow,
        limit: u32,
    ) -> Result<(), UnifiedError> {
        let now_secs = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|elapsed| elapsed.as_secs())
            .unwrap_or(0);
        let minute = now_secs / 60;
        // Seconds left in the current fixed window: the retry delay
        // and the reset timestamp, so callers can back off exactly.
        let reset_in_secs = 60 - (now_secs % 60);
        self.prune_rate_counters(minute);
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
            .with_metadata("limit", json!(limit))
            .with_metadata("limit_per_minute", json!(limit))
            .with_metadata("remaining", json!(0))
            .with_metadata("retry_after_secs", json!(reset_in_secs))
            .with_metadata("reset_in_secs", json!(reset_in_secs)));
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
        // No silent default: a path with no parseable version must
        // not resolve against `v1` (or any other version) -- an
        // unprefixed path could otherwise match a `v1` endpoint it
        // was never addressed to. Internal `/api/v1/...` paths do
        // carry a version (`strip_public_prefix` returns `Some("v1")`
        // for them), so only truly version-less paths fail here.
        let Some(version) = version else {
            return Err(UnifiedError::not_found(format!(
                "No API version in request path \"{raw_path}\""
            ))
            .with_metadata("reason", json!("unknown_version"))
            .with_metadata("path", json!(raw_path)));
        };
        // Not a registry endpoint (unknown path, disabled version,
        // method not allowed, not in this version): leave the
        // decision to normal routing, which returns its own 404.
        // One exception: a key presented to an internal route is
        // still validated, so a revoked or forged key is rejected
        // with 401 instead of silently passing through to the
        // handler unchecked.
        let resolved = match self.resolve_endpoint(method, &version, &relative_path) {
            Ok(resolved) => resolved,
            Err(_) => {
                if raw_path.starts_with("/api/v1/") {
                    if let Some(key) = presented_key
                        .map(str::trim)
                        .filter(|key| !key.is_empty())
                    {
                        crate::modules::apikeys::authenticate_key(key)?;
                    }
                }
                return Ok(None);
            }
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

#[cfg(test)]
mod tests {
    use super::*;

    fn seeded_state() -> ApiManagerState {
        let state = ApiManagerState::default();
        state.ensure_seeded_versions();
        state
    }

    #[test]
    fn check_public_access_rejects_version_less_path() {
        // A bare path carries no parseable version, so it must fail
        // instead of silently resolving against `v1`.
        let state = seeded_state();
        let error = state
            .check_public_access("GET", "/things", None, None)
            .unwrap_err();
        assert_eq!(error.code, 404);
        assert_eq!(
            error.metadata.get("reason").and_then(|value| value.as_str()),
            Some("unknown_version")
        );
    }

    #[test]
    fn check_public_access_rejects_unparseable_public_version() {
        let state = seeded_state();
        let error = state
            .check_public_access("GET", "/v/notaversion/things", None, None)
            .unwrap_err();
        assert_eq!(error.code, 404);
        assert_eq!(
            error.metadata.get("reason").and_then(|value| value.as_str()),
            Some("unknown_version")
        );
    }

    #[test]
    fn check_public_access_still_passes_internal_v1_through() {
        // `/api/v1/...` carries version `v1` explicitly, so it is
        // not a version-less path: with no registry match and no key
        // it passes through (`Ok(None)`) for normal routing to 404,
        // exactly as before.
        let state = seeded_state();
        assert!(state
            .check_public_access("GET", "/api/v1/cards", None, None)
            .unwrap()
            .is_none());
    }

    #[test]
    fn check_public_access_passes_unknown_versioned_path_through() {
        // A parseable version with no registry match still passes
        // through for normal routing; only the version-less case
        // fails outright.
        let state = seeded_state();
        assert!(state
            .check_public_access("GET", "/v/v1/no-such-endpoint", None, None)
            .unwrap()
            .is_none());
    }
}
