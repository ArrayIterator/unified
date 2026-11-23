//! Module registration: routes, hooks and metadata.

use super::ai_capabilities::AI_CAPABILITIES;
use super::handlers_ai::ai_capability;
use super::handlers_entries::{
    create_entry, delete_entry, get_entry, list_entries, list_entry_history, update_entry,
};
use super::handlers_frameworks::{
    create_framework, delete_framework, get_framework, list_frameworks, update_framework,
};
use super::handlers_metrics::{
    create_metric, delete_metric, get_metric, list_metrics, update_metric,
};
use super::handlers_periods::{
    close_period, create_period, delete_period, get_period, list_periods, reopen_period,
    update_period,
};
use super::handlers_reports::{
    export_period_csv, export_period_json, get_period_summary, list_audit,
};
use super::handlers_targets::{
    create_target, delete_target, get_target, list_targets, update_target,
};
use super::hooks::{
    ai_readiness_event, ai_summarize_event, on_ai_readiness_requested, on_ai_summarize_requested,
    on_user_deleted,
};
use super::state::EsgState;
use super::types::EVENT_USER_DELETED;
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

pub struct EsgModule {
    pub(crate) state: EsgState,
}

impl EsgModule {
    pub fn new() -> Self {
        Self {
            state: EsgState::default(),
        }
    }
}

impl Default for EsgModule {
    fn default() -> Self {
        Self::new()
    }
}

impl Module for EsgModule {
    fn id(&self) -> &'static str {
        "esg"
    }

    fn name(&self) -> &'static str {
        "ESG"
    }

    fn description(&self) -> &'static str {
        "Environmental, Social, and Governance sustainability reporting: admin-defined metric definitions (category, unit, data type, reporting frequency, framework tags) and reporting-framework definitions as data, reporting periods with open / closed status, metric values per period per scope (company / site reference ids) with source notes, document references, and full value history, per-metric targets per period with direction and deadline plus exact progress math in basis points, period summaries (metrics by category, completion, target attainment) with JSON and CSV export, an append-only audit trail of entry / target / period changes, and an AI capability that summarizes a period from its stored facts. Numbers are decimal strings computed on an integer-mantissa decimal type — f64 is never used for a stored or returned number."
    }

    fn is_core(&self) -> bool {
        false
    }

    fn requires(&self) -> &[&'static str] {
        &["users", "roles", "notifications"]
    }

    fn dependencies(&self) -> Vec<&'static str> {
        vec!["users", "roles", "notifications"]
    }

    fn available_as_product(&self) -> bool {
        true
    }

    fn product_category(&self) -> Option<&'static str> {
        Some("sustainability")
    }

    fn init(&self, ctx: &AppContext) -> UnifiedResult<()> {
        ai::register_ai_capabilities("esg", AI_CAPABILITIES);
        *self.state.hooks.write().expect("hooks lock poisoned") = Some(Arc::clone(&ctx.hooks));
        Ok(())
    }

    fn register_hooks(&self, bus: &HookBus) {
        let state = self.state.clone();
        bus.register(
            EVENT_USER_DELETED,
            "esg",
            Arc::new(move |ctx: &mut HookContext| on_user_deleted(state.clone(), ctx)),
        );
        // The AI module's runner emits these grounding events for
        // the summary and readiness capabilities; this module
        // answers with the period's stored facts.
        let state = self.state.clone();
        bus.register(
            ai_summarize_event(),
            "esg",
            Arc::new(move |ctx: &mut HookContext| on_ai_summarize_requested(state.clone(), ctx)),
        );
        let state = self.state.clone();
        bus.register(
            ai_readiness_event(),
            "esg",
            Arc::new(move |ctx: &mut HookContext| on_ai_readiness_requested(state.clone(), ctx)),
        );
    }

    fn configure_routes(&self, cfg: &mut web::ServiceConfig) {
        let state = web::Data::new(self.state.clone());
        cfg.service(
            web::scope("/api/v1/esg")
                .app_data(state)
                .route("/frameworks", web::get().to(list_frameworks))
                .route("/frameworks", web::post().to(create_framework))
                .route("/frameworks/{framework_id}", web::get().to(get_framework))
                .route(
                    "/frameworks/{framework_id}",
                    web::patch().to(update_framework),
                )
                .route(
                    "/frameworks/{framework_id}",
                    web::delete().to(delete_framework),
                )
                .route("/metrics", web::get().to(list_metrics))
                .route("/metrics", web::post().to(create_metric))
                .route("/metrics/{metric_id}", web::get().to(get_metric))
                .route("/metrics/{metric_id}", web::patch().to(update_metric))
                .route("/metrics/{metric_id}", web::delete().to(delete_metric))
                .route("/periods", web::get().to(list_periods))
                .route("/periods", web::post().to(create_period))
                .route("/periods/{period_id}", web::get().to(get_period))
                .route("/periods/{period_id}", web::patch().to(update_period))
                .route("/periods/{period_id}", web::delete().to(delete_period))
                .route("/periods/{period_id}/close", web::post().to(close_period))
                .route("/periods/{period_id}/reopen", web::post().to(reopen_period))
                .route(
                    "/periods/{period_id}/summary",
                    web::get().to(get_period_summary),
                )
                .route(
                    "/periods/{period_id}/export.json",
                    web::get().to(export_period_json),
                )
                .route(
                    "/periods/{period_id}/export.csv",
                    web::get().to(export_period_csv),
                )
                .route("/entries", web::get().to(list_entries))
                .route("/entries", web::post().to(create_entry))
                .route("/entries/{entry_id}", web::get().to(get_entry))
                .route("/entries/{entry_id}", web::patch().to(update_entry))
                .route("/entries/{entry_id}", web::delete().to(delete_entry))
                .route(
                    "/entries/{entry_id}/history",
                    web::get().to(list_entry_history),
                )
                .route("/targets", web::get().to(list_targets))
                .route("/targets", web::post().to(create_target))
                .route("/targets/{target_id}", web::get().to(get_target))
                .route("/targets/{target_id}", web::patch().to(update_target))
                .route("/targets/{target_id}", web::delete().to(delete_target))
                .route("/audit", web::get().to(list_audit))
                .route("/ai/{capability}", web::post().to(ai_capability)),
        );
    }

    fn route_info(&self) -> Vec<RouteInfo> {
        let auth = unified_core::auth::RouteAuth::Authenticated;
        vec![
            RouteInfo {
                method: "GET",
                path: "/api/v1/esg/frameworks",
                owner: "esg",
                description: "List reporting frameworks (paginated, search by name/description).",
                auth,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/esg/frameworks",
                owner: "esg",
                description: "Create a reporting framework definition (manager).",
                auth,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/esg/frameworks/{framework_id}",
                owner: "esg",
                description: "Get one reporting framework.",
                auth,
            },
            RouteInfo {
                method: "PATCH",
                path: "/api/v1/esg/frameworks/{framework_id}",
                owner: "esg",
                description: "Update a reporting framework definition (manager).",
                auth,
            },
            RouteInfo {
                method: "DELETE",
                path: "/api/v1/esg/frameworks/{framework_id}",
                owner: "esg",
                description: "Delete a reporting framework definition; metrics that tagged it keep their tags (manager).",
                auth,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/esg/metrics",
                owner: "esg",
                description: "List metric definitions (paginated, filter by category, framework, data type, active; search by name/description/unit).",
                auth,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/esg/metrics",
                owner: "esg",
                description: "Create a metric definition with category, unit, data type, frequency, and framework tags (manager).",
                auth,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/esg/metrics/{metric_id}",
                owner: "esg",
                description: "Get one metric definition.",
                auth,
            },
            RouteInfo {
                method: "PATCH",
                path: "/api/v1/esg/metrics/{metric_id}",
                owner: "esg",
                description: "Update a metric definition (manager).",
                auth,
            },
            RouteInfo {
                method: "DELETE",
                path: "/api/v1/esg/metrics/{metric_id}",
                owner: "esg",
                description: "Delete a metric definition; its entries, history, and targets are kept (manager).",
                auth,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/esg/periods",
                owner: "esg",
                description: "List reporting periods (paginated, filter by status; search by code/name).",
                auth,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/esg/periods",
                owner: "esg",
                description: "Create a reporting period with a code and a date range (manager).",
                auth,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/esg/periods/{period_id}",
                owner: "esg",
                description: "Get one reporting period.",
                auth,
            },
            RouteInfo {
                method: "PATCH",
                path: "/api/v1/esg/periods/{period_id}",
                owner: "esg",
                description: "Update a reporting period's code, name, or date range (manager).",
                auth,
            },
            RouteInfo {
                method: "DELETE",
                path: "/api/v1/esg/periods/{period_id}",
                owner: "esg",
                description: "Delete a reporting period; its entries, history, and targets are kept (manager).",
                auth,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/esg/periods/{period_id}/close",
                owner: "esg",
                description: "Close a reporting period: entries and targets can no longer be written for it; emits esg.period.closed (manager).",
                auth,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/esg/periods/{period_id}/reopen",
                owner: "esg",
                description: "Reopen a closed reporting period (manager).",
                auth,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/esg/periods/{period_id}/summary",
                owner: "esg",
                description: "Get the summary of a reporting period: metrics by category, completion, and target attainment.",
                auth,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/esg/periods/{period_id}/export.json",
                owner: "esg",
                description: "Export a reporting period as JSON (period, summary, entries, targets).",
                auth,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/esg/periods/{period_id}/export.csv",
                owner: "esg",
                description: "Export a reporting period as CSV (one row per metric and scope).",
                auth,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/esg/entries",
                owner: "esg",
                description: "List metric values (paginated, filter by metric, period, scope, entered_by; search value/source note).",
                auth,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/esg/entries",
                owner: "esg",
                description: "Record a metric value for a period and scope; a second value for the same combination updates the entry and appends a history row (manager).",
                auth,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/esg/entries/{entry_id}",
                owner: "esg",
                description: "Get one metric value.",
                auth,
            },
            RouteInfo {
                method: "PATCH",
                path: "/api/v1/esg/entries/{entry_id}",
                owner: "esg",
                description: "Update a metric value, source note, or document references; appends a history row (manager).",
                auth,
            },
            RouteInfo {
                method: "DELETE",
                path: "/api/v1/esg/entries/{entry_id}",
                owner: "esg",
                description: "Delete a metric value; its history and audit rows are kept (manager).",
                auth,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/esg/entries/{entry_id}/history",
                owner: "esg",
                description: "List the value history of one entry, oldest first (paginated).",
                auth,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/esg/targets",
                owner: "esg",
                description: "List targets (paginated, filter by metric, period, scope, direction).",
                auth,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/esg/targets",
                owner: "esg",
                description: "Set a target for a metric, period, and scope; setting it again replaces it (manager).",
                auth,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/esg/targets/{target_id}",
                owner: "esg",
                description: "Get one target.",
                auth,
            },
            RouteInfo {
                method: "PATCH",
                path: "/api/v1/esg/targets/{target_id}",
                owner: "esg",
                description: "Update a target's value, direction, or deadline (manager).",
                auth,
            },
            RouteInfo {
                method: "DELETE",
                path: "/api/v1/esg/targets/{target_id}",
                owner: "esg",
                description: "Delete a target; its audit rows are kept (manager).",
                auth,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/esg/audit",
                owner: "esg",
                description: "List the audit trail of entry, target, and period changes (paginated, filter by entity type, entity, action, actor; newest first).",
                auth,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/esg/ai/{capability}",
                owner: "esg",
                description: "Run one registered ESG AI capability through the AI module's gated runner.",
                auth,
            },
        ]
    }

    fn route_version_support(&self) -> RouteVersionSupport {
        RouteVersionSupport::All
    }
}

pub fn register(registry: &ModuleRegistry) {
    registry.register(Arc::new(EsgModule::new()));
}
