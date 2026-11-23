// bi module -- Module definition, registration, and route table.

use unified_core::context::AppContext;
use unified_core::error::UnifiedResult;
use unified_core::hooks::{HookBus, HookContext};
use unified_core::json;
use unified_core::modules::{Module, ModuleRegistry, RouteInfo, RouteVersionSupport};
use crate::modules::ai;
use actix_web::web;
use serde_json::json;
use std::sync::Arc;

use super::*;

// ---------------------------------------------------------------------------
// Module
// ---------------------------------------------------------------------------

pub struct BiModule {
    pub(crate) state: BiState,
}

impl BiModule {
    pub fn new() -> Self {
        Self {
            state: BiState::default(),
        }
    }
}

impl Default for BiModule {
    fn default() -> Self {
        Self::new()
    }
}

impl Module for BiModule {
    fn id(&self) -> &'static str {
        "bi"
    }

    fn name(&self) -> &'static str {
        "Business Intelligence + AI Insights"
    }

    fn description(&self) -> &'static str {
        "Custom report builder over source-module data, scheduled report delivery, transparent revenue and cash-flow forecasting (moving average plus linear trend), churn signals, and KPI dashboards. Aggregation runs over the in-memory mirror fed by source events plus {source}.data.requested read hooks -- never raw SQL against other modules' tables."
    }

    fn is_core(&self) -> bool {
        false
    }

    fn requires(&self) -> &[&'static str] {
        &[
            "users",
            "roles",
            "billing",
            "store",
            "notifications",
            "scheduler",
        ]
    }

    fn dependencies(&self) -> Vec<&'static str> {
        vec![
            "users",
            "roles",
            "billing",
            "store",
            "notifications",
            "scheduler",
        ]
    }

    fn available_as_product(&self) -> bool {
        true
    }

    fn product_category(&self) -> Option<&'static str> {
        Some("productivity")
    }

    fn init(&self, ctx: &AppContext) -> UnifiedResult<()> {

        ai::register_ai_capabilities("bi", AI_CAPABILITIES);
        *self
            .state
            .hooks
            .write()
            .expect("hooks lock poisoned") = Some(Arc::clone(&ctx.hooks));
        Ok(())
    }

    fn register_hooks(&self, bus: &HookBus) {
        let state = self.state.clone();
        bus.register(
            "invoice.payment.recorded",
            "bi",
            Arc::new(move |ctx: &mut HookContext| on_invoice_payment(state.clone(), ctx)),
        );
        let state = self.state.clone();
        bus.register(
            "invoice.paid",
            "bi",
            Arc::new(move |ctx: &mut HookContext| on_invoice_payment(state.clone(), ctx)),
        );
        let state = self.state.clone();
        bus.register(
            "payment.captured",
            "bi",
            Arc::new(move |ctx: &mut HookContext| on_payment_captured(state.clone(), ctx)),
        );
        let state = self.state.clone();
        bus.register(
            "store.order.completed",
            "bi",
            Arc::new(move |ctx: &mut HookContext| {
                on_store_order_completed(state.clone(), ctx)
            }),
        );
        let state = self.state.clone();
        bus.register(
            "order.paid",
            "bi",
            Arc::new(move |ctx: &mut HookContext| {
                on_store_order_completed(state.clone(), ctx)
            }),
        );
        let state = self.state.clone();
        bus.register(
            "store.order.refunded",
            "bi",
            Arc::new(move |ctx: &mut HookContext| {
                on_store_order_refunded(state.clone(), ctx)
            }),
        );
        for (event, kind) in [
            ("ticket.created", "ticket_created"),
            ("ticket.updated", "ticket_updated"),
        ] {
            let state = self.state.clone();
            let kind = kind.to_string();
            bus.register(
                event,
                "bi",
                Arc::new(move |ctx: &mut HookContext| {
                    on_ticket(state.clone(), &kind, ctx)
                }),
            );
        }
        for (event, kind) in [
            ("license.issued", "license_issued"),
            ("license.activated", "license_activated"),
            ("license.suspended", "license_suspended"),
            ("license.revoked", "license_revoked"),
            ("license.deactivated", "license_deactivated"),
        ] {
            let state = self.state.clone();
            let kind = kind.to_string();
            bus.register(
                event,
                "bi",
                Arc::new(move |ctx: &mut HookContext| {
                    on_license(state.clone(), &kind, ctx)
                }),
            );
        }
        for (event, kind) in [
            ("project.task.completed", "task_completed"),
            ("task.completed", "task_completed"),
            ("task.created", "task_created"),
        ] {
            let state = self.state.clone();
            let kind = kind.to_string();
            bus.register(
                event,
                "bi",
                Arc::new(move |ctx: &mut HookContext| {
                    on_task(state.clone(), &kind, ctx)
                }),
            );
        }
        let state = self.state.clone();
        bus.register(
            "project.created",
            "bi",
            Arc::new(move |ctx: &mut HookContext| {
                on_project(state.clone(), "project_created", ctx)
            }),
        );
    }

    fn configure_routes(&self, cfg: &mut web::ServiceConfig) {
        let state = web::Data::new(self.state.clone());
        cfg.service(
            web::scope("/api/v1/bi")
                .app_data(state)
                .route("/reports", web::get().to(list_reports))
                .route("/reports", web::post().to(create_report))
                .route("/reports/{id}", web::get().to(get_report))
                .route("/reports/{id}", web::patch().to(update_report))
                .route("/reports/{id}", web::delete().to(delete_report))
                .route("/reports/{id}/run", web::post().to(run_report))
                .route("/reports/{id}/export", web::get().to(export_report))
                .route("/reports/{id}/schedules", web::get().to(list_schedules))
                .route("/reports/{id}/schedules", web::post().to(create_schedule))
                .route(
                    "/reports/{id}/schedules/{schedule_id}",
                    web::get().to(get_schedule),
                )
                .route(
                    "/reports/{id}/schedules/{schedule_id}",
                    web::delete().to(delete_schedule),
                )
                .route(
                    "/reports/{id}/schedules/{schedule_id}/run-now",
                    web::post().to(run_schedule_now),
                )
                .route("/forecast/revenue", web::get().to(forecast_revenue))
                .route("/forecast/cashflow", web::get().to(forecast_cashflow))
                .route("/forecast/churn", web::get().to(forecast_churn))
                .route("/kpi-dashboards", web::get().to(list_dashboards))
                .route("/kpi-dashboards", web::post().to(create_dashboard))
                .route("/kpi-dashboards/{id}", web::get().to(get_dashboard))
                .route("/kpi-dashboards/{id}", web::patch().to(update_dashboard))
                .route("/kpi-dashboards/{id}", web::delete().to(delete_dashboard))
                .route(
                    "/kpi-dashboards/{id}/compute",
                    web::get().to(compute_dashboard),
                ),
        );
    }

    fn route_info(&self) -> Vec<RouteInfo> {
        vec![
            RouteInfo { method: "GET", path: "/api/v1/bi/reports", owner: "bi", description: "List saved reports (paginated, filter by source, company_id, visibility, owner_id).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/bi/reports", owner: "bi", description: "Create a saved report (source, fields, filters, grouping, chart type, shared vs private).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/bi/reports/{id}", owner: "bi", description: "Get one saved report (owner or shared in the same company).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "PATCH", path: "/api/v1/bi/reports/{id}", owner: "bi", description: "Update a saved report (owner or manage capability).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "DELETE", path: "/api/v1/bi/reports/{id}", owner: "bi", description: "Delete a saved report (and its schedules).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/bi/reports/{id}/run", owner: "bi", description: "Execute a saved report (rows + totals; insufficient-data error when the source has no rows).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/bi/reports/{id}/export", owner: "bi", description: "Export a report run as CSV or JSON (?format=csv|json, max 10,000 rows).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/bi/reports/{id}/schedules", owner: "bi", description: "List schedules of a report.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/bi/reports/{id}/schedules", owner: "bi", description: "Create a scheduled delivery (queued as bi.report.deliver; scheduler_status records queued/declined).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/bi/reports/{id}/schedules/{schedule_id}", owner: "bi", description: "Get one schedule.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "DELETE", path: "/api/v1/bi/reports/{id}/schedules/{schedule_id}", owner: "bi", description: "Delete a schedule.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/bi/reports/{id}/schedules/{schedule_id}/run-now", owner: "bi", description: "Run a scheduled report now and deliver it (emits bi.report.delivered).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/bi/forecast/revenue", owner: "bi", description: "Revenue forecast: 7-day moving average + linear trend; response carries method, inputs, and horizon.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/bi/forecast/cashflow", owner: "bi", description: "Cash-flow projection (net of refunds): 7-day moving average + linear trend; response carries method, inputs, and horizon.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/bi/forecast/churn", owner: "bi", description: "Churn signals: customers with no activity in N days plus suspended/revoked licenses and refunded orders.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/bi/kpi-dashboards", owner: "bi", description: "List KPI dashboards (paginated, filter by company_id, owner_id).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/bi/kpi-dashboards", owner: "bi", description: "Create a KPI dashboard (metric, source, period per KPI).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/bi/kpi-dashboards/{id}", owner: "bi", description: "Get one KPI dashboard.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "PATCH", path: "/api/v1/bi/kpi-dashboards/{id}", owner: "bi", description: "Update a KPI dashboard.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "DELETE", path: "/api/v1/bi/kpi-dashboards/{id}", owner: "bi", description: "Delete a KPI dashboard.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/bi/kpi-dashboards/{id}/compute", owner: "bi", description: "Compute every KPI of a dashboard on read (null + insufficient_data when a source has no rows).", auth: unified_core::auth::RouteAuth::Authenticated},
        ]
    }

    fn route_version_support(&self) -> RouteVersionSupport {
        RouteVersionSupport::All
    }
}

pub fn register(registry: &ModuleRegistry) {
    registry.register(Arc::new(BiModule::new()));
}
