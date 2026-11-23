// Dashboards - Navigation, capability, identity and row types.

use unified_core::db::models::{StoreOrder, User};
use unified_core::db::money::{minor_scale, pg_numeric_to_minor};
use chrono::{DateTime, Utc};
use diesel::data_types::PgNumeric;
use diesel::prelude::*;
use hmac::Hmac;
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use std::collections::HashSet;
use uuid::Uuid;

pub(crate) type HmacSha256 = Hmac<Sha256>;

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

/// Read access to school fees (SPP). Mirrors the billing module's
/// `PERM_SCHOOL_FEES_VIEW` permission, so roles that can read school
/// fees resolve this capability through `resolve_capabilities`.
pub const CAP_SCHOOL_FEES_VIEW: &str = "school_fees.view";

pub const CAP_TICKETS_READ: &str = "tickets:read";

pub const CAP_SERVICES_READ: &str = "services:read";

pub const CAP_APIKEYS_READ: &str = "apikeys:read";

/// Manage the site builder (pages, blocks, sections). Mirrors the
/// site builder module's `site-builder.manage` permission.
pub const CAP_SITE_BUILDER_MANAGE: &str = "site-builder.manage";

/// Manage email templates. Mirrors the email templates module's
/// `email_templates:manage` permission.
pub const CAP_EMAIL_TEMPLATES_MANAGE: &str = "email_templates:manage";

/// Manage themes. Mirrors the theme module's `theme:manage`
/// permission.
pub const CAP_THEME_MANAGE: &str = "theme:manage";

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
    CAP_SCHOOL_FEES_VIEW,
    CAP_TICKETS_READ,
    CAP_SERVICES_READ,
    CAP_APIKEYS_READ,
    CAP_SITE_BUILDER_MANAGE,
    CAP_EMAIL_TEMPLATES_MANAGE,
    CAP_THEME_MANAGE,
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

pub(crate) struct NavDef {
    pub(crate) id: &'static str,
    pub(crate) label: &'static str,
    pub(crate) href: &'static str,
    pub(crate) icon: &'static str,
    pub(crate) capability: &'static str,
    pub(crate) section: &'static str,
    pub(crate) scope: &'static str,
}

/// The static navigation table. Items are filtered by scope and by the
/// caller's capabilities; module entries are appended from the module
/// registry for the admin scope (see `nav_items`).
pub(crate) const NAV_DEFS: &[NavDef] = &[
    // Admin scope.
    NavDef { id: "admin.overview", label: "Overview", href: "/{admin}", icon: "icon-grid", capability: CAP_ADMIN_ACCESS, section: "overview", scope: "admin" },
    NavDef { id: "admin.modules", label: "Modules", href: "/{admin}/modules", icon: "icon-layers", capability: CAP_MODULES_MANAGE, section: "manage", scope: "admin" },
    NavDef { id: "admin.users", label: "Users & Roles", href: "/{admin}/users", icon: "icon-users", capability: CAP_USERS_MANAGE, section: "manage", scope: "admin" },
    NavDef { id: "admin.orders", label: "Orders & Invoices", href: "/{admin}/orders", icon: "icon-file", capability: CAP_ORDERS_MANAGE, section: "commerce", scope: "admin" },
    NavDef { id: "admin.school-fees", label: "School Fees", href: "/{admin}/school-fees", icon: "icon-file", capability: CAP_SCHOOL_FEES_VIEW, section: "commerce", scope: "admin" },
    NavDef { id: "admin.servers", label: "Servers & Clusters", href: "/{admin}/servers", icon: "icon-server", capability: CAP_SERVERS_MANAGE, section: "operations", scope: "admin" },
    NavDef { id: "admin.security", label: "Security", href: "/{admin}/security", icon: "icon-alert", capability: CAP_SECURITY_VIEW, section: "operations", scope: "admin" },
    NavDef { id: "admin.logs", label: "Logs", href: "/{admin}/logs", icon: "icon-activity", capability: CAP_LOGS_VIEW, section: "operations", scope: "admin" },
    NavDef { id: "admin.settings", label: "Settings", href: "/{admin}/settings", icon: "icon-settings", capability: CAP_SETTINGS_MANAGE, section: "manage", scope: "admin" },
    NavDef { id: "admin.site-builder", label: "Site Builder", href: "/{admin}/site-builder", icon: "icon-layers", capability: CAP_SITE_BUILDER_MANAGE, section: "manage", scope: "admin" },
    NavDef { id: "admin.email-templates", label: "Email Templates", href: "/{admin}/email-templates", icon: "icon-file", capability: CAP_EMAIL_TEMPLATES_MANAGE, section: "manage", scope: "admin" },
    NavDef { id: "admin.email-builder", label: "Email Builder", href: "/assets/email-builder.html", icon: "icon-command", capability: CAP_EMAIL_TEMPLATES_MANAGE, section: "manage", scope: "admin" },
    NavDef { id: "admin.themes", label: "Themes", href: "/{admin}/themes", icon: "icon-grid", capability: CAP_THEME_MANAGE, section: "manage", scope: "admin" },
    // User scope.
    NavDef { id: "user.overview", label: "Overview", href: "/{user}", icon: "icon-grid", capability: CAP_USER_ACCESS, section: "overview", scope: "user" },
    NavDef { id: "user.licenses", label: "My Licenses", href: "/{user}/licenses", icon: "icon-check-circle", capability: CAP_LICENSES_READ, section: "commerce", scope: "user" },
    NavDef { id: "user.downloads", label: "My Downloads", href: "/{user}/downloads", icon: "icon-package", capability: CAP_LICENSES_READ, section: "commerce", scope: "user" },
    NavDef { id: "user.orders", label: "My Orders", href: "/{user}/orders", icon: "icon-file", capability: CAP_ORDERS_READ, section: "commerce", scope: "user" },
    NavDef { id: "user.school-fees", label: "My School Fees", href: "/{user}/school-fees", icon: "icon-file", capability: CAP_SCHOOL_FEES_VIEW, section: "commerce", scope: "user" },
    NavDef { id: "user.tickets", label: "My Tickets", href: "/{user}/tickets", icon: "icon-bell", capability: CAP_TICKETS_READ, section: "support", scope: "user" },
    NavDef { id: "user.services", label: "My Services", href: "/{user}/services", icon: "icon-server", capability: CAP_SERVICES_READ, section: "operations", scope: "user" },
    NavDef { id: "user.apikeys", label: "API Keys", href: "/{user}/apikeys", icon: "icon-command", capability: CAP_APIKEYS_READ, section: "account", scope: "user" },
    NavDef { id: "user.profile", label: "Profile", href: "/{user}/profile", icon: "icon-users", capability: CAP_USER_ACCESS, section: "account", scope: "user" },
    // Reseller scope (rendered inside the user layout, never the admin one).
    NavDef { id: "reseller.clients", label: "My Clients", href: "/{user}/reseller/clients", icon: "icon-users", capability: CAP_RESELLER_CLIENTS_VIEW, section: "reseller", scope: "reseller" },
    NavDef { id: "reseller.payouts", label: "Commissions & Payouts", href: "/{user}/reseller/payouts", icon: "icon-activity", capability: CAP_RESELLER_PAYOUTS_VIEW, section: "reseller", scope: "reseller" },
];

// ---------------------------------------------------------------------------
// Identity and authentication
// ---------------------------------------------------------------------------

/// Claims of an Auth Manager JWT (`POST /api/v1/auth/token`).
#[derive(Debug, Clone, Deserialize)]
pub(crate) struct JwtClaims {
    pub(crate) sub: String,
    #[allow(dead_code)]
    pub(crate) iss: String,
    #[allow(dead_code)]
    iat: i64,
    pub(crate) exp: i64,
    #[allow(dead_code)]
    jti: String,
    #[serde(default)]
    pub(crate) scopes: Vec<String>,
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
    pub(crate) fn direct(user_id: Uuid, token_scopes: Vec<String>) -> Self {
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
pub(crate) struct CallerContext {
    pub(crate) identity: CallerIdentity,
    pub(crate) capabilities: HashSet<String>,
    pub(crate) profile: Option<User>,
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

/// One row of `store_orders`, as returned to the API: money in major
/// units as `f64`, converted from the `PgNumeric` columns of the
/// shared [`StoreOrder`] model (which cannot be serialized directly).
#[derive(Debug, Clone, Serialize)]
pub(crate) struct OrderRow {
    pub(crate) id: Uuid,
    pub(crate) user_id: String,
    subtotal: f64,
    discount_total: f64,
    total: f64,
    currency: String,
    coupon_code: Option<String>,
    pub(crate) company_id: Option<Uuid>,
    trial_until: Option<DateTime<Utc>>,
    status: String,
    provider: Option<String>,
    reference: String,
    pub(crate) created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

/// `PgNumeric` money to major units as `f64`, via the currency's
/// minor units (see `unified_core::db::money`); unrepresentable
/// values (NaN) read as zero.
fn pg_numeric_to_major(value: &PgNumeric, currency: &str) -> f64 {
    match pg_numeric_to_minor(value, currency) {
        Some(minor) => minor as f64 / 10f64.powi(i32::from(minor_scale(currency))),
        None => 0.0,
    }
}

impl From<StoreOrder> for OrderRow {
    fn from(order: StoreOrder) -> Self {
        Self {
            subtotal: pg_numeric_to_major(&order.subtotal, &order.currency),
            discount_total: pg_numeric_to_major(&order.discount_total, &order.currency),
            total: pg_numeric_to_major(&order.total, &order.currency),
            id: order.id,
            user_id: order.user_id,
            currency: order.currency,
            coupon_code: order.coupon_code,
            company_id: order.company_id,
            trial_until: order.trial_until,
            status: order.status,
            provider: order.provider,
            reference: order.reference,
            created_at: order.created_at,
            updated_at: order.updated_at,
        }
    }
}

/// One row of `store_downloads` (there is no shared model for it).
#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = unified_core::db::schema::store_downloads)]
pub(crate) struct DownloadRow {
    pub(crate) id: Uuid,
    license_id: Option<Uuid>,
    release_id: Option<Uuid>,
    software_slug: Option<String>,
    order_id: Option<Uuid>,
    downloaded_at: DateTime<Utc>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct NavQuery {
    #[serde(default)]
    pub(crate) scope: Option<String>,
    #[serde(default)]
    pub(crate) current: Option<String>,
}

/// Public user view: the `User` model already skips the password hash
/// and TOTP secret on serialization, but the shape is restated here
/// so the admin list never leaks a field added to the model later.
#[derive(Debug, Clone, Serialize)]
pub(crate) struct UserResponseView {
    pub(crate) id: Uuid,
    pub(crate) email: String,
    pub(crate) name: String,
    pub(crate) is_active: bool,
    pub(crate) company_id: Option<Uuid>,
    pub(crate) created_at: DateTime<Utc>,
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

#[derive(Debug, Deserialize)]
pub(crate) struct LinkClientInput {
    pub(crate) client_id: String,
}

// ---------------------------------------------------------------------------
// Admin view-as handlers
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub(crate) struct StartViewAsInput {
    pub(crate) target_user_id: String,
    pub(crate) reason: String,
}

#[derive(Debug, Deserialize)]
pub(crate) struct UpdateDashboardSettingsInput {
    #[serde(default)]
    pub(crate) view_as_ttl_minutes: Option<i64>,
}


#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_capabilities_contains_school_fees_view() {
        assert!(ALL_CAPABILITIES.contains(&CAP_SCHOOL_FEES_VIEW));
    }

    #[test]
    fn nav_defs_include_school_fees_for_admin_and_user() {
        let admin = NAV_DEFS
            .iter()
            .find(|def| def.id == "admin.school-fees")
            .expect("admin school-fees nav entry missing");
        assert_eq!(admin.capability, CAP_SCHOOL_FEES_VIEW);
        assert_eq!(admin.href, "/{admin}/school-fees");
        let user = NAV_DEFS
            .iter()
            .find(|def| def.id == "user.school-fees")
            .expect("user school-fees nav entry missing");
        assert_eq!(user.capability, CAP_SCHOOL_FEES_VIEW);
        assert_eq!(user.href, "/{user}/school-fees");
    }

    #[test]
    fn nav_defs_have_unique_ids() {
        let mut ids: Vec<&str> = NAV_DEFS.iter().map(|def| def.id).collect();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), NAV_DEFS.len());
    }

    #[test]
    fn new_admin_surfaces_have_nav_entries_and_capabilities() {
        // Every admin page added by the frontend work must be
        // reachable from the admin nav, with the capability its page
        // handler enforces.
        for (id, href, capability) in [
            ("admin.site-builder", "/{admin}/site-builder", CAP_SITE_BUILDER_MANAGE),
            ("admin.email-templates", "/{admin}/email-templates", CAP_EMAIL_TEMPLATES_MANAGE),
            ("admin.email-builder", "/assets/email-builder.html", CAP_EMAIL_TEMPLATES_MANAGE),
            ("admin.themes", "/{admin}/themes", CAP_THEME_MANAGE),
        ] {
            let def = NAV_DEFS
                .iter()
                .find(|def| def.id == id)
                .unwrap_or_else(|| panic!("nav entry {id} missing"));
            assert_eq!(def.href, href);
            assert_eq!(def.capability, capability);
            assert_eq!(def.scope, "admin");
            assert!(ALL_CAPABILITIES.contains(&capability));
        }
    }

    #[test]
    fn all_capabilities_are_unique() {
        let mut caps = ALL_CAPABILITIES.to_vec();
        caps.sort_unstable();
        caps.dedup();
        assert_eq!(caps.len(), ALL_CAPABILITIES.len());
    }
}
