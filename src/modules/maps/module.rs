// Map & Tagging - module definition: routes and registration.
//!
//! The maps module is a CORE module and the single shared
//! geospatial foundation: it is always active (`is_core`
//! returns true, so the extensions system refuses to disable
//! it) and it is not sold as a product
//! (`available_as_product` is false). Every other module that
//! needs a location consumes this one through its public
//! contract (`store::attach_location`,
//! `store::locations_for_entity`, `store::nearby`,
//! `store::within_geofence`, `store::record_position`) or its
//! HTTP API, and declares the dependency with
//! `requires() -> &["maps"]` (see docs/integration/MAP_TAGGING.md
//! for the dependency contract: enabling a consumer
//! auto-activates this module, and disabling it is blocked
//! while an enabled consumer still needs it - on top of the
//! core rule that already forbids disabling it at all).
//!
//! Routes (all authenticated; see `handlers.rs` for the C3
//! guard order - tenant first, then caller, then capability):
//!
//!   Locations:
//!     GET    /api/v1/maps/locations[?kind&layer_id&page&per_page]
//!     POST   /api/v1/maps/locations
//!     GET    /api/v1/maps/locations/{id}
//!     PUT    /api/v1/maps/locations/{id}
//!     DELETE /api/v1/maps/locations/{id}
//!     GET    /api/v1/maps/nearby?latitude&longitude&radius_km&kind
//!   Links (the generic join - any module's records):
//!     POST   /api/v1/maps/links
//!     DELETE /api/v1/maps/links
//!     GET    /api/v1/maps/entities/{entity_type}/{entity_id}/locations
//!   Layers:
//!     GET    /api/v1/maps/layers
//!     POST   /api/v1/maps/layers
//!     PUT    /api/v1/maps/layers/{id}
//!     DELETE /api/v1/maps/layers/{id}
//!   Geofences:
//!     GET    /api/v1/maps/geofences
//!     POST   /api/v1/maps/geofences
//!     GET    /api/v1/maps/geofences/{id}
//!     PUT    /api/v1/maps/geofences/{id}
//!     DELETE /api/v1/maps/geofences/{id}
//!     POST   /api/v1/maps/geofences/{id}/contains
//!   Positions (tracking history):
//!     POST   /api/v1/maps/positions
//!     GET    /api/v1/maps/positions/{entity_type}/{entity_id}[?from&to]
//!   Definitions:
//!     GET    /api/v1/maps/kinds
//!     POST   /api/v1/maps/kinds
//!     PUT    /api/v1/maps/kinds/{id}
//!     DELETE /api/v1/maps/kinds/{id}
//!     GET    /api/v1/maps/roles
//!     POST   /api/v1/maps/roles
//!     PUT    /api/v1/maps/roles/{id}
//!     DELETE /api/v1/maps/roles/{id}
//!   Providers (registry) and settings:
//!     GET    /api/v1/maps/providers
//!     POST   /api/v1/maps/providers
//!     PUT    /api/v1/maps/providers/{id}
//!     DELETE /api/v1/maps/providers/{id}
//!     GET    /api/v1/maps/settings
//!     PUT    /api/v1/maps/settings
//!
//! Hooks emitted (fire-and-forget; other modules react without
//! calling this module):
//!
//!   `maps.location_created` / `maps.location_updated` /
//!   `maps.location_deleted` - a pin changed.
//!   `maps.location_attached` / `maps.location_detached` - a
//!     location was attached to / detached from a record.
//!   `maps.position_recorded` - a GPS position was stored.
//!   `maps.geofence_entered` / `maps.geofence_exited` - an
//!     entity crossed a zone boundary between its previous and
//!     its new position.

use actix_web::web;
use std::sync::Arc;
use unified_core::context::AppContext;
use unified_core::error::UnifiedResult;
use unified_core::modules::{Module, ModuleRegistry, RouteInfo};

use super::crypto::set_encryption_secret;
use super::handlers::*;
use super::types::MODULE_ID;

pub struct MapsModule;

pub fn register(registry: &ModuleRegistry) {
    registry.register(Arc::new(MapsModule));
}

impl Module for MapsModule {
    fn id(&self) -> &'static str {
        MODULE_ID
    }

    fn name(&self) -> &'static str {
        "Map & Tagging"
    }

    fn description(&self) -> &'static str {
        "Shared geospatial foundation: locations, generic location links for any module's records, layers, geofences, GPS tracking history, and a configurable tile-provider registry (OpenStreetMap default, Mapbox, Google Maps, or any custom provider)."
    }

    /// Core module, always active: the extensions system refuses
    /// to disable a core module, which is exactly what a shared
    /// foundation needs. Stated explicitly even though the trait
    /// defaults to true, so the decision is visible here and
    /// cannot drift silently.
    fn is_core(&self) -> bool {
        true
    }

    /// Not sold as a product: every tenant gets the shared
    /// foundation; it never appears in the module offerings.
    fn available_as_product(&self) -> bool {
        false
    }

    fn dependencies(&self) -> Vec<&'static str> {
        // Standalone core: no other module is needed.
        vec![]
    }

    fn init(&self, ctx: &AppContext) -> UnifiedResult<()> {
        // Capture app.secret_key for provider API key encryption
        // (crypto.rs): no secret at boot means None, not a fallback
        // - storing a provider key is refused and resolving a
        // provider that needs one fails closed.
        let secret = ctx
            .config
            .read()
            .expect("config lock poisoned")
            .app
            .secret_key
            .clone();
        set_encryption_secret(&secret);
        Ok(())
    }

    fn route_info(&self) -> Vec<RouteInfo> {
        vec![
            RouteInfo {
                method: "GET",
                path: "/api/v1/maps/locations",
                owner: "maps",
                description: "List locations (requires maps.view).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/maps/locations",
                owner: "maps",
                description: "Create a location (requires maps.manage).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/maps/locations/{id}",
                owner: "maps",
                description: "Get a location (requires maps.view).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "PUT",
                path: "/api/v1/maps/locations/{id}",
                owner: "maps",
                description: "Update a location (requires maps.manage).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "DELETE",
                path: "/api/v1/maps/locations/{id}",
                owner: "maps",
                description: "Delete a location (requires maps.manage).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/maps/nearby",
                owner: "maps",
                description: "Nearby search (requires maps.view).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/maps/links",
                owner: "maps",
                description: "Attach a location to a record (requires maps.manage).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "DELETE",
                path: "/api/v1/maps/links",
                owner: "maps",
                description: "Detach a location from a record (requires maps.manage).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/maps/entities/{entity_type}/{entity_id}/locations",
                owner: "maps",
                description: "Locations attached to a record (requires maps.view).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/maps/layers",
                owner: "maps",
                description: "List layers (requires maps.view).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/maps/layers",
                owner: "maps",
                description: "Create a layer (requires maps.manage).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "PUT",
                path: "/api/v1/maps/layers/{id}",
                owner: "maps",
                description: "Update a layer (requires maps.manage).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "DELETE",
                path: "/api/v1/maps/layers/{id}",
                owner: "maps",
                description: "Delete a layer (requires maps.manage).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/maps/geofences",
                owner: "maps",
                description: "List geofences (requires maps.view).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/maps/geofences",
                owner: "maps",
                description: "Create a geofence (requires maps.manage).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/maps/geofences/{id}",
                owner: "maps",
                description: "Get a geofence (requires maps.view).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "PUT",
                path: "/api/v1/maps/geofences/{id}",
                owner: "maps",
                description: "Update a geofence (requires maps.manage).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "DELETE",
                path: "/api/v1/maps/geofences/{id}",
                owner: "maps",
                description: "Delete a geofence (requires maps.manage).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/maps/geofences/{id}/contains",
                owner: "maps",
                description: "Test a point against a geofence (requires maps.view).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/maps/positions",
                owner: "maps",
                description: "Record a GPS position (requires maps.track).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/maps/positions/{entity_type}/{entity_id}",
                owner: "maps",
                description: "Position history for an entity (requires maps.view).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/maps/kinds",
                owner: "maps",
                description: "List location kinds (requires maps.view).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/maps/kinds",
                owner: "maps",
                description: "Create a location kind (requires maps.manage).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "PUT",
                path: "/api/v1/maps/kinds/{id}",
                owner: "maps",
                description: "Update a location kind (requires maps.manage).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "DELETE",
                path: "/api/v1/maps/kinds/{id}",
                owner: "maps",
                description: "Delete a location kind (requires maps.manage).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/maps/roles",
                owner: "maps",
                description: "List link roles (requires maps.view).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/maps/roles",
                owner: "maps",
                description: "Create a link role (requires maps.manage).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "PUT",
                path: "/api/v1/maps/roles/{id}",
                owner: "maps",
                description: "Update a link role (requires maps.manage).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "DELETE",
                path: "/api/v1/maps/roles/{id}",
                owner: "maps",
                description: "Delete a link role (requires maps.manage).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/maps/providers",
                owner: "maps",
                description: "List tile providers (requires maps.view).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/maps/providers",
                owner: "maps",
                description: "Create a tile provider (requires maps.manage).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "PUT",
                path: "/api/v1/maps/providers/{id}",
                owner: "maps",
                description: "Update a tile provider (requires maps.manage).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "DELETE",
                path: "/api/v1/maps/providers/{id}",
                owner: "maps",
                description: "Delete a tile provider (requires maps.manage).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/maps/settings",
                owner: "maps",
                description: "Resolved map settings (requires maps.view).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "PUT",
                path: "/api/v1/maps/settings",
                owner: "maps",
                description: "Save map settings (requires maps.manage).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
        ]
    }

    fn configure_routes(&self, cfg: &mut web::ServiceConfig) {
        cfg.service(
            web::scope("/api/v1/maps")
                .route("/locations", web::get().to(list_locations_handler))
                .route("/locations", web::post().to(create_location_handler))
                .route("/locations/{id}", web::get().to(get_location_handler))
                .route("/locations/{id}", web::put().to(update_location_handler))
                .route("/locations/{id}", web::delete().to(delete_location_handler))
                .route("/nearby", web::get().to(nearby_handler))
                .route("/links", web::post().to(attach_link_handler))
                .route("/links", web::delete().to(detach_link_handler))
                .route(
                    "/entities/{entity_type}/{entity_id}/locations",
                    web::get().to(entity_locations_handler),
                )
                .route("/layers", web::get().to(list_layers_handler))
                .route("/layers", web::post().to(create_layer_handler))
                .route("/layers/{id}", web::put().to(update_layer_handler))
                .route("/layers/{id}", web::delete().to(delete_layer_handler))
                .route("/geofences", web::get().to(list_geofences_handler))
                .route("/geofences", web::post().to(create_geofence_handler))
                .route("/geofences/{id}", web::get().to(get_geofence_handler))
                .route("/geofences/{id}", web::put().to(update_geofence_handler))
                .route("/geofences/{id}", web::delete().to(delete_geofence_handler))
                .route("/geofences/{id}/contains", web::post().to(contains_handler))
                .route("/positions", web::post().to(record_position_handler))
                .route(
                    "/positions/{entity_type}/{entity_id}",
                    web::get().to(list_positions_handler),
                )
                .route("/kinds", web::get().to(list_kinds_handler))
                .route("/kinds", web::post().to(create_kind_handler))
                .route("/kinds/{id}", web::put().to(update_kind_handler))
                .route("/kinds/{id}", web::delete().to(delete_kind_handler))
                .route("/roles", web::get().to(list_roles_handler))
                .route("/roles", web::post().to(create_role_handler))
                .route("/roles/{id}", web::put().to(update_role_handler))
                .route("/roles/{id}", web::delete().to(delete_role_handler))
                .route("/providers", web::get().to(list_providers_handler))
                .route("/providers", web::post().to(create_provider_handler))
                .route("/providers/{id}", web::put().to(update_provider_handler))
                .route("/providers/{id}", web::delete().to(delete_provider_handler))
                .route("/settings", web::get().to(get_settings_handler))
                .route("/settings", web::put().to(save_settings_handler)),
        );
    }
}
