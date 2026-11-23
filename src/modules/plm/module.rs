//! Module registration: routes, hooks and metadata.

use super::ai_capabilities::AI_CAPABILITIES;
use super::handlers_admin::{get_settings, list_audit, update_settings};
use super::handlers_bom::{
    create_bom_version, delete_bom_version, get_bom_version, list_bom_versions,
};
use super::handlers_ecos::{
    create_eco, create_eco_approval, delete_eco, get_eco, list_eco_approvals, list_ecos,
    transition_eco, update_eco,
};
use super::handlers_stages::{
    create_stage, delete_stage, get_stage, list_stages, seed_stages, update_stage,
};
use super::handlers_versions::{
    create_version, create_version_approval, delete_version, diff_version, get_version,
    list_versions, transition_version, update_version,
};
use super::hooks::{on_bom_version_created, on_product_deleted, on_user_deleted};
use super::state::PlmState;
use super::types::{EVENT_BOM_VERSION_CREATED, EVENT_PRODUCT_DELETED, EVENT_USER_DELETED};
use crate::modules::ai;
use actix_web::web;
use std::sync::Arc;
use unified_core::context::AppContext;
use unified_core::error::UnifiedResult;
use unified_core::hooks::{HookBus, HookContext};
use unified_core::modules::{Module, ModuleRegistry, RouteInfo, RouteVersionSupport};

// ---------------------------------------------------------------------------
// Module
// ---------------------------------------------------------------------------

pub struct PlmModule {
    pub(crate) state: PlmState,
}

impl PlmModule {
    pub fn new() -> Self {
        Self {
            state: PlmState::default(),
        }
    }
}

impl Default for PlmModule {
    fn default() -> Self {
        Self::new()
    }
}

impl Module for PlmModule {
    fn id(&self) -> &'static str {
        "plm"
    }

    fn name(&self) -> &'static str {
        "PLM"
    }

    fn description(&self) -> &'static str {
        "Product lifecycle management: admin-defined lifecycle stages with ordered transitions and approval gates (default set seeded as data, never hard-coded), product versions with major.minor.patch revisions, change summaries, effective dates, and field-by-field diffs; engineering change orders with a status machine (draft, review, approved, implemented, closed, rejected), data-driven ECO types, impact assessment, linked products, versions, and documents, and ordered approval steps by approver role; versioned BOM snapshots linked to manufacturing BOMs and gated on an approved or implemented ECO; and a full audit trail of every transition, approval, and versioning event."
    }

    fn is_core(&self) -> bool {
        false
    }

    fn requires(&self) -> &[&'static str] {
        &[
            "users",
            "roles",
            "notifications",
            "products",
            "manufacturing",
            "documents",
        ]
    }

    fn dependencies(&self) -> Vec<&'static str> {
        vec![
            "users",
            "roles",
            "notifications",
            "products",
            "manufacturing",
            "documents",
        ]
    }

    fn available_as_product(&self) -> bool {
        true
    }

    fn product_category(&self) -> Option<&'static str> {
        Some("operations")
    }

    fn init(&self, ctx: &AppContext) -> UnifiedResult<()> {
        ai::register_ai_capabilities("plm", AI_CAPABILITIES);
        *self.state.hooks.write().expect("hooks lock poisoned") = Some(Arc::clone(&ctx.hooks));
        Ok(())
    }

    fn register_hooks(&self, bus: &HookBus) {
        let state = self.state.clone();
        bus.register(
            EVENT_USER_DELETED,
            "plm",
            Arc::new(move |ctx: &mut HookContext| on_user_deleted(state.clone(), ctx)),
        );
        let state = self.state.clone();
        bus.register(
            EVENT_PRODUCT_DELETED,
            "plm",
            Arc::new(move |ctx: &mut HookContext| on_product_deleted(state.clone(), ctx)),
        );
        let state = self.state.clone();
        bus.register(
            EVENT_BOM_VERSION_CREATED,
            "plm",
            Arc::new(move |ctx: &mut HookContext| on_bom_version_created(state.clone(), ctx)),
        );
    }

    fn configure_routes(&self, cfg: &mut web::ServiceConfig) {
        let state = web::Data::new(self.state.clone());
        cfg.service(
            web::scope("/api/v1/plm")
                .app_data(state)
                .route("/stages", web::get().to(list_stages))
                .route("/stages", web::post().to(create_stage))
                .route("/stages/seed", web::post().to(seed_stages))
                .route("/stages/{stage_id}", web::get().to(get_stage))
                .route("/stages/{stage_id}", web::patch().to(update_stage))
                .route("/stages/{stage_id}", web::delete().to(delete_stage))
                .route("/versions", web::get().to(list_versions))
                .route("/versions", web::post().to(create_version))
                .route("/versions/{version_id}", web::get().to(get_version))
                .route("/versions/{version_id}", web::patch().to(update_version))
                .route("/versions/{version_id}", web::delete().to(delete_version))
                .route(
                    "/versions/{version_id}/transition",
                    web::post().to(transition_version),
                )
                .route("/versions/{version_id}/diff", web::get().to(diff_version))
                .route(
                    "/versions/{version_id}/approvals",
                    web::post().to(create_version_approval),
                )
                .route("/ecos", web::get().to(list_ecos))
                .route("/ecos", web::post().to(create_eco))
                .route("/ecos/{eco_id}", web::get().to(get_eco))
                .route("/ecos/{eco_id}", web::patch().to(update_eco))
                .route("/ecos/{eco_id}", web::delete().to(delete_eco))
                .route("/ecos/{eco_id}/transition", web::post().to(transition_eco))
                .route(
                    "/ecos/{eco_id}/approvals",
                    web::get().to(list_eco_approvals),
                )
                .route(
                    "/ecos/{eco_id}/approvals",
                    web::post().to(create_eco_approval),
                )
                .route("/bom-versions", web::get().to(list_bom_versions))
                .route("/bom-versions", web::post().to(create_bom_version))
                .route(
                    "/bom-versions/{bom_version_id}",
                    web::get().to(get_bom_version),
                )
                .route(
                    "/bom-versions/{bom_version_id}",
                    web::delete().to(delete_bom_version),
                )
                .route("/settings", web::get().to(get_settings))
                .route("/settings", web::patch().to(update_settings))
                .route("/audit", web::get().to(list_audit)),
        );
    }

    fn route_info(&self) -> Vec<RouteInfo> {
        let auth = unified_core::auth::RouteAuth::Authenticated;
        vec![
            RouteInfo {
                method: "GET",
                path: "/api/v1/plm/stages",
                owner: "plm",
                description: "List lifecycle stages (paginated, filter by active; search by name/description).",
                auth,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/plm/stages",
                owner: "plm",
                description: "Create a lifecycle stage with its allowed transitions and approval gate (manager).",
                auth,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/plm/stages/seed",
                owner: "plm",
                description: "Seed the default lifecycle stage set for the tenant; existing slugs are skipped (manager).",
                auth,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/plm/stages/{stage_id}",
                owner: "plm",
                description: "Get one lifecycle stage.",
                auth,
            },
            RouteInfo {
                method: "PATCH",
                path: "/api/v1/plm/stages/{stage_id}",
                owner: "plm",
                description: "Update a lifecycle stage (manager).",
                auth,
            },
            RouteInfo {
                method: "DELETE",
                path: "/api/v1/plm/stages/{stage_id}",
                owner: "plm",
                description: "Delete a stage with no versions in it (manager).",
                auth,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/plm/versions",
                owner: "plm",
                description: "List product versions (paginated, filter by product_id, stage, is_current; search by summary/revision).",
                auth,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/plm/versions",
                owner: "plm",
                description: "Create a product version with explicit numbers or a bump off the superseded revision (manager).",
                auth,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/plm/versions/{version_id}",
                owner: "plm",
                description: "Get one product version with its approvals.",
                auth,
            },
            RouteInfo {
                method: "PATCH",
                path: "/api/v1/plm/versions/{version_id}",
                owner: "plm",
                description: "Update a version's summary, effective dates, or current flag (manager).",
                auth,
            },
            RouteInfo {
                method: "DELETE",
                path: "/api/v1/plm/versions/{version_id}",
                owner: "plm",
                description: "Delete a version not referenced by an ECO or a newer revision (manager).",
                auth,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/plm/versions/{version_id}/transition",
                owner: "plm",
                description: "Move a version to another lifecycle stage through the stage graph's allowed transitions, gated on stage-entry approvals (manager).",
                auth,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/plm/versions/{version_id}/diff",
                owner: "plm",
                description: "Field-by-field diff metadata between two versions of the same product (?against=).",
                auth,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/plm/versions/{version_id}/approvals",
                owner: "plm",
                description: "Record a stage-entry approval for a version (manager).",
                auth,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/plm/ecos",
                owner: "plm",
                description: "List engineering change orders (paginated, filter by status, eco_type, product_id; search by code/title).",
                auth,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/plm/ecos",
                owner: "plm",
                description: "Create an engineering change order with data-driven type, impact assessment, and approval steps (manager).",
                auth,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/plm/ecos/{eco_id}",
                owner: "plm",
                description: "Get one ECO with its approvals.",
                auth,
            },
            RouteInfo {
                method: "PATCH",
                path: "/api/v1/plm/ecos/{eco_id}",
                owner: "plm",
                description: "Update a draft or rejected ECO (manager).",
                auth,
            },
            RouteInfo {
                method: "DELETE",
                path: "/api/v1/plm/ecos/{eco_id}",
                owner: "plm",
                description: "Delete a draft or rejected ECO with no linked BOM versions (manager).",
                auth,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/plm/ecos/{eco_id}/transition",
                owner: "plm",
                description: "Move an ECO through its status machine; approval requires every required step approved (manager).",
                auth,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/plm/ecos/{eco_id}/approvals",
                owner: "plm",
                description: "List an ECO's approval decisions (paginated, filter by decision).",
                auth,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/plm/ecos/{eco_id}/approvals",
                owner: "plm",
                description: "Decide one approval step on an ECO; the caller must hold the step's approver role (manager).",
                auth,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/plm/bom-versions",
                owner: "plm",
                description: "List BOM snapshots (paginated, filter by bom_id, eco_id).",
                auth,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/plm/bom-versions",
                owner: "plm",
                description: "Version a BOM snapshot; strictly increasing per BOM and gated on an approved or implemented ECO when linked (manager).",
                auth,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/plm/bom-versions/{bom_version_id}",
                owner: "plm",
                description: "Get one BOM snapshot.",
                auth,
            },
            RouteInfo {
                method: "DELETE",
                path: "/api/v1/plm/bom-versions/{bom_version_id}",
                owner: "plm",
                description: "Delete an older BOM snapshot; the newest snapshot of a BOM cannot be deleted (manager).",
                auth,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/plm/settings",
                owner: "plm",
                description: "Get per-tenant PLM settings (ECO types, BOM ECO requirement).",
                auth,
            },
            RouteInfo {
                method: "PATCH",
                path: "/api/v1/plm/settings",
                owner: "plm",
                description: "Update per-tenant PLM settings (manager).",
                auth,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/plm/audit",
                owner: "plm",
                description: "List the PLM audit trail (paginated, filter by entity_type, entity_id, action).",
                auth,
            },
        ]
    }

    fn route_version_support(&self) -> RouteVersionSupport {
        RouteVersionSupport::All
    }
}

pub fn register(registry: &ModuleRegistry) {
    registry.register(Arc::new(PlmModule::new()));
}
