// Widgets - module definition: routes and registration.
//!
//! The widgets module is a STANDALONE CORE module: it owns the
//! widget catalogue and the per-zone layouts. Dashboards consume
//! it through the API below and the `widgets.widget_types` hook; they
//! own no widget logic.
//!
//! A zone is the kebab-case id of a widgetized page. Zones are NOT
//! registered in this module: any dashboard/admin page opts in by
//! declaring its zone, and the layout API accepts any well-formed
//! zone id. The front page is NOT a zone (it is site-builder
//! output). Seeded zones (the first widgetized pages):
//! `admin-dashboard`, `tenant-dashboard`, `user-dashboard`,
//! `reseller-dashboard`.
//!
//! How a page opts in:
//!
//!   React - the page calls `fetchWidgetLayout("<zone>")` and orders
//!     its sections with `orderWidgetSections(sections, layout)`
//!     (see `assets/react/src/api.ts`; `DashboardPage` and
//!     `AdminPage` are the working examples). A failed layout load
//!     keeps the page's fixed section order. Saving is `PUT
//!     /api/v1/widgets/layouts/<zone>` with `{ items, shared? }`.
//!   Handlebars - the template marks the zone container with
//!     `data-widget-zone="<zone>"` and each widget section with
//!     `data-widget="<id>"` (the id is a catalogue id from `GET
//!     /api/v1/widgets/types?context=<zone>`), and the page script
//!     loads the layout for the zone, applies the order, and offers
//!     pin / add / remove / drag-to-reorder (Pointer Events only).
//! The demo (`demo/dashboard.html` + the "Widgets"
//!     section of `demo/assets/demo.js`) is the reference client
//!     for this shape; the app's own `assets/js/dashboard.ts` does
//!     not have a widget client yet, so a Handlebars page opts in
//!     by copying that reference shape.
//!
//! Layouts are keyed per (zone, scope, user). By default every user
//! with access to a widgetized page has their OWN layout for that
//! zone (pin / add / remove / order). Load order: the user's own
//! row, then the zone's SHARED (team) default row, then the
//! built-in default. The shared default is written with
//! `"shared": true` in the PUT body and deleted with
//! `DELETE ...?shared=true`; it is only a starting fallback, never
//! the live layout.
//!
//! Scope: the `tenant-dashboard` zone is tenant-scoped (fail closed
//! without a resolved tenant + company); every other zone is
//! platform scope. A zone's scope is therefore fixed by its id
//! today; a tenant page that needs a tenant-scoped SHARED default
//! for its own zone is a follow-up (per-user layouts are safe
//! either way: the owner is always the user).
//!
//! Routes (all authenticated; see `handlers.rs` for the capability
//! and scoping rules):
//!
//!   GET    /api/v1/widgets/types[?context=...]
//!   GET    /api/v1/widgets/layouts/{context}
//!   PUT    /api/v1/widgets/layouts/{context}
//!   DELETE /api/v1/widgets/layouts/{context}
//!
//! Hooks:
//!
//!   Emitted:
//!     `widgets.widget_types`  - collect widget definitions from
//!       other modules (each result is one definition object, or an
//!       array of them).
//!     `widgets.layout_changed` - after a layout is saved or reset.

use unified_core::modules::{Module, ModuleRegistry, RouteInfo};
use actix_web::web;
use std::sync::Arc;

use super::handlers::*;
use super::types::*;

pub struct WidgetsModule;

pub fn register(registry: &ModuleRegistry) {
    registry.register(Arc::new(WidgetsModule));
}

impl Module for WidgetsModule {
    fn id(&self) -> &'static str {
        "widgets"
    }

    fn name(&self) -> &'static str {
        "Widgets"
    }

    fn description(&self) -> &'static str {
        "Dashboard widgets: a hookable catalogue and per-context DB-backed layouts (pin, add, remove, reorder)."
    }

    fn dependencies(&self) -> Vec<&'static str> {
        // Standalone core: no other module is needed.
        vec![]
    }

    fn route_info(&self) -> Vec<RouteInfo> {
        vec![
            RouteInfo {
                method: "GET",
                path: "/api/v1/widgets/types",
                owner: "widgets",
                description: "List widget types (requires widgets.view).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/widgets/layouts/{context}",
                owner: "widgets",
                description: "Get layout for a zone: own row, then shared default, then built-in (requires widgets.view).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "PUT",
                path: "/api/v1/widgets/layouts/{context}",
                owner: "widgets",
                description: "Save layout for a zone - own layout, or the shared default with \"shared\": true (requires widgets.manage).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "DELETE",
                path: "/api/v1/widgets/layouts/{context}",
                owner: "widgets",
                description: "Reset layout for a zone - deletes the caller's row (or the shared default with ?shared=true) (requires widgets.manage).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
        ]
    }

    fn configure_routes(&self, cfg: &mut web::ServiceConfig) {
        cfg.service(
            web::scope("/api/v1/widgets")
                .route("/types", web::get().to(list_widget_types))
                .route("/layouts/{context}", web::get().to(get_layout))
                .route("/layouts/{context}", web::put().to(save_layout))
                .route("/layouts/{context}", web::delete().to(reset_layout_handler)),
        );
    }
}
