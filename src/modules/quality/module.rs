//! quality module - Module registration: struct, Default, Module impl, register.

use unified_core::context::AppContext;
use unified_core::error::UnifiedResult;
use unified_core::hooks::{HookBus, HookContext};
use unified_core::modules::{Module, ModuleRegistry, RouteInfo, RouteVersionSupport};
use crate::modules::ai;
use actix_web::web;
use std::sync::Arc;

use super::constants::INVENTORY_GOODS_RECEIVED;
use super::constants::LOGISTICS_SHIPMENT_INSPECTION_REQUESTED;
use super::constants::MANUFACTURING_INSPECTION_REQUESTED;
use super::constants::PROCUREMENT_QUALITY_CHECK_REQUESTED;
use super::constants::QUALITY_BLOCKS_QUERY;
use super::hooks::on_blocks_query;
use super::hooks::on_inventory_goods_received;
use super::hooks::on_logistics_shipment_inspection_requested;
use super::hooks::on_manufacturing_inspection_requested;
use super::hooks::on_procurement_quality_check_requested;
use super::state::QualityState;
use super::ai_capabilities::AI_CAPABILITIES;
use super::handlers_alerts::{acknowledge_alert, get_alert, list_alerts, list_blocks, resolve_alert};
use super::handlers_capas::{create_capa, get_capa, list_capas, transition_capa, update_capa};
use super::handlers_inspections::{cancel_inspection, create_inspection, get_inspection, list_inspections, submit_inspection};
use super::handlers_points::{activate_point, create_criterion, create_point, deactivate_point, delete_criterion, delete_point, get_point, list_criteria, list_points, update_criterion, update_point};
use super::reports::{get_settings, report_backlog, report_failure_causes, report_pass_rate, report_supplier_score, update_settings};

// ---------------------------------------------------------------------------
// Module
// ---------------------------------------------------------------------------

pub struct QualityModule {
    pub(crate) state: QualityState,
}

impl QualityModule {
    pub fn new() -> Self {
        Self {
            state: QualityState::default(),
        }
    }
}

impl Default for QualityModule {
    fn default() -> Self {
        Self::new()
    }
}

impl Module for QualityModule {
    fn id(&self) -> &'static str {
        "quality"
    }

    fn name(&self) -> &'static str {
        "Quality Management"
    }

    fn description(&self) -> &'static str {
        "Quality management: data-driven quality points with tolerance criteria, inspections with measured results, failure alerts that can block the related record until resolved, CAPA records with verification of effectiveness, and transparent quality reports (pass rate, top failure causes, backlog, supplier quality score). All scopes, kinds, policies, causes, and scoring inputs are admin-configurable settings, never hard-coded."
    }

    fn is_core(&self) -> bool {
        false
    }

    fn requires(&self) -> &[&'static str] {
        &["roles", "storage", "documents", "notifications"]
    }

    fn dependencies(&self) -> Vec<&'static str> {
        vec!["roles", "storage", "documents", "notifications"]
    }

    fn available_as_product(&self) -> bool {
        true
    }

    fn product_category(&self) -> Option<&'static str> {
        Some("productivity")
    }

    fn route_version_support(&self) -> RouteVersionSupport {
        RouteVersionSupport::All
    }

    fn init(&self, ctx: &AppContext) -> UnifiedResult<()> {

        ai::register_ai_capabilities("quality", AI_CAPABILITIES);
        *self.state.hooks.write().expect("hooks lock poisoned") = Some(Arc::clone(&ctx.hooks));
        Ok(())
    }

    fn register_hooks(&self, bus: &HookBus) {
        let state = self.state.clone();
        bus.register(
            QUALITY_BLOCKS_QUERY,
            "quality",
            Arc::new(move |ctx: &mut HookContext| on_blocks_query(state.clone(), ctx)),
        );
        let state = self.state.clone();
        bus.register(
            MANUFACTURING_INSPECTION_REQUESTED,
            "quality",
            Arc::new(move |ctx: &mut HookContext| {
                on_manufacturing_inspection_requested(state.clone(), ctx)
            }),
        );
        let state = self.state.clone();
        bus.register(
            INVENTORY_GOODS_RECEIVED,
            "quality",
            Arc::new(move |ctx: &mut HookContext| on_inventory_goods_received(state.clone(), ctx)),
        );
        let state = self.state.clone();
        bus.register(
            PROCUREMENT_QUALITY_CHECK_REQUESTED,
            "quality",
            Arc::new(move |ctx: &mut HookContext| {
                on_procurement_quality_check_requested(state.clone(), ctx)
            }),
        );
        let state = self.state.clone();
        bus.register(
            LOGISTICS_SHIPMENT_INSPECTION_REQUESTED,
            "quality",
            Arc::new(move |ctx: &mut HookContext| {
                on_logistics_shipment_inspection_requested(state.clone(), ctx)
            }),
        );
    }

    fn configure_routes(&self, cfg: &mut web::ServiceConfig) {
        let state = web::Data::new(self.state.clone());
        cfg.service(
            web::scope("/api/v1/quality")
                .app_data(state)
                .route("/points", web::get().to(list_points))
                .route("/points", web::post().to(create_point))
                .route("/points/{id}", web::get().to(get_point))
                .route("/points/{id}", web::patch().to(update_point))
                .route("/points/{id}", web::delete().to(delete_point))
                .route("/points/{id}/activate", web::post().to(activate_point))
                .route("/points/{id}/deactivate", web::post().to(deactivate_point))
                .route("/points/{id}/criteria", web::get().to(list_criteria))
                .route("/points/{id}/criteria", web::post().to(create_criterion))
                .route("/criteria/{id}", web::patch().to(update_criterion))
                .route("/criteria/{id}", web::delete().to(delete_criterion))
                .route("/inspections", web::get().to(list_inspections))
                .route("/inspections", web::post().to(create_inspection))
                .route("/inspections/{id}", web::get().to(get_inspection))
                .route(
                    "/inspections/{id}/submit",
                    web::post().to(submit_inspection),
                )
                .route(
                    "/inspections/{id}/cancel",
                    web::post().to(cancel_inspection),
                )
                .route("/alerts", web::get().to(list_alerts))
                .route("/alerts/{id}", web::get().to(get_alert))
                .route(
                    "/alerts/{id}/acknowledge",
                    web::post().to(acknowledge_alert),
                )
                .route("/alerts/{id}/resolve", web::post().to(resolve_alert))
                .route("/blocks", web::get().to(list_blocks))
                .route("/capas", web::get().to(list_capas))
                .route("/capas", web::post().to(create_capa))
                .route("/capas/{id}", web::get().to(get_capa))
                .route("/capas/{id}", web::patch().to(update_capa))
                .route("/capas/{id}/transition", web::post().to(transition_capa))
                .route("/reports/pass-rate", web::get().to(report_pass_rate))
                .route(
                    "/reports/failure-causes",
                    web::get().to(report_failure_causes),
                )
                .route("/reports/backlog", web::get().to(report_backlog))
                .route(
                    "/reports/supplier-score",
                    web::get().to(report_supplier_score),
                )
                .route("/settings", web::get().to(get_settings))
                .route("/settings", web::put().to(update_settings)),
        );
    }

    fn route_info(&self) -> Vec<RouteInfo> {
        vec![
            RouteInfo {
                method: "GET",
                path: "/api/v1/quality/points",
                owner: "quality",
                description: "List quality points (paginated, filter by scope, check_kind, on_fail_policy, active, target_ref).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/quality/points",
                owner: "quality",
                description: "Create a quality point (manager; scope, check kind, fail policy, and severity validated against settings).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/quality/points/{id}",
                owner: "quality",
                description: "Get one quality point with its criteria.",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "PATCH",
                path: "/api/v1/quality/points/{id}",
                owner: "quality",
                description: "Update a quality point (manager).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "DELETE",
                path: "/api/v1/quality/points/{id}",
                owner: "quality",
                description: "Delete a quality point with no inspections (manager); otherwise deactivate it.",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/quality/points/{id}/activate",
                owner: "quality",
                description: "Activate a quality point (manager).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/quality/points/{id}/deactivate",
                owner: "quality",
                description: "Deactivate a quality point (manager); no new inspections may be requested against it.",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/quality/points/{id}/criteria",
                owner: "quality",
                description: "List the criteria of a quality point (paginated, filter by kind).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/quality/points/{id}/criteria",
                owner: "quality",
                description: "Add a tolerance criterion to a quality point (manager; numeric bands, boolean or text expectations, per-criterion failure cause).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "PATCH",
                path: "/api/v1/quality/criteria/{id}",
                owner: "quality",
                description: "Update a criterion (manager; tolerance shape re-validated).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "DELETE",
                path: "/api/v1/quality/criteria/{id}",
                owner: "quality",
                description: "Delete a criterion not referenced by completed inspections (manager).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/quality/inspections",
                owner: "quality",
                description: "List inspections (paginated, filter by point_id, target_ref, target_kind, status, result, inspector_id).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/quality/inspections",
                owner: "quality",
                description: "Request an inspection against an active quality point (manager or inspector).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/quality/inspections/{id}",
                owner: "quality",
                description: "Get one inspection with its point and alert, if any.",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/quality/inspections/{id}/submit",
                owner: "quality",
                description: "Submit measurements; every criterion is evaluated against its stored tolerance and the inspection completes as pass / fail / conditional. A failure raises an alert and blocks the record when the point policy is block.",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/quality/inspections/{id}/cancel",
                owner: "quality",
                description: "Cancel a requested inspection (manager or inspector).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/quality/alerts",
                owner: "quality",
                description: "List quality alerts (paginated, filter by status, severity, point_id, target_ref, blocked).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/quality/alerts/{id}",
                owner: "quality",
                description: "Get one alert with its inspection and CAPAs.",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/quality/alerts/{id}/acknowledge",
                owner: "quality",
                description: "Acknowledge an open alert (manager).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/quality/alerts/{id}/resolve",
                owner: "quality",
                description: "Resolve an alert (manager); resolving a blocking alert unblocks its target.",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/quality/blocks",
                owner: "quality",
                description: "List currently blocked targets (open blocking alerts) so owning modules can refuse to proceed with them.",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/quality/capas",
                owner: "quality",
                description: "List CAPAs (paginated, filter by state, owner_id, inspection_id, overdue).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/quality/capas",
                owner: "quality",
                description: "Open a CAPA for a failed inspection (manager).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/quality/capas/{id}",
                owner: "quality",
                description: "Get one CAPA with its inspection.",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "PATCH",
                path: "/api/v1/quality/capas/{id}",
                owner: "quality",
                description: "Update a CAPA that is not closed (manager).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/quality/capas/{id}/transition",
                owner: "quality",
                description: "Move a CAPA through open -> in_progress -> verified -> closed (manager; verification needs notes when verification is required).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/quality/reports/pass-rate",
                owner: "quality",
                description: "Pass rate per product / point / period, with the calculation method in the response.",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/quality/reports/failure-causes",
                owner: "quality",
                description: "Top failure causes over failed inspections in a period, with counts and shares.",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/quality/reports/backlog",
                owner: "quality",
                description: "Inspection backlog: open alerts, blocking alerts, open and overdue CAPAs, requested inspections.",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/quality/reports/supplier-score",
                owner: "quality",
                description: "Supplier quality score from inspection history, with every input and the formula in the response.",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/quality/settings",
                owner: "quality",
                description: "Get the configurable quality settings.",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "PUT",
                path: "/api/v1/quality/settings",
                owner: "quality",
                description: "Update the configurable quality settings (manager).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
        ]
    }
}

/// Register this module with a module registry. The coordinator
/// calls this from the binary's module list; `mod.rs` is owned by
/// the coordinator, so this module never edits it itself.
pub fn register(registry: &ModuleRegistry) {
    registry.register(Arc::new(QualityModule::new()));
}
