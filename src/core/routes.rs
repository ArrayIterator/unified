// Customizable route registry.
//
// Every named page route in the application has a built-in default
// path and an optional override. Auth and dashboard pages must never
// be hardcoded: templates, handlers, and redirects resolve their
// paths through this registry instead of embedding literals such as
// `/login` or `/dashboard`.
//
// Precedence (highest wins):
//   1. settings override — set at runtime through the Routes admin
//      API (`PUT /api/v1/routes/{name}`); stored in the registry.
//   2. config override — set in the configuration file under the
//      `routes:` section (`RoutesSection` below) and applied at
//      startup.
//   3. built-in default — the `DEFAULT_ROUTES` table in this file.
//
// Deprecated: the legacy `app.dashboard_path` config value. It is
// NOT a config override layer of its own anymore: `routes.
// user_dashboard` is canonical, and `app.dashboard_path` only acts
// as a fallback for it — applied (with a logged deprecation
// warning) when `routes.user_dashboard` is not set, and ignored
// (also with a warning) when it is. See
// `RouteRegistry::apply_legacy_dashboard_path`. The default
// `app.dashboard_path` value (`"dashboard"`) is a no-op and warns
// about nothing.
//
// When an effective path changes, the previous path keeps working as
// a permanent (301) redirect to the new path. Redirect policy
// (latest-only): only the immediately previous path of a route
// redirects. Changing a path twice drops the older redirect, so at
// most one redirect exists per route and redirects never chain — a
// redirect target is always the route's current effective path.
// Bookmarks made against the very first path therefore stop working
// after a second change; operators who rename a route twice should
// expect that and re-publish the link.
//
// Validation rules for an override path (see `validate_path_shape`):
//   - must start with `/`, must not be `/` or empty;
//   - may contain only letters, digits, `-`, `_`, `/`, and `.`;
//   - must not contain a `..` segment, an empty segment (no `//`),
//     a query string (`?`), or a fragment (`#`);
//   - must not fall under a reserved prefix: `/api/`, `/.well-known/`,
//     `/assets/`, `/theme/`;
//   - must not conflict with any other route's effective path: an
//     exact match is a conflict, and a prefix collision is a conflict
//     too — one path is a prefix of the other at a segment boundary
//     (for example `/dashboard` conflicts with `/dashboard/tasks`).
//     A trailing slash is normalized away before comparison.
// Invalid overrides are rejected with HTTP 422 and the reason in the
// error metadata; nothing is stored and no redirect is created.
//
// Named routes and their defaults (verified against the owning
// modules; no page is invented here):
//   - `login`, `register`, `forgot_password`, `reset_password`,
//     `logout` — auth pages. The auth module currently exposes only
//     the `/api/v1/auth-manager` API; these names reserve the page
//     paths so the page handlers (owned by the dashboards/auth work)
//     mount at the registry-resolved path instead of a literal.
//   - `user_dashboard` — default `/dashboard` (matches the existing
//     `app.dashboard_path` default `"dashboard"` and the templates).
//   - `admin_dashboard` — default `/admin` (admin layout under
//     `/admin/*`, separate from the customer dashboard).
//   - `status_page` — default `/status`, the public HTML status page
//     registered by the statuspage module (`GET /status`).
// There is no `gateway_status` page anywhere in the codebase, so no
// such route is registered; adding one requires a real page first.
//
// Persistence note: overrides live in memory in this registry, the
// same durability as the settings module's in-memory store. No
// database table or migration is added by this file; if overrides
// must survive a restart without the config file, the integrator
// should persist them (see the migration sketch in the coordinator
// report) and re-apply them at startup with `apply_config_map` or
// `set_override` with source `settings`.

use crate::core::error::{UnifiedError, UnifiedResult};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::RwLock;

/// Event emitted on the hook bus whenever a route override changes
/// (set or cleared). Payload: `{"route", "old_path", "new_path",
/// "source", "actor", "changed_at"}`. The audit module records the
/// event once `"route.changed"` is added to its audited event list.
pub const ROUTE_CHANGED_EVENT: &str = "route.changed";

/// Reserved path prefixes. An override may never fall under one of
/// these, so API routes, ACME challenges, static assets, and theme
/// assets can never be shadowed by a renamed page.
pub const RESERVED_PREFIXES: &[&str] = &["/api/", "/.well-known/", "/assets/", "/theme/"];

/// One built-in named route: its name, default path, and description.
#[derive(Debug, Clone, Copy)]
pub struct DefaultRoute {
    pub name: &'static str,
    pub default_path: &'static str,
    pub description: &'static str,
}

/// Built-in named routes with their default paths.
pub const DEFAULT_ROUTES: &[DefaultRoute] = &[
    DefaultRoute {
        name: "login",
        default_path: "/login",
        description: "Sign-in page.",
    },
    DefaultRoute {
        name: "register",
        default_path: "/register",
        description: "Registration page.",
    },
    DefaultRoute {
        name: "forgot_password",
        default_path: "/forgot-password",
        description: "Forgot-password page.",
    },
    DefaultRoute {
        name: "reset_password",
        default_path: "/reset-password",
        description: "Reset-password page.",
    },
    DefaultRoute {
        name: "logout",
        default_path: "/logout",
        description: "Sign-out endpoint (redirects back to the sign-in page).",
    },
    DefaultRoute {
        name: "user_dashboard",
        default_path: "/dashboard",
        description: "Customer dashboard.",
    },
    DefaultRoute {
        name: "admin_dashboard",
        default_path: "/admin",
        description: "Admin dashboard.",
    },
    DefaultRoute {
        name: "status_page",
        default_path: "/status",
        description: "Public status page.",
    },
];

/// Where the effective path of a route comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum OverrideSource {
    /// No override: the built-in default applies.
    None,
    /// Override from the configuration file.
    Config,
    /// Override set at runtime through the Routes admin API.
    Settings,
}

impl OverrideSource {
    pub fn as_str(&self) -> &'static str {
        match self {
            OverrideSource::None => "none",
            OverrideSource::Config => "config",
            OverrideSource::Settings => "settings",
        }
    }

    pub fn from_str(value: &str) -> Option<Self> {
        match value {
            "none" => Some(OverrideSource::None),
            "config" => Some(OverrideSource::Config),
            "settings" => Some(OverrideSource::Settings),
            _ => None,
        }
    }
}

/// One named route as reported by the API and the registry listing.
#[derive(Debug, Clone, Serialize)]
pub struct RouteEntry {
    pub name: String,
    pub default_path: String,
    pub current_path: String,
    pub source: OverrideSource,
    pub description: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<DateTime<Utc>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub previous_path: Option<String>,
}

/// One active old-path redirect.
#[derive(Debug, Clone, Serialize)]
pub struct RedirectEntry {
    pub route: String,
    pub old_path: String,
    pub new_path: String,
    pub changed_at: DateTime<Utc>,
}

/// Result of a successful override change.
#[derive(Debug, Clone, Serialize)]
pub struct RouteChange {
    pub route: String,
    pub old_path: String,
    pub new_path: String,
    pub source: OverrideSource,
    pub changed: bool,
    pub changed_at: DateTime<Utc>,
}

/// Per-route mutable state: the two override layers, plus
/// last-change bookkeeping.
#[derive(Debug, Clone, Default)]
struct RouteState {
    config_override: Option<String>,
    settings_override: Option<String>,
    updated_at: Option<DateTime<Utc>>,
    previous_path: Option<String>,
}

#[derive(Debug, Default)]
struct RegistryState {
    routes: HashMap<String, RouteState>,
    /// Redirects keyed by old path. Latest-only per route: at most
    /// one entry exists for any route name.
    redirects: HashMap<String, RedirectEntry>,
}

/// Thread-safe registry of named routes, their overrides, and the
/// active old-path redirects.
#[derive(Debug, Default)]
pub struct RouteRegistry {
    state: RwLock<RegistryState>,
}

/// Configuration-file section for route overrides. The integrator
/// adds one field to `AppConfig` (`pub routes: RoutesSection`) and
/// validates it through `RoutesSection::validate`. Every field is
/// optional; an absent field leaves the default (or the legacy
/// `app.dashboard_path` for `user_dashboard`) in place.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct RoutesSection {
    pub login: Option<String>,
    pub register: Option<String>,
    pub forgot_password: Option<String>,
    pub reset_password: Option<String>,
    pub logout: Option<String>,
    pub user_dashboard: Option<String>,
    pub admin_dashboard: Option<String>,
    pub status_page: Option<String>,
}

impl RoutesSection {
    /// Config override for one named route, if set.
    pub fn get(&self, name: &str) -> Option<&str> {
        match name {
            "login" => self.login.as_deref(),
            "register" => self.register.as_deref(),
            "forgot_password" => self.forgot_password.as_deref(),
            "reset_password" => self.reset_password.as_deref(),
            "logout" => self.logout.as_deref(),
            "user_dashboard" => self.user_dashboard.as_deref(),
            "admin_dashboard" => self.admin_dashboard.as_deref(),
            "status_page" => self.status_page.as_deref(),
            _ => None,
        }
    }

    /// Validate every configured override: shape rules plus
    /// conflicts between the effective paths of all routes. Returns
    /// the list of problems; an empty list means the section is
    /// valid. Used by `AppConfig::validate` through the integrator
    /// snippet in the coordinator report.
    pub fn validate(&self) -> Vec<String> {
        let mut problems = Vec::new();
        let mut effective: Vec<(&str, String)> = Vec::new();
        for route in DEFAULT_ROUTES {
            match self.get(route.name) {
                Some(raw) => match normalize_path(raw) {
                    Ok(path) => {
                        if let Err(error) = validate_path_shape(&path) {
                            problems.push(format!("routes.{}: {}", route.name, error.message));
                        } else {
                            effective.push((route.name, path));
                        }
                    }
                    Err(error) => {
                        problems.push(format!("routes.{}: {}", route.name, error.message));
                    }
                },
                None => effective.push((route.name, route.default_path.to_string())),
            }
        }
        for (index, (name, path)) in effective.iter().enumerate() {
            for (other_name, other_path) in effective.iter().skip(index + 1) {
                if paths_conflict(path, other_path) {
                    problems.push(format!(
                        "routes.{name} (\"{path}\") conflicts with routes.{other_name} (\"{other_path}\")"
                    ));
                }
            }
        }
        problems
    }
}

/// Split a normalized path into its segments.
fn path_segments(path: &str) -> Vec<&str> {
    path.split('/').filter(|segment| !segment.is_empty()).collect()
}

/// True when the two paths conflict: they are equal, or one is a
/// prefix of the other at a segment boundary (for example
/// `/dashboard` conflicts with `/dashboard/tasks`, but `/dash`
/// does not conflict with `/dashboard`).
pub fn paths_conflict(first: &str, second: &str) -> bool {
    if first == second {
        return true;
    }
    let first_segments = path_segments(first);
    let second_segments = path_segments(second);
    let (shorter, longer) = if first_segments.len() <= second_segments.len() {
        (&first_segments, &second_segments)
    } else {
        (&second_segments, &first_segments)
    };
    longer[..shorter.len()] == shorter[..]
}

/// Normalize a raw path: trim surrounding whitespace and drop one
/// trailing slash (except for the root, which is rejected later).
/// Returns a 422 error when the value is empty.
pub fn normalize_path(raw: &str) -> UnifiedResult<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(UnifiedError::unprocessable("Path must not be empty")
            .with_metadata("reason", serde_json::Value::String("empty_path".to_string())));
    }
    let mut normalized = trimmed.to_string();
    while normalized.len() > 1 && normalized.ends_with('/') {
        normalized.pop();
    }
    Ok(normalized)
}

/// Validate only the shape of a path (no conflict check). The path
/// must already be normalized. Returns a 422 error whose metadata
/// carries a machine-readable `reason`.
pub fn validate_path_shape(path: &str) -> UnifiedResult<()> {
    if !path.starts_with('/') {
        return Err(UnifiedError::unprocessable(format!(
            "Path \"{path}\" must start with '/'"
        ))
        .with_metadata("reason", serde_json::Value::String("must_start_with_slash".to_string())));
    }
    if path == "/" {
        return Err(UnifiedError::unprocessable("Path must not be '/'")
            .with_metadata("reason", serde_json::Value::String("root_path".to_string())));
    }
    if path.contains('?') || path.contains('#') {
        return Err(UnifiedError::unprocessable(format!(
            "Path \"{path}\" must not contain a query string or fragment"
        ))
        .with_metadata("reason", serde_json::Value::String("query_or_fragment".to_string())));
    }
    if !path
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '/' | '.'))
    {
        return Err(UnifiedError::unprocessable(format!(
            "Path \"{path}\" contains an unsafe character (only letters, digits, '-', '_', '/' and '.' are allowed)"
        ))
        .with_metadata("reason", serde_json::Value::String("unsafe_characters".to_string())));
    }
    for segment in path_segments(path) {
        if segment == ".." {
            return Err(UnifiedError::unprocessable(format!(
                "Path \"{path}\" must not contain a '..' segment"
            ))
            .with_metadata("reason", serde_json::Value::String("dotdot_segment".to_string())));
        }
        if segment == "." {
            return Err(UnifiedError::unprocessable(format!(
                "Path \"{path}\" must not contain a '.' segment"
            ))
            .with_metadata("reason", serde_json::Value::String("dot_segment".to_string())));
        }
    }
    if path.contains("//") {
        return Err(UnifiedError::unprocessable(format!(
            "Path \"{path}\" must not contain an empty segment ('//')"
        ))
        .with_metadata("reason", serde_json::Value::String("empty_segment".to_string())));
    }
    for prefix in RESERVED_PREFIXES {
        if path.starts_with(prefix) {
            return Err(UnifiedError::unprocessable(format!(
                "Path \"{path}\" falls under reserved prefix \"{prefix}\""
            ))
            .with_metadata("reason", serde_json::Value::String("reserved_prefix".to_string()))
            .with_metadata("reserved_prefix", serde_json::Value::String(prefix.to_string())));
        }
    }
    Ok(())
}

impl RouteRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Build a registry seeded from a configuration section. Invalid
    /// entries are skipped and returned as problem strings; valid
    /// entries are stored as config overrides. Startup seeding never
    /// creates redirects.
    pub fn from_config_section(section: &RoutesSection) -> (Self, Vec<String>) {
        let registry = Self::new();
        let problems = section.validate();
        if problems.is_empty() {
            for route in DEFAULT_ROUTES {
                if let Some(raw) = section.get(route.name) {
                    if let Ok(path) = normalize_path(raw) {
                        registry
                            .state
                            .write()
                            .expect("route registry lock poisoned")
                            .routes
                            .entry(route.name.to_string())
                            .or_default()
                            .config_override = Some(path);
                    }
                }
            }
        }
        // When validation fails the section is rejected as a whole, so
        // no partial override applies; the caller reports `problems`.
        (registry, problems)
    }

    /// Apply config overrides from a plain name -> path map (used by
    /// the integrator when the `routes` config section is wired in, or
    /// by tests). Returns one problem string per rejected entry;
    /// valid entries are stored as config overrides and never create
    /// redirects. Unknown names are reported, not stored.
    pub fn apply_config_map(&self, overrides: &HashMap<String, String>) -> Vec<String> {
        let mut problems = Vec::new();
        let mut valid: Vec<(String, String)> = Vec::new();
        for (name, raw) in overrides {
            if default_route(name).is_none() {
                problems.push(format!("Unknown route \"{name}\""));
                continue;
            }
            match normalize_path(raw).and_then(|path| {
                validate_path_shape(&path)?;
                Ok(path)
            }) {
                Ok(path) => valid.push((name.clone(), path)),
                Err(error) => problems.push(format!("routes.{name}: {}", error.message)),
            }
        }
        // Conflict check across the would-be effective paths.
        let mut effective: HashMap<String, String> = HashMap::new();
        for route in DEFAULT_ROUTES {
            effective.insert(route.name.to_string(), route.default_path.to_string());
        }
        {
            let state = self.state.read().expect("route registry lock poisoned");
            for (name, route_state) in &state.routes {
                if let Some(path) = &route_state.settings_override {
                    effective.insert(name.clone(), path.clone());
                } else if let Some(path) = &route_state.config_override {
                    effective.insert(name.clone(), path.clone());
                }
            }
        }
        for (name, path) in &valid {
            effective.insert(name.clone(), path.clone());
        }
        let mut conflicting: Vec<String> = Vec::new();
        for (name, path) in &valid {
            for (other_name, other_path) in &effective {
                if other_name != name && paths_conflict(path, other_path) {
                    conflicting.push(format!(
                        "routes.{name} (\"{path}\") conflicts with route \"{other_name}\" (\"{other_path}\")"
                    ));
                }
            }
        }
        if !conflicting.is_empty() {
            problems.extend(conflicting);
            return problems;
        }
        let mut state = self.state.write().expect("route registry lock poisoned");
        for (name, path) in valid {
            state
                .routes
                .entry(name)
                .or_default()
                .config_override = Some(path);
        }
        problems
    }

    /// Honour the legacy `app.dashboard_path` config value as a
    /// fallback for `user_dashboard`. DEPRECATED: `routes.
    /// user_dashboard` is canonical. This only applies when no
    /// `routes.user_dashboard` config override is set, and it logs a
    /// deprecation warning in both cases where a non-default legacy
    /// value is present: when it is applied as the fallback, and
    /// when it is ignored because `routes.user_dashboard` wins. The
    /// value is a bare segment (`"dashboard"`), so it is prefixed
    /// with `/`. Returns a problem string when the value is present
    /// but invalid; the built-in default (`"dashboard"`) is a no-op
    /// and warns about nothing. The guard makes the outcome
    /// independent of call order: whether the `routes:` section is
    /// applied before or after this call, `routes.user_dashboard`
    /// always wins over the legacy value.
    pub fn apply_legacy_dashboard_path(&self, value: &str) -> Option<String> {
        let trimmed = value.trim();
        if trimmed.is_empty() || trimmed == "dashboard" {
            return None;
        }
        {
            let state = self.state.read().expect("route registry lock poisoned");
            if let Some(route_state) = state.routes.get("user_dashboard") {
                if route_state.config_override.is_some() {
                    log::warn!(
                        "app.dashboard_path is deprecated and is ignored because routes.user_dashboard is set; \
                         remove app.dashboard_path from the configuration and use routes.user_dashboard instead"
                    );
                    return None;
                }
            }
        }
        let candidate = format!("/{trimmed}");
        match normalize_path(&candidate).and_then(|path| {
            validate_path_shape(&path)?;
            Ok(path)
        }) {
            Ok(path) => {
                log::warn!(
                    "app.dashboard_path is deprecated; it is applied only as a fallback for routes.user_dashboard \
                     (\"{path}\"). Set routes.user_dashboard in the configuration instead and remove app.dashboard_path"
                );
                let mut state = self.state.write().expect("route registry lock poisoned");
                state
                    .routes
                    .entry("user_dashboard".to_string())
                    .or_default()
                    .config_override = Some(path);
                None
            }
            Err(error) => Some(format!("app.dashboard_path: {}", error.message)),
        }
    }

    /// Effective path for a named route, or `None` for an unknown name.
    pub fn resolve(&self, name: &str) -> Option<String> {
        self.entry(name).map(|entry| entry.current_path)
    }

    /// Effective path for a named route, falling back to `fallback`
    /// for unknown names. Use this in templates and handlers that
    /// must always produce a link.
    pub fn route_url(&self, name: &str) -> String {
        self.resolve(name)
            .unwrap_or_else(|| default_route(name).map(|route| route.default_path.to_string()).unwrap_or_else(|| "/".to_string()))
    }

    /// Alias for `resolve`, named for callers that think in URLs.
    pub fn route_url_or(&self, name: &str, fallback: &str) -> String {
        self.resolve(name).unwrap_or_else(|| fallback.to_string())
    }

    /// Built-in default path for a named route, or `None` when unknown.
    pub fn default_path(&self, name: &str) -> Option<&'static str> {
        default_route(name).map(|route| route.default_path)
    }

    /// Where the effective path comes from: settings, config, or none.
    pub fn override_source(&self, name: &str) -> Option<OverrideSource> {
        self.entry(name).map(|entry| entry.source)
    }

    /// Full listing of every named route, sorted by name.
    pub fn list(&self) -> Vec<RouteEntry> {
        let mut entries: Vec<RouteEntry> = DEFAULT_ROUTES
            .iter()
            .map(|route| self.entry(route.name).expect("default route is known"))
            .collect();
        entries.sort_by(|a, b| a.name.cmp(&b.name));
        entries
    }

    /// One route entry, or `None` for an unknown name.
    pub fn entry(&self, name: &str) -> Option<RouteEntry> {
        let route = default_route(name)?;
        let state = self.state.read().expect("route registry lock poisoned");
        let route_state = state.routes.get(name);
        let (current_path, source) = match route_state {
            Some(RouteState {
                settings_override: Some(path),
                ..
            }) => (path.clone(), OverrideSource::Settings),
            Some(RouteState {
                config_override: Some(path),
                ..
            }) => (path.clone(), OverrideSource::Config),
            _ => (route.default_path.to_string(), OverrideSource::None),
        };
        Some(RouteEntry {
            name: route.name.to_string(),
            default_path: route.default_path.to_string(),
            current_path,
            source,
            description: route.description.to_string(),
            updated_at: route_state.and_then(|state| state.updated_at),
            previous_path: route_state.and_then(|state| state.previous_path.clone()),
        })
    }

    /// Set an override for a named route. `source` must be `Config`
    /// or `Settings` (`None` is rejected). Validation failures are
    /// 422 errors with the reason in metadata; unknown names are 404.
    /// When the effective path changes, the old path is recorded as a
    /// redirect (latest-only per route: any older redirect for this
    /// route is removed first). Setting the same path the route
    /// already resolves to stores the override and reports
    /// `changed: false` with no redirect.
    pub fn set_override(
        &self,
        name: &str,
        raw_path: &str,
        source: OverrideSource,
    ) -> UnifiedResult<RouteChange> {
        if source == OverrideSource::None {
            return Err(UnifiedError::bad_request(
                "Override source must be \"config\" or \"settings\"",
            ));
        }
        let route = default_route(name).ok_or_else(|| {
            UnifiedError::not_found(format!("Unknown route \"{name}\""))
        })?;
        let path = normalize_path(raw_path)?;
        validate_path_shape(&path)?;
        let mut state = self.state.write().expect("route registry lock poisoned");
        // Conflict check against every other route's effective path.
        for other in DEFAULT_ROUTES {
            if other.name == name {
                continue;
            }
            let other_path = effective_path_locked(&state, other);
            if paths_conflict(&path, &other_path) {
                return Err(UnifiedError::unprocessable(format!(
                    "Path \"{path}\" conflicts with route \"{}\" (\"{other_path}\")",
                    other.name
                ))
                .with_metadata("reason", serde_json::Value::String("path_conflict".to_string()))
                .with_metadata(
                    "conflicting_route",
                    serde_json::Value::String(other.name.to_string()),
                )
                .with_metadata(
                    "conflicting_path",
                    serde_json::Value::String(other_path),
                ));
            }
        }
        let old_path = effective_path_locked(&state, route);
        let changed = old_path != path;
        let now = Utc::now();
        let route_state = state.routes.entry(name.to_string()).or_default();
        match source {
            OverrideSource::Config => route_state.config_override = Some(path.clone()),
            OverrideSource::Settings => route_state.settings_override = Some(path.clone()),
            OverrideSource::None => {}
        }
        if changed {
            // Latest-only: drop any older redirect for this route, then
            // record old -> new. The redirect target is always the
            // current effective path, so redirects never chain.
            state
                .redirects
                .retain(|_, redirect| redirect.route != name);
            state.redirects.insert(
                old_path.clone(),
                RedirectEntry {
                    route: name.to_string(),
                    old_path: old_path.clone(),
                    new_path: path.clone(),
                    changed_at: now,
                },
            );
            route_state.previous_path = Some(old_path.clone());
            route_state.updated_at = Some(now);
        }
        Ok(RouteChange {
            route: name.to_string(),
            old_path,
            new_path: path,
            source,
            changed,
            changed_at: now,
        })
    }

    /// Clear the override from one layer (`Config` or `Settings`) for
    /// a named route. The effective path falls back to the remaining
    /// layer or the default. When the effective path changes, the old
    /// path is recorded as a redirect (latest-only, as in
    /// `set_override`). Returns 404 when the name is unknown or when
    /// that layer holds no override.
    pub fn clear_override(
        &self,
        name: &str,
        source: OverrideSource,
    ) -> UnifiedResult<RouteChange> {
        if source == OverrideSource::None {
            return Err(UnifiedError::bad_request(
                "Override source must be \"config\" or \"settings\"",
            ));
        }
        let route = default_route(name).ok_or_else(|| {
            UnifiedError::not_found(format!("Unknown route \"{name}\""))
        })?;
        let mut state = self.state.write().expect("route registry lock poisoned");
        let old_path = effective_path_locked(&state, route);
        let had_override = match state.routes.get_mut(name) {
            Some(route_state) => match source {
                OverrideSource::Config => route_state.config_override.take().is_some(),
                OverrideSource::Settings => route_state.settings_override.take().is_some(),
                OverrideSource::None => false,
            },
            None => false,
        };
        if !had_override {
            return Err(UnifiedError::not_found(format!(
                "Route \"{name}\" has no {} override to clear",
                source.as_str()
            )));
        }
        let new_path = effective_path_locked(&state, route);
        let changed = old_path != new_path;
        let now = Utc::now();
        if changed {
            state
                .redirects
                .retain(|_, redirect| redirect.route != name);
            state.redirects.insert(
                old_path.clone(),
                RedirectEntry {
                    route: name.to_string(),
                    old_path: old_path.clone(),
                    new_path: new_path.clone(),
                    changed_at: now,
                },
            );
            if let Some(route_state) = state.routes.get_mut(name) {
                route_state.previous_path = Some(old_path.clone());
                route_state.updated_at = Some(now);
            }
        }
        Ok(RouteChange {
            route: name.to_string(),
            old_path,
            new_path,
            source,
            changed,
            changed_at: now,
        })
    }

    /// Resolve an old path to the current path of its route, for the
    /// 301 redirect handler. Returns `None` when the path is not a
    /// recorded old path, or when it already is a current effective
    /// path. The returned target is always a current effective path,
    /// so a redirect never points at another old path (no chaining).
    pub fn resolve_redirect(&self, raw_path: &str) -> Option<String> {
        let path = normalize_path(raw_path).ok()?;
        let state = self.state.read().expect("route registry lock poisoned");
        let redirect = state.redirects.get(&path)?;
        let current = effective_path_locked(
            &state,
            default_route(&redirect.route)?,
        );
        if current == path {
            return None;
        }
        Some(current)
    }

    /// All active redirects, newest first.
    pub fn list_redirects(&self) -> Vec<RedirectEntry> {
        let state = self.state.read().expect("route registry lock poisoned");
        let mut redirects: Vec<RedirectEntry> = state.redirects.values().cloned().collect();
        redirects.sort_by(|a, b| b.changed_at.cmp(&a.changed_at).then(a.route.cmp(&b.route)));
        redirects
    }

    /// Validate a candidate override without storing it. Used by
    /// config validation and by callers that want a dry run.
    pub fn validate_override(&self, name: &str, raw_path: &str) -> UnifiedResult<String> {
        if default_route(name).is_none() {
            return Err(UnifiedError::not_found(format!("Unknown route \"{name}\"")));
        }
        let path = normalize_path(raw_path)?;
        validate_path_shape(&path)?;
        let state = self.state.read().expect("route registry lock poisoned");
        for other in DEFAULT_ROUTES {
            if other.name == name {
                continue;
            }
            let other_path = effective_path_locked(&state, other);
            if paths_conflict(&path, &other_path) {
                return Err(UnifiedError::unprocessable(format!(
                    "Path \"{path}\" conflicts with route \"{}\" (\"{other_path}\")",
                    other.name
                ))
                .with_metadata("reason", serde_json::Value::String("path_conflict".to_string()))
                .with_metadata(
                    "conflicting_route",
                    serde_json::Value::String(other.name.to_string()),
                ));
            }
        }
        Ok(path)
    }
}

fn default_route(name: &str) -> Option<&'static DefaultRoute> {
    DEFAULT_ROUTES.iter().find(|route| route.name == name)
}

/// Effective path of a route from already-locked state.
fn effective_path_locked(state: &RegistryState, route: &DefaultRoute) -> String {
    match state.routes.get(route.name) {
        Some(RouteState {
            settings_override: Some(path),
            ..
        }) => path.clone(),
        Some(RouteState {
            config_override: Some(path),
            ..
        }) => path.clone(),
        _ => route.default_path.to_string(),
    }
}
