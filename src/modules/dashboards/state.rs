// Dashboards - In-memory dashboard state.

use unified_core::context::AppContext;
use unified_core::hooks::HookBus;
use unified_core::routes::RouteRegistry;
use unified_core::db::DbPool;
use crate::themes::engine::ThemeEngine;
use chrono::Utc;
use dashmap::DashMap;
use serde_json::{json, Value};
use std::sync::{Arc, OnceLock, RwLock};
use uuid::Uuid;

use super::{DEFAULT_ADMIN_PATH, DEFAULT_USER_PATH, DEFAULT_VIEW_AS_TTL_MINUTES,
    EVENT_VIEW_AS_EXPIRED, ResellerLink, ViewAsSession};

/// Shared state for the dashboards module. The hook bus, configuration
/// lock, and database pool lock are filled in by `init` (they live on
/// `AppContext`, which handlers already receive, but the middleware -
/// built from `register_middleware` - only gets this state). The route
/// registry is the single shared instance from `AppContext`
/// (`ctx.routes`), attached by `init` together with the theme
/// engine's copy of the same handle (see
/// `DashboardsModule::with_route_registry`, a pre-init hint that
/// `init` supersedes with the context registry).
pub struct DashboardsState {
    pub engine: Arc<ThemeEngine>,
    pub view_as: DashMap<Uuid, ViewAsSession>,
    pub reseller_links: DashMap<(Uuid, Uuid), ResellerLink>,
    pub(crate) admin_prefix: RwLock<String>,
    pub(crate) user_prefix: RwLock<String>,
    pub(crate) view_as_ttl_minutes: RwLock<i64>,
    pub(crate) hooks: OnceLock<Arc<HookBus>>,
    pub(crate) config: OnceLock<Arc<RwLock<unified_core::config::AppConfig>>>,
    pub(crate) db: OnceLock<Arc<RwLock<Option<DbPool>>>>,
    /// The single shared route registry from `AppContext`, attached
    /// by `init`. A plain lock (not `OnceLock`) so `init` can always
    /// unify on the context registry, even when a different instance
    /// was attached earlier through `with_route_registry`: the
    /// prefixes and the theme engine must never resolve through
    /// different registries.
    pub(crate) route_registry: RwLock<Option<Arc<RouteRegistry>>>,
}

impl DashboardsState {
    pub(crate) fn new() -> Self {
        Self {
            engine: Arc::new(ThemeEngine::new("themes")),
            view_as: DashMap::new(),
            reseller_links: DashMap::new(),
            admin_prefix: RwLock::new(DEFAULT_ADMIN_PATH.to_string()),
            user_prefix: RwLock::new(DEFAULT_USER_PATH.to_string()),
            view_as_ttl_minutes: RwLock::new(DEFAULT_VIEW_AS_TTL_MINUTES),
            hooks: OnceLock::new(),
            config: OnceLock::new(),
            db: OnceLock::new(),
            route_registry: RwLock::new(None),
        }
    }

    /// The dashboard prefixes, without a leading slash, as resolved
    /// from a route path (`/dashboard` -> `dashboard`). Only
    /// single-segment paths are usable as Actix scopes; anything else
    /// is ignored so a bad override can never break route mounting.
    pub(crate) fn first_segment(path: &str) -> Option<String> {
        let trimmed = path.trim().trim_start_matches('/');
        let first = trimmed.split('/').next().unwrap_or("");
        if first.is_empty()
            || !first
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        {
            return None;
        }
        Some(first.to_string())
    }

    pub(crate) fn admin_prefix(&self) -> String {
        self.admin_prefix.read().expect("admin prefix lock poisoned").clone()
    }

    pub(crate) fn user_prefix(&self) -> String {
        self.user_prefix.read().expect("user prefix lock poisoned").clone()
    }

    pub(crate) fn view_as_ttl_minutes(&self) -> i64 {
        *self.view_as_ttl_minutes.read().expect("view-as ttl lock poisoned")
    }

    /// Replace `{admin}` / `{user}` placeholders in a nav href with
    /// the configured dashboard prefixes.
    pub(crate) fn expand_href(&self, href: &str) -> String {
        href.replace("{admin}", &self.admin_prefix())
            .replace("{user}", &self.user_prefix())
    }

    /// True when the request path is under a dashboard prefix
    /// (`/admin`, `/{user_prefix}`, or `/api/v1/dashboards`).
    pub(crate) fn is_dashboard_path(&self, path: &str) -> bool {
        if path == "/api/v1/dashboards" || path.starts_with("/api/v1/dashboards/") {
            return true;
        }
        let first = path.trim_start_matches('/').split('/').next().unwrap_or("");
        first == self.admin_prefix() || first == self.user_prefix()
    }

    /// Return the caller's active view-as session, if any. An expired
    /// session is removed and reported with the `admin.view_as.expired`
    /// audit event instead of being returned.
    pub(crate) fn take_expired_or_active_view_as(
        &self,
        admin_id: Uuid,
        ctx: &AppContext,
    ) -> Option<ViewAsSession> {
        let session = self.view_as.get(&admin_id)?.value().clone();
        if session.expires_at <= Utc::now() {
            self.view_as.remove(&admin_id);
            ctx.hooks.emit(
                EVENT_VIEW_AS_EXPIRED,
                json!({
                    "actor_id": admin_id.to_string(),
                    "target_user_id": session.target_user_id.to_string(),
                    "reason": session.reason,
                    "resource": format!("user:{}", session.target_user_id),
                    "expired_at": session.expires_at.to_rfc3339(),
                }),
            );
            return None;
        }
        Some(session)
    }

    /// Emit an audit event through the stashed hook bus when `init`
    /// has run; a no-op otherwise (the middleware path before `init`).
    pub(crate) fn emit_audit(&self, event: &str, payload: Value) {
        if let Some(hooks) = self.hooks.get() {
            hooks.emit(event, payload);
        }
    }
}
