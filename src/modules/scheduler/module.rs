// Scheduler - Module struct, route table and registration.

// Scheduler / Queue module.
//
// Named jobs with an interval or cron-like schedule, run history, and a
// background queue with retries and backoff. Job kinds dispatch to real
// handlers that emit the corresponding hook (`mail.send_requested`,
// `geo.update_requested`, `ssl.renew`, `cleanup.requested`) and record a
// run result. Unknown kinds fail honestly with 422.
//
// Scheduling: a background ticker started in `init` (`start_ticker`
// in `service.rs`) calls `run_due_jobs` every 30 seconds, so jobs
// with a schedule fire on their own — interval jobs once their
// interval elapsed since the last fire (or `created_at`), cron jobs
// once per matching minute. Manual runs (`POST /jobs/{id}/run`) and
// queue items still dispatch through the same `dispatch` path.
//
// Built-in jobs: `init` seeds one interval job for kind
// `tenant.destroy_due` (every 3600 seconds, see
// `seed_builtin_jobs` in `service.rs`) so grace-expired destroy
// requests are swept on a schedule with no manual setup. The
// seeded job is an ordinary job afterwards: an admin can change
// its interval or delete it through the jobs API
// (`PUT` / `DELETE /api/v1/scheduler/jobs/{id}`), and seeding
// never creates a second job of that kind.
//
// Context: `init` also stores the live `AppContext`'s handles in
// the state (see `ContextHandles` in `state.rs`) so `dispatch`
// can run work that needs a context. The `tenant.destroy_due` arm
// is the one arm that does real work itself instead of only
// emitting an event: it rebuilds the context and runs the
// tenant-destroy sweep (`tenant_destroy::process_due_destroys`),
// failing closed when no context was stored.
//
// Authentication: every route is gated by `authorize` in
// `handlers.rs` — `X-User-Id` and `X-Role` headers are required and
// the role must hold `scheduler.view` (reads) or `scheduler.manage`
// (create, update, delete, run, enqueue) through the `role.check`
// hook. The check fails closed when the roles module does not
// answer. Note the limit of this gate: scheduler jobs carry no
// tenant field, so jobs are global control-plane objects — the gate
// is caller identity plus permission, not per-tenant scoping.
//
// Listener wiring: `approvals.sweep_expiry_requested` is consumed
// by the Approvals module (`on_sweep_expiry_requested`, the same
// sweep as `POST /requests/sweep-expiry`) and `web3.poll_requested`
// by the Web3 module (`on_poll_requested`, which starts the async
// watched-address poll on a background thread), so dispatching
// `approvals.sweep_expiry` or `web3.poll` has a real consumer.

use unified_core::context::AppContext;
use unified_core::error::UnifiedResult;
use unified_core::hooks::{HookBus, HookContext};
use unified_core::modules::{Module, ModuleRegistry, RouteInfo};
use actix_web::web;
use std::sync::Arc;

use super::{SchedulerState, create_job, delete_job, enqueue, get_job, list_jobs, list_queue,
    list_runs, on_scheduler_enqueue, run_job, update_job};

// ---------------------------------------------------------------------------
// Module
// ---------------------------------------------------------------------------

pub struct SchedulerModule {
    state: Arc<SchedulerState>,
}

impl SchedulerModule {
    pub fn new() -> Self {
        Self {
            state: Arc::new(SchedulerState::new()),
        }
    }

    /// Test-only access to the module state, so tests can prove
    /// what `init` seeded and stored.
    #[cfg(test)]
    pub(crate) fn state_for_test(&self) -> Arc<SchedulerState> {
        self.state.clone()
    }
}

impl Default for SchedulerModule {
    fn default() -> Self {
        Self::new()
    }
}

impl Module for SchedulerModule {
    fn id(&self) -> &'static str {
        "scheduler"
    }

    fn name(&self) -> &'static str {
        "Scheduler"
    }

    fn description(&self) -> &'static str {
        "Scheduler and queue: named jobs with run history and a background queue with retries."
    }

    fn init(&self, ctx: &AppContext) -> UnifiedResult<()> {
        *self
            .state
            .hooks
            .write()
            .expect("hooks lock poisoned") = ctx.hooks.clone();
        // Keep the live context's handles so `dispatch` can run
        // work that needs one (the `tenant.destroy_due` sweep).
        self.state.set_context(ctx);
        // Seed the built-in jobs (idempotent — see
        // `seed_builtin_jobs`), then start the ticker so scheduled
        // jobs fire on their own. Starting twice is a no-op (see
        // `start_ticker`).
        super::service::seed_builtin_jobs(&self.state);
        super::service::start_ticker(self.state.clone());
        Ok(())
    }

    fn register_hooks(&self, bus: &HookBus) {
        let state = self.state.clone();
        bus.register(
            "scheduler.enqueue",
            "scheduler",
            Arc::new(move |ctx: &mut HookContext| {
                on_scheduler_enqueue(state.clone(), ctx)
            }),
        );
    }

    fn route_info(&self) -> Vec<RouteInfo> {
        vec![
            RouteInfo { method: "GET", path: "/api/v1/scheduler/jobs", owner: "scheduler", description: "List jobs", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/scheduler/jobs", owner: "scheduler", description: "Create job", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/scheduler/jobs/{id}", owner: "scheduler", description: "Get job", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "PUT", path: "/api/v1/scheduler/jobs/{id}", owner: "scheduler", description: "Update job", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "DELETE", path: "/api/v1/scheduler/jobs/{id}", owner: "scheduler", description: "Delete job", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/scheduler/jobs/{id}/run", owner: "scheduler", description: "Run job", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/scheduler/jobs/{id}/runs", owner: "scheduler", description: "List runs", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/scheduler/queue", owner: "scheduler", description: "List queue", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/scheduler/queue", owner: "scheduler", description: "Enqueue", auth: unified_core::auth::RouteAuth::Authenticated},
        ]
    }

    fn configure_routes(&self, cfg: &mut web::ServiceConfig) {
        cfg.app_data(web::Data::new(self.state.clone()));
        cfg.service(
            web::scope("/api/v1/scheduler")
                .route("/jobs", web::get().to(list_jobs))
                .route("/jobs", web::post().to(create_job))
                .route("/jobs/{id}", web::get().to(get_job))
                .route("/jobs/{id}", web::put().to(update_job))
                .route("/jobs/{id}", web::delete().to(delete_job))
                .route("/jobs/{id}/run", web::post().to(run_job))
                .route("/jobs/{id}/runs", web::get().to(list_runs))
                .route("/queue", web::get().to(list_queue))
                .route("/queue", web::post().to(enqueue)),
        );
    }
}

pub fn register(registry: &ModuleRegistry) {
    registry.register(Arc::new(SchedulerModule::new()));
}
