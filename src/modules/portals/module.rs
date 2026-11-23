//! Portals - module struct, route table and registration.
//!
//! Part of the `portals` module split; see `src/modules/portals.rs`.

use unified_core::context::AppContext;
use unified_core::error::UnifiedResult;
use unified_core::hooks::HookBus;
use unified_core::hooks::HookContext;
use unified_core::modules::Module;
use unified_core::modules::ModuleRegistry;
use unified_core::modules::RouteInfo;
use unified_core::modules::RouteVersionSupport;
use crate::modules::ai;
use actix_web::web;
use std::sync::Arc;

use super::constants::DOWNLOAD_FINISHED;
use super::constants::DOWNLOAD_STARTED;
use super::constants::INVOICE_CREATED;
use super::constants::INVOICE_PAID;
use super::constants::INVOICE_VOIDED;
use super::constants::LICENSE_ACTIVATED;
use super::constants::LICENSE_ISSUED;
use super::constants::LICENSE_REVOKED;
use super::constants::LICENSE_SUSPENDED;
use super::constants::ORDER_CANCELLED;
use super::constants::ORDER_COMPLETED;
use super::constants::ORDER_CREATED;
use super::constants::TICKET_CREATED;
use super::constants::TICKET_UPDATED;
use super::handlers::create_portal;
use super::handlers::delete_portal;
use super::handlers::get_portal;
use super::handlers::list_portals;
use super::handlers::portal_analytics;
use super::handlers::update_domain_status;
use super::handlers::update_portal;
use super::menus::create_menu;
use super::menus::delete_menu;
use super::menus::get_menu;
use super::menus::list_menus;
use super::menus::reorder_menus;
use super::menus::update_menu;
use super::projection::on_projection_event;
use super::public::client_downloads;
use super::public::client_invoices;
use super::public::client_licenses;
use super::public::client_portal_home;
use super::public::client_subscriptions;
use super::public::client_tickets;
use super::public::get_public_portal;
use super::state::PortalsState;
use super::ai_capabilities::AI_CAPABILITIES;

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

        ai::register_ai_capabilities("portals", AI_CAPABILITIES);
        *self.state.hooks.write().expect("hooks lock poisoned") = Some(Arc::clone(&ctx.hooks));
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
                .route(
                    "/{id}/domain-status",
                    web::patch().to(update_domain_status),
                )
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
                .route(
                    "/{slug}/my/subscriptions",
                    web::get().to(client_subscriptions),
                )
                .route("/{slug}/my/licenses", web::get().to(client_licenses)),
        );
    }

    fn route_info(&self) -> Vec<RouteInfo> {
        vec![
            RouteInfo {
                method: "GET",
                path: "/api/v1/portals",
                owner: "portals",
                description: "List portals (paginated, filter by company_id, status, search).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/portals",
                owner: "portals",
                description: "Create a branded portal for a company (slug, colours, sections, optional custom domain).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/portals/{id}",
                owner: "portals",
                description: "Get one portal.",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "PATCH",
                path: "/api/v1/portals/{id}",
                owner: "portals",
                description: "Update a portal (branding, sections, status, custom domain).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "PATCH",
                path: "/api/v1/portals/{id}/domain-status",
                owner: "portals",
                description: "Write back a custom-domain verification outcome (verified or rejected) for a portal whose domain_status is requested; requires portals.manage.",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "DELETE",
                path: "/api/v1/portals/{id}",
                owner: "portals",
                description: "Delete a portal with its menus and page views (projection rows belong to the owning modules and are kept).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/portals/{id}/analytics",
                owner: "portals",
                description: "Portal page-view analytics (paginated views plus totals).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/portals/{id}/menus",
                owner: "portals",
                description: "List menu entries of a portal (position order).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/portals/{id}/menus",
                owner: "portals",
                description: "Create a menu entry (section target or URL target, role visibility).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/portals/{id}/menus/{menu_id}",
                owner: "portals",
                description: "Get one menu entry.",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "PATCH",
                path: "/api/v1/portals/{id}/menus/{menu_id}",
                owner: "portals",
                description: "Update a menu entry.",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "DELETE",
                path: "/api/v1/portals/{id}/menus/{menu_id}",
                owner: "portals",
                description: "Delete a menu entry.",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/portals/{id}/menus/reorder",
                owner: "portals",
                description: "Reorder menu entries by id list.",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/portals/public/{slug}",
                owner: "portals",
                description: "PUBLIC: one portal's branding, visible sections, and menus; records a page view (no auth; gated by the API Manager ApiAccessRule).",
                auth: unified_core::auth::RouteAuth::Public,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/portals/{slug}/my",
                owner: "portals",
                description: "Client portal home: branding plus the caller's per-section record counts (scoped to the caller's company and user id).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/portals/{slug}/my/invoices",
                owner: "portals",
                description: "Client invoices: only the caller's own invoices of the caller's company (paginated, filter by status).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/portals/{slug}/my/tickets",
                owner: "portals",
                description: "Client tickets: only the caller's own tickets of the caller's company (paginated, filter by status).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/portals/{slug}/my/downloads",
                owner: "portals",
                description: "Client downloads: only the caller's own downloads of the caller's company (paginated, filter by status).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/portals/{slug}/my/subscriptions",
                owner: "portals",
                description: "Client subscriptions: only the caller's own subscriptions of the caller's company (paginated, filter by status).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/portals/{slug}/my/licenses",
                owner: "portals",
                description: "Client licenses: only the caller's own licenses of the caller's company (paginated, filter by status).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
        ]
    }

    fn route_version_support(&self) -> RouteVersionSupport {
        RouteVersionSupport::All
    }
}

pub fn register(registry: &ModuleRegistry) {
    registry.register(Arc::new(PortalsModule::new()));
}
