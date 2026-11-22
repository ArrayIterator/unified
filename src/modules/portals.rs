// Client Portal (white-label) module — one branded portal per
// company: logo, colours, custom domain / subdomain, configurable
// pages (which sections are visible, in which order) and menus, plus
// read-only client data views (my invoices, tickets, downloads,
// subscriptions, licenses) that aggregate the owning modules'
// records.
//
// Routes live under `/api/v1/portals`:
//
//   Portals (admin):
//     GET/POST / — list portals / create a portal.
//     GET/PATCH/DELETE /{id}.
//     GET  /{id}/analytics — page-view analytics (paginated views
//       plus totals).
//   Portal menus (admin):
//     GET/POST /{id}/menus.
//     GET/PATCH/DELETE /{id}/menus/{menu_id}.
//     POST /{id}/menus/reorder — reorder menus by id list.
//   Public (no auth; see the public-access note below):
//     GET  /public/{slug} — public portal branding, visible
//       sections and menus; records a page view and emits
//       `portals.page.viewed`.
//   Client data (authenticated client, scoped server-side):
//     GET  /{slug}/my — the caller's portal home: branding plus a
//       per-section record count for the caller.
//     GET  /{slug}/my/invoices — the caller's invoices only.
//     GET  /{slug}/my/tickets — the caller's tickets only.
//     GET  /{slug}/my/downloads — the caller's downloads only.
//     GET  /{slug}/my/subscriptions — the caller's subscriptions only.
//     GET  /{slug}/my/licenses — the caller's licenses only.
//
// HONESTY — READ THIS FIRST. What is real in this module:
// in-memory portal, menu, page-view, and read-only projection
// management; validated hex colours, slugs, hostnames, and section
// lists; per-company portals with unique slugs and unique custom
// domains; menu ordering and per-role menu visibility; client
// scoping enforced server-side on every `my/*` endpoint (the
// portal must belong to the caller's company and every record
// returned must belong to the caller); page-view recording with a
// per-portal cap; and `portals.created`, `portals.updated`,
// `portals.page.viewed`, and `portals.menu.updated` events on the
// hook bus. What is NOT built here: actual domain provisioning
// (the domains module owns domains — this module only stores the
// requested custom domain and emits `portals.domain.requested` so
// the domains module can verify and provision it; until the
// domains module claims that event, `domain_status` stays
// `requested`); a second auth system (client login uses the auth
// manager — this module only reads the `X-User-Id` / `X-Role` /
// `X-Company-Id` headers the auth layer sets, exactly like the
// events module); and live billing / ticket / store / licensing
// data: the `my/*` views are a read-only projection built from
// the owning modules' hook events (`invoice.created`,
// `invoice.paid`, `invoice.voided`, `ticket.created`,
// `ticket.updated`, `store.order.created`,
// `store.order.completed`, `store.order.cancelled`,
// `license.issued`, `license.activated`, `license.suspended`,
// `license.revoked`, `download.started`, `download.finished`).
// Records created before this module started, or announced without
// a company id and a user id, are not in the projection — the
// owning modules remain the source of truth and this module never
// writes back to them. The logo is metadata only (storage target
// + key, exactly like the events module cover image): the bytes
// live in the storage module.
//
// Public-access gate: the API Manager module enforces public access
// through its admin-managed `ApiAccessRule` records (default deny:
// an endpoint is public only when a rule exists with
// `public_access: true`, and denied outright when a rule exists
// with `allowed: false`). There is no reusable per-request
// "is this endpoint public?" helper to call from a handler — the
// check lives in the API Manager middleware / `resolve_public`
// path — so the public route below relies on that gate: an
// administrator must create the matching `ApiAccessRule` row for
// `GET /api/v1/portals/public/*`. The route is registered in
// `route_info()` with `route_version_support()` defaulting to
// `All`, so the API Manager's per-version registry picks it up. Do
// NOT treat the absence of a capability check on a public route as
// an open door: without the rule the API Manager denies the call.
//
// Actor identity: until the auth module provides an extractor, the
// caller is identified by the `X-User-Id` header (required) and
// `X-Role` carries the role slug the capability check runs
// against, exactly like the ai module. Client data endpoints also
// require `X-Company-Id`: the portal and every record must belong
// to that company. Capability checks use the `role.check` hook and
// fail closed (no answer means not allowed).
//
// Events emitted on the hook bus:
//   - `portals.created`, `portals.updated`, `portals.deleted`
//   - `portals.page.viewed`
//   - `portals.menu.updated` (menu created, updated, deleted, or
//     reordered)
//   - `portals.domain.requested` (a custom domain was set or
//     changed; payload `{"portal_id", "slug", "company_id",
//     "domain"}` for the domains module to verify and provision)
// Events this module listens to (read-only projection only; this
// module never writes back to the owning modules):
//   - `invoice.created`, `invoice.paid`, `invoice.voided`
//   - `ticket.created`, `ticket.updated`
//   - `store.order.created`, `store.order.completed`,
//     `store.order.cancelled`
//   - `license.issued`, `license.activated`, `license.suspended`,
//     `license.revoked`
//   - `download.started`, `download.finished`

use crate::core::context::AppContext;
use crate::core::error::{UnifiedError, UnifiedResult};
use crate::core::hooks::{HookBus, HookContext};
use crate::core::json;
use crate::core::modules::{Module, ModuleRegistry, RouteInfo, RouteVersionSupport};
use crate::core::pagination::{PageResult, PaginationParams};
use actix_web::{web, HttpRequest, HttpResponse};
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::sync::{Arc, RwLock};
use uuid::Uuid;

/// Maximum lengths enforced on user supplied text.
const MAX_NAME_LEN: usize = 200;
const MAX_SLUG_LEN: usize = 128;
const MAX_LABEL_LEN: usize = 128;
const MAX_URL_LEN: usize = 2048;
const MAX_DOMAIN_LEN: usize = 253;
const MAX_COMPANY_LEN: usize = 128;
const MAX_STORAGE_KEY_LEN: usize = 512;
const MAX_ROLES_PER_MENU: usize = 20;
const MAX_ROLE_LEN: usize = 80;
const MAX_MENUS_PER_PORTAL: usize = 50;
/// Maximum page-view rows kept per portal; the oldest view is
/// dropped once the cap is reached so views stay bounded.
const MAX_VIEWS_PER_PORTAL: usize = 10_000;

/// Portal sections a client may see. The canonical order is the
/// default section order of a new portal.
const SECTION_NAMES: [&str; 5] = [
    "invoices",
    "tickets",
    "downloads",
    "subscriptions",
    "licenses",
];

/// Capability names checked through the `role.check` hook.
const PERM_PORTALS_VIEW: &str = "portals.view";
const PERM_PORTALS_MANAGE: &str = "portals.manage";

/// Hook event names this module listens to for its read-only
/// client-data projection.
const INVOICE_CREATED: &str = "invoice.created";
const INVOICE_PAID: &str = "invoice.paid";
const INVOICE_VOIDED: &str = "invoice.voided";
const TICKET_CREATED: &str = "ticket.created";
const TICKET_UPDATED: &str = "ticket.updated";
const ORDER_CREATED: &str = "store.order.created";
const ORDER_COMPLETED: &str = "store.order.completed";
const ORDER_CANCELLED: &str = "store.order.cancelled";
const LICENSE_ISSUED: &str = "license.issued";
const LICENSE_ACTIVATED: &str = "license.activated";
const LICENSE_SUSPENDED: &str = "license.suspended";
const LICENSE_REVOKED: &str = "license.revoked";
const DOWNLOAD_STARTED: &str = "download.started";
const DOWNLOAD_FINISHED: &str = "download.finished";

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

/// Lifecycle of a portal: `draft` -> `active`; `disabled` hides the
/// portal from public and client routes until reactivated.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PortalStatus {
    Draft,
    Active,
    Disabled,
}

impl Default for PortalStatus {
    fn default() -> Self {
        Self::Draft
    }
}

impl PortalStatus {
    fn as_str(&self) -> &'static str {
        match self {
            Self::Draft => "draft",
            Self::Active => "active",
            Self::Disabled => "disabled",
        }
    }
}

/// One branded client portal, owned by one company.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Portal {
    pub id: Uuid,
    pub company_id: String,
    pub name: String,
    pub slug: String,
    /// Custom domain requested for this portal (for example
    /// `portal.example.com`). Provisioning and verification belong
    /// to the domains module; see `domain_status`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub custom_domain: Option<String>,
    /// `none` (no domain requested), `requested`, `verified`, or
    /// `rejected`. Only the domains module may move this past
    /// `requested`, through its own listener on
    /// `portals.domain.requested`.
    pub domain_status: String,
    /// Storage module reference (target + key) for the portal logo.
    /// Metadata only: no image bytes live in this module.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub logo_storage_target: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub logo_storage_key: Option<String>,
    /// Brand colours, validated `#RGB` / `#RRGGBB` hex.
    pub primary_color: String,
    pub secondary_color: String,
    pub accent_color: String,
    pub background_color: String,
    pub text_color: String,
    /// Visible sections, in display order. Every entry is one of
    /// `SECTION_NAMES`; duplicates are rejected.
    #[serde(default)]
    pub sections: Vec<String>,
    pub status: PortalStatus,
    pub created_by: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// One menu entry of a portal. `target_kind` is `section` (the
/// `target` is a section name from `SECTION_NAMES`) or `url` (the
/// `target` is an `https://` / `http://` URL). `roles` limits the
/// entry to client roles; an empty list means every client role.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PortalMenu {
    pub id: Uuid,
    pub portal_id: Uuid,
    pub label: String,
    pub target_kind: String,
    pub target: String,
    pub position: i64,
    pub visible: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub roles: Vec<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// One row of the read-only client-data projection: a display view
/// of a record owned by the billing / tickets / store / licensing
/// modules. The owning module stays the source of truth; this row
/// carries only what the portal renders, and this module never
/// writes back to the source record.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PortalRecord {
    /// Source record id, as announced on the hook bus.
    pub id: String,
    /// `invoice`, `ticket`, `download`, `subscription`, or
    /// `license`.
    pub kind: String,
    pub company_id: String,
    pub user_id: String,
    pub title: String,
    pub status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub amount: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub currency: Option<String>,
    /// Owning module id (`billing`, `tickets`, `store`,
    /// `licensing`, `downloader`).
    pub source_module: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub link: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// One recorded portal page view (public portal render or client
/// home). No secrets are stored: only the viewer id when the
/// caller was authenticated, the client IP, and the user agent.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PortalView {
    pub id: Uuid,
    pub portal_id: Uuid,
    pub slug: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub viewer_user_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ip: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user_agent: Option<String>,
    pub viewed_at: DateTime<Utc>,
}

// ---------------------------------------------------------------------------
// Request bodies
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct CreatePortalRequest {
    pub company_id: String,
    pub name: String,
    pub slug: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub custom_domain: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub logo_storage_target: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub logo_storage_key: Option<String>,
    pub primary_color: Option<String>,
    pub secondary_color: Option<String>,
    pub accent_color: Option<String>,
    pub background_color: Option<String>,
    pub text_color: Option<String>,
    #[serde(default)]
    pub sections: Vec<String>,
    pub status: Option<PortalStatus>,
}

#[derive(Debug, Deserialize)]
pub struct UpdatePortalRequest {
    pub name: Option<String>,
    pub slug: Option<String>,
    pub custom_domain: Option<Option<String>>,
    pub logo_storage_target: Option<Option<String>>,
    pub logo_storage_key: Option<Option<String>>,
    pub primary_color: Option<String>,
    pub secondary_color: Option<String>,
    pub accent_color: Option<String>,
    pub background_color: Option<String>,
    pub text_color: Option<String>,
    pub sections: Option<Vec<String>>,
    pub status: Option<PortalStatus>,
}

#[derive(Debug, Deserialize)]
pub struct CreateMenuRequest {
    pub label: String,
    pub target_kind: String,
    pub target: String,
    pub position: Option<i64>,
    #[serde(default = "default_visible")]
    pub visible: bool,
    #[serde(default)]
    pub roles: Vec<String>,
}

fn default_visible() -> bool {
    true
}

#[derive(Debug, Deserialize)]
pub struct UpdateMenuRequest {
    pub label: Option<String>,
    pub target_kind: Option<String>,
    pub target: Option<String>,
    pub position: Option<i64>,
    pub visible: Option<bool>,
    pub roles: Option<Vec<String>>,
}

#[derive(Debug, Deserialize)]
pub struct ReorderMenusRequest {
    pub menu_ids: Vec<Uuid>,
}

// ---------------------------------------------------------------------------
// Shared state
// ---------------------------------------------------------------------------

/// In-memory stores for portals. The hook bus is attached in `init`
/// (the bus lives in `AppContext`); handlers tolerate a missing bus
/// by skipping event emission. Durable rows for these types are
/// defined by migration `2026-11-22-000046_portals`; until that
/// migration is executed, portals do not survive a restart.
#[derive(Clone)]
pub struct PortalsState {
    pub portals: Arc<DashMap<Uuid, Portal>>,
    pub menus: Arc<DashMap<Uuid, PortalMenu>>,
    pub records: Arc<DashMap<String, PortalRecord>>,
    pub views: Arc<DashMap<Uuid, PortalView>>,
    pub hooks: Arc<RwLock<Option<Arc<HookBus>>>>,
}

impl Default for PortalsState {
    fn default() -> Self {
        Self {
            portals: Arc::new(DashMap::new()),
            menus: Arc::new(DashMap::new()),
            records: Arc::new(DashMap::new()),
            views: Arc::new(DashMap::new()),
            hooks: Arc::new(RwLock::new(None)),
        }
    }
}

impl PortalsState {
    fn bus(&self) -> Option<Arc<HookBus>> {
        self.hooks.read().expect("hooks lock poisoned").clone()
    }

    fn emit(&self, event: &str, data: Value) {
        if let Some(bus) = self.bus() {
            bus.emit(event, data);
        }
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Caller identity: `X-User-Id` (required) plus `X-Role` (required
/// for capability checks), exactly like the ai module.
struct Caller {
    actor: String,
    role: String,
}

fn caller(req: &HttpRequest) -> UnifiedResult<Caller> {
    let actor = req
        .headers()
        .get("x-user-id")
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .ok_or_else(|| UnifiedError::unauthorized("Missing X-User-Id header"))?;
    let role = req
        .headers()
        .get("x-role")
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .ok_or_else(|| UnifiedError::unauthorized("Missing X-Role header"))?;
    Ok(Caller { actor, role })
}

/// The caller's company, from the `X-Company-Id` header set by the
/// auth layer. Client data endpoints require it: without a company
/// the server cannot scope the records, so the call is rejected
/// instead of returning unscoped data.
fn caller_company(req: &HttpRequest) -> UnifiedResult<String> {
    req.headers()
        .get("x-company-id")
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .ok_or_else(|| UnifiedError::unauthorized("Missing X-Company-Id header"))
}

/// Ask the roles module whether `role` holds `permission` through
/// the `role.check` hook. Fails closed: no answer means not allowed
/// (same fail-closed shape as the ai module's capability check).
fn check_capability(ctx: &AppContext, role: &str, permission: &str) -> bool {
    let context = ctx.hooks.emit(
        "role.check",
        json!({ "role": role, "permission": permission, "allowed": false }),
    );
    context
        .get("allowed")
        .and_then(|value| value.as_bool())
        .unwrap_or(false)
}

fn require_permission(ctx: &AppContext, role: &str, permission: &str) -> UnifiedResult<()> {
    if check_capability(ctx, role, permission) {
        Ok(())
    } else {
        Err(UnifiedError::forbidden(format!(
            "Role \"{role}\" does not hold permission \"{permission}\""
        )))
    }
}

fn parse_uuid(raw: &str, what: &str) -> UnifiedResult<Uuid> {
    Uuid::parse_str(raw).map_err(|_| UnifiedError::bad_request(format!("Invalid {what} id: {raw}")))
}

fn require_text(value: &str, field: &str, max: usize) -> UnifiedResult<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err(UnifiedError::bad_request(format!("{field} must not be empty")));
    }
    if trimmed.len() > max {
        return Err(UnifiedError::bad_request(format!(
            "{field} must be at most {max} characters"
        )));
    }
    Ok(trimmed.to_string())
}

fn ok<T: Serialize>(data: T) -> HttpResponse {
    json::success_response(data, 200)
}

fn created<T: Serialize>(data: T) -> HttpResponse {
    json::success_response(data, 201)
}

fn err(error: UnifiedError, debug: bool) -> HttpResponse {
    json::error_response(&error, debug)
}

fn paginate<T: Serialize + Clone>(items: Vec<T>, params: &PaginationParams) -> PageResult<T> {
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

/// A slug is lowercase letters, digits, and single hyphens only,
/// never leading or trailing with a hyphen, at most 128 chars.
fn valid_slug(slug: &str) -> bool {
    if slug.is_empty() || slug.len() > MAX_SLUG_LEN {
        return false;
    }
    if slug.starts_with('-') || slug.ends_with('-') || slug.contains("--") {
        return false;
    }
    slug.chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

fn slugify(title: &str) -> String {
    let mut out = String::new();
    let mut last_dash = true;
    for ch in title.to_lowercase().chars() {
        if ch.is_ascii_lowercase() || ch.is_ascii_digit() {
            out.push(ch);
            last_dash = false;
        } else if !last_dash {
            out.push('-');
            last_dash = true;
        }
    }
    while out.ends_with('-') {
        out.pop();
    }
    if out.is_empty() {
        out.push_str("portal");
    }
    out.truncate(MAX_SLUG_LEN);
    while out.ends_with('-') {
        out.pop();
    }
    out
}

/// A brand colour is `#RGB` or `#RRGGBB` hex, nothing else.
fn valid_hex_color(color: &str) -> bool {
    let digits = color.strip_prefix('#').unwrap_or("");
    (digits.len() == 3 || digits.len() == 6)
        && digits.chars().all(|c| c.is_ascii_hexdigit())
}

fn require_hex_color(value: &str, field: &str) -> UnifiedResult<String> {
    let trimmed = value.trim();
    if !valid_hex_color(trimmed) {
        return Err(UnifiedError::bad_request(format!(
            "{field} must be a hex colour such as \"#0B6B3A\" or \"#FFF\""
        )));
    }
    Ok(trimmed.to_uppercase())
}

/// A custom domain is a hostname: dot-separated labels of letters,
/// digits, and hyphens, never leading or trailing with a hyphen or
/// a dot, at least one dot, at most 253 chars. No scheme, port, or
/// path is allowed.
fn valid_domain(domain: &str) -> bool {
    if domain.is_empty() || domain.len() > MAX_DOMAIN_LEN {
        return false;
    }
    if domain.starts_with('.')
        || domain.ends_with('.')
        || domain.starts_with('-')
        || domain.ends_with('-')
        || !domain.contains('.')
    {
        return false;
    }
    domain.split('.').all(|label| {
        !label.is_empty()
            && label.len() <= 63
            && !label.starts_with('-')
            && !label.ends_with('-')
            && label
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-')
    })
}

/// A menu URL target is an `http://` or `https://` URL, at most
/// 2048 chars, with no whitespace.
fn valid_url(url: &str) -> bool {
    if url.is_empty() || url.len() > MAX_URL_LEN || url.chars().any(char::is_whitespace) {
        return false;
    }
    url.starts_with("http://") || url.starts_with("https://")
}

/// Validate a section list: every entry must be a known section,
/// no duplicates. An empty list is allowed (a portal with no
/// visible sections).
fn validate_sections(sections: &[String]) -> UnifiedResult<()> {
    if sections.len() > SECTION_NAMES.len() {
        return Err(UnifiedError::bad_request(format!(
            "at most {} sections are allowed",
            SECTION_NAMES.len()
        )));
    }
    let mut seen: Vec<&str> = Vec::new();
    for section in sections {
        if !SECTION_NAMES.contains(&section.as_str()) {
            return Err(UnifiedError::bad_request(format!(
                "Unknown portal section \"{section}\" (expected one of: {})",
                SECTION_NAMES.join(", ")
            )));
        }
        if seen.contains(&section.as_str()) {
            return Err(UnifiedError::bad_request(format!(
                "Duplicate portal section \"{section}\""
            )));
        }
        seen.push(section.as_str());
    }
    Ok(())
}

fn validate_menu_target(target_kind: &str, target: &str) -> UnifiedResult<()> {
    match target_kind {
        "section" => {
            if !SECTION_NAMES.contains(&target) {
                return Err(UnifiedError::bad_request(format!(
                    "Menu target \"{target}\" is not a known portal section (expected one of: {})",
                    SECTION_NAMES.join(", ")
                )));
            }
            Ok(())
        }
        "url" => {
            if !valid_url(target) {
                return Err(UnifiedError::bad_request(
                    "Menu URL target must start with \"http://\" or \"https://\" and contain no whitespace",
                ));
            }
            Ok(())
        }
        other => Err(UnifiedError::bad_request(format!(
            "Menu target_kind must be \"section\" or \"url\", not \"{other}\""
        ))),
    }
}

fn validate_roles(roles: &[String]) -> UnifiedResult<()> {
    if roles.len() > MAX_ROLES_PER_MENU {
        return Err(UnifiedError::bad_request(format!(
            "at most {MAX_ROLES_PER_MENU} roles are allowed per menu entry"
        )));
    }
    let mut seen: Vec<&str> = Vec::new();
    for role in roles {
        let trimmed = role.trim();
        if trimmed.is_empty() || trimmed.len() > MAX_ROLE_LEN {
            return Err(UnifiedError::bad_request(
                "menu roles must not be empty and must be at most 80 characters",
            ));
        }
        if seen.contains(&trimmed) {
            return Err(UnifiedError::bad_request(format!(
                "Duplicate menu role \"{trimmed}\""
            )));
        }
        seen.push(trimmed);
    }
    Ok(())
}

fn slug_in_use(state: &PortalsState, slug: &str, except: Option<Uuid>) -> bool {
    state
        .portals
        .iter()
        .any(|entry| entry.value().slug == slug && Some(entry.value().id) != except)
}

fn domain_in_use(state: &PortalsState, domain: &str, except: Option<Uuid>) -> bool {
    state.portals.iter().any(|entry| {
        entry.value().custom_domain.as_deref() == Some(domain)
            && Some(entry.value().id) != except
    })
}

fn load_portal(state: &PortalsState, raw_id: &str) -> UnifiedResult<Portal> {
    let id = parse_uuid(raw_id, "portal")?;
    state
        .portals
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Portal not found: {id}")))
}

fn load_portal_by_slug(state: &PortalsState, slug: &str) -> UnifiedResult<Portal> {
    state
        .portals
        .iter()
        .map(|entry| entry.value().clone())
        .find(|portal| portal.slug == slug)
        .ok_or_else(|| UnifiedError::not_found(format!("Portal not found: {slug}")))
}

fn load_menu(state: &PortalsState, portal_id: Uuid, raw_id: &str) -> UnifiedResult<PortalMenu> {
    let id = parse_uuid(raw_id, "menu")?;
    state
        .menus
        .get(&id)
        .map(|entry| entry.value().clone())
        .filter(|menu| menu.portal_id == portal_id)
        .ok_or_else(|| UnifiedError::not_found(format!("Menu entry not found: {id}")))
}

fn menus_for(state: &PortalsState, portal_id: Uuid) -> Vec<PortalMenu> {
    let mut items: Vec<PortalMenu> = state
        .menus
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|menu| menu.portal_id == portal_id)
        .collect();
    items.sort_by_key(|menu| (menu.position, menu.created_at));
    items
}

/// A menu entry is visible to a client role when it is enabled and
/// its role list is empty (every role) or contains the role.
fn menu_visible_to(menu: &PortalMenu, role: &str) -> bool {
    menu.visible && (menu.roles.is_empty() || menu.roles.iter().any(|entry| entry == role))
}

/// Map a portal section name (plural, as stored on the portal)
/// to the projection record kind (singular, as stored on the
/// record).
fn section_kind(section: &str) -> &'static str {
    match section {
        "invoices" => "invoice",
        "tickets" => "ticket",
        "downloads" => "download",
        "subscriptions" => "subscription",
        "licenses" => "license",
        _ => "",
    }
}

/// Record a page view, dropping the oldest view of the portal once
/// the per-portal cap is reached.
fn record_view(state: &PortalsState, portal: &Portal, req: &HttpRequest) {
    let viewer_user_id = req
        .headers()
        .get("x-user-id")
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string);
    let ip = req
        .headers()
        .get("x-forwarded-for")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.split(',').next())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .or_else(|| {
            req.connection_info()
                .realip_remote_addr()
                .map(str::to_string)
        });
    let user_agent = req
        .headers()
        .get("user-agent")
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string);
    let view = PortalView {
        id: Uuid::now_v7(),
        portal_id: portal.id,
        slug: portal.slug.clone(),
        viewer_user_id: viewer_user_id.clone(),
        ip,
        user_agent,
        viewed_at: Utc::now(),
    };
    let portal_views: Vec<PortalView> = state
        .views
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|entry| entry.portal_id == portal.id)
        .collect();
    if portal_views.len() >= MAX_VIEWS_PER_PORTAL {
        if let Some(oldest) = portal_views.iter().min_by_key(|entry| entry.viewed_at) {
            state.views.remove(&oldest.id);
        }
    }
    state.views.insert(view.id, view);
    state.emit(
        "portals.page.viewed",
        json!({
            "portal_id": portal.id.to_string(),
            "slug": portal.slug,
            "viewer_user_id": viewer_user_id,
        }),
    );
}

/// Public view of a portal: branding, visible sections, and the
/// menus the caller's role may see. Client records are never part
/// of this view.
fn public_portal_view(state: &PortalsState, portal: &Portal, role: Option<&str>) -> Value {
    let menus: Vec<Value> = menus_for(state, portal.id)
        .into_iter()
        .filter(|menu| match role {
            Some(role) => menu_visible_to(menu, role),
            None => menu.visible,
        })
        .map(|menu| {
            json!({
                "id": menu.id.to_string(),
                "label": menu.label,
                "target_kind": menu.target_kind,
                "target": menu.target,
                "position": menu.position,
            })
        })
        .collect();
    json!({
        "id": portal.id.to_string(),
        "company_id": portal.company_id,
        "name": portal.name,
        "slug": portal.slug,
        "custom_domain": portal.custom_domain,
        "domain_status": portal.domain_status,
        "logo_storage_target": portal.logo_storage_target,
        "logo_storage_key": portal.logo_storage_key,
        "primary_color": portal.primary_color,
        "secondary_color": portal.secondary_color,
        "accent_color": portal.accent_color,
        "background_color": portal.background_color,
        "text_color": portal.text_color,
        "sections": portal.sections,
        "status": portal.status.as_str(),
        "menus": menus,
    })
}

fn default_sections() -> Vec<String> {
    SECTION_NAMES.iter().map(|name| name.to_string()).collect()
}

fn default_color(primary: bool) -> String {
    if primary {
        "#0B6B3A".to_string()
    } else {
        "#FFFFFF".to_string()
    }
}

// ---------------------------------------------------------------------------
// Read-only projection (listeners on the owning modules' events)
// ---------------------------------------------------------------------------

/// Upsert one projection row from a hook payload. Payload shapes
/// differ per owning module, so the ids, amount, and status are
/// read defensively from the common key spellings; a payload with
/// no company id or no user id is skipped and reported, because
/// the portal can never scope such a record to a client.
fn upsert_projection(
    state: &PortalsState,
    ctx: &mut HookContext,
    kind: &str,
    source_module: &str,
    default_status: &str,
) {
    let id = ctx
        .get("id")
        .and_then(|value| value.as_str())
        .map(str::to_string)
        .or_else(|| {
            ctx.get("record_id")
                .and_then(|value| value.as_str())
                .map(str::to_string)
        });
    let company_id = ctx
        .get("company_id")
        .and_then(|value| value.as_str())
        .map(str::to_string)
        .or_else(|| {
            ctx.get("customer_id")
                .and_then(|value| value.as_str())
                .map(str::to_string)
        });
    let user_id = ctx
        .get("user_id")
        .and_then(|value| value.as_str())
        .map(str::to_string)
        .or_else(|| {
            ctx.get("customer_id")
                .and_then(|value| value.as_str())
                .map(str::to_string)
        })
        .or_else(|| {
            ctx.get("owner_id")
                .and_then(|value| value.as_str())
                .map(str::to_string)
        });
    let (Some(id), Some(company_id), Some(user_id)) = (id, company_id, user_id) else {
        ctx.add_result(json!({
            "portals": "skipped",
            "kind": kind,
            "reason": "payload carries no id, company id, or user id, so the record cannot be scoped to a portal client",
        }));
        return;
    };
    let title = ctx
        .get("title")
        .and_then(|value| value.as_str())
        .map(str::to_string)
        .or_else(|| {
            ctx.get("name")
                .and_then(|value| value.as_str())
                .map(str::to_string)
        })
        .unwrap_or_else(|| format!("{kind} {id}"));
    let status = ctx
        .get("status")
        .and_then(|value| value.as_str())
        .map(str::to_string)
        .unwrap_or_else(|| default_status.to_string());
    let amount = ctx
        .get("total_amount")
        .and_then(|value| value.as_f64())
        .or_else(|| ctx.get("amount").and_then(|value| value.as_f64()))
        .or_else(|| ctx.get("total").and_then(|value| value.as_f64()));
    let currency = ctx
        .get("currency")
        .and_then(|value| value.as_str())
        .map(str::to_string);
    let link = ctx
        .get("link")
        .and_then(|value| value.as_str())
        .map(str::to_string);
    let now = Utc::now();
    let key = format!("{kind}:{id}");
    let created_at = state
        .records
        .get(&key)
        .map(|entry| entry.value().created_at)
        .unwrap_or(now);
    let record = PortalRecord {
        id: id.clone(),
        kind: kind.to_string(),
        company_id,
        user_id,
        title,
        status,
        amount,
        currency,
        source_module: source_module.to_string(),
        link,
        created_at,
        updated_at: now,
    };
    state.records.insert(key, record);
    ctx.add_result(json!({
        "portals": "projected",
        "kind": kind,
        "id": id,
    }));
}

/// One listener shared by every projection event: the event name
/// decides the record kind, source module, and default status.
fn on_projection_event(state: PortalsState, event: &str, ctx: &mut HookContext) {
    let (kind, source_module, default_status) = match event {
        INVOICE_CREATED => ("invoice", "billing", "draft"),
        INVOICE_PAID => ("invoice", "billing", "paid"),
        INVOICE_VOIDED => ("invoice", "billing", "voided"),
        TICKET_CREATED => ("ticket", "tickets", "open"),
        TICKET_UPDATED => ("ticket", "tickets", "open"),
        ORDER_CREATED => ("subscription", "store", "created"),
        ORDER_COMPLETED => ("download", "store", "completed"),
        ORDER_CANCELLED => ("subscription", "store", "cancelled"),
        LICENSE_ISSUED => ("license", "licensing", "active"),
        LICENSE_ACTIVATED => ("license", "licensing", "active"),
        LICENSE_SUSPENDED => ("license", "licensing", "suspended"),
        LICENSE_REVOKED => ("license", "licensing", "revoked"),
        DOWNLOAD_STARTED => ("download", "downloader", "downloading"),
        DOWNLOAD_FINISHED => ("download", "downloader", "finished"),
        _ => return,
    };
    upsert_projection(&state, ctx, kind, source_module, default_status);
}

// ---------------------------------------------------------------------------
// Module
// ---------------------------------------------------------------------------

pub struct PortalsModule {
    state: PortalsState,
}

impl PortalsModule {
    pub fn new() -> Self {
        Self {
            state: PortalsState::default(),
        }
    }
}

impl Default for PortalsModule {
    fn default() -> Self {
        Self::new()
    }
}

impl Module for PortalsModule {
    fn id(&self) -> &'static str {
        "portals"
    }

    fn name(&self) -> &'static str {
        "Client Portal (White-Label)"
    }

    fn description(&self) -> &'static str {
        "White-label client portals per company: branding, custom domain, configurable sections and menus, plus read-only client views of invoices, tickets, downloads, subscriptions, and licenses aggregated from the owning modules."
    }

    fn requires(&self) -> &[&'static str] {
        &[
            "users",
            "roles",
            "billing",
            "tickets",
            "store",
            "licensing",
            "domains",
            "storage",
            "auth_manager",
        ]
    }

    fn dependencies(&self) -> Vec<&'static str> {
        vec![
            "users",
            "roles",
            "billing",
            "tickets",
            "store",
            "licensing",
            "domains",
            "storage",
            "auth_manager",
        ]
    }

    fn available_as_product(&self) -> bool {
        true
    }

    fn product_category(&self) -> Option<&'static str> {
        Some("productivity")
    }

    fn init(&self, ctx: &AppContext) -> UnifiedResult<()> {
        *self
            .state
            .hooks
            .write()
            .expect("hooks lock poisoned") = Some(Arc::clone(&ctx.hooks));
        Ok(())
    }

    fn register_hooks(&self, bus: &HookBus) {
        for event in [
            INVOICE_CREATED,
            INVOICE_PAID,
            INVOICE_VOIDED,
            TICKET_CREATED,
            TICKET_UPDATED,
            ORDER_CREATED,
            ORDER_COMPLETED,
            ORDER_CANCELLED,
            LICENSE_ISSUED,
            LICENSE_ACTIVATED,
            LICENSE_SUSPENDED,
            LICENSE_REVOKED,
            DOWNLOAD_STARTED,
            DOWNLOAD_FINISHED,
        ] {
            let state = self.state.clone();
            let event_name = event.to_string();
            bus.register(
                event,
                "portals",
                Arc::new(move |ctx: &mut HookContext| {
                    on_projection_event(state.clone(), &event_name, ctx)
                }),
            );
        }
    }

    fn configure_routes(&self, cfg: &mut web::ServiceConfig) {
        let state = web::Data::new(self.state.clone());
        cfg.service(
            web::scope("/api/v1/portals")
                .app_data(state)
                .route("", web::get().to(list_portals))
                .route("", web::post().to(create_portal))
                .route("/public/{slug}", web::get().to(get_public_portal))
                .route("/{id}", web::get().to(get_portal))
                .route("/{id}", web::patch().to(update_portal))
                .route("/{id}", web::delete().to(delete_portal))
                .route("/{id}/analytics", web::get().to(portal_analytics))
                .route("/{id}/menus", web::get().to(list_menus))
                .route("/{id}/menus", web::post().to(create_menu))
                .route("/{id}/menus/reorder", web::post().to(reorder_menus))
                .route("/{id}/menus/{menu_id}", web::get().to(get_menu))
                .route("/{id}/menus/{menu_id}", web::patch().to(update_menu))
                .route("/{id}/menus/{menu_id}", web::delete().to(delete_menu))
                .route("/{slug}/my", web::get().to(client_portal_home))
                .route("/{slug}/my/invoices", web::get().to(client_invoices))
                .route("/{slug}/my/tickets", web::get().to(client_tickets))
                .route("/{slug}/my/downloads", web::get().to(client_downloads))
                .route("/{slug}/my/subscriptions", web::get().to(client_subscriptions))
                .route("/{slug}/my/licenses", web::get().to(client_licenses)),
        );
    }

    fn route_info(&self) -> Vec<RouteInfo> {
        vec![
            RouteInfo { method: "GET", path: "/api/v1/portals", owner: "portals", description: "List portals (paginated, filter by company_id, status, search)." },
            RouteInfo { method: "POST", path: "/api/v1/portals", owner: "portals", description: "Create a branded portal for a company (slug, colours, sections, optional custom domain)." },
            RouteInfo { method: "GET", path: "/api/v1/portals/{id}", owner: "portals", description: "Get one portal." },
            RouteInfo { method: "PATCH", path: "/api/v1/portals/{id}", owner: "portals", description: "Update a portal (branding, sections, status, custom domain)." },
            RouteInfo { method: "DELETE", path: "/api/v1/portals/{id}", owner: "portals", description: "Delete a portal with its menus and page views (projection rows belong to the owning modules and are kept)." },
            RouteInfo { method: "GET", path: "/api/v1/portals/{id}/analytics", owner: "portals", description: "Portal page-view analytics (paginated views plus totals)." },
            RouteInfo { method: "GET", path: "/api/v1/portals/{id}/menus", owner: "portals", description: "List menu entries of a portal (position order)." },
            RouteInfo { method: "POST", path: "/api/v1/portals/{id}/menus", owner: "portals", description: "Create a menu entry (section target or URL target, role visibility)." },
            RouteInfo { method: "GET", path: "/api/v1/portals/{id}/menus/{menu_id}", owner: "portals", description: "Get one menu entry." },
            RouteInfo { method: "PATCH", path: "/api/v1/portals/{id}/menus/{menu_id}", owner: "portals", description: "Update a menu entry." },
            RouteInfo { method: "DELETE", path: "/api/v1/portals/{id}/menus/{menu_id}", owner: "portals", description: "Delete a menu entry." },
            RouteInfo { method: "POST", path: "/api/v1/portals/{id}/menus/reorder", owner: "portals", description: "Reorder menu entries by id list." },
            RouteInfo { method: "GET", path: "/api/v1/portals/public/{slug}", owner: "portals", description: "PUBLIC: one portal's branding, visible sections, and menus; records a page view (no auth; gated by the API Manager ApiAccessRule)." },
            RouteInfo { method: "GET", path: "/api/v1/portals/{slug}/my", owner: "portals", description: "Client portal home: branding plus the caller's per-section record counts (scoped to the caller's company and user id)." },
            RouteInfo { method: "GET", path: "/api/v1/portals/{slug}/my/invoices", owner: "portals", description: "Client invoices: only the caller's own invoices of the caller's company (paginated, filter by status)." },
            RouteInfo { method: "GET", path: "/api/v1/portals/{slug}/my/tickets", owner: "portals", description: "Client tickets: only the caller's own tickets of the caller's company (paginated, filter by status)." },
            RouteInfo { method: "GET", path: "/api/v1/portals/{slug}/my/downloads", owner: "portals", description: "Client downloads: only the caller's own downloads of the caller's company (paginated, filter by status)." },
            RouteInfo { method: "GET", path: "/api/v1/portals/{slug}/my/subscriptions", owner: "portals", description: "Client subscriptions: only the caller's own subscriptions of the caller's company (paginated, filter by status)." },
            RouteInfo { method: "GET", path: "/api/v1/portals/{slug}/my/licenses", owner: "portals", description: "Client licenses: only the caller's own licenses of the caller's company (paginated, filter by status)." },
        ]
    }

    fn route_version_support(&self) -> RouteVersionSupport {
        RouteVersionSupport::All
    }
}

pub fn register(registry: &ModuleRegistry) {
    registry.register(Arc::new(PortalsModule::new()));
}

// ---------------------------------------------------------------------------
// Portal handlers
// ---------------------------------------------------------------------------

async fn list_portals(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<PortalsState>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PORTALS_VIEW)?;
        let params = query.into_inner();
        let company_filter = params.filters.get("company_id").cloned();
        let status_filter = params.filters.get("status").cloned();
        let search = params.search.clone().unwrap_or_default().to_lowercase();
        let mut items: Vec<Portal> = state
            .portals
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|portal| {
                if let Some(company) = &company_filter {
                    if &portal.company_id != company {
                        return false;
                    }
                }
                if let Some(status) = &status_filter {
                    if portal.status.as_str() != status.as_str() {
                        return false;
                    }
                }
                if !search.is_empty() {
                    let haystack = format!("{} {} {}", portal.name, portal.slug, portal.company_id)
                        .to_lowercase();
                    if !haystack.contains(&search) {
                        return false;
                    }
                }
                true
            })
            .collect();
        let sort = params.allowed_sort(&["name", "slug", "created_at"], "created_at");
        let descending = params.normalized().order == "desc";
        items.sort_by(|a, b| {
            let ordering = match sort.as_str() {
                "name" => a.name.cmp(&b.name),
                "slug" => a.slug.cmp(&b.slug),
                _ => a.created_at.cmp(&b.created_at),
            };
            if descending {
                ordering.reverse()
            } else {
                ordering
            }
        });
        Ok(paginate(items, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn create_portal(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<PortalsState>,
    req: HttpRequest,
    body: web::Json<CreatePortalRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Portal> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PORTALS_MANAGE)?;
        let input = body.into_inner();
        let company_id = require_text(&input.company_id, "company_id", MAX_COMPANY_LEN)?;
        let name = require_text(&input.name, "name", MAX_NAME_LEN)?;
        let slug = match &input.slug {
            Some(raw) => {
                let slug = raw.trim().to_lowercase();
                if !valid_slug(&slug) {
                    return Err(UnifiedError::bad_request(
                        "slug must be lowercase letters, digits, and single hyphens only (no leading/trailing hyphen, max 128 characters)",
                    ));
                }
                slug
            }
            None => slugify(&name),
        };
        if slug_in_use(&state, &slug, None) {
            return Err(UnifiedError::conflict(format!("slug is already in use: {slug}")));
        }
        let custom_domain = match &input.custom_domain {
            Some(raw) => {
                let domain = raw.trim().to_lowercase();
                if !valid_domain(&domain) {
                    return Err(UnifiedError::bad_request(format!(
                        "Invalid custom domain: {domain}"
                    )));
                }
                if domain_in_use(&state, &domain, None) {
                    return Err(UnifiedError::conflict(format!(
                        "custom domain is already in use: {domain}"
                    )));
                }
                Some(domain)
            }
            None => None,
        };
        let logo_storage_target = match &input.logo_storage_target {
            Some(raw) => Some(require_text(raw, "logo_storage_target", MAX_STORAGE_KEY_LEN)?),
            None => None,
        };
        let logo_storage_key = match &input.logo_storage_key {
            Some(raw) => Some(require_text(raw, "logo_storage_key", MAX_STORAGE_KEY_LEN)?),
            None => None,
        };
        if logo_storage_key.is_some() != logo_storage_target.is_some() {
            return Err(UnifiedError::bad_request(
                "logo_storage_target and logo_storage_key must be given together (storage module reference)"
            ));
        }
        let primary_color = match &input.primary_color {
            Some(raw) => require_hex_color(raw, "primary_color")?,
            None => default_color(true),
        };
        let secondary_color = match &input.secondary_color {
            Some(raw) => require_hex_color(raw, "secondary_color")?,
            None => default_color(false),
        };
        let accent_color = match &input.accent_color {
            Some(raw) => require_hex_color(raw, "accent_color")?,
            None => "#C9A227".to_string(),
        };
        let background_color = match &input.background_color {
            Some(raw) => require_hex_color(raw, "background_color")?,
            None => "#FFFFFF".to_string(),
        };
        let text_color = match &input.text_color {
            Some(raw) => require_hex_color(raw, "text_color")?,
            None => "#1A1A1A".to_string(),
        };
        let sections = if input.sections.is_empty() {
            default_sections()
        } else {
            validate_sections(&input.sections)?;
            input.sections.clone()
        };
        let now = Utc::now();
        let portal = Portal {
            id: Uuid::now_v7(),
            company_id: company_id.clone(),
            name,
            slug: slug.clone(),
            custom_domain: custom_domain.clone(),
            domain_status: if custom_domain.is_some() {
                "requested".to_string()
            } else {
                "none".to_string()
            },
            logo_storage_target,
            logo_storage_key,
            primary_color,
            secondary_color,
            accent_color,
            background_color,
            text_color,
            sections,
            status: input.status.unwrap_or_default(),
            created_by: caller.actor.clone(),
            created_at: now,
            updated_at: now,
        };
        state.portals.insert(portal.id, portal.clone());
        if let Some(domain) = &custom_domain {
            state.emit(
                "portals.domain.requested",
                json!({
                    "portal_id": portal.id.to_string(),
                    "slug": portal.slug,
                    "company_id": company_id,
                    "domain": domain,
                }),
            );
        }
        state.emit(
            "portals.created",
            json!({
                "portal_id": portal.id.to_string(),
                "slug": portal.slug,
                "company_id": portal.company_id,
                "status": portal.status.as_str(),
            }),
        );
        Ok(portal)
    })();
    match result {
        Ok(portal) => created(portal),
        Err(error) => err(error, debug),
    }
}

async fn get_portal(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<PortalsState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Portal> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PORTALS_VIEW)?;
        load_portal(&state, &path.into_inner())
    })();
    match result {
        Ok(portal) => ok(portal),
        Err(error) => err(error, debug),
    }
}

async fn update_portal(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<PortalsState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdatePortalRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Portal> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PORTALS_MANAGE)?;
        let mut portal = load_portal(&state, &path.into_inner())?;
        let input = body.into_inner();
        let mut domain_changed = false;
        if let Some(name) = &input.name {
            portal.name = require_text(name, "name", MAX_NAME_LEN)?;
        }
        if let Some(slug) = &input.slug {
            let slug = slug.trim().to_lowercase();
            if !valid_slug(&slug) {
                return Err(UnifiedError::bad_request(
                    "slug must be lowercase letters, digits, and single hyphens only",
                ));
            }
            if slug_in_use(&state, &slug, Some(portal.id)) {
                return Err(UnifiedError::conflict(format!("slug is already in use: {slug}")));
            }
            portal.slug = slug;
        }
        if let Some(domain) = &input.custom_domain {
            match domain {
                Some(raw) => {
                    let domain = raw.trim().to_lowercase();
                    if !valid_domain(&domain) {
                        return Err(UnifiedError::bad_request(format!(
                            "Invalid custom domain: {domain}"
                        )));
                    }
                    if domain_in_use(&state, &domain, Some(portal.id)) {
                        return Err(UnifiedError::conflict(format!(
                            "custom domain is already in use: {domain}"
                        )));
                    }
                    if portal.custom_domain.as_deref() != Some(domain.as_str()) {
                        domain_changed = true;
                    }
                    portal.custom_domain = Some(domain);
                    portal.domain_status = "requested".to_string();
                }
                None => {
                    if portal.custom_domain.is_some() {
                        domain_changed = true;
                    }
                    portal.custom_domain = None;
                    portal.domain_status = "none".to_string();
                }
            }
        }
        if let Some(target) = &input.logo_storage_target {
            portal.logo_storage_target = match target {
                Some(raw) => Some(require_text(raw, "logo_storage_target", MAX_STORAGE_KEY_LEN)?),
                None => None,
            };
        }
        if let Some(key) = &input.logo_storage_key {
            portal.logo_storage_key = match key {
                Some(raw) => Some(require_text(raw, "logo_storage_key", MAX_STORAGE_KEY_LEN)?),
                None => None,
            };
        }
        if portal.logo_storage_key.is_some() != portal.logo_storage_target.is_some() {
            return Err(UnifiedError::bad_request(
                "logo_storage_target and logo_storage_key must be given together",
            ));
        }
        if let Some(color) = &input.primary_color {
            portal.primary_color = require_hex_color(color, "primary_color")?;
        }
        if let Some(color) = &input.secondary_color {
            portal.secondary_color = require_hex_color(color, "secondary_color")?;
        }
        if let Some(color) = &input.accent_color {
            portal.accent_color = require_hex_color(color, "accent_color")?;
        }
        if let Some(color) = &input.background_color {
            portal.background_color = require_hex_color(color, "background_color")?;
        }
        if let Some(color) = &input.text_color {
            portal.text_color = require_hex_color(color, "text_color")?;
        }
        if let Some(sections) = &input.sections {
            validate_sections(sections)?;
            portal.sections = sections.clone();
        }
        if let Some(status) = input.status {
            portal.status = status;
        }
        portal.updated_at = Utc::now();
        state.portals.insert(portal.id, portal.clone());
        if domain_changed {
            if let Some(domain) = &portal.custom_domain {
                state.emit(
                    "portals.domain.requested",
                    json!({
                        "portal_id": portal.id.to_string(),
                        "slug": portal.slug,
                        "company_id": portal.company_id,
                        "domain": domain,
                    }),
                );
            }
        }
        state.emit(
            "portals.updated",
            json!({
                "portal_id": portal.id.to_string(),
                "slug": portal.slug,
                "status": portal.status.as_str(),
            }),
        );
        Ok(portal)
    })();
    match result {
        Ok(portal) => ok(portal),
        Err(error) => err(error, debug),
    }
}

async fn delete_portal(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<PortalsState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PORTALS_MANAGE)?;
        let portal = load_portal(&state, &path.into_inner())?;
        let id = portal.id;
        state.portals.remove(&id);
        let stale_menus: Vec<Uuid> = state
            .menus
            .iter()
            .filter(|entry| entry.value().portal_id == id)
            .map(|entry| *entry.key())
            .collect();
        for key in stale_menus {
            state.menus.remove(&key);
        }
        let stale_views: Vec<Uuid> = state
            .views
            .iter()
            .filter(|entry| entry.value().portal_id == id)
            .map(|entry| *entry.key())
            .collect();
        for key in stale_views {
            state.views.remove(&key);
        }
        state.emit(
            "portals.deleted",
            json!({ "portal_id": id.to_string(), "slug": portal.slug }),
        );
        Ok(json!({ "deleted": true, "portal_id": id.to_string() }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn portal_analytics(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<PortalsState>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PORTALS_VIEW)?;
        let portal = load_portal(&state, &path.into_inner())?;
        let params = query.into_inner();
        let mut views: Vec<PortalView> = state
            .views
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|view| view.portal_id == portal.id)
            .collect();
        views.sort_by_key(|view| std::cmp::Reverse(view.viewed_at));
        let total_views = views.len() as u64;
        let mut unique_viewers: Vec<String> = views
            .iter()
            .filter_map(|view| view.viewer_user_id.clone())
            .collect();
        unique_viewers.sort();
        unique_viewers.dedup();
        let page = paginate(views, &params).to_json();
        Ok(json!({
            "portal_id": portal.id.to_string(),
            "slug": portal.slug,
            "total_views": total_views,
            "unique_viewers": unique_viewers.len() as u64,
            "views": page,
        }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

// ---------------------------------------------------------------------------
// Menu handlers
// ---------------------------------------------------------------------------

async fn list_menus(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<PortalsState>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PORTALS_VIEW)?;
        let portal = load_portal(&state, &path.into_inner())?;
        let params = query.into_inner();
        let items = menus_for(&state, portal.id);
        Ok(paginate(items, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn create_menu(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<PortalsState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<CreateMenuRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PortalMenu> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PORTALS_MANAGE)?;
        let portal = load_portal(&state, &path.into_inner())?;
        let input = body.into_inner();
        if menus_for(&state, portal.id).len() >= MAX_MENUS_PER_PORTAL {
            return Err(UnifiedError::bad_request(format!(
                "at most {MAX_MENUS_PER_PORTAL} menu entries are allowed per portal"
            )));
        }
        let label = require_text(&input.label, "label", MAX_LABEL_LEN)?;
        let target_kind = input.target_kind.trim().to_string();
        let target = input.target.trim().to_string();
        validate_menu_target(&target_kind, &target)?;
        validate_roles(&input.roles)?;
        let roles: Vec<String> = input
            .roles
            .iter()
            .map(|role| role.trim().to_string())
            .collect();
        let position = match input.position {
            Some(position) if position >= 0 => position,
            Some(_) => {
                return Err(UnifiedError::bad_request(
                    "menu position must be >= 0",
                ))
            }
            None => menus_for(&state, portal.id)
                .iter()
                .map(|menu| menu.position)
                .max()
                .unwrap_or(-1)
                + 1,
        };
        let now = Utc::now();
        let menu = PortalMenu {
            id: Uuid::now_v7(),
            portal_id: portal.id,
            label,
            target_kind,
            target,
            position,
            visible: input.visible,
            roles,
            created_at: now,
            updated_at: now,
        };
        state.menus.insert(menu.id, menu.clone());
        state.emit(
            "portals.menu.updated",
            json!({
                "portal_id": portal.id.to_string(),
                "menu_id": menu.id.to_string(),
                "action": "created",
            }),
        );
        Ok(menu)
    })();
    match result {
        Ok(menu) => created(menu),
        Err(error) => err(error, debug),
    }
}

async fn get_menu(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<PortalsState>,
    req: HttpRequest,
    path: web::Path<(String, String)>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PortalMenu> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PORTALS_VIEW)?;
        let (portal_id, menu_id) = path.into_inner();
        let portal = load_portal(&state, &portal_id)?;
        load_menu(&state, portal.id, &menu_id)
    })();
    match result {
        Ok(menu) => ok(menu),
        Err(error) => err(error, debug),
    }
}

async fn update_menu(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<PortalsState>,
    req: HttpRequest,
    path: web::Path<(String, String)>,
    body: web::Json<UpdateMenuRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PortalMenu> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PORTALS_MANAGE)?;
        let (portal_id, menu_id) = path.into_inner();
        let portal = load_portal(&state, &portal_id)?;
        let mut menu = load_menu(&state, portal.id, &menu_id)?;
        let input = body.into_inner();
        if let Some(label) = &input.label {
            menu.label = require_text(label, "label", MAX_LABEL_LEN)?;
        }
        if let Some(target_kind) = &input.target_kind {
            menu.target_kind = target_kind.trim().to_string();
        }
        if let Some(target) = &input.target {
            menu.target = target.trim().to_string();
        }
        validate_menu_target(&menu.target_kind, &menu.target)?;
        if let Some(position) = input.position {
            if position < 0 {
                return Err(UnifiedError::bad_request("menu position must be >= 0"));
            }
            menu.position = position;
        }
        if let Some(visible) = input.visible {
            menu.visible = visible;
        }
        if let Some(roles) = &input.roles {
            validate_roles(roles)?;
            menu.roles = roles.iter().map(|role| role.trim().to_string()).collect();
        }
        menu.updated_at = Utc::now();
        state.menus.insert(menu.id, menu.clone());
        state.emit(
            "portals.menu.updated",
            json!({
                "portal_id": portal.id.to_string(),
                "menu_id": menu.id.to_string(),
                "action": "updated",
            }),
        );
        Ok(menu)
    })();
    match result {
        Ok(menu) => ok(menu),
        Err(error) => err(error, debug),
    }
}

async fn delete_menu(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<PortalsState>,
    req: HttpRequest,
    path: web::Path<(String, String)>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PORTALS_MANAGE)?;
        let (portal_id, menu_id) = path.into_inner();
        let portal = load_portal(&state, &portal_id)?;
        let menu = load_menu(&state, portal.id, &menu_id)?;
        state.menus.remove(&menu.id);
        state.emit(
            "portals.menu.updated",
            json!({
                "portal_id": portal.id.to_string(),
                "menu_id": menu.id.to_string(),
                "action": "deleted",
            }),
        );
        Ok(json!({ "deleted": true, "menu_id": menu.id.to_string() }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn reorder_menus(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<PortalsState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<ReorderMenusRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Vec<PortalMenu>> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PORTALS_MANAGE)?;
        let portal = load_portal(&state, &path.into_inner())?;
        let input = body.into_inner();
        let current = menus_for(&state, portal.id);
        if input.menu_ids.len() != current.len() {
            return Err(UnifiedError::bad_request(
                "menu_ids must list every menu entry of the portal exactly once",
            ));
        }
        let mut seen: Vec<Uuid> = Vec::new();
        for id in &input.menu_ids {
            if seen.contains(id) {
                return Err(UnifiedError::bad_request(format!(
                    "Duplicate menu id in reorder list: {id}"
                )));
            }
            seen.push(*id);
            if !current.iter().any(|menu| menu.id == *id) {
                return Err(UnifiedError::bad_request(format!(
                    "Menu id does not belong to this portal: {id}"
                )));
            }
        }
        let now = Utc::now();
        let mut updated: Vec<PortalMenu> = Vec::new();
        for (position, id) in input.menu_ids.iter().enumerate() {
            if let Some(mut menu) = state.menus.get_mut(id) {
                menu.position = position as i64;
                menu.updated_at = now;
                updated.push(menu.value().clone());
            }
        }
        updated.sort_by_key(|menu| menu.position);
        state.emit(
            "portals.menu.updated",
            json!({
                "portal_id": portal.id.to_string(),
                "action": "reordered",
            }),
        );
        Ok(updated)
    })();
    match result {
        Ok(menus) => ok(menus),
        Err(error) => err(error, debug),
    }
}

// ---------------------------------------------------------------------------
// Public portal
// ---------------------------------------------------------------------------

async fn get_public_portal(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<PortalsState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    // No capability check here on purpose: this is the public
    // portal render. The API Manager gate (ApiAccessRule with
    // public_access for this path, default deny) is what makes it
    // public — see the module header.
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let portal = load_portal_by_slug(&state, &path.into_inner())?;
        if portal.status != PortalStatus::Active {
            return Err(UnifiedError::not_found(format!(
                "Portal not found: {}",
                portal.slug
            )));
        }
        let role = req
            .headers()
            .get("x-role")
            .and_then(|value| value.to_str().ok())
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string);
        let view = public_portal_view(&state, &portal, role.as_deref());
        record_view(&state, &portal, &req);
        Ok(view)
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

// ---------------------------------------------------------------------------
// Client data (scoped server-side)
// ---------------------------------------------------------------------------

/// Load the portal for a client call and enforce scoping: the
/// portal must be active, must belong to the caller's company, and
/// the requested section must be visible on the portal. Returns
/// the portal, the caller, and the company id.
fn client_portal(
    state: &PortalsState,
    req: &HttpRequest,
    slug: &str,
    section: &str,
) -> UnifiedResult<(Portal, Caller, String)> {
    let caller = caller(req)?;
    let company_id = caller_company(req)?;
    let portal = load_portal_by_slug(state, slug)?;
    if portal.status != PortalStatus::Active {
        return Err(UnifiedError::not_found(format!(
            "Portal not found: {}",
            portal.slug
        )));
    }
    if portal.company_id != company_id {
        return Err(UnifiedError::forbidden(
            "This portal does not belong to the caller's company",
        ));
    }
    if !portal.sections.iter().any(|entry| entry == section) {
        return Err(UnifiedError::forbidden(format!(
            "Section \"{section}\" is not enabled on this portal"
        )));
    }
    Ok((portal, caller, company_id))
}

/// Shared list logic for one `my/*` kind: capability check, portal
/// scoping, then only the caller's own records of the caller's
/// company, newest first, paginated, filterable by status.
fn client_records(
    ctx: &AppContext,
    state: &PortalsState,
    req: &HttpRequest,
    slug: &str,
    kind: &str,
    params: &PaginationParams,
) -> UnifiedResult<Value> {
    let (portal, caller, company_id) = client_portal(state, req, slug, kind)?;
    require_permission(ctx, &caller.role, PERM_PORTALS_VIEW)?;
    let record_kind = section_kind(kind);
    let status_filter = params.filters.get("status").cloned();
    let mut items: Vec<PortalRecord> = state
        .records
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|record| {
            record.kind == record_kind
                && record.company_id == company_id
                && record.user_id == caller.actor
                && match &status_filter {
                    Some(status) => &record.status == status,
                    None => true,
                }
        })
        .collect();
    items.sort_by_key(|record| std::cmp::Reverse(record.created_at));
    let page = paginate(items, params).to_json();
    Ok(json!({
        "portal_id": portal.id.to_string(),
        "slug": portal.slug,
        "kind": kind,
        "records": page,
    }))
}

async fn client_portal_home(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<PortalsState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let slug = path.into_inner();
        let caller = caller(&req)?;
        let company_id = caller_company(&req)?;
        require_permission(&ctx, &caller.role, PERM_PORTALS_VIEW)?;
        let portal = load_portal_by_slug(&state, &slug)?;
        if portal.status != PortalStatus::Active {
            return Err(UnifiedError::not_found(format!(
                "Portal not found: {}",
                portal.slug
            )));
        }
        if portal.company_id != company_id {
            return Err(UnifiedError::forbidden(
                "This portal does not belong to the caller's company",
            ));
        }
        let mut counts: Vec<Value> = Vec::new();
        for section in &portal.sections {
            let record_kind = section_kind(section);
            let count = state
                .records
                .iter()
                .filter(|entry| {
                    entry.value().kind == record_kind
                        && entry.value().company_id == company_id
                        && entry.value().user_id == caller.actor
                })
                .count();
            counts.push(json!({ "section": section, "count": count }));
        }
        let mut view = public_portal_view(&state, &portal, Some(&caller.role));
        if let Value::Object(map) = &mut view {
            map.insert("record_counts".to_string(), Value::Array(counts));
        }
        record_view(&state, &portal, &req);
        Ok(view)
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn client_invoices(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<PortalsState>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let params = query.into_inner();
    let result = client_records(&ctx, &state, &req, &path.into_inner(), "invoices", &params);
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn client_tickets(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<PortalsState>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let params = query.into_inner();
    let result = client_records(&ctx, &state, &req, &path.into_inner(), "tickets", &params);
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn client_downloads(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<PortalsState>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let params = query.into_inner();
    let result = client_records(&ctx, &state, &req, &path.into_inner(), "downloads", &params);
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn client_subscriptions(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<PortalsState>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let params = query.into_inner();
    let result = client_records(&ctx, &state, &req, &path.into_inner(), "subscriptions", &params);
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn client_licenses(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<PortalsState>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let params = query.into_inner();
    let result = client_records(&ctx, &state, &req, &path.into_inner(), "licenses", &params);
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}
