// Saved Views - Module struct, route table and registration.

// Saved Filters / Views module.
//
// Saved filters and views, per user and per module: a name, the module
// id the view belongs to, the stored filter definition, sort order,
// visibility (`private` or `shared`), and whether the view is the
// user's default for that module. `apply` returns the stored filter
// definition so the client can run it against the module's own list
// endpoint — this module never executes another module's query itself.
//
// Validation rules (enforced on create and update):
//   - `module_id` must be a registered module id, checked against the
//     live module registry (`AppContext.modules`), so a view can never
//     point at a module that does not exist.
//   - `filter` must be a JSON object whose values are scalars (null,
//     boolean, number, string) or arrays of scalars only. Nested
//     objects and nested arrays are rejected: a stored filter is a
//     flat field/value map, never executable logic.
//   - `visibility` is `private` or `shared`; `digest`-style values
//     are not accepted here.
//   - `name` is required, 1..=100 characters.
//   - At most one default view per (user, module): setting a default
//     clears the flag on the user's other views for that module.
//
// Persistence: views live in an in-memory store (the same approach the
// notifications and audit modules use). The Diesel table for durable
// persistence does not exist in crates/unified-core/src/db/schema.rs` yet; the migration
// SQL and the schema entries are supplied to the integrator separately.
// Nothing in this file references a table that does not exist yet, so
// it compiles against the current schema.

use unified_core::modules::{Module, ModuleRegistry, RouteInfo};
use actix_web::web;
use std::sync::Arc;

use super::{SavedViewsState, apply_view, create_view, delete_view, get_default_view,
    get_view_handler, list_views, set_default_view, update_view};

// ---------------------------------------------------------------------------
// Module.
// ---------------------------------------------------------------------------

pub struct SavedViewsModule {
    state: Arc<SavedViewsState>,
}

impl SavedViewsModule {
    pub fn new() -> Self {
        Self {
            state: Arc::new(SavedViewsState::new()),
        }
    }
}

impl Default for SavedViewsModule {
    fn default() -> Self {
        Self::new()
    }
}

impl Module for SavedViewsModule {
    fn id(&self) -> &'static str {
        "saved_views"
    }

    fn name(&self) -> &'static str {
        "Saved Filters and Views"
    }

    fn description(&self) -> &'static str {
        "Saved filters and views per user and per module, with private/shared visibility and one default view per user and module."
    }

    fn configure_routes(&self, cfg: &mut web::ServiceConfig) {
        cfg.app_data(web::Data::new(self.state.clone()));
        cfg.service(
            web::scope("/api/v1/saved-views")
                .route("", web::get().to(list_views))
                .route("", web::post().to(create_view))
                .route("/defaults", web::get().to(get_default_view))
                .route("/{id}", web::get().to(get_view_handler))
                .route("/{id}", web::put().to(update_view))
                .route("/{id}", web::delete().to(delete_view))
                .route("/{id}/set-default", web::post().to(set_default_view))
                .route("/{id}/apply", web::get().to(apply_view)),
        );
    }

    fn route_info(&self) -> Vec<RouteInfo> {
        vec![
            RouteInfo { method: "GET", path: "/api/v1/saved-views", owner: "saved_views", description: "List saved views by module, paginated, with visibility rules.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/saved-views", owner: "saved_views", description: "Create a saved view.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/saved-views/defaults", owner: "saved_views", description: "The default view for one user and module.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/saved-views/{id}", owner: "saved_views", description: "Get one saved view.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "PUT", path: "/api/v1/saved-views/{id}", owner: "saved_views", description: "Update a saved view.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "DELETE", path: "/api/v1/saved-views/{id}", owner: "saved_views", description: "Delete a saved view.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/saved-views/{id}/set-default", owner: "saved_views", description: "Make a view the default for its user and module.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/saved-views/{id}/apply", owner: "saved_views", description: "Return the stored filter definition for the client to run.", auth: unified_core::auth::RouteAuth::Authenticated},
        ]
    }
}

pub fn register(registry: &ModuleRegistry) {
    registry.register(Arc::new(SavedViewsModule::new()));
}
