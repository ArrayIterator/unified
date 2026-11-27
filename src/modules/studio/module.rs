// Studio - Module struct, route table and registration.

// Studio module - user-side customizer (metadata-driven only).
//
// A Studio-style customizer: administrators and power users add custom
// fields, validation rules, saved list views, and form layouts to any
// record type, without touching code and without changing the database
// schema at runtime.
//
// HARD BOUNDARY - READ THIS FIRST. This module is STRICTLY
// METADATA-DRIVEN. It never generates code, never alters a database
// table at runtime, never executes user-supplied code, and never adds
// a column. Customizations are metadata records that the owning
// module reads when it renders a record; custom values live in one
// generic key-value table (`studio_field_values`: record_type +
// record_id + field_key + typed JSON value). A customization can
// change what a form shows, never what the system stores.
//
// Routes live under `/api/v1/studio`:
//
//   Field types (closed vocabulary registry, read-only):
//     GET  /field-types - the seven field types and what each supports.
//   Record types (admin-editable registry; modules register theirs
//   through the `studio.record_type.register` hook):
//     GET/POST /record-types
//     GET/DELETE /record-types/{record_type}
//     GET/POST /record-types/{record_type}/system-fields
//   Custom field definitions (versioned):
//     GET/POST /fields
//     GET  /fields/record-type/{record_type}?company_id=...
//     GET/PUT/DELETE /fields/{id}
//     POST /fields/{id}/archive
//   Custom values (generic key-value store):
//     GET/POST /values
//     GET/DELETE /values/{id}
//     POST /values/validate - validate a value without saving it.
//     GET  /records/{record_type}/{record_id}/custom-fields
//        - effective definitions merged with stored values.
//   Saved list views (per user / per company, private or shared):
//     GET/POST /views
//     GET/PUT/DELETE /views/{id}
//     POST /views/{id}/set-default
//   Form layouts (field order / visibility per record type, company):
//     GET/POST /layouts
//     GET  /layouts/record-type/{record_type}?company_id=...
//     GET/PUT/DELETE /layouts/{id}
//
// Field types (closed vocabulary, same precedent as the closed
// `ProductCategory` set in `unified_core::modules`): `text`,
// `number`, `decimal`, `date`, `boolean`, `select`, `user_reference`.
// Everything else about a field - its options, defaults, help text,
// and validation rules - is data stored on the definition row.
//
// Field definitions are VERSIONED. Creating a definition stores
// version 1. Updating a definition never mutates the old row: it
// stores a new row with the same `field_key` and version + 1 and
// marks the old row `superseded`. Stored values keep pointing at the
// definition version they were validated against, so existing values
// keep working after an edit. The effective definition for a record
// type is the highest active version; a company-specific definition
// shadows a global one with the same field key.
//
// Validation rules (all data on the definition, all enforced
// server-side when a value is saved):
//   - `required` - a value must be present and non-empty.
//   - `min` / `max` - numeric bounds for `number` and `decimal`.
//   - `min_length` / `max_length` - text length bounds.
//   - `pattern` - a regular expression a `text` value must match.
//     The pattern is compiled at definition time: an invalid regex
//     is rejected when the definition is created, never at use time.
//   - `options` - the allowed values for `select` (data, not code).
//
// Safety:
//   - A record type must be registered (see the registry below)
//     before any definition, value, view, or layout may reference
//     it. Unknown record types are rejected.
//   - Custom field keys must start with the `x_` prefix (a
//     configurable default, see `FIELD_KEY_PREFIX`) so a custom field
//     can never collide with a system field.
//   - System fields are registered per record type with a
//     `hideable` flag. A layout may never hide a system field whose
//     `hideable` is false, and a layout may never hide a system
//     field marked `required`. The owning module, not Studio, owns
//     the final render - Studio only records the layout metadata.
//   - Company scoping: the caller's company scope is resolved from
//     server-side sources only (verified caller, tenant context,
//     `user.company_requested` hook) - never from a body/query
//     `company_id` or the `X-Company-Id` header, both of which are
//     client-asserted - and a body/query `company_id` that disagrees
//     with it is rejected (403); list endpoints only return the
//     caller's own company rows plus global rows, and single-row
//     reads of another company's rows answer 404. Platform is an
//     explicitly-decided scope (control-plane request by a verified
//     admin with no resolvable company); an unresolved caller reads
//     global rows only. Record types and system fields carry no company
//     and are global. Residual: the in-memory stores carry no
//     `tenant_id`, so full tenant partitioning still needs the
//     store-shape change described in `auth.rs`.
//
// Hook contract (the core of this module). Query hooks - owning
// modules ask, Studio answers by adding a result to the hook
// context; the requester reads `context.results`:
//   - `studio.fields.requested`
//     Request payload: `{"record_type": "crm.lead", "company_id": ""}`
//     (empty `company_id` means global definitions only).
//     Result payload: `{"record_type": "...", "company_id": "...",
//     "definitions": [<FieldDefinition>, ...]}` - the effective
//     definitions (highest active version per field key, company
//     definitions shadowing global ones).
//   - `studio.layout.requested`
//     Request payload: `{"record_type": "crm.lead", "company_id": ""}`.
//     Result payload: `{"record_type": "...", "company_id": "...",
//     "layout": <FormLayout | null>}` - the default layout for that
//     company, falling back to the global default layout, or null
//     when no layout exists.
//   - `studio.views.requested`
//     Request payload: `{"record_type": "crm.lead",
//     "company_id": "...", "user_id": "..."}`.
//     Result payload: `{"record_type": "...", "views": [<ListView>,
//     ...]}` - the views visible to that user (their own views plus
//     shared company views), defaults first.
//   - `studio.record_type.register`
//     Request payload: `{"record_type": "crm.lead",
//     "module_id": "crm", "label": "Lead", "description": "...",
//     "system_fields": [{"field_key": "name", "label": "Name",
//     "field_type": "text", "required": true, "hideable": false}]}`.
//     Result payload: `{"registered": true, "record_type": "..."}`.
//     Any module may register its record types this way; the same
//     registration is also available as `POST /record-types`.
//
//   Extension points, currently unemitted: the four query hooks
//     above (`studio.fields.requested`, `studio.layout.requested`,
//     `studio.views.requested`, `studio.record_type.register`) have
//     listeners registered in `register_hooks` below, ready to
//     answer, but NO emitter of any of them exists anywhere in the
//     tree outside this module today - no owning module asks Studio
//     through the hook bus yet (only the `.changed` events below
//     are actually emitted, by this module itself). The listeners
//     are kept deliberately: they are the extension points other
//     modules emit to once they adopt Studio metadata when
//     rendering. Do not invent emitters here; the owning modules
//     add the `emit` call when they adopt the contract.
// Events emitted on the hook bus:
//   - `studio.fields.changed` - `{"field_key", "record_type",
//     "company_id", "version", "definition_id", "action":
//     "created" | "updated" | "archived"}`. Owning modules listen to
//     this to invalidate cached definitions.
//   - `studio.values.changed` - `{"record_type", "record_id",
//     "field_key", "company_id", "action": "set" | "deleted"}`.
//   - `studio.views.changed` - `{"view_id", "record_type",
//     "company_id", "action": "created" | "updated" | "deleted" |
//     "default_changed"}`.
//   - `studio.layouts.changed` - `{"layout_id", "record_type",
//     "company_id", "action": "created" | "updated" | "deleted"}`.
//   - `studio.record_type.registered` - `{"record_type",
//     "module_id", "label"}`.
//
// Actor identity: until the auth module provides an extractor, the
// caller is identified by the `X-User-Id` header (required) and
// `X-Role` carries the role slug the capability check runs against,
// exactly like the properties module. Capability checks use the
// `role.check` hook and fail closed (no answer means not allowed).
// Capabilities: `studio.view` (read), `studio.customize` (fields,
// values, views, layouts), `studio.admin` (record types, system
// fields). There are no public routes: every route requires a
// caller and a capability, and the API Manager's allowed-public-
// access controls apply through the versioned public API registry
// (`route_info` + `route_version_support` default `All`).
//
// Persistence: the live data lives in in-memory stores (the same
// approach the saved-views, notifications, and audit modules use).
// The migration `2026-11-22-000085_studio` creates the durable
// tables (`studio_record_types`, `studio_system_fields`,
// `studio_field_definitions`, `studio_field_values`,
// `studio_list_views`, `studio_form_layouts`) as persistence
// targets; this file never references a table that does not exist
// yet and never runs raw SQL, so it compiles against the current
// schema. The migration has never been executed against a live
// PostgreSQL database.
//
// Honesty - what is real here: metadata CRUD with server-side
// validation, versioned definitions, the generic key-value value
// store, saved views, layouts, the record-type registry, and the
// hook contract above. What is NOT built here: the owning modules
// do not yet read Studio metadata when they render (each owning
// module must adopt the query hooks), there is no migration of
// values when a definition's type changes (the old values stay
// attached to the old version), and there is no drag-and-drop
// layout builder UI - layouts are JSON metadata over HTTP.

use crate::modules::ai;
use actix_web::web;
use std::sync::Arc;
use unified_core::context::AppContext;
use unified_core::error::UnifiedResult;
use unified_core::hooks::{HookBus, HookContext};
use unified_core::modules::{Module, ModuleRegistry, RouteInfo, RouteVersionSupport};

use super::ai_capabilities::AI_CAPABILITIES;
use super::{
    HOOK_FIELDS_REQUESTED, HOOK_LAYOUT_REQUESTED, HOOK_RECORD_TYPE_REGISTER, HOOK_VIEWS_REQUESTED,
    StudioState, archive_definition, create_definition, create_layout, create_record_type,
    create_system_field, create_view, delete_definition, delete_layout, delete_record_type,
    delete_value, delete_view, get_default_layout, get_definition, get_layout, get_record_type,
    get_value, get_view, list_definitions, list_effective_definitions, list_field_types,
    list_layouts, list_record_types, list_system_fields, list_values, list_views,
    on_fields_requested, on_layout_requested, on_record_type_register, on_views_requested,
    resolved_custom_fields, set_default_view, set_value, update_definition, update_layout,
    update_view, validate_value_handler,
};

// Module.

pub struct StudioModule {
    state: StudioState,
}

impl StudioModule {
    pub fn new() -> Self {
        Self {
            state: StudioState::new(),
        }
    }
}

impl Default for StudioModule {
    fn default() -> Self {
        Self::new()
    }
}

impl Module for StudioModule {
    fn id(&self) -> &'static str {
        "studio"
    }

    fn name(&self) -> &'static str {
        "Studio"
    }

    fn description(&self) -> &'static str {
        "User-side customizer: metadata-driven custom fields, validation rules, saved list views, and form layouts for any registered record type. No code generation and no runtime schema changes."
    }

    fn dependencies(&self) -> Vec<&'static str> {
        vec!["users", "roles", "company"]
    }

    fn available_as_product(&self) -> bool {
        true
    }

    fn product_category(&self) -> Option<&'static str> {
        Some("productivity")
    }

    fn init(&self, ctx: &AppContext) -> UnifiedResult<()> {
        ai::register_ai_capabilities("studio", AI_CAPABILITIES);
        *self.state.hooks.write().expect("hooks lock poisoned") = Some(Arc::clone(&ctx.hooks));
        Ok(())
    }

    /// Query-hook listeners for the four Studio extension points
    /// (`studio.fields.requested`, `studio.layout.requested`,
    /// `studio.views.requested`, `studio.record_type.register`).
    /// These listeners are ready but currently unemitted: no module
    /// outside Studio emits these hooks yet (see the "Extension
    /// points, currently unemitted" note in the module docs above).
    /// The registrations are kept so owning modules can start
    /// emitting to them without a Studio change.
    fn register_hooks(&self, bus: &HookBus) {
        let state = Arc::new(self.state.clone());
        bus.register(
            HOOK_FIELDS_REQUESTED,
            "studio",
            Arc::new(move |ctx: &mut HookContext| on_fields_requested(Arc::clone(&state), ctx)),
        );
        let state = Arc::new(self.state.clone());
        bus.register(
            HOOK_LAYOUT_REQUESTED,
            "studio",
            Arc::new(move |ctx: &mut HookContext| on_layout_requested(Arc::clone(&state), ctx)),
        );
        let state = Arc::new(self.state.clone());
        bus.register(
            HOOK_VIEWS_REQUESTED,
            "studio",
            Arc::new(move |ctx: &mut HookContext| on_views_requested(Arc::clone(&state), ctx)),
        );
        let state = Arc::new(self.state.clone());
        bus.register(
            HOOK_RECORD_TYPE_REGISTER,
            "studio",
            Arc::new(move |ctx: &mut HookContext| on_record_type_register(Arc::clone(&state), ctx)),
        );
    }

    fn configure_routes(&self, cfg: &mut web::ServiceConfig) {
        let state = web::Data::new(self.state.clone());
        cfg.service(
            web::scope("/api/v1/studio")
                .app_data(state)
                .route("/field-types", web::get().to(list_field_types))
                .route("/record-types", web::get().to(list_record_types))
                .route("/record-types", web::post().to(create_record_type))
                .route(
                    "/record-types/{record_type}",
                    web::get().to(get_record_type),
                )
                .route(
                    "/record-types/{record_type}",
                    web::delete().to(delete_record_type),
                )
                .route(
                    "/record-types/{record_type}/system-fields",
                    web::get().to(list_system_fields),
                )
                .route(
                    "/record-types/{record_type}/system-fields",
                    web::post().to(create_system_field),
                )
                .route("/fields", web::get().to(list_definitions))
                .route("/fields", web::post().to(create_definition))
                .route(
                    "/fields/record-type/{record_type}",
                    web::get().to(list_effective_definitions),
                )
                .route("/fields/{id}", web::get().to(get_definition))
                .route("/fields/{id}", web::put().to(update_definition))
                .route("/fields/{id}", web::delete().to(delete_definition))
                .route("/fields/{id}/archive", web::post().to(archive_definition))
                .route("/values", web::get().to(list_values))
                .route("/values", web::post().to(set_value))
                .route("/values/validate", web::post().to(validate_value_handler))
                .route("/values/{id}", web::get().to(get_value))
                .route("/values/{id}", web::delete().to(delete_value))
                .route(
                    "/records/{record_type}/{record_id}/custom-fields",
                    web::get().to(resolved_custom_fields),
                )
                .route("/views", web::get().to(list_views))
                .route("/views", web::post().to(create_view))
                .route("/views/{id}", web::get().to(get_view))
                .route("/views/{id}", web::put().to(update_view))
                .route("/views/{id}", web::delete().to(delete_view))
                .route("/views/{id}/set-default", web::post().to(set_default_view))
                .route("/layouts", web::get().to(list_layouts))
                .route("/layouts", web::post().to(create_layout))
                .route(
                    "/layouts/record-type/{record_type}",
                    web::get().to(get_default_layout),
                )
                .route("/layouts/{id}", web::get().to(get_layout))
                .route("/layouts/{id}", web::put().to(update_layout))
                .route("/layouts/{id}", web::delete().to(delete_layout)),
        );
    }

    fn route_info(&self) -> Vec<RouteInfo> {
        vec![
            RouteInfo {
                method: "GET",
                path: "/api/v1/studio/field-types",
                owner: "studio",
                description: "List the seven field types and what each supports.",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/studio/record-types",
                owner: "studio",
                description: "List registered record types (paginated, filter by module_id).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/studio/record-types",
                owner: "studio",
                description: "Register a record type (admin only), optionally with system fields.",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/studio/record-types/{record_type}",
                owner: "studio",
                description: "Get one registered record type.",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "DELETE",
                path: "/api/v1/studio/record-types/{record_type}",
                owner: "studio",
                description: "Remove a record type with no definitions, values, views, or layouts (admin only).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/studio/record-types/{record_type}/system-fields",
                owner: "studio",
                description: "List the system fields of a record type (paginated).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/studio/record-types/{record_type}/system-fields",
                owner: "studio",
                description: "Register a system field for a record type (admin only).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/studio/fields",
                owner: "studio",
                description: "List field definitions (paginated, filter by record_type, company_id, status, field_type).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/studio/fields",
                owner: "studio",
                description: "Create a field definition (version 1).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/studio/fields/record-type/{record_type}",
                owner: "studio",
                description: "Effective definitions for a record type and company (highest active version per field key).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/studio/fields/{id}",
                owner: "studio",
                description: "Get one field definition version.",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "PUT",
                path: "/api/v1/studio/fields/{id}",
                owner: "studio",
                description: "Update a definition by creating a new version; the old version is superseded, never mutated.",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "DELETE",
                path: "/api/v1/studio/fields/{id}",
                owner: "studio",
                description: "Archive a field definition (soft delete; values keep working).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/studio/fields/{id}/archive",
                owner: "studio",
                description: "Archive a field definition.",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/studio/values",
                owner: "studio",
                description: "List stored custom values (paginated, filter by record_type, record_id, company_id, field_key).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/studio/values",
                owner: "studio",
                description: "Set (upsert) one custom value, validated against its definition.",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/studio/values/validate",
                owner: "studio",
                description: "Validate a value against a definition without saving it.",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/studio/values/{id}",
                owner: "studio",
                description: "Get one stored custom value.",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "DELETE",
                path: "/api/v1/studio/values/{id}",
                owner: "studio",
                description: "Delete one stored custom value.",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/studio/records/{record_type}/{record_id}/custom-fields",
                owner: "studio",
                description: "Effective definitions for one record merged with its stored values.",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/studio/views",
                owner: "studio",
                description: "List list views visible to the caller (paginated, filter by record_type, company_id, visibility).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/studio/views",
                owner: "studio",
                description: "Create a saved list view (private or shared, optionally default).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/studio/views/{id}",
                owner: "studio",
                description: "Get one list view (owner or shared only).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "PUT",
                path: "/api/v1/studio/views/{id}",
                owner: "studio",
                description: "Update a list view (owner only).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "DELETE",
                path: "/api/v1/studio/views/{id}",
                owner: "studio",
                description: "Delete a list view (owner only).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/studio/views/{id}/set-default",
                owner: "studio",
                description: "Make a list view the default for its scope (owner only).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/studio/layouts",
                owner: "studio",
                description: "List form layouts (paginated, filter by record_type, company_id).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/studio/layouts",
                owner: "studio",
                description: "Create a form layout (system-required / non-hideable fields can never be hidden).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/studio/layouts/record-type/{record_type}",
                owner: "studio",
                description: "The default form layout for a record type and company (company default, else global default).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/studio/layouts/{id}",
                owner: "studio",
                description: "Get one form layout.",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "PUT",
                path: "/api/v1/studio/layouts/{id}",
                owner: "studio",
                description: "Update a form layout.",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "DELETE",
                path: "/api/v1/studio/layouts/{id}",
                owner: "studio",
                description: "Delete a form layout.",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
        ]
    }

    fn route_version_support(&self) -> RouteVersionSupport {
        RouteVersionSupport::All
    }
}

pub fn register(registry: &ModuleRegistry) {
    registry.register(Arc::new(StudioModule::new()));
}
