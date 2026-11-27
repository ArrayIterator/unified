//! Dashboards module - Dashboards module - separate admin, user (client), and reseller dashboards.
//!
//! Split into submodules by concern; the public API is
//! re-exported below. The full route map and module notes
//! live in `dashboards/module.rs`.

pub mod types;
mod state;
mod auth;
mod service;
mod handlers_admin;
mod handlers_user;
mod handlers_reseller;
mod middleware;
pub mod module;

pub use self::types::{CAP_ADMIN_ACCESS, CAP_APIKEYS_READ, CAP_LICENSES_READ, CAP_LOGS_VIEW,
    CAP_MODULES_MANAGE, CAP_ORDERS_MANAGE, CAP_ORDERS_READ, CAP_RESELLER_ACCESS,
    CAP_RESELLER_CLIENTS_MANAGE, CAP_RESELLER_CLIENTS_VIEW, CAP_RESELLER_PAYOUTS_VIEW,
    CAP_SCHOOL_FEES_VIEW, CAP_SECURITY_VIEW, CAP_SERVERS_MANAGE, CAP_SERVICES_READ,
    CAP_SETTINGS_MANAGE, CAP_SITE_BUILDER_MANAGE, CAP_EMAIL_TEMPLATES_MANAGE, CAP_THEME_MANAGE,
    CAP_TICKETS_READ, CAP_USERS_MANAGE, CAP_USER_ACCESS, CAP_VIEW_AS_USER, DEFAULT_USER_PATH,
    EVENT_RESELLER_CLIENT_LINKED, EVENT_RESELLER_CLIENT_UNLINKED, EVENT_VIEW_AS_ENDED,
    EVENT_VIEW_AS_EXPIRED, MAX_VIEW_AS_TTL_MINUTES};
pub(crate) use self::types::{ALL_CAPABILITIES, CallerContext, CallerIdentity, DEFAULT_ADMIN_PATH,
    DEFAULT_VIEW_AS_TTL_MINUTES, DownloadRow, EVENT_CAPABILITIES_REQUESTED, EVENT_SECTION_REQUESTED,
    EVENT_VIEW_AS_STARTED, HmacSha256, JwtClaims, LinkClientInput, MIN_VIEW_AS_TTL_MINUTES,
    NAV_DEFS, NavDef, NavItem, NavQuery, OrderRow, ResellerLink, StartViewAsInput,
    UpdateDashboardSettingsInput, UserResponseView, ViewAsSession};
pub(crate) use self::state::{DashboardsState};
pub(crate) use self::auth::{authenticate, cookie_value, extract_token, has_capability,
    identity_from_extensions, nav_items, require_capability, require_capability_write,
    require_identity, require_real_capability, require_real_capability_write, verify_jwt};
pub(crate) use self::service::{admin_overview_data, dashboard_settings_json, db_pool,
    effective_scope, expand_wildcard, get_scope, hook_section, hook_section_entry,
    linked_client_ids, overview_tenant_scope, page_data, paginate_vec, render_page_response,
    reseller_overview_data, resolve_capabilities, run_blocking, section_flags,
    user_overview_data};
pub(crate) use self::handlers_admin::{admin_page, admin_page_inner, admin_page_root,
    get_admin_modules, get_admin_overview, get_admin_users, get_nav};
pub(crate) use self::handlers_user::{get_user_downloads, get_user_licenses, get_user_orders,
    get_user_overview, user_page, user_page_inner, user_page_root};
pub(crate) use self::handlers_reseller::{get_dashboard_settings, get_reseller_clients,
    get_reseller_overview, get_view_as, link_reseller_client, start_view_as, stop_view_as,
    unlink_reseller_client, update_dashboard_settings};
pub(crate) use self::middleware::{DashboardsAuthMiddleware};
pub use self::module::{DashboardsModule, register};
