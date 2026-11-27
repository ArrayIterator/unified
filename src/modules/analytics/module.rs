//! analytics - module.

// Analytics and reports - sales, licenses, usage, uptime and revenue.
//
// Every route below requires an authenticated caller (`X-User-Id` +
// `X-Role` headers, capability checked through the `role.check`
// hook, fail-closed): `analytics.view` for reads,
// `analytics.manage` for writes. A missing identity header is
// 401; a role without the capability is 403.
//
// Routes under `/api/v1/analytics`:
//
//   Dashboard and summaries:
//     GET    /dashboard              key numbers: revenue 30d, active
//                                    licenses, uptime %, open tasks,
//                                    capacity headroom
//     GET    /sales                  recorded sales and refunds (paginated)
//     POST   /sales                  record a sale or refund (fallback
//                                    when no store event arrives)
//     GET    /sales/summary          revenue by day / product / currency
//     GET    /licenses/summary       license counts by status and product
//     GET    /usage/summary          API calls, downloads, logins by kind
//     GET    /usage                  recorded metric samples (paginated)
//     POST   /metrics                record one metric sample
//     GET    /uptime/summary         uptime % from health samples
//     GET    /health                 recorded health samples (paginated)
//     POST   /health                 record one health sample
//     GET    /revenue/summary        totals by currency, refunds deducted
//   Report definitions:
//     GET    /reports                list report definitions (paginated)
//     POST   /reports                create a report definition
//     GET    /reports/{id}           one definition
//     PUT    /reports/{id}           update a definition
//     DELETE /reports/{id}           delete a definition (and its runs)
//     POST   /reports/{id}/run       generate a report run now
//     GET    /reports/{id}/runs      runs of a definition (paginated)
//     GET    /reports/{id}/export    download the latest run as CSV or JSON
//   Report runs:
//     GET    /runs/{id}              one run, rows paginated
//   Scheduled reports:
//     GET    /scheduled-reports      list scheduled reports (paginated)
//     POST   /scheduled-reports      create a scheduled report
//     GET    /scheduled-reports/{id}  one scheduled report
//     PUT    /scheduled-reports/{id}  update a scheduled report
//     DELETE /scheduled-reports/{id}  delete a scheduled report
//     POST   /scheduled-reports/{id}/run-now  run now and enqueue the
//                                    scheduler job (see note below)
//
// Events consumed (listeners registered in `register_hooks`):
//
//   store.order.completed   -> sale record (gross revenue)
//   store.order.refunded    -> refund record (deducted from revenue)
//   license.issued          -> license snapshot, status active
//   license.activated       -> license snapshot, status active
//   license.suspended       -> license snapshot, status suspended
//   license.revoked         -> license snapshot, status revoked
//   download.finished       -> metric sample, kind `download`
//   auth.login              -> metric sample, kind `login`
//   cluster.health.changed  -> health sample for the node
//   task.created / task.status.changed / task.completed
//                           -> open-task mirror for the dashboard
//   planner.pool.created / planner.plan.approved /
//   planner.plan.activated / planner.plan.expired
//                           -> capacity mirror for dashboard headroom
//   analytics.report.run_requested
//                           -> run a report (integration point for the
//                             scheduler once `report.generate` is a
//                             known scheduler job kind)
//
// Notes on the sources, stated honestly:
//
//   - The store module swaps its private hook bus for the global bus in
//     its `init`, so `store.order.*` events do reach the global bus and
//     these listeners see them. When no store event arrives (for
//     example sales imported from elsewhere), POST /sales records the
//     same shape manually.
//   - `license.deactivated` is deliberately NOT mapped to a license
//     status: the licensing module emits it when one activation (one
//     device) is removed, not when the license itself changes state.
//   - The service monitor emits no health events today, so uptime comes
//     from cluster health events plus POST /health samples. With no
//     samples at all the dashboard reports `uptime_pct: null` instead
//     of inventing a number.
//   - Scheduled reports: the scheduler only accepts the job kinds
//     `mail.send`, `geo.update`, `ssl.renew` and `cleanup` today, so
//     `scheduler.enqueue` with kind `report.generate` is refused until
//     the scheduler gains that kind (snippet in the coordinator
//     report). Run-now still runs the report synchronously, so it
//     always produces a run; the enqueue result is reported in the
//     response instead of being hidden.
//
// Events emitted on the global hook bus:
//
//   analytics.report.generated   a report run finished
//   analytics.report.exported    a report export was downloaded
//
// Storage is in memory (DashMap), the same pattern as the store and
// scheduler modules. Durable persistence is a follow-up for the
// integrator: the migration sketch is in the coordinator report.

use unified_core::error::UnifiedResult;
use unified_core::hooks::{events, HookBus, HookContext};
use unified_core::modules::{Module, ModuleRegistry, RouteInfo};
use crate::modules::ai;
use actix_web::{ http::header, web };
use std::sync::Arc;
use super::{events::*, helpers::*, types::*, views::*, inputs::*, state::*, hooks::*, reports::*, handlers_dashboard::*, handlers_definitions::*, handlers_runs::*, handlers_scheduled::*};
use super::hooks::record_metric as record_hook_metric;
use super::ai_capabilities::AI_CAPABILITIES;



// Module definition.
pub struct AnalyticsModule {
    pub(crate) state: Arc<AnalyticsState>,
}


impl AnalyticsModule {
    pub fn new() -> Self {
        Self {
            state: Arc::new(AnalyticsState::new()),
        }
    }
}


impl Default for AnalyticsModule {
    fn default() -> Self {
        Self::new()
    }
}


impl Module for AnalyticsModule {
    fn id(&self) -> &'static str {
        "analytics"
    }

    fn name(&self) -> &'static str {
        "Analytics & Reports"
    }

    fn description(&self) -> &'static str {
        "Sales, license, usage, uptime and revenue analytics with report definitions, runs, CSV/JSON export and scheduled reports."
    }

    fn register_hooks(&self, bus: &HookBus) {
        let state = self.state.clone();
        bus.register(
            STORE_ORDER_COMPLETED,
            "analytics",
            Arc::new(move |ctx: &mut HookContext| {
                on_store_order_completed(state.clone(), &ctx.data.clone());
            }),
        );
        let state = self.state.clone();
        bus.register(
            STORE_ORDER_REFUNDED,
            "analytics",
            Arc::new(move |ctx: &mut HookContext| {
                on_store_order_refunded(state.clone(), &ctx.data.clone());
            }),
        );
        let state = self.state.clone();
        bus.register(
            events::LICENSE_ISSUED,
            "analytics",
            Arc::new(move |ctx: &mut HookContext| {
                upsert_license(&state, &ctx.data.clone(), "active");
            }),
        );
        let state = self.state.clone();
        bus.register(
            LICENSE_ACTIVATED,
            "analytics",
            Arc::new(move |ctx: &mut HookContext| {
                upsert_license(&state, &ctx.data.clone(), "active");
            }),
        );
        let state = self.state.clone();
        bus.register(
            LICENSE_SUSPENDED,
            "analytics",
            Arc::new(move |ctx: &mut HookContext| {
                upsert_license(&state, &ctx.data.clone(), "suspended");
            }),
        );
        let state = self.state.clone();
        bus.register(
            events::LICENSE_REVOKED,
            "analytics",
            Arc::new(move |ctx: &mut HookContext| {
                upsert_license(&state, &ctx.data.clone(), "revoked");
            }),
        );
        let state = self.state.clone();
        bus.register(
            events::DOWNLOAD_FINISHED,
            "analytics",
            Arc::new(move |_ctx: &mut HookContext| {
                record_hook_metric(&state, "download", Some("store".to_string()));
            }),
        );
        let state = self.state.clone();
        bus.register(
            events::AUTH_LOGIN,
            "analytics",
            Arc::new(move |_ctx: &mut HookContext| {
                record_hook_metric(&state, "login", Some("auth".to_string()));
            }),
        );
        let state = self.state.clone();
        bus.register(
            CLUSTER_HEALTH_CHANGED,
            "analytics",
            Arc::new(move |ctx: &mut HookContext| {
                on_cluster_health_changed(state.clone(), &ctx.data.clone());
            }),
        );
        for event in [TASK_CREATED, TASK_STATUS_CHANGED, TASK_COMPLETED] {
            let state = self.state.clone();
            bus.register(
                event,
                "analytics",
                Arc::new(move |ctx: &mut HookContext| {
                    on_task_event(state.clone(), &ctx.data.clone());
                }),
            );
        }
        let state = self.state.clone();
        bus.register(
            PLANNER_POOL_CREATED,
            "analytics",
            Arc::new(move |ctx: &mut HookContext| {
                on_planner_pool_created(state.clone(), &ctx.data.clone());
            }),
        );
        for (event, active) in [
            (PLANNER_PLAN_APPROVED, true),
            (PLANNER_PLAN_ACTIVATED, true),
            (PLANNER_PLAN_EXPIRED, false),
        ] {
            let state = self.state.clone();
            bus.register(
                event,
                "analytics",
                Arc::new(move |ctx: &mut HookContext| {
                    on_planner_plan_event(state.clone(), &ctx.data.clone(), active);
                }),
            );
        }
        let state = self.state.clone();
        bus.register(
            ANALYTICS_REPORT_RUN_REQUESTED,
            "analytics",
            Arc::new(move |ctx: &mut HookContext| {
                on_report_run_requested(state.clone(), &ctx.data.clone());
            }),
        );
    }

    fn init(&self, ctx: &unified_core::context::AppContext) -> UnifiedResult<()> {

        ai::register_ai_capabilities("analytics", AI_CAPABILITIES);
        *self
            .state
            .hooks
            .write()
            .expect("hooks lock poisoned") = ctx.hooks.clone();
        Ok(())
    }

    fn configure_routes(&self, cfg: &mut web::ServiceConfig) {
        cfg.app_data(web::Data::new(self.state.clone()));
        cfg.service(
            web::scope("/api/v1/analytics")
                .route("/dashboard", web::get().to(get_dashboard))
                .route("/sales", web::get().to(list_sales))
                .route("/sales", web::post().to(record_sale))
                .route("/sales/summary", web::get().to(get_sales_summary))
                .route("/licenses/summary", web::get().to(get_license_summary))
                .route("/revenue/summary", web::get().to(get_revenue_summary))
                .route("/usage", web::get().to(list_metrics))
                .route("/usage/summary", web::get().to(get_usage_summary))
                .route("/metrics", web::post().to(super::handlers_dashboard::record_metric_handler))
                .route("/health", web::get().to(list_health))
                .route("/health", web::post().to(record_health))
                .route("/uptime/summary", web::get().to(get_uptime_summary))
                .route("/reports", web::get().to(list_reports))
                .route("/reports", web::post().to(create_report))
                .route("/reports/{id}", web::get().to(get_report))
                .route("/reports/{id}", web::put().to(update_report))
                .route("/reports/{id}", web::delete().to(delete_report))
                .route("/reports/{id}/run", web::post().to(run_report_now))
                .route("/reports/{id}/runs", web::get().to(list_runs))
                .route("/reports/{id}/export", web::get().to(export_report))
                .route("/runs/{id}", web::get().to(get_run))
                .route("/scheduled-reports", web::get().to(list_scheduled))
                .route("/scheduled-reports", web::post().to(create_scheduled))
                .route("/scheduled-reports/{id}", web::get().to(get_scheduled))
                .route("/scheduled-reports/{id}", web::put().to(update_scheduled))
                .route("/scheduled-reports/{id}", web::delete().to(delete_scheduled))
                .route("/scheduled-reports/{id}/run-now", web::post().to(run_scheduled_now)),
        );
    }

    fn route_info(&self) -> Vec<RouteInfo> {
        vec![
            RouteInfo { method: "GET", path: "/api/v1/analytics/dashboard", owner: "analytics", description: "Dashboard summary: revenue 30d, active licenses, uptime %, open tasks, capacity headroom.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/analytics/sales", owner: "analytics", description: "List recorded sales and refunds.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/analytics/sales", owner: "analytics", description: "Record a sale or refund manually.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/analytics/sales/summary", owner: "analytics", description: "Revenue by day for a date range.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/analytics/licenses/summary", owner: "analytics", description: "License counts by status and product.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/analytics/revenue/summary", owner: "analytics", description: "Revenue totals by currency, refunds deducted.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/analytics/usage", owner: "analytics", description: "List recorded metric samples.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/analytics/usage/summary", owner: "analytics", description: "Usage counts by kind and day.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/analytics/metrics", owner: "analytics", description: "Record one metric sample (api_call, download, login, upload).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/analytics/health", owner: "analytics", description: "List recorded health samples.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/analytics/health", owner: "analytics", description: "Record one health sample.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/analytics/uptime/summary", owner: "analytics", description: "Uptime percentage from health samples.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/analytics/reports", owner: "analytics", description: "List report definitions.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/analytics/reports", owner: "analytics", description: "Create a report definition.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/analytics/reports/{id}", owner: "analytics", description: "Get one report definition.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "PUT", path: "/api/v1/analytics/reports/{id}", owner: "analytics", description: "Update a report definition.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "DELETE", path: "/api/v1/analytics/reports/{id}", owner: "analytics", description: "Delete a report definition and its runs.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/analytics/reports/{id}/run", owner: "analytics", description: "Generate a report run now.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/analytics/reports/{id}/runs", owner: "analytics", description: "List runs of a report definition.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/analytics/reports/{id}/export", owner: "analytics", description: "Download the latest run as CSV or JSON (raw file).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/analytics/runs/{id}", owner: "analytics", description: "Get one report run with paginated rows.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/analytics/scheduled-reports", owner: "analytics", description: "List scheduled reports.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/analytics/scheduled-reports", owner: "analytics", description: "Create a scheduled report.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/analytics/scheduled-reports/{id}", owner: "analytics", description: "Get one scheduled report.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "PUT", path: "/api/v1/analytics/scheduled-reports/{id}", owner: "analytics", description: "Update a scheduled report.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "DELETE", path: "/api/v1/analytics/scheduled-reports/{id}", owner: "analytics", description: "Delete a scheduled report.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/analytics/scheduled-reports/{id}/run-now", owner: "analytics", description: "Run a scheduled report now and enqueue the scheduler job.", auth: unified_core::auth::RouteAuth::Authenticated},
        ]
    }
}


pub fn register(registry: &ModuleRegistry) {
    registry.register(Arc::new(AnalyticsModule::new()));
}
