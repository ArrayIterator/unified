// Properties — Module.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::{HookBus, HookContext};
use unified_core::json;
use unified_core::modules::{Module, ModuleRegistry, RouteInfo, RouteVersionSupport};
use unified_core::pagination::{PageResult, PaginationParams};
use crate::modules::ai;
use actix_web::{web, HttpRequest, HttpResponse};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::sync::{Arc, RwLock};
use std::time::Duration;
use uuid::Uuid;

use super::*;

// ---------------------------------------------------------------------------
// Module
// ---------------------------------------------------------------------------

pub struct PropertiesModule {
    state: PropertiesState,
}

impl PropertiesModule {
    pub fn new() -> Self {
        Self {
            state: PropertiesState::default(),
        }
    }
}

impl Default for PropertiesModule {
    fn default() -> Self {
        Self::new()
    }
}

impl Module for PropertiesModule {
    fn id(&self) -> &'static str {
        "properties"
    }

    fn name(&self) -> &'static str {
        "Property Manager"
    }

    fn description(&self) -> &'static str {
        "Land parcels and housing developments with 2D site plans (master plans), blocks / plots for sale with a status machine and audit history, location search through a pluggable geocoder provider, server-generated SVG plans and print pages, and a buy flow that hands checkout to the store module and marks blocks sold on payment.captured."
    }

    fn requires(&self) -> &[&'static str] {
        // Locations are the shared maps module's job: parcels,
        // blocks, and developments are all located, and this
        // module consumes the maps contract for pins, nearby
        // search, and geofences instead of keeping its own
        // geospatial store. Maps is core (always active), so
        // this only documents the dependency and blocks
        // disabling maps while properties is enabled.
        &[
            "users", "roles", "store", "billing", "notifications", "company", "crm", "storage",
            "maps",
        ]
    }

    fn dependencies(&self) -> Vec<&'static str> {
        vec![
            "users", "roles", "store", "billing", "notifications", "company", "crm", "storage",
            "maps",
        ]
    }

    fn available_as_product(&self) -> bool {
        true
    }

    fn product_category(&self) -> Option<&'static str> {
        Some("commerce")
    }

    fn init(&self, ctx: &AppContext) -> UnifiedResult<()> {

        ai::register_ai_capabilities("properties", AI_CAPABILITIES);
        *self
            .state
            .hooks
            .write()
            .expect("hooks lock poisoned") = Some(Arc::clone(&ctx.hooks));
        Ok(())
    }

    fn register_hooks(&self, bus: &HookBus) {
        let state = Arc::new(self.state.clone());
        bus.register(
            PROPERTIES_ORDER_CREATED,
            "properties",
            Arc::new(move |ctx: &mut HookContext| {
                on_properties_order_created(Arc::clone(&state), ctx)
            }),
        );
        let state = Arc::new(self.state.clone());
        bus.register(
            PAYMENT_CAPTURED,
            "properties",
            Arc::new(move |ctx: &mut HookContext| {
                on_payment_captured(Arc::clone(&state), ctx)
            }),
        );
        let state = Arc::new(self.state.clone());
        bus.register(
            BANK_TRANSFER_VERIFICATION_CREATED,
            "properties",
            Arc::new(move |ctx: &mut HookContext| {
                on_bank_transfer_verification_created(Arc::clone(&state), ctx)
            }),
        );
        let state = Arc::new(self.state.clone());
        bus.register(
            BANK_TRANSFER_REJECTED,
            "properties",
            Arc::new(move |ctx: &mut HookContext| {
                on_bank_transfer_rejected(Arc::clone(&state), ctx)
            }),
        );
    }

    fn configure_routes(&self, cfg: &mut web::ServiceConfig) {
        let state = web::Data::new(self.state.clone());
        cfg.service(
            web::scope("/api/v1/properties")
                .app_data(state)
                .route("", web::get().to(list_properties))
                .route("", web::post().to(create_property))
                .route("/public", web::get().to(list_public_properties))
                .route("/public/{slug}", web::get().to(get_public_property))
                .route("/public/{slug}/plans/{plan_id}/svg", web::get().to(get_public_plan_svg))
                .route("/public/{slug}/plans/{plan_id}/print", web::get().to(get_public_plan_print))
                .route("/location/search", web::get().to(search_location))
                .route("/location/reverse", web::get().to(reverse_location))
                .route("/location/settings", web::get().to(get_location_settings))
                .route("/location/settings", web::patch().to(update_location_settings))
                .route("/{id}", web::get().to(get_property))
                .route("/{id}", web::patch().to(update_property))
                .route("/{id}", web::delete().to(delete_property))
                .route("/{id}/publish", web::post().to(publish_property))
                .route("/{id}/archive", web::post().to(archive_property))
                .route("/{id}/mark-sold-out", web::post().to(mark_property_sold_out))
                .route("/{id}/plans", web::get().to(list_plans))
                .route("/{id}/plans", web::post().to(create_plan))
                .route("/{id}/plans/{plan_id}", web::get().to(get_plan))
                .route("/{id}/plans/{plan_id}", web::patch().to(update_plan))
                .route("/{id}/plans/{plan_id}", web::delete().to(delete_plan))
                .route("/{id}/plans/{plan_id}/svg", web::get().to(get_plan_svg))
                .route("/{id}/plans/{plan_id}/print", web::get().to(get_plan_print))
                .route("/{id}/plans/{plan_id}/blocks", web::get().to(list_blocks))
                .route("/{id}/plans/{plan_id}/blocks", web::post().to(create_block))
                .route("/{id}/plans/{plan_id}/blocks/export", web::get().to(export_blocks))
                .route(
                    "/{id}/plans/{plan_id}/blocks/{block_id}",
                    web::get().to(get_block),
                )
                .route(
                    "/{id}/plans/{plan_id}/blocks/{block_id}",
                    web::patch().to(update_block),
                )
                .route(
                    "/{id}/plans/{plan_id}/blocks/{block_id}",
                    web::delete().to(delete_block),
                )
                .route(
                    "/{id}/plans/{plan_id}/blocks/{block_id}/reserve",
                    web::post().to(reserve_block),
                )
                .route(
                    "/{id}/plans/{plan_id}/blocks/{block_id}/release",
                    web::post().to(release_block),
                )
                .route(
                    "/{id}/plans/{plan_id}/blocks/{block_id}/purchase",
                    web::post().to(purchase_block),
                )
                .route(
                    "/{id}/plans/{plan_id}/blocks/{block_id}/assign",
                    web::post().to(assign_block),
                )
                .route(
                    "/{id}/plans/{plan_id}/blocks/{block_id}/block",
                    web::post().to(block_block),
                )
                .route(
                    "/{id}/plans/{plan_id}/blocks/{block_id}/unblock",
                    web::post().to(unblock_block),
                )
                .route(
                    "/{id}/plans/{plan_id}/blocks/{block_id}/correct-status",
                    web::post().to(correct_block_status),
                )
                .route(
                    "/{id}/plans/{plan_id}/blocks/{block_id}/history",
                    web::get().to(list_block_history),
                ),
        );
    }

    fn route_info(&self) -> Vec<RouteInfo> {
        vec![
            RouteInfo { method: "GET", path: "/api/v1/properties", owner: "properties", description: "List properties (paginated, filter by status, company_id, search).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/properties", owner: "properties", description: "Create a property (draft).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/properties/{id}", owner: "properties", description: "Get one property.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "PATCH", path: "/api/v1/properties/{id}", owner: "properties", description: "Update a draft or published property (creator or manage capability).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "DELETE", path: "/api/v1/properties/{id}", owner: "properties", description: "Delete a draft or archived property (cascades to plans, blocks, history, purchases).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/properties/{id}/publish", owner: "properties", description: "Publish a draft property.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/properties/{id}/archive", owner: "properties", description: "Archive a property (terminal).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/properties/{id}/mark-sold-out", owner: "properties", description: "Mark a published property sold out.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/properties/{id}/plans", owner: "properties", description: "List plans of a property (paginated).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/properties/{id}/plans", owner: "properties", description: "Create a site plan (boundary must be Polygon / MultiPolygon).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/properties/{id}/plans/{plan_id}", owner: "properties", description: "Get one plan with its blocks.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "PATCH", path: "/api/v1/properties/{id}/plans/{plan_id}", owner: "properties", description: "Update a plan (name, description, boundary geometry).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "DELETE", path: "/api/v1/properties/{id}/plans/{plan_id}", owner: "properties", description: "Delete a plan with no reserved, sold, or assigned blocks.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/properties/{id}/plans/{plan_id}/svg", owner: "properties", description: "Render a plan as SVG (status colours, labels, fitted viewBox).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/properties/{id}/plans/{plan_id}/print", owner: "properties", description: "Print-friendly HTML page for a plan (inline SVG + block table).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/properties/{id}/plans/{plan_id}/blocks", owner: "properties", description: "List blocks of a plan (paginated, filter by status).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/properties/{id}/plans/{plan_id}/blocks", owner: "properties", description: "Create a block (GeoJSON validated; area / length computed server-side).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/properties/{id}/plans/{plan_id}/blocks/{block_id}", owner: "properties", description: "Get one block.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "PATCH", path: "/api/v1/properties/{id}/plans/{plan_id}/blocks/{block_id}", owner: "properties", description: "Update a block (edit rules depend on status; sold / assigned blocks only allow note edits).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "DELETE", path: "/api/v1/properties/{id}/plans/{plan_id}/blocks/{block_id}", owner: "properties", description: "Delete an available or blocked block.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/properties/{id}/plans/{plan_id}/blocks/{block_id}/reserve", owner: "properties", description: "Reserve an available block for a buyer (self or, with manage, another user).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/properties/{id}/plans/{plan_id}/blocks/{block_id}/release", owner: "properties", description: "Release a reserved block back to available (holder or manage; cancels pending purchases).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/properties/{id}/plans/{plan_id}/blocks/{block_id}/purchase", owner: "properties", description: "Start a block purchase: reserves the block and emits properties.block.purchase_requested for the store module.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/properties/{id}/plans/{plan_id}/blocks/{block_id}/assign", owner: "properties", description: "Assign an available or reserved block to a buyer (manage only).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/properties/{id}/plans/{plan_id}/blocks/{block_id}/block", owner: "properties", description: "Block a block (admin-only; not for sold blocks).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/properties/{id}/plans/{plan_id}/blocks/{block_id}/unblock", owner: "properties", description: "Unblock a blocked block back to available (manage only).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/properties/{id}/plans/{plan_id}/blocks/{block_id}/correct-status", owner: "properties", description: "Audited admin correction of a sold block (target available / reserved / assigned, reason required).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/properties/{id}/plans/{plan_id}/blocks/{block_id}/history", owner: "properties", description: "List the audit history of a block (paginated, newest first).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/properties/{id}/plans/{plan_id}/blocks/export", owner: "properties", description: "Export a plan's blocks as CSV or JSON (?format=csv|json).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/properties/location/search", owner: "properties", description: "Search locations through the configured geocoder provider (none provider answers with a not-configured error).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/properties/location/reverse", owner: "properties", description: "Reverse-geocode coordinates through the configured geocoder provider.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/properties/location/settings", owner: "properties", description: "Get the geocoder provider settings (manage only).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "PATCH", path: "/api/v1/properties/location/settings", owner: "properties", description: "Update the geocoder provider settings (manage only; provider is rebuilt and validated before saving).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/properties/public", owner: "properties", description: "PUBLIC: list published properties (no auth; gated by the API Manager ApiAccessRule for this path).", auth: unified_core::auth::RouteAuth::Public},
            RouteInfo { method: "GET", path: "/api/v1/properties/public/{slug}", owner: "properties", description: "PUBLIC: one published property with plans and blocks, buyer and order ids stripped (no auth; gated by the API Manager ApiAccessRule).", auth: unified_core::auth::RouteAuth::Public},
            RouteInfo { method: "GET", path: "/api/v1/properties/public/{slug}/plans/{plan_id}/svg", owner: "properties", description: "PUBLIC: one published property's plan as SVG (no auth; gated by the API Manager ApiAccessRule).", auth: unified_core::auth::RouteAuth::Public},
            RouteInfo { method: "GET", path: "/api/v1/properties/public/{slug}/plans/{plan_id}/print", owner: "properties", description: "PUBLIC: print page for one published property's plan, without the buyer column (no auth; gated by the API Manager ApiAccessRule).", auth: unified_core::auth::RouteAuth::Public},
        ]
    }

    fn route_version_support(&self) -> RouteVersionSupport {
        RouteVersionSupport::All
    }
}

pub fn register(registry: &ModuleRegistry) {
    registry.register(Arc::new(PropertiesModule::new()));
}
