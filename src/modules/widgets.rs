//! Widgets module — a standalone core module for dashboard widgets.
//!
//! The catalogue and the per-zone layouts live here. Dashboards
//! (admin, user, tenant, reseller) CONSUME this module through its
//! API and hooks; they own no widget logic of their own. The widgets
//! module is for admin pages only: it does not touch the site
//! builder, its blocks, or the front page.
//!
//! A zone is the kebab-case id of a widgetized page. Any
//! dashboard/admin page can declare its own zone (see `module.rs`
//! for how a page opts in) — zones are not registered here. Layouts
//! are keyed per (zone, scope, owner): every user has their OWN
//! layout per zone, falling back to the zone's shared default, then
//! to the built-in default.
//!
//! Split into submodules by concern; the public API is re-exported
//! below. The full route map and module notes live in
//! `widgets/module.rs`.

pub mod types;
mod helpers;
mod registry;
mod service;
mod validation;
mod handlers;
pub mod module;

pub use self::types::{CAP_WIDGETS_MANAGE, CAP_WIDGETS_VIEW, CONTEXT_ADMIN_DASHBOARD,
    CONTEXT_RESELLER_DASHBOARD, CONTEXT_TENANT_DASHBOARD, CONTEXT_USER_DASHBOARD,
    EVENT_LAYOUT_CHANGED, EVENT_WIDGET_TYPES, MAX_LAYOUT_ITEMS, OWNER_DEFAULT, SCOPE_ID_PLATFORM,
    SCOPE_PLATFORM, SCOPE_TENANT, SaveLayoutInput, WidgetDefinition, WidgetItem, WidgetLayout};
pub(crate) use self::helpers::{caller, caller_company, check_capability,
    require_caller_company, require_permission, require_tenant, Caller};
pub(crate) use self::registry::collect_widget_types;
pub(crate) use self::service::{default_layout, load_layout, reset_layout, save_layout};
pub(crate) use self::validation::{validate_layout_items, validate_zone};
pub use self::module::{WidgetsModule, register};
