// Dashboards module — separate admin, user (client), and reseller dashboards.
//
// Three scopes, one design system:
//
//   - Admin dashboard under `/admin` (prefix fixed; see the config note
//     below): administrators and staff. Full management scope: system
//     overview, module management, users and roles, orders and invoices,
//     servers and clusters, security, logs, settings.
//   - User (client) dashboard under `/{app.dashboard_path}` (default
//     `/dashboard`, read from the application configuration in `init`;
//     exactly one user-dashboard prefix, never `/app`): the caller's own
//     licenses, downloads, orders, tickets, services, profile, and API
//     keys. Every handler filters by the effective user id.
//   - Reseller: the user dashboard plus a subset for the reseller's own
//     clients (their orders and services) and a commission and payout
//     view. Never the full admin scope.
//
// Enforcement is server-side, in two layers (defence in depth):
//
//   Layer 1 — middleware (`DashboardsAuthMiddleware`, registered through
//   `Module::register_middleware`, priority 600 so it runs after the
//   security-hardening middleware). It authenticates every request under
//   a dashboard prefix and rejects unauthenticated requests with 401 in
//   the standard error envelope before any handler runs. It also expires
//   view-as sessions. Non-dashboard paths pass through untouched.
//
//   Layer 2 — per-handler capability checks (`require_capability`).
//   Handlers never trust the middleware alone: they re-authenticate
//   from the request when no identity is present in the request
//   extensions, resolve the caller's capabilities, and return 403 in
//   the standard error envelope when the capability is missing. A user
//   guessing an admin URL gets 403; a reseller hitting an admin-only
//   route gets 403.
//
// Authentication
// ---------------
// Bearer tokens issued by the Auth Manager (`POST /api/v1/auth/token`):
// HS256 JWTs whose `sub` claim is the user's UUID. Verification here
// mirrors the Auth Manager exactly (base64url without padding,
// HMAC-SHA256 over `header.payload`, signing key
// `"{secret_key}:{salt_key}"` from the application configuration when
// both are set). The token may arrive as an `Authorization: Bearer`
// header or as the session cookie (`app.session_name`, default
// `vault`); page handlers set that cookie from a presented Bearer
// token so browser fetches from the rendered pages authenticate with
// `credentials: "same-origin"`.
//
// Known gaps (see the coordinator report for the integrator snippets):
//   - `src/auth/session.rs` tokens carry a numeric (`u64`) user id and
//     are never issued by any module, while users are UUID-keyed, so
//     they cannot identify a caller and are not accepted here.
//   - The Auth Manager keeps its JWT revocation list in its own
//     private state, so a revoked token cannot be detected here until
//     revocation is shared (hook or shared store).
//   - When both `app.secret_key` and `app.salt_key` are empty the Auth
//     Manager falls back to an in-memory secret this module cannot
//     read; dashboard authentication then fails closed with 401.
//
// Capabilities
// ------------
// Capabilities are `resource:action` strings evaluated with the Roles
// module's `permission_allowed` (see `crate::modules::roles`). The
// `admin` role carries `"*": ["*"]` and therefore holds every
// capability below. The `customer` role already carries
// `orders:read` and `licenses:read`. Capabilities come from two
// sources, unioned: the JWT `scopes` claim, and the caller's role
// permissions read from the database (`users` -> `user_roles` ->
// `roles.permissions`). When no database pool is configured, the
// `dashboard.capabilities_requested` hook is emitted and any handler
// may contribute a `capabilities` array for the user id.
//
// Declared by this module (no dotted `admin.access` spelling: the
// roles system splits on the first `:` into resource and action):
//   `admin:access`, `users:manage`, `modules:manage`,
//   `security:view`, `orders:manage`, `servers:manage`, `logs:view`,
//   `settings:manage`, `admin:view_as_user`, `user:access`,
//   `reseller:access`, `reseller:clients.view`,
//   `reseller:clients.manage`, `reseller:payouts.view`,
//   `licenses:read`, `orders:read`, `tickets:read`, `services:read`,
//   `apikeys:read`.
// `user:access` is the baseline for any authenticated caller: holding
// a valid token is what makes someone a user of their own dashboard.
//
// Reseller clients
// ----------------
// No reseller-to-client link exists in the schema yet. The working
// rule is an explicit link store owned by this module (in-memory,
// like the Roles and Audit stores): a reseller links a client id, and
// every reseller handler filters by the linked client ids. A migration
// snippet for a persistent `reseller_clients` table is given in the
// coordinator report.
//
// Overview data
// -------------
// Counts and rows that live in Diesel tables are read directly
// (`users`, `roles`, `licenses`, `store_orders`,
// `store_entitlements`, `store_downloads`). Sections owned by other
// modules (tickets, services, API keys, security, servers, logs,
// commissions) are assembled through the `dashboard.section_requested`
// hook: this module emits `{ "section", "scope", "user_id" }` and
// merges the first matching result `{ "section", "available", "data" }`.
// A section no module answers is returned as `available: false` with
// the reason, never as invented data. Owning modules can answer the
// hook to light a section up without any change here.
//
// Templates
// ---------
// Template names must be flat (`validate_template_name` in the theme
// engine rejects `/`, so `templates/default/admin/layout.hbs` would
// break registration of the whole embedded default theme). The pages
// are therefore `admin-layout` / `admin-dashboard` / `user-layout` /
// `user-dashboard`, with `admin-nav` / `user-nav` partials under
// `templates/default/partials/`. They reuse the shared design tokens
// and classes from `assets/css/dashboard.css` plus the layout-only
// `assets/css/admin.css` / `assets/css/client.css`.
//
// Customizable dashboard paths
// ----------------------------
// The dashboard prefixes come from the single shared route registry
// (`AppContext.routes`), attached by `init` together with the theme
// engine's copy of the same handle: `user_dashboard` and
// `admin_dashboard` resolve through the registry's config < settings
// override layers, and a `route.changed` hook handler keeps the
// stored prefixes current. Without a shared registry the prefixes
// fall back to `app.dashboard_path` (user dashboard) and the `admin`
// default.
// Actix scopes are registered once at startup, so a live path change
// needs a restart to serve under the new prefix; the Routes module's
// server-level redirect covers the old path until then.

use crate::core::context::AppContext;
use crate::core::error::{UnifiedError, UnifiedResult};
use crate::core::hooks::HookBus;
use crate::core::json::{error_response, success_response};
use crate::core::middleware::{Middleware, MiddlewareResult, NextFn};
use crate::core::modules::{Module, ModuleRegistry, RouteInfo};
use crate::core::pagination::{PageResult, PaginationParams};
use crate::core::routes::{RouteRegistry, ROUTE_CHANGED_EVENT};
use crate::db::models::{License, User};
use crate::db::DbPool;
use crate::modules::roles::permission_allowed;
use crate::themes::engine::ThemeEngine;
use actix_web::dev::ServiceRequest;
use actix_web::http::header;
use actix_web::{web, HttpRequest, HttpResponse};
use base64::Engine;
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use diesel::pg::PgConnection;
use diesel::prelude::*;
use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::Sha256;
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, OnceLock, RwLock};
use uuid::Uuid;

type HmacSha256 = Hmac<Sha256>;

/// Default lifetime of a view-as session, in minutes.
pub const DEFAULT_VIEW_AS_TTL_MINUTES: i64 = 30;
/// Hard limits for a view-as lifetime set through the settings endpoint.
pub const MIN_VIEW_AS_TTL_MINUTES: i64 = 1;
pub const MAX_VIEW_AS_TTL_MINUTES: i64 = 480;

/// Default admin dashboard prefix. The user prefix comes from
/// `app.dashboard_path` in the configuration; there is no matching
/// `app.admin_path` setting yet (see the coordinator report for the
/// config snippet), so the admin prefix is fixed until it lands.
pub const DEFAULT_ADMIN_PATH: &str = "admin";
pub const DEFAULT_USER_PATH: &str = "dashboard";

// ---------------------------------------------------------------------------
// Capabilities declared by this module (resource:action, see the header).
// ---------------------------------------------------------------------------

pub const CAP_ADMIN_ACCESS: &str = "admin:access";
pub const CAP_USERS_MANAGE: &str = "users:manage";
pub const CAP_MODULES_MANAGE: &str = "modules:manage";
pub const CAP_SECURITY_VIEW: &str = "security:view";
pub const CAP_ORDERS_MANAGE: &str = "orders:manage";
pub const CAP_SERVERS_MANAGE: &str = "servers:manage";
pub const CAP_LOGS_VIEW: &str = "logs:view";
pub const CAP_SETTINGS_MANAGE: &str = "settings:manage";
pub const CAP_VIEW_AS_USER: &str = "admin:view_as_user";
pub const CAP_USER_ACCESS: &str = "user:access";
pub const CAP_RESELLER_ACCESS: &str = "reseller:access";
pub const CAP_RESELLER_CLIENTS_VIEW: &str = "reseller:clients.view";
pub const CAP_RESELLER_CLIENTS_MANAGE: &str = "reseller:clients.manage";
pub const CAP_RESELLER_PAYOUTS_VIEW: &str = "reseller:payouts.view";
pub const CAP_LICENSES_READ: &str = "licenses:read";
pub const CAP_ORDERS_READ: &str = "orders:read";
pub const CAP_TICKETS_READ: &str = "tickets:read";
pub const CAP_SERVICES_READ: &str = "services:read";
pub const CAP_APIKEYS_READ: &str = "apikeys:read";

/// Every capability this module enforces, used to expand wildcard
/// grants and to report the caller's effective capability set.
pub const ALL_CAPABILITIES: &[&str] = &[
    CAP_ADMIN_ACCESS,
    CAP_USERS_MANAGE,
    CAP_MODULES_MANAGE,
    CAP_SECURITY_VIEW,
    CAP_ORDERS_MANAGE,
    CAP_SERVERS_MANAGE,
    CAP_LOGS_VIEW,
    CAP_SETTINGS_MANAGE,
    CAP_VIEW_AS_USER,
    CAP_USER_ACCESS,
    CAP_RESELLER_ACCESS,
    CAP_RESELLER_CLIENTS_VIEW,
    CAP_RESELLER_CLIENTS_MANAGE,
    CAP_RESELLER_PAYOUTS_VIEW,
    CAP_LICENSES_READ,
    CAP_ORDERS_READ,
    CAP_TICKETS_READ,
    CAP_SERVICES_READ,
    CAP_APIKEYS_READ,
];

/// Hook event emitted when a section owned by another module is
/// needed. Handlers answer with a result object
/// `{ "section", "available": true, "data": ... }`.
pub const EVENT_SECTION_REQUESTED: &str = "dashboard.section_requested";
/// Hook event emitted when no database pool is configured and a
/// caller's capabilities cannot be read from role rows. Handlers may
/// contribute `{ "capabilities": ["resource:action", ...] }`.
pub const EVENT_CAPABILITIES_REQUESTED: &str = "dashboard.capabilities_requested";
/// Audit events for admin view-as. The Audit module only records
/// events listed in its `AUDITED_EVENTS`; these names must be added
/// there (snippet in the coordinator report) before the entries land
/// in the audit log. Emitting them is correct regardless.
pub const EVENT_VIEW_AS_STARTED: &str = "admin.view_as.started";
pub const EVENT_VIEW_AS_ENDED: &str = "admin.view_as.ended";
pub const EVENT_VIEW_AS_EXPIRED: &str = "admin.view_as.expired";
pub const EVENT_RESELLER_CLIENT_LINKED: &str = "reseller.client.linked";
pub const EVENT_RESELLER_CLIENT_UNLINKED: &str = "reseller.client.unlinked";

// ---------------------------------------------------------------------------
// Navigation
// ---------------------------------------------------------------------------

/// One navigation item. `href` is a template: `{admin}` and `{user}`
/// are replaced with the configured dashboard prefixes at render time.
#[derive(Debug, Clone, Serialize)]
pub struct NavItem {
    pub id: String,
    pub label: String,
    pub href: String,
    pub icon: &'static str,
    pub capability: &'static str,
    pub section: &'static str,
    #[serde(default)]
    pub active: bool,
}

struct NavDef {
    id: &'static str,
    label: &'static str,
    href: &'static str,
    icon: &'static str,
    capability: &'static str,
    section: &'static str,
    scope: &'static str,
}

/// The static navigation table. Items are filtered by scope and by the
/// caller's capabilities; module entries are appended from the module
/// registry for the admin scope (see `nav_items`).
const NAV_DEFS: &[NavDef] = &[
    // Admin scope.
    NavDef { id: "admin.overview", label: "Overview", href: "/{admin}", icon: "icon-grid", capability: CAP_ADMIN_ACCESS, section: "overview", scope: "admin" },
    NavDef { id: "admin.modules", label: "Modules", href: "/{admin}/modules", icon: "icon-layers", capability: CAP_MODULES_MANAGE, section: "manage", scope: "admin" },
    NavDef { id: "admin.users", label: "Users & Roles", href: "/{admin}/users", icon: "icon-users", capability: CAP_USERS_MANAGE, section: "manage", scope: "admin" },
    NavDef { id: "admin.orders", label: "Orders & Invoices", href: "/{admin}/orders", icon: "icon-file", capability: CAP_ORDERS_MANAGE, section: "commerce", scope: "admin" },
    NavDef { id: "admin.servers", label: "Servers & Clusters", href: "/{admin}/servers", icon: "icon-server", capability: CAP_SERVERS_MANAGE, section: "operations", scope: "admin" },
    NavDef { id: "admin.security", label: "Security", href: "/{admin}/security", icon: "icon-alert", capability: CAP_SECURITY_VIEW, section: "operations", scope: "admin" },
    NavDef { id: "admin.logs", label: "Logs", href: "/{admin}/logs", icon: "icon-activity", capability: CAP_LOGS_VIEW, section: "operations", scope: "admin" },
    NavDef { id: "admin.settings", label: "Settings", href: "/{admin}/settings", icon: "icon-settings", capability: CAP_SETTINGS_MANAGE, section: "manage", scope: "admin" },
    // User scope.
    NavDef { id: "user.overview", label: "Overview", href: "/{user}", icon: "icon-grid", capability: CAP_USER_ACCESS, section: "overview", scope: "user" },
    NavDef { id: "user.licenses", label: "My Licenses", href: "/{user}/licenses", icon: "icon-check-circle", capability: CAP_LICENSES_READ, section: "commerce", scope: "user" },
    NavDef { id: "user.downloads", label: "My Downloads", href: "/{user}/downloads", icon: "icon-package", capability: CAP_LICENSES_READ, section: "commerce", scope: "user" },
    NavDef { id: "user.orders", label: "My Orders", href: "/{user}/orders", icon: "icon-file", capability: CAP_ORDERS_READ, section: "commerce", scope: "user" },
    NavDef { id: "user.tickets", label: "My Tickets", href: "/{user}/tickets", icon: "icon-bell", capability: CAP_TICKETS_READ, section: "support", scope: "user" },
    NavDef { id: "user.services", label: "My Services", href: "/{user}/services", icon: "icon-server", capability: CAP_SERVICES_READ, section: "operations", scope: "user" },
    NavDef { id: "user.apikeys", label: "API Keys", href: "/{user}/apikeys", icon: "icon-command", capability: CAP_APIKEYS_READ, section: "account", scope: "user" },
    NavDef { id: "user.profile", label: "Profile", href: "/{user}/profile", icon: "icon-users", capability: CAP_USER_ACCESS, section: "account", scope: "user" },
    // Reseller scope (rendered inside the user layout, never the admin one).
    NavDef { id: "reseller.clients", label: "My Clients", href: "/{user}/reseller/clients", icon: "icon-users", capability: CAP_RESELLER_CLIENTS_VIEW, section: "reseller", scope: "reseller" },
    NavDef { id: "reseller.payouts", label: "Commissions & Payouts", href: "/{user}/reseller/payouts", icon: "icon-activity", capability: CAP_RESELLER_PAYOUTS_VIEW, section: "reseller", scope: "reseller" },
];

/// Build the navigation for one scope, filtered by the caller's
/// capabilities. Admin navigation additionally lists every registered
/// module (from `ctx.modules.summary()`) under the Modules section, so
/// the nav is built from the live module registry, not a hard-coded
/// list. `current` marks the item whose href matches the current path.
fn nav_items(
    ctx: &AppContext,
    state: &DashboardsState,
    scope: &str,
    capabilities: &HashSet<String>,
    current: Option<&str>,
) -> Vec<NavItem> {
    let mut items: Vec<NavItem> = NAV_DEFS
        .iter()
        .filter(|def| def.scope == scope)
        .filter(|def| has_capability(capabilities, def.capability))
        .map(|def| NavItem {
            id: def.id.to_string(),
            label: def.label.to_string(),
            href: state.expand_href(def.href),
            icon: def.icon,
            capability: def.capability,
            section: def.section,
            active: current.is_some_and(|path| path == state.expand_href(def.href)),
        })
        .collect();
    if scope == "admin" && has_capability(capabilities, CAP_MODULES_MANAGE) {
        for module in ctx.modules.summary() {
            let href = format!("/{}/modules/{}", state.admin_prefix(), module.id);
            items.push(NavItem {
                id: format!("module.{}", module.id),
                label: module.name.to_string(),
                href: href.clone(),
                icon: "icon-layers",
                capability: CAP_MODULES_MANAGE,
                section: "modules",
                active: current.is_some_and(|path| path == href),
            });
        }
    }
    items
}

/// True when the capability set grants `capability`. The wildcard
/// scope `*` grants everything; `user:access` is the authenticated
/// baseline and is always present in a resolved set.
fn has_capability(capabilities: &HashSet<String>, capability: &str) -> bool {
    capabilities.contains("*") || capabilities.contains(capability)
}

// ---------------------------------------------------------------------------
// Identity and authentication
// ---------------------------------------------------------------------------

/// Claims of an Auth Manager JWT (`POST /api/v1/auth/token`).
#[derive(Debug, Clone, Deserialize)]
struct JwtClaims {
    sub: String,
    #[allow(dead_code)]
    iss: String,
    #[allow(dead_code)]
    iat: i64,
    exp: i64,
    #[allow(dead_code)]
    jti: String,
    #[serde(default)]
    scopes: Vec<String>,
}

/// The authenticated caller, as carried in request extensions by the
/// middleware and re-resolved by handlers (layer 2). When an admin is
/// viewing as a user, `user_id` is the TARGET user and
/// `impersonated_by` is the admin; capabilities are always resolved
/// for `user_id`, never for the admin, so impersonation cannot
/// escalate.
#[derive(Debug, Clone)]
pub struct CallerIdentity {
    /// Effective user id (the target while viewing as a user).
    pub user_id: Uuid,
    /// The real authenticated user id (the admin while viewing as a user).
    pub real_user_id: Uuid,
    /// Raw capability strings from the JWT `scopes` claim.
    pub token_scopes: Vec<String>,
    pub impersonating: bool,
    pub impersonated_by: Option<Uuid>,
    pub view_as_expires_at: Option<DateTime<Utc>>,
}

impl CallerIdentity {
    fn direct(user_id: Uuid, token_scopes: Vec<String>) -> Self {
        Self {
            user_id,
            real_user_id: user_id,
            token_scopes,
            impersonating: false,
            impersonated_by: None,
            view_as_expires_at: None,
        }
    }
}

/// The caller plus their resolved capabilities and profile, as used
/// by every handler after the capability check.
struct CallerContext {
    identity: CallerIdentity,
    capabilities: HashSet<String>,
    profile: Option<User>,
}

/// Extract a Bearer token from the `Authorization` header, falling
/// back to the session cookie (`app.session_name`).
fn extract_token(req: &HttpRequest, session_cookie_name: &str) -> Option<String> {
    if let Some(header) = req.headers().get(header::AUTHORIZATION) {
        if let Ok(value) = header.to_str() {
            let trimmed = value.trim();
            if let Some(token) = trimmed.strip_prefix("Bearer ") {
                if !token.trim().is_empty() {
                    return Some(token.trim().to_string());
                }
            }
            if let Some(token) = trimmed.strip_prefix("bearer ") {
                if !token.trim().is_empty() {
                    return Some(token.trim().to_string());
                }
            }
        }
    }
    cookie_value(req, session_cookie_name)
}

/// Read one cookie value from the raw `Cookie` header.
fn cookie_value(req: &HttpRequest, name: &str) -> Option<String> {
    let header = req.headers().get(header::COOKIE)?.to_str().ok()?;
    for part in header.split(';') {
        let part = part.trim();
        if let Some((key, value)) = part.split_once('=') {
            if key.trim() == name && !value.trim().is_empty() {
                return Some(value.trim().to_string());
            }
        }
    }
    None
}

/// Verify an Auth Manager JWT. This mirrors
/// `AuthManagerState::verify_token` exactly: three base64url segments
/// without padding, HMAC-SHA256 over `header.payload`, signing key
/// `"{secret_key}:{salt_key}"` from the application configuration.
/// Returns the claims on success; every failure is a 401.
fn verify_jwt(token: &str, secret_key: &str, salt_key: &str) -> UnifiedResult<JwtClaims> {
    if secret_key.is_empty() || salt_key.is_empty() {
        return Err(UnifiedError::unauthorized(
            "Dashboard authentication is not configured: app.secret_key and app.salt_key must both be set",
        )
        .with_metadata("kind", json!("auth_not_configured")));
    }
    let parts: Vec<&str> = token.split('.').collect();
    if parts.len() != 3 {
        return Err(UnifiedError::unauthorized(
            "Token must have three base64url segments",
        ));
    }
    let header_bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(parts[0])
        .map_err(|_| UnifiedError::unauthorized("Token header is not valid base64url"))?;
    let header: Value = serde_json::from_slice(&header_bytes)
        .map_err(|_| UnifiedError::unauthorized("Token header is not valid JSON"))?;
    if header.get("alg").and_then(|value| value.as_str()) != Some("HS256") {
        return Err(UnifiedError::unauthorized(
            "Token algorithm must be HS256",
        ));
    }
    let signing_key = format!("{secret_key}:{salt_key}");
    let signing_input = format!("{}.{}", parts[0], parts[1]);
    let provided = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(parts[2])
        .map_err(|_| UnifiedError::unauthorized("Token signature is not valid base64url"))?;
    let mut mac = HmacSha256::new_from_slice(signing_key.as_bytes())
        .map_err(|error| UnifiedError::internal(format!("HMAC initialisation failed: {error}")))?;
    mac.update(signing_input.as_bytes());
    mac.verify_slice(&provided)
        .map_err(|_| UnifiedError::unauthorized("Token signature is invalid"))?;
    let payload_bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(parts[1])
        .map_err(|_| UnifiedError::unauthorized("Token payload is not valid base64url"))?;
    let claims: JwtClaims = serde_json::from_slice(&payload_bytes)
        .map_err(|_| UnifiedError::unauthorized("Token payload is not valid JSON"))?;
    if claims.exp <= Utc::now().timestamp() {
        return Err(UnifiedError::unauthorized("Token has expired"));
    }
    Ok(claims)
}

/// Authenticate a request: Bearer header or session cookie, verified
/// as an Auth Manager JWT whose `sub` is the user's UUID. View-as
/// sessions are applied here: when the real caller has an active,
/// unexpired view-as session, the effective identity is the target
/// user. Expired sessions are removed and reported as expired.
fn authenticate(
    req: &HttpRequest,
    ctx: &AppContext,
    state: &DashboardsState,
) -> UnifiedResult<CallerIdentity> {
    let (secret_key, salt_key, session_name) = {
        let config = ctx.config.read().expect("config lock poisoned");
        (
            config.app.secret_key.clone(),
            config.app.salt_key.clone(),
            config.app.session_name.clone(),
        )
    };
    let token = extract_token(req, &session_name)
        .ok_or_else(|| UnifiedError::unauthorized("Authentication required: provide a Bearer token"))?;
    let claims = verify_jwt(&token, &secret_key, &salt_key)?;
    let user_id = Uuid::parse_str(claims.sub.trim()).map_err(|_| {
        UnifiedError::unauthorized("Token subject is not a user id")
            .with_metadata("kind", json!("invalid_subject"))
    })?;
    let mut identity = CallerIdentity::direct(user_id, claims.scopes);
    if let Some(session) = state.take_expired_or_active_view_as(user_id, ctx) {
        identity.user_id = session.target_user_id;
        identity.impersonating = true;
        identity.impersonated_by = Some(user_id);
        identity.view_as_expires_at = Some(session.expires_at);
    }
    Ok(identity)
}

/// Read the identity the middleware stored in the request extensions,
/// if any.
fn identity_from_extensions(req: &HttpRequest) -> Option<CallerIdentity> {
    req.extensions().get::<CallerIdentity>().cloned()
}

// ---------------------------------------------------------------------------
// Module state
// ---------------------------------------------------------------------------

/// One active admin view-as session. Keyed by the admin's user id in
/// `DashboardsState::view_as`. The impersonated session keeps the
/// target user's capabilities only; the admin id is retained so the
/// session can be ended and audited.
#[derive(Debug, Clone, Serialize)]
pub struct ViewAsSession {
    pub admin_id: Uuid,
    pub target_user_id: Uuid,
    pub reason: String,
    pub started_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
}

/// One reseller-to-client link. `verified` is true when the client id
/// was confirmed to exist in the users table at link time (a database
/// pool was available); without a pool the link is still recorded but
/// unverified.
#[derive(Debug, Clone, Serialize)]
pub struct ResellerLink {
    pub reseller_id: Uuid,
    pub client_id: Uuid,
    pub verified: bool,
    pub created_at: DateTime<Utc>,
}

/// Shared state for the dashboards module. The hook bus, configuration
/// lock, and database pool lock are filled in by `init` (they live on
/// `AppContext`, which handlers already receive, but the middleware —
/// built from `register_middleware` — only gets this state). The route
/// registry is the single shared instance from `AppContext`
/// (`ctx.routes`), attached by `init` together with the theme
/// engine's copy of the same handle (see
/// `DashboardsModule::with_route_registry`, a pre-init hint that
/// `init` supersedes with the context registry).
pub struct DashboardsState {
    pub engine: Arc<ThemeEngine>,
    pub view_as: DashMap<Uuid, ViewAsSession>,
    pub reseller_links: DashMap<(Uuid, Uuid), ResellerLink>,
    admin_prefix: RwLock<String>,
    user_prefix: RwLock<String>,
    view_as_ttl_minutes: RwLock<i64>,
    hooks: OnceLock<Arc<HookBus>>,
    config: OnceLock<Arc<RwLock<crate::core::config::AppConfig>>>,
    db: OnceLock<Arc<RwLock<Option<DbPool>>>>,
    /// The single shared route registry from `AppContext`, attached
    /// by `init`. A plain lock (not `OnceLock`) so `init` can always
    /// unify on the context registry, even when a different instance
    /// was attached earlier through `with_route_registry`: the
    /// prefixes and the theme engine must never resolve through
    /// different registries.
    route_registry: RwLock<Option<Arc<RouteRegistry>>>,
}

impl DashboardsState {
    fn new() -> Self {
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
    fn first_segment(path: &str) -> Option<String> {
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

    fn admin_prefix(&self) -> String {
        self.admin_prefix.read().expect("admin prefix lock poisoned").clone()
    }

    fn user_prefix(&self) -> String {
        self.user_prefix.read().expect("user prefix lock poisoned").clone()
    }

    fn view_as_ttl_minutes(&self) -> i64 {
        *self.view_as_ttl_minutes.read().expect("view-as ttl lock poisoned")
    }

    /// Replace `{admin}` / `{user}` placeholders in a nav href with
    /// the configured dashboard prefixes.
    fn expand_href(&self, href: &str) -> String {
        href.replace("{admin}", &self.admin_prefix())
            .replace("{user}", &self.user_prefix())
    }

    /// True when the request path is under a dashboard prefix
    /// (`/admin`, `/{user_prefix}`, or `/api/v1/dashboards`).
    fn is_dashboard_path(&self, path: &str) -> bool {
        if path == "/api/v1/dashboards" || path.starts_with("/api/v1/dashboards/") {
            return true;
        }
        let first = path.trim_start_matches('/').split('/').next().unwrap_or("");
        first == self.admin_prefix() || first == self.user_prefix()
    }

    /// Return the caller's active view-as session, if any. An expired
    /// session is removed and reported with the `admin.view_as.expired`
    /// audit event instead of being returned.
    fn take_expired_or_active_view_as(
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
    fn emit_audit(&self, event: &str, payload: Value) {
        if let Some(hooks) = self.hooks.get() {
            hooks.emit(event, payload);
        }
    }
}

// ---------------------------------------------------------------------------
// Database access (blocking Diesel work runs on the blocking pool)
// ---------------------------------------------------------------------------

fn db_pool(ctx: &AppContext) -> UnifiedResult<DbPool> {
    ctx.db
        .read()
        .expect("db lock poisoned")
        .clone()
        .ok_or_else(|| {
            UnifiedError::new(
                "Database is not configured: this dashboard section needs a database connection",
                503,
            )
            .with_metadata("kind", json!("database_not_configured"))
        })
}

/// Run blocking Diesel work off the async worker threads.
async fn run_blocking<F, T>(pool: DbPool, work: F) -> UnifiedResult<T>
where
    F: FnOnce(&mut PgConnection) -> UnifiedResult<T> + Send + 'static,
    T: Send + 'static,
{
    tokio::task::spawn_blocking(move || {
        let mut connection = pool.get().map_err(|error| {
            UnifiedError::internal(format!("Cannot borrow a database connection: {error}"))
        })?;
        work(&mut connection)
    })
    .await
    .map_err(|error| UnifiedError::internal(format!("Database task failed: {error}")))?
}

/// One row of `store_orders` (there is no shared model for it).
#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = crate::db::schema::store_orders)]
struct OrderRow {
    id: Uuid,
    user_id: String,
    subtotal: f64,
    discount_total: f64,
    total: f64,
    currency: String,
    coupon_code: Option<String>,
    company_id: Option<Uuid>,
    trial_until: Option<DateTime<Utc>>,
    status: String,
    provider: Option<String>,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

/// One row of `store_downloads` (there is no shared model for it).
#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = crate::db::schema::store_downloads)]
struct DownloadRow {
    id: Uuid,
    license_id: Option<Uuid>,
    release_id: Option<Uuid>,
    software_slug: Option<String>,
    order_id: Option<Uuid>,
    downloaded_at: DateTime<Utc>,
}

/// Resolve the caller's capabilities: the JWT scopes unioned with the
/// permissions of every role assigned to the user in the database.
/// Returns the user profile as well. When no database pool is
/// configured, the `dashboard.capabilities_requested` hook is emitted
/// and any contributed `capabilities` arrays are merged in; the
/// profile is then unavailable and only the token scopes apply.
async fn resolve_capabilities(
    ctx: &AppContext,
    identity: &CallerIdentity,
) -> UnifiedResult<(HashSet<String>, Option<User>)> {
    let mut capabilities: HashSet<String> = HashSet::new();
    for scope in &identity.token_scopes {
        let scope = scope.trim();
        if scope.is_empty() {
            continue;
        }
        if scope == "*" {
            capabilities.insert("*".to_string());
        } else if scope.contains(':') {
            capabilities.insert(scope.to_string());
        }
    }
    // Any authenticated caller is a user of their own dashboard.
    capabilities.insert(CAP_USER_ACCESS.to_string());
    let pool = match ctx.db.read().expect("db lock poisoned").clone() {
        Some(pool) => pool,
        None => {
            let hook = ctx.hooks.emit(
                EVENT_CAPABILITIES_REQUESTED,
                json!({ "user_id": identity.user_id.to_string() }),
            );
            for result in &hook.results {
                if let Some(list) = result.get("capabilities").and_then(|value| value.as_array()) {
                    for item in list {
                        if let Some(capability) = item.as_str() {
                            if capability == "*" || capability.contains(':') {
                                capabilities.insert(capability.to_string());
                            }
                        }
                    }
                }
            }
            return Ok((expand_wildcard(capabilities), None));
        }
    };
    let user_id = identity.user_id;
    let (profile, permissions) = run_blocking(pool, move |connection| {
        use crate::db::schema::user_roles::dsl as user_roles_dsl;
        use crate::db::schema::users::dsl as users_dsl;
        let profile = users_dsl::users
            .find(user_id)
            .select(User::as_select())
            .first::<User>(connection)
            .optional()
            .map_err(UnifiedError::from)?;
        let Some(profile) = profile else {
            return Err(UnifiedError::unauthorized(
                "Authenticated user does not exist",
            )
            .with_metadata("kind", json!("unknown_user")));
        };
        if !profile.is_active {
            return Err(UnifiedError::unauthorized("User account is disabled")
                .with_metadata("kind", json!("user_disabled")));
        }
        let role_ids: Vec<Uuid> = user_roles_dsl::user_roles
            .filter(user_roles_dsl::user_id.eq(user_id))
            .select(user_roles_dsl::role_id)
            .load::<Uuid>(connection)
            .map_err(UnifiedError::from)?;
        // Role permissions are `jsonb`, but Diesel here has no
        // `serde_json` feature, so `Value` cannot be loaded from the
        // column directly (nothing else in the codebase does so
        // either). Read the JSON as text and parse it instead.
        let mut permissions: Vec<Value> = Vec::new();
        if !role_ids.is_empty() {
            #[derive(Debug, QueryableByName)]
            struct PermissionRow {
                #[diesel(sql_type = diesel::sql_types::Text)]
                permissions: String,
            }
            let rows = diesel::sql_query(
                "SELECT permissions::text AS permissions FROM roles WHERE id = ANY($1)",
            )
            .bind::<diesel::sql_types::Array<diesel::sql_types::Uuid>, _>(role_ids)
            .load::<PermissionRow>(connection)
            .map_err(UnifiedError::from)?;
            for row in rows {
                if let Ok(value) = serde_json::from_str::<Value>(&row.permissions) {
                    permissions.push(value);
                }
            }
        }
        Ok((Some(profile), permissions))
    })
    .await?;
    for permissions in &permissions {
        for capability in ALL_CAPABILITIES {
            if permission_allowed(permissions, capability) {
                capabilities.insert(capability.to_string());
            }
        }
        // `permission_allowed` grants `"*"` only for a literal
        // `"*": ["*"]` entry, so a wildcard role expands to every
        // capability through `expand_wildcard` below.
        if permission_allowed(permissions, "*") {
            capabilities.insert("*".to_string());
        }
    }
    Ok((expand_wildcard(capabilities), profile))
}

/// A wildcard grant expands to every declared capability so the
/// scope endpoint and the navigation can list them explicitly.
fn expand_wildcard(mut capabilities: HashSet<String>) -> HashSet<String> {
    if capabilities.contains("*") {
        for capability in ALL_CAPABILITIES {
            capabilities.insert(capability.to_string());
        }
    }
    capabilities
}

/// The caller's effective scope: `admin` when they hold
/// `admin:access`, else `reseller` when they hold `reseller:access`,
/// else `user`. Scopes available to the caller, highest first.
fn effective_scope(capabilities: &HashSet<String>) -> (&'static str, Vec<&'static str>) {
    if has_capability(capabilities, CAP_ADMIN_ACCESS) {
        ("admin", vec!["admin", "reseller", "user"])
    } else if has_capability(capabilities, CAP_RESELLER_ACCESS) {
        ("reseller", vec!["reseller", "user"])
    } else {
        ("user", vec!["user"])
    }
}

// ---------------------------------------------------------------------------
// Layer 2 — per-handler guards (defence in depth over the middleware)
// ---------------------------------------------------------------------------

/// Resolve the caller for a handler: prefer the identity the
/// middleware stored in the request extensions, otherwise
/// re-authenticate from the request itself, so a handler stays
/// protected even when the middleware is bypassed or misconfigured.
/// Returns an error `HttpResponse` (standard envelope) on failure.
async fn require_identity(
    req: &HttpRequest,
    ctx: &AppContext,
    state: &DashboardsState,
) -> Result<CallerIdentity, HttpResponse> {
    if let Some(identity) = identity_from_extensions(req) {
        return Ok(identity);
    }
    let debug = ctx.is_debug();
    authenticate(req, ctx, state).map_err(|error| error_response(&error, debug))
}

/// Enforce one capability for a handler. `CAP_USER_ACCESS` is the
/// authenticated baseline and always passes. Returns the caller
/// context (identity, resolved capabilities, profile) on success, or
/// a 401 / 403 `HttpResponse` in the standard envelope.
async fn require_capability(
    req: &HttpRequest,
    ctx: &AppContext,
    state: &DashboardsState,
    capability: &str,
) -> Result<CallerContext, HttpResponse> {
    let debug = ctx.is_debug();
    let identity = require_identity(req, ctx, state).await?;
    let (capabilities, profile) = resolve_capabilities(ctx, &identity)
        .await
        .map_err(|error| error_response(&error, debug))?;
    if capability != CAP_USER_ACCESS && !has_capability(&capabilities, capability) {
        let (scope, _) = effective_scope(&capabilities);
        return Err(error_response(
            &UnifiedError::forbidden(format!(
                "Capability \"{capability}\" is required for this dashboard section"
            ))
            .with_metadata("capability", json!(capability))
            .with_metadata("scope", json!(scope)),
            debug,
        ));
    }
    Ok(CallerContext {
        identity,
        capabilities,
        profile,
    })
}

/// The same check, but against the REAL authenticated user instead of
/// the effective (possibly impersonated) identity. Used by the
/// view-as stop endpoint: while impersonating, the effective identity
/// is the target user, who must never be able to end (or extend) the
/// admin's session — only the admin can.
async fn require_real_capability(
    req: &HttpRequest,
    ctx: &AppContext,
    state: &DashboardsState,
    capability: &str,
) -> Result<CallerContext, HttpResponse> {
    let debug = ctx.is_debug();
    let identity = require_identity(req, ctx, state).await?;
    let real = CallerIdentity::direct(identity.real_user_id, identity.token_scopes.clone());
    let (capabilities, profile) = resolve_capabilities(ctx, &real)
        .await
        .map_err(|error| error_response(&error, debug))?;
    if !has_capability(&capabilities, capability) {
        return Err(error_response(
            &UnifiedError::forbidden(format!(
                "Capability \"{capability}\" is required for this dashboard section"
            ))
            .with_metadata("capability", json!(capability)),
            debug,
        ));
    }
    Ok(CallerContext {
        identity,
        capabilities,
        profile,
    })
}

/// Ask owning modules for one dashboard section through the hook bus.
/// Returns the first matching result, or an `available: false`
/// placeholder when no module answers. Never invents section data.
fn hook_section(ctx: &AppContext, section: &str, scope: &str, user_id: Uuid) -> Value {
    let hook = ctx.hooks.emit(
        EVENT_SECTION_REQUESTED,
        json!({
            "section": section,
            "scope": scope,
            "user_id": user_id.to_string(),
        }),
    );
    for result in &hook.results {
        if result.get("section").and_then(|value| value.as_str()) == Some(section) {
            return result.clone();
        }
    }
    json!({
        "section": section,
        "available": false,
        "data": Value::Null,
        "reason": "No module answered dashboard.section_requested for this section",
    })
}

/// Paginate an already-collected in-memory list with the standard
/// pagination parameters.
fn paginate_vec<T: Clone + Serialize>(items: Vec<T>, params: &PaginationParams) -> PageResult<T> {
    let pagination = params.normalized();
    let total = items.len() as u64;
    let start = ((pagination.page - 1) * pagination.per_page) as usize;
    let page_items: Vec<T> = items
        .into_iter()
        .skip(start)
        .take(pagination.per_page as usize)
        .collect();
    PageResult::new(page_items, total, &pagination)
}

/// Shared page data every rendered dashboard page receives.
fn page_data(
    ctx: &AppContext,
    state: &DashboardsState,
    caller: &CallerContext,
    scope: &str,
    title: &str,
    current_path: &str,
) -> Value {
    let nav = nav_items(ctx, state, scope, &caller.capabilities, Some(current_path));
    let (effective_scope_name, _) = effective_scope(&caller.capabilities);
    let view_as = if caller.identity.impersonating {
        let session = state.view_as.get(&caller.identity.real_user_id);
        json!({
            "active": true,
            "admin_id": caller.identity.real_user_id.to_string(),
            "target_user_id": caller.identity.user_id.to_string(),
            "target_name": caller.profile.as_ref().map(|user| user.name.clone()),
            "expires_at": caller.identity.view_as_expires_at.map(|time| time.to_rfc3339()),
            "reason": session.map(|entry| entry.value().reason.clone()),
        })
    } else {
        json!({ "active": false })
    };
    json!({
        "app_name": "Unified",
        "title": title,
        "theme": "light",
        "scope": scope,
        "is_reseller": scope == "reseller",
        "effective_scope": effective_scope_name,
        "nav": nav,
        "view_as": view_as,
        "user": caller.profile.as_ref().map(|user| json!({
            "id": user.id.to_string(),
            "email": user.email.clone(),
            "name": user.name.clone(),
            "company_id": user.company_id.map(|id| id.to_string()),
        })),
        "user_id": caller.identity.user_id.to_string(),
        "admin_path": state.admin_prefix(),
        "user_path": state.user_prefix(),
    })
}

/// Render a content template inside its layout template and return
/// the HTML response. When the page request carried a Bearer token,
/// the response also sets the session cookie so browser fetches from
/// the page authenticate with `credentials: "same-origin"`.
fn render_page_response(
    req: &HttpRequest,
    ctx: &AppContext,
    state: &DashboardsState,
    content_template: &str,
    layout_template: &str,
    mut data: Value,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let body = match state.engine.render(content_template, &data) {
        Ok(html) => html,
        Err(error) => return error_response(&error, debug),
    };
    if let Some(object) = data.as_object_mut() {
        object.insert("body".to_string(), Value::String(body));
    }
    let html = match state.engine.render(layout_template, &data) {
        Ok(html) => html,
        Err(error) => return error_response(&error, debug),
    };
    let mut response = HttpResponse::Ok()
        .content_type("text/html; charset=utf-8")
        .body(html);
    let session_name = {
        let config = ctx.config.read().expect("config lock poisoned");
        config.app.session_name.clone()
    };
    if let Some(header_value) = req.headers().get(header::AUTHORIZATION) {
        if let Ok(value) = header_value.to_str() {
            if let Some(token) = value
                .trim()
                .strip_prefix("Bearer ")
                .or_else(|| value.trim().strip_prefix("bearer "))
            {
                let cookie = format!(
                    "{session_name}={token}; Path=/; HttpOnly; SameSite=Lax"
                );
                response.headers_mut().insert(
                    header::SET_COOKIE,
                    cookie.parse().unwrap_or_else(|_| {
                        actix_web::http::header::HeaderValue::from_static("")
                    }),
                );
            }
        }
    }
    response
}

// ---------------------------------------------------------------------------
// Overview data (database-backed counts and rows; hook-merged sections)
// ---------------------------------------------------------------------------

/// Admin overview: system facts from the live registry and database
/// counts. Sections owned by other modules arrive through the hook
/// bus and report `available: false` until their owners answer.
async fn admin_overview_data(ctx: &AppContext, caller: &CallerContext) -> UnifiedResult<Value> {
    let pool = db_pool(ctx).ok();
    let (users_total, roles_total, licenses_total, orders_total) = match &pool {
        Some(pool) => {
            let pool = pool.clone();
            run_blocking(pool, |connection| {
                use crate::db::schema::licenses::dsl as licenses_dsl;
                use crate::db::schema::roles::dsl as roles_dsl;
                use crate::db::schema::store_orders::dsl as orders_dsl;
                use crate::db::schema::users::dsl as users_dsl;
                let users_total = users_dsl::users.count().get_result::<i64>(connection)?;
                let roles_total = roles_dsl::roles.count().get_result::<i64>(connection)?;
                let licenses_total =
                    licenses_dsl::licenses.count().get_result::<i64>(connection)?;
                let orders_total =
                    orders_dsl::store_orders.count().get_result::<i64>(connection)?;
                Ok((users_total, roles_total, licenses_total, orders_total))
            })
            .await?
        }
        None => (0, 0, 0, 0),
    };
    let modules = ctx.modules.summary();
    let dependency_report = ctx.modules.validate_dependencies();
    let scope = "admin";
    Ok(json!({
        "system": {
            "uptime_seconds": ctx.uptime_seconds(),
            "modules_total": modules.len(),
            "modules": modules,
            "dependencies_valid": dependency_report.valid,
            "dependency_issues": dependency_report.issues,
            "db_available": pool.is_some(),
            "paused": ctx.is_paused(),
            "maintenance": ctx.is_maintenance(),
        },
        "counts": {
            "users": users_total,
            "roles": roles_total,
            "licenses": licenses_total,
            "orders": orders_total,
        },
        "sections": [
            hook_section(ctx, "servers", scope, caller.identity.user_id),
            hook_section(ctx, "security", scope, caller.identity.user_id),
            hook_section(ctx, "logs", scope, caller.identity.user_id),
        ],
    }))
}

/// User overview: only the caller's own data. Licenses, orders,
/// entitlements, and downloads are read from the database and always
/// filtered by the effective user id; tickets, services, and API keys
/// arrive through the hook bus.
async fn user_overview_data(ctx: &AppContext, caller: &CallerContext) -> UnifiedResult<Value> {
    let pool = db_pool(ctx).ok();
    let user_id = caller.identity.user_id;
    let (licenses_total, active_licenses, orders_total, entitlements_total, downloads_total, recent_licenses, recent_orders) =
        match &pool {
            Some(pool) => {
                let pool = pool.clone();
                run_blocking(pool, move |connection| {
                    use crate::db::schema::licenses::dsl as licenses_dsl;
                    use crate::db::schema::store_downloads::dsl as downloads_dsl;
                    use crate::db::schema::store_entitlements::dsl as entitlements_dsl;
                    use crate::db::schema::store_orders::dsl as orders_dsl;
                    let licenses_total = licenses_dsl::licenses
                        .filter(licenses_dsl::user_id.eq(user_id))
                        .count()
                        .get_result::<i64>(connection)?;
                    let active_licenses = licenses_dsl::licenses
                        .filter(licenses_dsl::user_id.eq(user_id))
                        .filter(licenses_dsl::status.eq("active"))
                        .count()
                        .get_result::<i64>(connection)?;
                    let orders_total = orders_dsl::store_orders
                        .filter(orders_dsl::user_id.eq(user_id.to_string()))
                        .count()
                        .get_result::<i64>(connection)?;
                    let entitlements_total = entitlements_dsl::store_entitlements
                        .filter(entitlements_dsl::user_id.eq(user_id.to_string()))
                        .count()
                        .get_result::<i64>(connection)?;
                    let license_ids: Vec<Uuid> = licenses_dsl::licenses
                        .filter(licenses_dsl::user_id.eq(user_id))
                        .select(licenses_dsl::id)
                        .load::<Uuid>(connection)?;
                    let downloads_total = if license_ids.is_empty() {
                        0
                    } else {
                        downloads_dsl::store_downloads
                            .filter(downloads_dsl::license_id.eq_any(&license_ids))
                            .count()
                            .get_result::<i64>(connection)?
                    };
                    let recent_licenses = licenses_dsl::licenses
                        .filter(licenses_dsl::user_id.eq(user_id))
                        .order(licenses_dsl::created_at.desc())
                        .limit(5)
                        .select(License::as_select())
                        .load::<License>(connection)?;
                    let recent_orders = orders_dsl::store_orders
                        .filter(orders_dsl::user_id.eq(user_id.to_string()))
                        .order(orders_dsl::created_at.desc())
                        .limit(5)
                        .select(OrderRow::as_select())
                        .load::<OrderRow>(connection)?;
                    Ok((
                        licenses_total,
                        active_licenses,
                        orders_total,
                        entitlements_total,
                        downloads_total,
                        recent_licenses,
                        recent_orders,
                    ))
                })
                .await?
            }
            None => (0, 0, 0, 0, 0, Vec::new(), Vec::new()),
        };
    let scope = "user";
    Ok(json!({
        "db_available": pool.is_some(),
        "counts": {
            "licenses": licenses_total,
            "active_licenses": active_licenses,
            "orders": orders_total,
            "entitlements": entitlements_total,
            "downloads": downloads_total,
        },
        "recent_licenses": recent_licenses,
        "recent_orders": recent_orders,
        "sections": [
            hook_section(ctx, "tickets", scope, caller.identity.user_id),
            hook_section(ctx, "services", scope, caller.identity.user_id),
            hook_section(ctx, "apikeys", scope, caller.identity.user_id),
        ],
    }))
}

/// Reseller overview: the reseller's linked clients and those
/// clients' orders (scoped to the link store, never to all users).
/// Commissions and payouts are owned by the billing side and arrive
/// through the hook bus.
async fn reseller_overview_data(
    ctx: &AppContext,
    state: &DashboardsState,
    caller: &CallerContext,
) -> UnifiedResult<Value> {
    let reseller_id = caller.identity.user_id;
    let client_ids = linked_client_ids(state, reseller_id);
    let pool = db_pool(ctx).ok();
    let (client_orders_total, client_orders_value) = match &pool {
        Some(pool) if !client_ids.is_empty() => {
            let pool = pool.clone();
            let client_id_strings: Vec<String> =
                client_ids.iter().map(|id| id.to_string()).collect();
            run_blocking(pool, move |connection| {
                use crate::db::schema::store_orders::dsl as orders_dsl;
                let total = orders_dsl::store_orders
                    .filter(orders_dsl::user_id.eq_any(&client_id_strings))
                    .count()
                    .get_result::<i64>(connection)?;
                let value: Option<f64> = orders_dsl::store_orders
                    .filter(orders_dsl::user_id.eq_any(&client_id_strings))
                    .select(diesel::dsl::sum(orders_dsl::total))
                    .first::<Option<f64>>(connection)?;
                Ok((total, value.unwrap_or(0.0)))
            })
            .await?
        }
        _ => (0, 0.0),
    };
    Ok(json!({
        "db_available": pool.is_some(),
        "clients_total": client_ids.len(),
        "client_ids": client_ids.iter().map(|id| id.to_string()).collect::<Vec<_>>(),
        "counts": {
            "client_orders": client_orders_total,
            "client_orders_value": client_orders_value,
        },
        "sections": [
            hook_section(ctx, "commissions", "reseller", caller.identity.user_id),
        ],
    }))
}

/// Client ids linked to one reseller, sorted for stable output.
fn linked_client_ids(state: &DashboardsState, reseller_id: Uuid) -> Vec<Uuid> {
    let mut ids: Vec<Uuid> = state
        .reseller_links
        .iter()
        .filter(|entry| entry.key().0 == reseller_id)
        .map(|entry| entry.key().1)
        .collect();
    ids.sort();
    ids
}

// ---------------------------------------------------------------------------
// API handlers
// ---------------------------------------------------------------------------

async fn get_scope(
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<DashboardsState>>,
) -> HttpResponse {
    let caller = match require_capability(&req, &ctx, &state, CAP_USER_ACCESS).await {
        Ok(caller) => caller,
        Err(response) => return response,
    };
    let (scope, scopes_available) = effective_scope(&caller.capabilities);
    let mut capabilities: Vec<String> = caller.capabilities.iter().cloned().collect();
    capabilities.sort();
    success_response(
        json!({
            "scope": scope,
            "scopes_available": scopes_available,
            "capabilities": capabilities,
            "user_id": caller.identity.user_id.to_string(),
            "real_user_id": caller.identity.real_user_id.to_string(),
            "impersonating": caller.identity.impersonating,
            "impersonated_by": caller.identity.impersonated_by.map(|id| id.to_string()),
            "view_as_expires_at": caller.identity.view_as_expires_at.map(|time| time.to_rfc3339()),
            "admin_path": state.admin_prefix(),
            "user_path": state.user_prefix(),
        }),
        200,
    )
}

#[derive(Debug, Deserialize)]
struct NavQuery {
    #[serde(default)]
    scope: Option<String>,
    #[serde(default)]
    current: Option<String>,
}

async fn get_nav(
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<DashboardsState>>,
    query: web::Query<NavQuery>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let caller = match require_capability(&req, &ctx, &state, CAP_USER_ACCESS).await {
        Ok(caller) => caller,
        Err(response) => return response,
    };
    let requested = query.scope.clone().unwrap_or_else(|| {
        let (scope, _) = effective_scope(&caller.capabilities);
        scope.to_string()
    });
    if !["admin", "user", "reseller"].contains(&requested.as_str()) {
        return error_response(
            &UnifiedError::bad_request(format!(
                "Unknown nav scope \"{requested}\": expected admin, user, or reseller"
            )),
            debug,
        );
    }
    // The requested scope must be one the caller may use; asking for
    // the admin nav without admin access is a 403, not an empty list.
    let required = match requested.as_str() {
        "admin" => CAP_ADMIN_ACCESS,
        "reseller" => CAP_RESELLER_ACCESS,
        _ => CAP_USER_ACCESS,
    };
    if required != CAP_USER_ACCESS && !has_capability(&caller.capabilities, required) {
        return error_response(
            &UnifiedError::forbidden(format!(
                "Capability \"{required}\" is required for the {requested} navigation"
            ))
            .with_metadata("capability", json!(required))
            .with_metadata("scope", json!(requested)),
            debug,
        );
    }
    let items = nav_items(
        &ctx,
        &state,
        &requested,
        &caller.capabilities,
        query.current.as_deref(),
    );
    success_response(json!({ "scope": requested, "items": items }), 200)
}

async fn get_admin_overview(
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<DashboardsState>>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let caller = match require_capability(&req, &ctx, &state, CAP_ADMIN_ACCESS).await {
        Ok(caller) => caller,
        Err(response) => return response,
    };
    match admin_overview_data(&ctx, &caller).await {
        Ok(data) => success_response(data, 200),
        Err(error) => error_response(&error, debug),
    }
}

async fn get_admin_modules(
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<DashboardsState>>,
) -> HttpResponse {
    let caller = match require_capability(&req, &ctx, &state, CAP_MODULES_MANAGE).await {
        Ok(caller) => caller,
        Err(response) => return response,
    };
    let _ = caller;
    let report = ctx.modules.validate_dependencies();
    success_response(
        json!({
            "modules": ctx.modules.summary(),
            "dependencies_valid": report.valid,
            "dependency_issues": report.issues,
        }),
        200,
    )
}

async fn get_admin_users(
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<DashboardsState>>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let caller = match require_capability(&req, &ctx, &state, CAP_USERS_MANAGE).await {
        Ok(caller) => caller,
        Err(response) => return response,
    };
    let _ = caller;
    let params = query.into_inner();
    let pagination = params.normalized();
    let sort = params.allowed_sort(&["name", "email", "created_at"], "created_at");
    let descending = pagination.order == "desc";
    let pool = match db_pool(&ctx) {
        Ok(pool) => pool,
        Err(error) => return error_response(&error, debug),
    };
    let search = pagination.search.clone().map(|value| value.to_lowercase());
    let company_filter = pagination
        .filters
        .get("company_id")
        .and_then(|value| Uuid::parse_str(value).ok());
    let result = run_blocking(pool, move |connection| {
        use crate::db::schema::user_roles::dsl as user_roles_dsl;
        use crate::db::schema::users::dsl as users_dsl;
        let mut users: Vec<User> = users_dsl::users
            .select(User::as_select())
            .load::<User>(connection)
            .map_err(UnifiedError::from)?;
        users.retain(|user| {
            if let Some(needle) = &search {
                let haystack = format!("{} {}", user.name, user.email).to_lowercase();
                if !haystack.contains(needle.as_str()) {
                    return false;
                }
            }
            if let Some(company_id) = company_filter {
                if user.company_id != Some(company_id) {
                    return false;
                }
            }
            true
        });
        users.sort_by(|a, b| match sort.as_str() {
            "name" => a.name.cmp(&b.name),
            "email" => a.email.cmp(&b.email),
            _ => a.created_at.cmp(&b.created_at),
        });
        if descending {
            users.reverse();
        }
        let total = users.len() as u64;
        let start = ((pagination.page - 1) * pagination.per_page) as usize;
        let end = (start + pagination.per_page as usize).min(users.len());
        let page_users: Vec<User> = if start < users.len() {
            users[start..end].to_vec()
        } else {
            Vec::new()
        };
        let ids: Vec<Uuid> = page_users.iter().map(|user| user.id).collect();
        let assignments: Vec<(Uuid, Uuid)> = if ids.is_empty() {
            Vec::new()
        } else {
            user_roles_dsl::user_roles
                .filter(user_roles_dsl::user_id.eq_any(&ids))
                .select((user_roles_dsl::user_id, user_roles_dsl::role_id))
                .load::<(Uuid, Uuid)>(connection)
                .map_err(UnifiedError::from)?
        };
        let mut by_user: HashMap<Uuid, Vec<Uuid>> = HashMap::new();
        for (user_id, role_id) in assignments {
            by_user.entry(user_id).or_default().push(role_id);
        }
        let items: Vec<Value> = page_users
            .iter()
            .map(|user| {
                let mut view = serde_json::to_value(UserResponseView::from(user)).unwrap_or(json!({}));
                if let Some(object) = view.as_object_mut() {
                    object.insert(
                        "roles".to_string(),
                        json!(by_user.get(&user.id).cloned().unwrap_or_default()),
                    );
                }
                view
            })
            .collect();
        Ok((items, total))
    })
    .await;
    match result {
        Ok((items, total)) => success_response(PageResult::new(items, total, &pagination).to_json(), 200),
        Err(error) => error_response(&error, debug),
    }
}

/// Public user view: the `User` model already skips the password hash
/// and TOTP secret on serialization, but the shape is restated here
/// so the admin list never leaks a field added to the model later.
#[derive(Debug, Clone, Serialize)]
struct UserResponseView {
    id: Uuid,
    email: String,
    name: String,
    is_active: bool,
    company_id: Option<Uuid>,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

impl From<&User> for UserResponseView {
    fn from(user: &User) -> Self {
        Self {
            id: user.id,
            email: user.email.clone(),
            name: user.name.clone(),
            is_active: user.is_active,
            company_id: user.company_id,
            created_at: user.created_at,
            updated_at: user.updated_at,
        }
    }
}

async fn get_user_overview(
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<DashboardsState>>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let caller = match require_capability(&req, &ctx, &state, CAP_USER_ACCESS).await {
        Ok(caller) => caller,
        Err(response) => return response,
    };
    match user_overview_data(&ctx, &caller).await {
        Ok(data) => success_response(data, 200),
        Err(error) => error_response(&error, debug),
    }
}

async fn get_user_licenses(
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<DashboardsState>>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let caller = match require_capability(&req, &ctx, &state, CAP_LICENSES_READ).await {
        Ok(caller) => caller,
        Err(response) => return response,
    };
    let params = query.into_inner();
    let pagination = params.normalized();
    let sort = params.allowed_sort(&["product", "status", "created_at"], "created_at");
    let descending = pagination.order == "desc";
    let status_filter = pagination.filters.get("status").cloned();
    let pool = match db_pool(&ctx) {
        Ok(pool) => pool,
        Err(error) => return error_response(&error, debug),
    };
    let user_id = caller.identity.user_id;
    let result = run_blocking(pool, move |connection| {
        use crate::db::schema::licenses::dsl as licenses_dsl;
        let mut count_query = licenses_dsl::licenses
            .filter(licenses_dsl::user_id.eq(user_id))
            .into_boxed();
        if let Some(status) = &status_filter {
            count_query = count_query.filter(licenses_dsl::status.eq(status.clone()));
        }
        let total = count_query
            .count()
            .get_result::<i64>(connection)
            .map_err(UnifiedError::from)?;
        let mut query = licenses_dsl::licenses
            .filter(licenses_dsl::user_id.eq(user_id))
            .into_boxed();
        if let Some(status) = &status_filter {
            query = query.filter(licenses_dsl::status.eq(status.clone()));
        }
        let items = match (sort.as_str(), descending) {
            ("product", false) => query
                .order(licenses_dsl::product.asc())
                .limit(pagination.per_page as i64)
                .offset(((pagination.page - 1) * pagination.per_page) as i64)
                .select(License::as_select())
                .load::<License>(connection),
            ("product", true) => query
                .order(licenses_dsl::product.desc())
                .limit(pagination.per_page as i64)
                .offset(((pagination.page - 1) * pagination.per_page) as i64)
                .select(License::as_select())
                .load::<License>(connection),
            ("status", false) => query
                .order(licenses_dsl::status.asc())
                .limit(pagination.per_page as i64)
                .offset(((pagination.page - 1) * pagination.per_page) as i64)
                .select(License::as_select())
                .load::<License>(connection),
            ("status", true) => query
                .order(licenses_dsl::status.desc())
                .limit(pagination.per_page as i64)
                .offset(((pagination.page - 1) * pagination.per_page) as i64)
                .select(License::as_select())
                .load::<License>(connection),
            (_, true) => query
                .order(licenses_dsl::created_at.asc())
                .limit(pagination.per_page as i64)
                .offset(((pagination.page - 1) * pagination.per_page) as i64)
                .select(License::as_select())
                .load::<License>(connection),
            (_, false) => query
                .order(licenses_dsl::created_at.desc())
                .limit(pagination.per_page as i64)
                .offset(((pagination.page - 1) * pagination.per_page) as i64)
                .select(License::as_select())
                .load::<License>(connection),
        }
        .map_err(UnifiedError::from)?;
        Ok((items, total as u64))
    })
    .await;
    match result {
        Ok((items, total)) => {
            success_response(PageResult::new(items, total, &pagination).to_json(), 200)
        }
        Err(error) => error_response(&error, debug),
    }
}

async fn get_user_orders(
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<DashboardsState>>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let caller = match require_capability(&req, &ctx, &state, CAP_ORDERS_READ).await {
        Ok(caller) => caller,
        Err(response) => return response,
    };
    let params = query.into_inner();
    let pagination = params.normalized();
    let sort = params.allowed_sort(&["total", "status", "created_at"], "created_at");
    let descending = pagination.order == "desc";
    let status_filter = pagination.filters.get("status").cloned();
    let pool = match db_pool(&ctx) {
        Ok(pool) => pool,
        Err(error) => return error_response(&error, debug),
    };
    let user_id = caller.identity.user_id.to_string();
    let result = run_blocking(pool, move |connection| {
        use crate::db::schema::store_orders::dsl as orders_dsl;
        let mut count_query = orders_dsl::store_orders
            .filter(orders_dsl::user_id.eq(&user_id))
            .into_boxed();
        if let Some(status) = &status_filter {
            count_query = count_query.filter(orders_dsl::status.eq(status.clone()));
        }
        let total = count_query
            .count()
            .get_result::<i64>(connection)
            .map_err(UnifiedError::from)?;
        let mut query = orders_dsl::store_orders
            .filter(orders_dsl::user_id.eq(&user_id))
            .into_boxed();
        if let Some(status) = &status_filter {
            query = query.filter(orders_dsl::status.eq(status.clone()));
        }
        let items = match (sort.as_str(), descending) {
            ("total", false) => query
                .order(orders_dsl::total.asc())
                .limit(pagination.per_page as i64)
                .offset(((pagination.page - 1) * pagination.per_page) as i64)
                .select(OrderRow::as_select())
                .load::<OrderRow>(connection),
            ("total", true) => query
                .order(orders_dsl::total.desc())
                .limit(pagination.per_page as i64)
                .offset(((pagination.page - 1) * pagination.per_page) as i64)
                .select(OrderRow::as_select())
                .load::<OrderRow>(connection),
            ("status", false) => query
                .order(orders_dsl::status.asc())
                .limit(pagination.per_page as i64)
                .offset(((pagination.page - 1) * pagination.per_page) as i64)
                .select(OrderRow::as_select())
                .load::<OrderRow>(connection),
            ("status", true) => query
                .order(orders_dsl::status.desc())
                .limit(pagination.per_page as i64)
                .offset(((pagination.page - 1) * pagination.per_page) as i64)
                .select(OrderRow::as_select())
                .load::<OrderRow>(connection),
            (_, true) => query
                .order(orders_dsl::created_at.asc())
                .limit(pagination.per_page as i64)
                .offset(((pagination.page - 1) * pagination.per_page) as i64)
                .select(OrderRow::as_select())
                .load::<OrderRow>(connection),
            (_, false) => query
                .order(orders_dsl::created_at.desc())
                .limit(pagination.per_page as i64)
                .offset(((pagination.page - 1) * pagination.per_page) as i64)
                .select(OrderRow::as_select())
                .load::<OrderRow>(connection),
        }
        .map_err(UnifiedError::from)?;
        Ok((items, total as u64))
    })
    .await;
    match result {
        Ok((items, total)) => {
            success_response(PageResult::new(items, total, &pagination).to_json(), 200)
        }
        Err(error) => error_response(&error, debug),
    }
}

async fn get_user_downloads(
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<DashboardsState>>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let caller = match require_capability(&req, &ctx, &state, CAP_LICENSES_READ).await {
        Ok(caller) => caller,
        Err(response) => return response,
    };
    let params = query.into_inner();
    let pagination = params.normalized();
    let pool = match db_pool(&ctx) {
        Ok(pool) => pool,
        Err(error) => return error_response(&error, debug),
    };
    let user_id = caller.identity.user_id;
    let result = run_blocking(pool, move |connection| {
        use crate::db::schema::licenses::dsl as licenses_dsl;
        use crate::db::schema::store_downloads::dsl as downloads_dsl;
        let license_ids: Vec<Uuid> = licenses_dsl::licenses
            .filter(licenses_dsl::user_id.eq(user_id))
            .select(licenses_dsl::id)
            .load::<Uuid>(connection)
            .map_err(UnifiedError::from)?;
        if license_ids.is_empty() {
            return Ok((Vec::new(), 0u64));
        }
        let total = downloads_dsl::store_downloads
            .filter(downloads_dsl::license_id.eq_any(&license_ids))
            .count()
            .get_result::<i64>(connection)
            .map_err(UnifiedError::from)?;
        let items = downloads_dsl::store_downloads
            .filter(downloads_dsl::license_id.eq_any(&license_ids))
            .order(downloads_dsl::downloaded_at.desc())
            .limit(pagination.per_page as i64)
            .offset(((pagination.page - 1) * pagination.per_page) as i64)
            .select(DownloadRow::as_select())
            .load::<DownloadRow>(connection)
            .map_err(UnifiedError::from)?;
        Ok((items, total as u64))
    })
    .await;
    match result {
        Ok((items, total)) => {
            success_response(PageResult::new(items, total, &pagination).to_json(), 200)
        }
        Err(error) => error_response(&error, debug),
    }
}

// ---------------------------------------------------------------------------
// Reseller handlers
// ---------------------------------------------------------------------------

async fn get_reseller_overview(
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<DashboardsState>>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let caller = match require_capability(&req, &ctx, &state, CAP_RESELLER_ACCESS).await {
        Ok(caller) => caller,
        Err(response) => return response,
    };
    match reseller_overview_data(&ctx, &state, &caller).await {
        Ok(data) => success_response(data, 200),
        Err(error) => error_response(&error, debug),
    }
}

async fn get_reseller_clients(
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<DashboardsState>>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let caller = match require_capability(&req, &ctx, &state, CAP_RESELLER_CLIENTS_VIEW).await {
        Ok(caller) => caller,
        Err(response) => return response,
    };
    let params = query.into_inner();
    let links: Vec<ResellerLink> = linked_client_ids(&state, caller.identity.user_id)
        .into_iter()
        .filter_map(|client_id| {
            state
                .reseller_links
                .get(&(caller.identity.user_id, client_id))
                .map(|entry| entry.value().clone())
        })
        .collect();
    // Attach the client profile from the database when available; a
    // link without a database row still lists, with `profile: null`.
    let pool = ctx.db.read().expect("db lock poisoned").clone();
    let profiles: HashMap<Uuid, UserResponseView> = match &pool {
        Some(pool) if !links.is_empty() => {
            let ids: Vec<Uuid> = links.iter().map(|link| link.client_id).collect();
            let pool = pool.clone();
            run_blocking(pool, move |connection| {
                use crate::db::schema::users::dsl as users_dsl;
                let users = users_dsl::users
                    .filter(users_dsl::id.eq_any(&ids))
                    .select(User::as_select())
                    .load::<User>(connection)
                    .map_err(UnifiedError::from)?;
                let mut map = HashMap::new();
                for user in users {
                    map.insert(user.id, UserResponseView::from(&user));
                }
                Ok(map)
            })
            .await
            .unwrap_or_default()
        }
        _ => HashMap::new(),
    };
    let items: Vec<Value> = links
        .iter()
        .map(|link| {
            json!({
                "reseller_id": link.reseller_id.to_string(),
                "client_id": link.client_id.to_string(),
                "verified": link.verified,
                "created_at": link.created_at.to_rfc3339(),
                "profile": profiles.get(&link.client_id),
            })
        })
        .collect();
    success_response(paginate_vec(items, &params).to_json(), 200)
}

#[derive(Debug, Deserialize)]
struct LinkClientInput {
    client_id: String,
}

async fn link_reseller_client(
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<DashboardsState>>,
    body: web::Json<LinkClientInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let caller = match require_capability(&req, &ctx, &state, CAP_RESELLER_CLIENTS_MANAGE).await {
        Ok(caller) => caller,
        Err(response) => return response,
    };
    let client_id = match Uuid::parse_str(body.client_id.trim()) {
        Ok(id) => id,
        Err(_) => {
            return error_response(
                &UnifiedError::bad_request("client_id must be a valid UUID"),
                debug,
            )
        }
    };
    if client_id == caller.identity.user_id {
        return error_response(
            &UnifiedError::bad_request("A reseller cannot link themselves as a client"),
            debug,
        );
    }
    if state
        .reseller_links
        .contains_key(&(caller.identity.user_id, client_id))
    {
        return error_response(
            &UnifiedError::conflict(format!(
                "Client \"{client_id}\" is already linked to this reseller"
            )),
            debug,
        );
    }
    // Verify the client exists when a database is available; without
    // one the link is recorded as unverified instead of refused.
    let verified = match ctx.db.read().expect("db lock poisoned").clone() {
        Some(pool) => {
            let found = run_blocking(pool, move |connection| {
                use crate::db::schema::users::dsl as users_dsl;
                let found = users_dsl::users
                    .find(client_id)
                    .select(User::as_select())
                    .first::<User>(connection)
                    .optional()
                    .map_err(UnifiedError::from)?;
                Ok(found.is_some())
            })
            .await;
            match found {
                Ok(true) => true,
                Ok(false) => {
                    return error_response(
                        &UnifiedError::not_found(format!(
                            "Client user \"{client_id}\" does not exist"
                        )),
                        debug,
                    )
                }
                Err(error) => return error_response(&error, debug),
            }
        }
        None => false,
    };
    let link = ResellerLink {
        reseller_id: caller.identity.user_id,
        client_id,
        verified,
        created_at: Utc::now(),
    };
    state
        .reseller_links
        .insert((caller.identity.user_id, client_id), link.clone());
    ctx.hooks.emit(
        EVENT_RESELLER_CLIENT_LINKED,
        json!({
            "actor_id": caller.identity.user_id.to_string(),
            "reseller_id": caller.identity.user_id.to_string(),
            "client_id": client_id.to_string(),
            "verified": verified,
            "resource": format!("reseller:{}:client:{client_id}", caller.identity.user_id),
        }),
    );
    success_response(link, 201)
}

async fn unlink_reseller_client(
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<DashboardsState>>,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let caller = match require_capability(&req, &ctx, &state, CAP_RESELLER_CLIENTS_MANAGE).await {
        Ok(caller) => caller,
        Err(response) => return response,
    };
    let client_id = match Uuid::parse_str(path.into_inner().trim()) {
        Ok(id) => id,
        Err(_) => {
            return error_response(
                &UnifiedError::bad_request("client_id must be a valid UUID"),
                debug,
            )
        }
    };
    match state
        .reseller_links
        .remove(&(caller.identity.user_id, client_id))
    {
        Some((_, link)) => {
            ctx.hooks.emit(
                EVENT_RESELLER_CLIENT_UNLINKED,
                json!({
                    "actor_id": caller.identity.user_id.to_string(),
                    "reseller_id": caller.identity.user_id.to_string(),
                    "client_id": client_id.to_string(),
                    "resource": format!("reseller:{}:client:{client_id}", caller.identity.user_id),
                }),
            );
            let _ = link;
            success_response(json!({ "unlinked": true, "client_id": client_id.to_string() }), 200)
        }
        None => error_response(
            &UnifiedError::not_found(format!(
                "Client \"{client_id}\" is not linked to this reseller"
            )),
            debug,
        ),
    }
}

// ---------------------------------------------------------------------------
// Admin view-as handlers
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct StartViewAsInput {
    target_user_id: String,
    reason: String,
}

async fn start_view_as(
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<DashboardsState>>,
    body: web::Json<StartViewAsInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let caller = match require_capability(&req, &ctx, &state, CAP_VIEW_AS_USER).await {
        Ok(caller) => caller,
        Err(response) => return response,
    };
    let input = body.into_inner();
    let reason = input.reason.trim().to_string();
    if reason.is_empty() || reason.chars().count() > 500 {
        return error_response(
            &UnifiedError::bad_request("reason is required and must be at most 500 characters"),
            debug,
        );
    }
    let target_id = match Uuid::parse_str(input.target_user_id.trim()) {
        Ok(id) => id,
        Err(_) => {
            return error_response(
                &UnifiedError::bad_request("target_user_id must be a valid UUID"),
                debug,
            )
        }
    };
    if target_id == caller.identity.user_id {
        return error_response(
            &UnifiedError::bad_request("An admin cannot view as themselves"),
            debug,
        );
    }
    if state.view_as.contains_key(&caller.identity.user_id) {
        return error_response(
            &UnifiedError::conflict(
                "A view-as session is already active for this admin; stop it first",
            ),
            debug,
        );
    }
    // The target must exist and be active when a database is
    // available; without one the session is still created, and the
    // target resolves (or fails closed) on the next request.
    if let Some(pool) = ctx.db.read().expect("db lock poisoned").clone() {
        let found = run_blocking(pool, move |connection| {
            use crate::db::schema::users::dsl as users_dsl;
            let found = users_dsl::users
                .find(target_id)
                .select(User::as_select())
                .first::<User>(connection)
                .optional()
                .map_err(UnifiedError::from)?;
            Ok(found)
        })
        .await;
        match found {
            Ok(Some(user)) if user.is_active => {}
            Ok(Some(_)) => {
                return error_response(
                    &UnifiedError::forbidden(format!(
                        "Target user \"{target_id}\" is disabled"
                    )),
                    debug,
                )
            }
            Ok(None) => {
                return error_response(
                    &UnifiedError::not_found(format!(
                        "Target user \"{target_id}\" does not exist"
                    )),
                    debug,
                )
            }
            Err(error) => return error_response(&error, debug),
        }
    }
    let now = Utc::now();
    let session = ViewAsSession {
        admin_id: caller.identity.user_id,
        target_user_id: target_id,
        reason: reason.clone(),
        started_at: now,
        expires_at: now + chrono::Duration::minutes(state.view_as_ttl_minutes()),
    };
    state.view_as.insert(caller.identity.user_id, session.clone());
    ctx.hooks.emit(
        EVENT_VIEW_AS_STARTED,
        json!({
            "actor_id": caller.identity.user_id.to_string(),
            "admin_id": caller.identity.user_id.to_string(),
            "target_user_id": target_id.to_string(),
            "reason": reason,
            "resource": format!("user:{target_id}"),
            "expires_at": session.expires_at.to_rfc3339(),
        }),
    );
    success_response(session, 201)
}

async fn get_view_as(
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<DashboardsState>>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let caller = match require_capability(&req, &ctx, &state, CAP_VIEW_AS_USER).await {
        Ok(caller) => caller,
        Err(response) => return response,
    };
    match state.view_as.get(&caller.identity.user_id) {
        Some(entry) => {
            let session = entry.value().clone();
            if session.expires_at <= Utc::now() {
                drop(entry);
                state.view_as.remove(&caller.identity.user_id);
                ctx.hooks.emit(
                    EVENT_VIEW_AS_EXPIRED,
                    json!({
                        "actor_id": caller.identity.user_id.to_string(),
                        "target_user_id": session.target_user_id.to_string(),
                        "reason": session.reason,
                        "resource": format!("user:{}", session.target_user_id),
                        "expired_at": session.expires_at.to_rfc3339(),
                    }),
                );
                error_response(
                    &UnifiedError::not_found("No active view-as session"),
                    debug,
                )
            } else {
                success_response(session, 200)
            }
        }
        None => error_response(
            &UnifiedError::not_found("No active view-as session"),
            debug,
        ),
    }
}

async fn stop_view_as(
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<DashboardsState>>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    // Checked against the REAL admin identity (see
    // `require_real_capability`): the impersonated target must never
    // be able to end the admin's session.
    let caller = match require_real_capability(&req, &ctx, &state, CAP_VIEW_AS_USER).await {
        Ok(caller) => caller,
        Err(response) => return response,
    };
    match state.view_as.remove(&caller.identity.real_user_id) {
        Some((_, session)) => {
            ctx.hooks.emit(
                EVENT_VIEW_AS_ENDED,
                json!({
                    "actor_id": caller.identity.real_user_id.to_string(),
                    "admin_id": caller.identity.real_user_id.to_string(),
                    "target_user_id": session.target_user_id.to_string(),
                    "reason": session.reason,
                    "resource": format!("user:{}", session.target_user_id),
                }),
            );
            success_response(
                json!({
                    "stopped": true,
                    "admin_id": caller.identity.real_user_id.to_string(),
                    "target_user_id": session.target_user_id.to_string(),
                }),
                200,
            )
        }
        None => error_response(
            &UnifiedError::not_found("No active view-as session"),
            debug,
        ),
    }
}

// ---------------------------------------------------------------------------
// Admin dashboard settings (runtime settings owned by this module)
// ---------------------------------------------------------------------------

async fn get_dashboard_settings(
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<DashboardsState>>,
) -> HttpResponse {
    let caller = match require_capability(&req, &ctx, &state, CAP_SETTINGS_MANAGE).await {
        Ok(caller) => caller,
        Err(response) => return response,
    };
    let _ = caller;
    success_response(dashboard_settings_json(&ctx, &state), 200)
}

fn dashboard_settings_json(ctx: &AppContext, state: &DashboardsState) -> Value {
    let session_name = {
        let config = ctx.config.read().expect("config lock poisoned");
        config.app.session_name.clone()
    };
    json!({
        "view_as_ttl_minutes": state.view_as_ttl_minutes(),
        "view_as_ttl_range": { "min": MIN_VIEW_AS_TTL_MINUTES, "max": MAX_VIEW_AS_TTL_MINUTES },
        "admin_path": state.admin_prefix(),
        "user_path": state.user_prefix(),
        "user_path_config_key": "app.dashboard_path",
        "shared_route_registry": state.route_registry.read().expect("route registry lock poisoned").is_some(),
        "auth": {
            "mechanism": "auth-manager-jwt",
            "token_header": "Authorization: Bearer <token>",
            "session_cookie": session_name,
        },
    })
}

#[derive(Debug, Deserialize)]
struct UpdateDashboardSettingsInput {
    #[serde(default)]
    view_as_ttl_minutes: Option<i64>,
}

async fn update_dashboard_settings(
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<DashboardsState>>,
    body: web::Json<UpdateDashboardSettingsInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let caller = match require_capability(&req, &ctx, &state, CAP_SETTINGS_MANAGE).await {
        Ok(caller) => caller,
        Err(response) => return response,
    };
    let _ = caller;
    if let Some(ttl) = body.view_as_ttl_minutes {
        if !(MIN_VIEW_AS_TTL_MINUTES..=MAX_VIEW_AS_TTL_MINUTES).contains(&ttl) {
            return error_response(
                &UnifiedError::bad_request(format!(
                    "view_as_ttl_minutes must be between {MIN_VIEW_AS_TTL_MINUTES} and {MAX_VIEW_AS_TTL_MINUTES}"
                )),
                debug,
            );
        }
        *state.view_as_ttl_minutes.write().expect("view-as ttl lock poisoned") = ttl;
    }
    success_response(dashboard_settings_json(&ctx, &state), 200)
}

// ---------------------------------------------------------------------------
// Page handlers (HTML; auth failures stay in the standard JSON envelope)
// ---------------------------------------------------------------------------

async fn admin_page(
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<DashboardsState>>,
    path: web::Path<String>,
) -> HttpResponse {
    let tail = path.into_inner();
    admin_page_inner(req, ctx, state, &tail).await
}

async fn admin_page_inner(
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<DashboardsState>>,
    tail: &str,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let first = tail.split('/').next().unwrap_or("");
    let section = if first.is_empty() { "overview" } else { first };
    let allowed = ["overview", "modules", "users", "orders", "servers", "security", "logs", "settings"];
    if !allowed.contains(&section) {
        return error_response(
            &UnifiedError::not_found(format!("Admin section \"{section}\" does not exist")),
            debug,
        );
    }
    let capability = match section {
        "modules" => CAP_MODULES_MANAGE,
        "users" => CAP_USERS_MANAGE,
        "orders" => CAP_ORDERS_MANAGE,
        "servers" => CAP_SERVERS_MANAGE,
        "security" => CAP_SECURITY_VIEW,
        "logs" => CAP_LOGS_VIEW,
        "settings" => CAP_SETTINGS_MANAGE,
        _ => CAP_ADMIN_ACCESS,
    };
    let caller = match require_capability(&req, &ctx, &state, capability).await {
        Ok(caller) => caller,
        Err(response) => return response,
    };
    let current_path = format!("/{}/{}", state.admin_prefix(), tail.trim_matches('/'));
    let current_path = current_path.trim_end_matches('/').to_string();
    let current_path = if current_path.is_empty() {
        format!("/{}", state.admin_prefix())
    } else {
        current_path
    };
    let mut data = page_data(&ctx, &state, &caller, "admin", &format!("Admin — {section}"), &current_path);
    let overview = match admin_overview_data(&ctx, &caller).await {
        Ok(data) => data,
        Err(error) => return error_response(&error, debug),
    };
    if let Some(object) = data.as_object_mut() {
        object.insert("section".to_string(), Value::String(section.to_string()));
        object.insert("overview".to_string(), overview);
    }
    render_page_response(&req, &ctx, &state, "admin-dashboard", "admin-layout", data)
}

async fn admin_page_root(
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<DashboardsState>>,
) -> HttpResponse {
    admin_page_inner(req, ctx, state, "").await
}

async fn user_page(
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<DashboardsState>>,
    path: web::Path<String>,
) -> HttpResponse {
    let raw = path.into_inner();
    user_page_inner(req, ctx, state, &raw).await
}

async fn user_page_inner(
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<DashboardsState>>,
    raw: &str,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let segments: Vec<&str> = raw.split('/').filter(|part| !part.is_empty()).collect();
    let (section, subsection) = match segments.as_slice() {
        [] => ("overview", None),
        [first] => (*first, None),
        ["reseller", second] => ("reseller", Some(*second)),
        [first, ..] => (*first, None),
    };
    let allowed = ["overview", "licenses", "downloads", "orders", "tickets", "services", "apikeys", "profile", "reseller"];
    if !allowed.contains(&section) {
        return error_response(
            &UnifiedError::not_found(format!("Dashboard section \"{section}\" does not exist")),
            debug,
        );
    }
    if section == "reseller" && !matches!(subsection, Some("clients") | Some("payouts")) {
        return error_response(
            &UnifiedError::not_found("Reseller section must be clients or payouts".to_string()),
            debug,
        );
    }
    let capability = match (section, subsection) {
        ("licenses", _) | ("downloads", _) => CAP_LICENSES_READ,
        ("orders", _) => CAP_ORDERS_READ,
        ("tickets", _) => CAP_TICKETS_READ,
        ("services", _) => CAP_SERVICES_READ,
        ("apikeys", _) => CAP_APIKEYS_READ,
        ("reseller", Some("clients")) => CAP_RESELLER_CLIENTS_VIEW,
        ("reseller", Some("payouts")) => CAP_RESELLER_PAYOUTS_VIEW,
        ("reseller", _) => CAP_RESELLER_ACCESS,
        _ => CAP_USER_ACCESS,
    };
    let caller = match require_capability(&req, &ctx, &state, capability).await {
        Ok(caller) => caller,
        Err(response) => return response,
    };
    let current_path = format!("/{}/{}", state.user_prefix(), raw.trim_matches('/'));
    let current_path = current_path.trim_end_matches('/').to_string();
    let current_path = if current_path.is_empty() {
        format!("/{}", state.user_prefix())
    } else {
        current_path
    };
    let scope = if section == "reseller" { "reseller" } else { "user" };
    let mut data = page_data(&ctx, &state, &caller, scope, &format!("Dashboard — {section}"), &current_path);
    let overview = match user_overview_data(&ctx, &caller).await {
        Ok(data) => data,
        Err(error) => return error_response(&error, debug),
    };
    let mut extra = json!({});
    if section == "reseller" {
        match reseller_overview_data(&ctx, &state, &caller).await {
            Ok(data) => extra = data,
            Err(error) => return error_response(&error, debug),
        }
    }
    if let Some(object) = data.as_object_mut() {
        object.insert("section".to_string(), Value::String(section.to_string()));
        object.insert("subsection".to_string(), subsection.map(Value::String).unwrap_or(Value::Null));
        object.insert("overview".to_string(), overview);
        object.insert("reseller".to_string(), extra);
    }
    render_page_response(&req, &ctx, &state, "user-dashboard", "user-layout", data)
}

async fn user_page_root(
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<DashboardsState>>,
) -> HttpResponse {
    user_page_inner(req, ctx, state, "").await
}

// ---------------------------------------------------------------------------
// Layer 1 — middleware (authentication + view-as expiry on dashboard paths)
// ---------------------------------------------------------------------------

/// Authenticates every request under a dashboard prefix and stores
/// the caller in the request extensions. Unauthenticated requests get
/// 401 in the standard error envelope before any handler runs.
/// Non-dashboard paths pass through untouched. Capability checks
/// stay in the handlers (layer 2); this middleware deliberately does
/// not decide which sections a caller may see.
pub struct DashboardsAuthMiddleware {
    state: Arc<DashboardsState>,
}

impl std::fmt::Debug for DashboardsAuthMiddleware {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "DashboardsAuthMiddleware")
    }
}

impl Clone for DashboardsAuthMiddleware {
    fn clone(&self) -> Self {
        Self { state: self.state.clone() }
    }
}

impl DashboardsAuthMiddleware {
    fn new(state: Arc<DashboardsState>) -> Self {
        Self { state }
    }
}

impl Middleware for DashboardsAuthMiddleware {
    /// After the security-hardening middleware (priorities 700-1000),
    /// before default-priority module middleware.
    fn priority(&self) -> isize {
        600
    }

    fn handle(&self, req: ServiceRequest, next: NextFn) -> MiddlewareResult {
        let state = self.state.clone();
        let path = req.path().to_string();
        if !state.is_dashboard_path(&path) {
            return Box::pin(async move { next(req).await });
        }
        // The middleware only has the state, not the AppContext, so it
        // rebuilds the minimal authentication inputs from the stashed
        // config and hook handles filled in by `init`. When `init`
        // has not run, fail closed: dashboard paths are not served
        // without authentication.
        let Some(config) = state.config.get() else {
            let error = UnifiedError::unauthorized(
                "Dashboard authentication is not configured",
            );
            let response = HttpResponse::Unauthorized().json(error.to_json(false));
            return Box::pin(async move { Ok(req.into_response(response)) });
        };
        let (secret_key, salt_key, session_name) = {
            let config = config.read().expect("config lock poisoned");
            (
                config.app.secret_key.clone(),
                config.app.salt_key.clone(),
                config.app.session_name.clone(),
            )
        };
        let token = extract_token(&req, &session_name);
        let identity = match token {
            Some(token) => match verify_jwt(&token, &secret_key, &salt_key) {
                Ok(claims) => match Uuid::parse_str(claims.sub.trim()) {
                    Ok(user_id) => {
                        let mut identity =
                            CallerIdentity::direct(user_id, claims.scopes);
                        // Apply (and expire) a view-as session. The
                        // audit event for expiry goes through the
                        // stashed hook bus when available.
                        if let Some(session) = state.view_as.get(&user_id) {
                            let session = session.value().clone();
                            if session.expires_at <= Utc::now() {
                                state.view_as.remove(&user_id);
                                state.emit_audit(
                                    EVENT_VIEW_AS_EXPIRED,
                                    json!({
                                        "actor_id": user_id.to_string(),
                                        "target_user_id": session.target_user_id.to_string(),
                                        "reason": session.reason,
                                        "resource": format!("user:{}", session.target_user_id),
                                        "expired_at": session.expires_at.to_rfc3339(),
                                    }),
                                );
                            } else {
                                identity.user_id = session.target_user_id;
                                identity.impersonating = true;
                                identity.impersonated_by = Some(user_id);
                                identity.view_as_expires_at = Some(session.expires_at);
                            }
                        }
                        Some(identity)
                    }
                    None => None,
                },
                Err(_) => None,
            },
            None => None,
        };
        let Some(identity) = identity else {
            let error = UnifiedError::unauthorized(
                "Authentication required: provide a Bearer token",
            );
            let response = HttpResponse::Unauthorized().json(error.to_json(false));
            return Box::pin(async move { Ok(req.into_response(response)) });
        };
        let mut req = req;
        req.extensions_mut().insert(identity);
        Box::pin(async move { next(req).await })
    }
}

// ---------------------------------------------------------------------------
// Module definition
// ---------------------------------------------------------------------------

pub struct DashboardsModule {
    state: Arc<DashboardsState>,
}

impl DashboardsModule {
    pub fn new() -> Self {
        Self { state: Arc::new(DashboardsState::new()) }
    }

    /// Build the module around a `RouteRegistry` before `init` runs
    /// (for example the shared instance from `AppContext`). `init`
    /// always replaces it with the context registry (`ctx.routes`),
    /// so this is only a pre-init hint: the module, its prefixes, and
    /// the theme engine all resolve through the one shared registry
    /// after `init`, never through a private copy. The dashboards
    /// module resolves the `user_dashboard` and `admin_dashboard`
    /// paths from that registry in `init` instead of from the
    /// configuration alone, so config and admin-settings overrides
    /// for the dashboard paths apply here too. Without a shared
    /// registry the module falls back to `app.dashboard_path` (user)
    /// and `admin`.
    pub fn with_route_registry(registry: Arc<RouteRegistry>) -> Self {
        let module = Self::new();
        *module
            .state
            .route_registry
            .write()
            .expect("route registry lock poisoned") = Some(registry);
        module
    }
}

impl Default for DashboardsModule {
    fn default() -> Self {
        Self::new()
    }
}

impl Module for DashboardsModule {
    fn id(&self) -> &'static str {
        "dashboards"
    }

    fn name(&self) -> &'static str {
        "Dashboards"
    }

    fn description(&self) -> &'static str {
        "Separate admin, user, and reseller dashboards with server-side capability enforcement, role-aware navigation, and audited admin view-as-user."
    }

    fn requires(&self) -> &[&'static str] {
        &["roles", "users", "audit"]
    }

    /// Attach the shared route registry from the context (and the
    /// same instance to the theme engine), then resolve the dashboard
    /// prefixes from it and stash the handles the middleware needs.
    /// The registry's effective paths for `user_dashboard` and
    /// `admin_dashboard` win over the configuration
    /// (`app.dashboard_path` for the user dashboard, default
    /// `dashboard`), because the registry already layers the config
    /// value under admin-settings overrides. `init` runs before the
    /// HTTP server starts, so the prefixes are fixed before
    /// `configure_routes` registers the page scopes.
    fn init(&self, ctx: &AppContext) -> UnifiedResult<()> {
        // Unify on the single shared registry from the context: the
        // same instance the Routes module serves, the server redirect
        // handler resolves through, and `AppContext::new` seeded from
        // the `routes:` config section. Attaching it here (instead of
        // keeping whatever `with_route_registry` may have set) keeps
        // the prefixes, the settings endpoint, and the theme engine
        // on one registry, so a `routes:` override reaches all of them.
        *self.state.route_registry.write().expect("route registry lock poisoned") =
            Some(Arc::clone(&ctx.routes));
        // Attach the same shared registry to the theme engine so its
        // `route_url` helper resolves config and settings overrides
        // instead of the built-in defaults.
        self.state.engine.set_route_registry(Arc::clone(&ctx.routes));
        let registry = self
            .state
            .route_registry
            .read()
            .expect("route registry lock poisoned")
            .clone();
        if let Some(registry) = registry {
            if let Some(path) = registry.resolve("user_dashboard") {
                if let Some(segment) = DashboardsState::first_segment(&path) {
                    *self.state.user_prefix.write().expect("user prefix lock poisoned") =
                        segment;
                }
            }
            if let Some(path) = registry.resolve("admin_dashboard") {
                if let Some(segment) = DashboardsState::first_segment(&path) {
                    *self.state.admin_prefix.write().expect("admin prefix lock poisoned") =
                        segment;
                }
            }
        } else {
            let config = ctx.config.read().expect("config lock poisoned");
            let user_path = config.app.dashboard_path.trim();
            if !user_path.is_empty()
                && user_path
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
            {
                *self.state.user_prefix.write().expect("user prefix lock poisoned") =
                    user_path.to_string();
            }
        }
        let _ = self.state.hooks.set(ctx.hooks.clone());
        let _ = self.state.config.set(ctx.config.clone());
        let _ = self.state.db.set(ctx.db.clone());
        Ok(())
    }

    /// Follow `route.changed` for the two dashboard routes so the
    /// navigation, the scope endpoint, and the settings endpoint keep
    /// reporting the current paths after an admin override. Actix
    /// scopes are registered once at startup, so serving under a
    /// newly overridden prefix still needs a restart; the Routes
    /// module's server-level redirect covers the old path until then.
    fn register_hooks(&self, bus: &HookBus) {
        let state = self.state.clone();
        bus.register(
            ROUTE_CHANGED_EVENT,
            "dashboards",
            Arc::new(move |ctx: &mut crate::core::hooks::HookContext| {
                let route = ctx
                    .get("route")
                    .and_then(|value| value.as_str())
                    .unwrap_or("");
                let new_path = ctx
                    .get("new_path")
                    .and_then(|value| value.as_str())
                    .unwrap_or("");
                let Some(segment) = DashboardsState::first_segment(new_path) else {
                    return;
                };
                match route {
                    "user_dashboard" => {
                        *state.user_prefix.write().expect("user prefix lock poisoned") =
                            segment;
                    }
                    "admin_dashboard" => {
                        *state.admin_prefix.write().expect("admin prefix lock poisoned") =
                            segment;
                    }
                    _ => {}
                }
            }),
        );
    }

    fn register_middleware(&self, registry: &mut crate::core::middleware::MiddlewareManager) {
        registry.register(DashboardsAuthMiddleware::new(self.state.clone()));
    }

    fn configure_routes(&self, cfg: &mut web::ServiceConfig) {
        let state = self.state.clone();
        let admin_scope = format!("/{}", state.admin_prefix());
        let user_scope = format!("/{}", state.user_prefix());
        cfg.service(
            web::scope("/api/v1/dashboards")
                .app_data(web::Data::new(state.clone()))
                .route("/scope", web::get().to(get_scope))
                .route("/nav", web::get().to(get_nav))
                .route("/admin/overview", web::get().to(get_admin_overview))
                .route("/admin/modules", web::get().to(get_admin_modules))
                .route("/admin/users", web::get().to(get_admin_users))
                .route("/admin/settings", web::get().to(get_dashboard_settings))
                .route("/admin/settings", web::put().to(update_dashboard_settings))
                .route("/admin/view-as", web::get().to(get_view_as))
                .route("/admin/view-as", web::post().to(start_view_as))
                .route("/admin/view-as", web::delete().to(stop_view_as))
                .route("/user/overview", web::get().to(get_user_overview))
                .route("/user/licenses", web::get().to(get_user_licenses))
                .route("/user/orders", web::get().to(get_user_orders))
                .route("/user/downloads", web::get().to(get_user_downloads))
                .route("/reseller/overview", web::get().to(get_reseller_overview))
                .route("/reseller/clients", web::get().to(get_reseller_clients))
                .route("/reseller/clients", web::post().to(link_reseller_client))
                .route("/reseller/clients/{client_id}", web::delete().to(unlink_reseller_client)),
        );
        cfg.service(
            web::scope(&admin_scope)
                .app_data(web::Data::new(state.clone()))
                .route("", web::get().to(admin_page_root))
                .route("/{tail:.*}", web::get().to(admin_page)),
        );
        cfg.service(
            web::scope(&user_scope)
                .app_data(web::Data::new(state.clone()))
                .route("", web::get().to(user_page_root))
                .route("/{path:.*}", web::get().to(user_page)),
        );
    }

    fn route_info(&self) -> Vec<RouteInfo> {
        vec![
            RouteInfo { method: "GET", path: "/api/v1/dashboards/scope", owner: "dashboards", description: "Caller's effective scope, capabilities, and impersonation state" },
            RouteInfo { method: "GET", path: "/api/v1/dashboards/nav", owner: "dashboards", description: "Role-aware navigation items for one scope" },
            RouteInfo { method: "GET", path: "/api/v1/dashboards/admin/overview", owner: "dashboards", description: "Admin system overview" },
            RouteInfo { method: "GET", path: "/api/v1/dashboards/admin/modules", owner: "dashboards", description: "Admin module list with dependency validation" },
            RouteInfo { method: "GET", path: "/api/v1/dashboards/admin/users", owner: "dashboards", description: "Admin paginated user list with roles" },
            RouteInfo { method: "GET", path: "/api/v1/dashboards/admin/settings", owner: "dashboards", description: "Dashboard runtime settings" },
            RouteInfo { method: "PUT", path: "/api/v1/dashboards/admin/settings", owner: "dashboards", description: "Update dashboard runtime settings" },
            RouteInfo { method: "GET", path: "/api/v1/dashboards/admin/view-as", owner: "dashboards", description: "Current admin view-as session" },
            RouteInfo { method: "POST", path: "/api/v1/dashboards/admin/view-as", owner: "dashboards", description: "Start an admin view-as-user session" },
            RouteInfo { method: "DELETE", path: "/api/v1/dashboards/admin/view-as", owner: "dashboards", description: "Stop the active view-as session" },
            RouteInfo { method: "GET", path: "/api/v1/dashboards/user/overview", owner: "dashboards", description: "Caller's own overview" },
            RouteInfo { method: "GET", path: "/api/v1/dashboards/user/licenses", owner: "dashboards", description: "Caller's own licenses, paginated" },
            RouteInfo { method: "GET", path: "/api/v1/dashboards/user/orders", owner: "dashboards", description: "Caller's own orders, paginated" },
            RouteInfo { method: "GET", path: "/api/v1/dashboards/user/downloads", owner: "dashboards", description: "Caller's own downloads, paginated" },
            RouteInfo { method: "GET", path: "/api/v1/dashboards/reseller/overview", owner: "dashboards", description: "Reseller overview for linked clients" },
            RouteInfo { method: "GET", path: "/api/v1/dashboards/reseller/clients", owner: "dashboards", description: "Reseller linked clients, paginated" },
            RouteInfo { method: "POST", path: "/api/v1/dashboards/reseller/clients", owner: "dashboards", description: "Link a client to the reseller" },
            RouteInfo { method: "DELETE", path: "/api/v1/dashboards/reseller/clients/{client_id}", owner: "dashboards", description: "Unlink a client from the reseller" },
            RouteInfo { method: "GET", path: "/admin", owner: "dashboards", description: "Admin dashboard page" },
            RouteInfo { method: "GET", path: "/admin/{section}", owner: "dashboards", description: "Admin dashboard section page" },
            RouteInfo { method: "GET", path: "/{dashboard_path}", owner: "dashboards", description: "User dashboard page (prefix from app.dashboard_path)" },
            RouteInfo { method: "GET", path: "/{dashboard_path}/{path}", owner: "dashboards", description: "User dashboard section page" },
        ]
    }
}

pub fn register(registry: &ModuleRegistry) {
    registry.register(Arc::new(DashboardsModule::new()));
}
