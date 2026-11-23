// Data Tools module — orphaned-data resolver, expired-data purging,
// and cleanup rules + runs.
//
// What it owns: the `cleanup_rules` + `cleanup_runs` tables
// (migration 000125), the minimal `document_shares` table (no sharing
// feature exists yet — see the migration header), the resolver, the
// scan / run endpoints, and the admin page. What it does NOT own:
// the Scheduler's WHEN (the `cleanup` kind emits
// `cleanup.requested`, this module answers it), other modules'
// tables (reached ONLY through the `data_tools.cleanup.<target>`
// hook — never directly), backups, or Postgres VACUUM.
//
// Routes live under `/api/v1/data-tools`:
//
//   Rules:
//     GET    /rules                         list retention rules
//     PATCH  /rules/{id}                    tune one rule (admin)
//   Scan / run:
//     POST   /scan                          dry-run preview (counts, zero deletes)
//     POST   /run                           execute a cleanup (dry_run flag)
//     GET    /runs                          run history (paginated)
//   Admin page:
//     GET    /admin                         rules table + scan now + run + history (HTML)
//
// Events emitted on the hook bus (never direct calls to other modules):
//   - `data_tools.cleanup.<target>` (per target: documents sessions,
//     revoked tokens, downloads, spool temps, cross-check) — the
//     owning module answers with counts + deletes.
//   - `data_tools.run.started` / `data_tools.run.completed` (the run
//     audit events; the `cleanup_runs` row is the audit record).
//   - `data_tools.scan.completed` (after every dry-run scan).
// This module also LISTENS to the scheduler:
//   - `cleanup.requested`: runs a cleanup with the run recorded as
//     `triggered_by: scheduler`.
//
// Authentication: every route is gated by `authorize` in
// `validation.rs` — the verified caller plus the `data_tools:read`
// (reads) or `data_tools:admin` (changes, scans, runs, admin page)
// capability through the `role.check` hook. The check fails closed
// when the roles module does not answer. Rules are platform
// control-plane objects (like the Scheduler's jobs), so the gate is
// caller identity plus permission, not per-tenant scoping.

use super::*;
use crate::modules::ai;
use actix_web::web;
use std::sync::Arc;
use unified_core::context::AppContext;
use unified_core::error::UnifiedResult;
use unified_core::hooks::{HookBus, HookContext};
use unified_core::modules::{Module, ModuleRegistry, RouteInfo};

use super::events::on_cleanup_requested;

// ---------------------------------------------------------------------------
// Module
// ---------------------------------------------------------------------------

pub struct DataToolsModule {
    state: Arc<DataToolsState>,
}

impl DataToolsModule {
    pub fn new() -> Self {
        Self {
            state: Arc::new(DataToolsState::new()),
        }
    }

    /// Test-only access to the module state, so tests can prove
    /// what `init` seeded and what a run recorded.
    #[cfg(test)]
    pub(crate) fn state_for_test(&self) -> Arc<DataToolsState> {
        self.state.clone()
    }
}

impl Default for DataToolsModule {
    fn default() -> Self {
        Self::new()
    }
}

impl Module for DataToolsModule {
    fn id(&self) -> &'static str {
        "data_tools"
    }

    fn name(&self) -> &'static str {
        "Data Tools"
    }

    fn description(&self) -> &'static str {
        "Data Tools: orphaned-data resolver, expired-data purging, and cleanup rules with dry-run scan and run history."
    }

    fn requires(&self) -> &[&'static str] {
        &["roles"]
    }

    fn requires_libraries(&self) -> &[&'static str] {
        &["dashmap"]
    }

    fn init(&self, ctx: &AppContext) -> UnifiedResult<()> {
        ai::register_ai_capabilities("data_tools", AI_CAPABILITIES);
        *self.state.hooks.write().expect("hooks lock poisoned") = ctx.hooks.clone();
        Ok(())
    }

    fn register_hooks(&self, bus: &HookBus) {
        // The Scheduler owns WHEN: a `cleanup` job emits
        // `cleanup.requested` and this module runs the cleanup.
        let state = self.state.clone();
        bus.register(
            events::CLEANUP_REQUESTED,
            "data_tools",
            Arc::new(move |ctx: &mut HookContext| on_cleanup_requested(state.clone(), ctx)),
        );
    }

    fn route_info(&self) -> Vec<RouteInfo> {
        vec![
            RouteInfo {
                method: "GET",
                path: "/api/v1/data-tools/rules",
                owner: "data_tools",
                description: "List cleanup rules",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "PATCH",
                path: "/api/v1/data-tools/rules/{id}",
                owner: "data_tools",
                description: "Update a cleanup rule",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/data-tools/scan",
                owner: "data_tools",
                description: "Dry-run scan preview (zero deletes)",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/data-tools/run",
                owner: "data_tools",
                description: "Run a cleanup",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/data-tools/runs",
                owner: "data_tools",
                description: "List cleanup runs",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/data-tools/admin",
                owner: "data_tools",
                description: "Data Tools admin page (HTML)",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
        ]
    }

    fn configure_routes(&self, cfg: &mut web::ServiceConfig) {
        cfg.app_data(web::Data::new(self.state.clone()));
        cfg.service(
            web::scope("/api/v1/data-tools")
                .route("/rules", web::get().to(list_rules))
                .route("/rules/{id}", web::patch().to(update_rule))
                .route("/scan", web::post().to(scan_now))
                .route("/run", web::post().to(run_now))
                .route("/runs", web::get().to(list_runs))
                .route("/admin", web::get().to(admin_page)),
        );
    }
}

pub fn register(registry: &ModuleRegistry) {
    registry.register(Arc::new(DataToolsModule::new()));
}
