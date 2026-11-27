// Workflows module - module registration and routes.
//
// Split from `src/modules/workflows.rs`; the module root re-exports the public API of this module.

use unified_core::context::AppContext;
use unified_core::error::UnifiedResult;
use unified_core::hooks::{HookBus, HookContext};
use unified_core::modules::{Module, ModuleRegistry, RouteInfo, RouteVersionSupport};
use crate::modules::ai;
use actix_web::web;
use std::sync::Arc;

use super::constants::{JOB_KIND_WORKFLOWS_RUN, WORKFLOWS_RUN_REQUESTED};
use super::handlers::{create_workflow, delete_workflow, disable_workflow, duplicate_workflow, enable_workflow, get_workflow, get_workflow_run, list_trigger_subscriptions, list_workflow_runs, list_workflows, retry_workflow_run, run_workflow, test_workflow, update_workflow, webhook_trigger};
use super::runs::{on_workflows_run_requested};
use super::state::{WorkflowsState};
use super::ai_capabilities::AI_CAPABILITIES;

// Re-exported for the integrator's tests: the scheduler job kind
// this module enqueues, and the hook event the scheduler must emit
// when that job fires.
pub const WORKFLOWS_RUN_JOB_KIND: &str = JOB_KIND_WORKFLOWS_RUN;
pub const WORKFLOWS_RUN_REQUESTED_EVENT: &str = WORKFLOWS_RUN_REQUESTED;

// Module

pub struct WorkflowsModule {
    pub(crate) state: WorkflowsState,
}

impl WorkflowsModule {
    pub fn new() -> Self {
        Self {
            state: WorkflowsState::default(),
        }
    }
}

impl Default for WorkflowsModule {
    fn default() -> Self {
        Self::new()
    }
}

impl Module for WorkflowsModule {
    fn id(&self) -> &'static str {
        "workflows"
    }

    fn name(&self) -> &'static str {
        "Workflow Automation"
    }

    fn description(&self) -> &'static str {
        "Workflow automation / autopilot: event, schedule, webhook, and manual triggers; nested condition groups; ordered actions (notification, email, task, ticket, record update, outbound webhook, assign, tag, run another workflow); per-run logs, dry-run testing, retries, and loop / recursion guards."
    }

    fn requires(&self) -> &[&'static str] {
        &["users", "roles", "notifications", "mail", "tickets", "tasks", "scheduler"]
    }

    fn dependencies(&self) -> Vec<&'static str> {
        vec!["users", "roles", "notifications", "mail", "tickets", "tasks", "scheduler"]
    }

    fn requires_libraries(&self) -> &[&'static str] {
        &["reqwest", "url", "sha2"]
    }

    fn available_as_product(&self) -> bool {
        true
    }

    fn product_category(&self) -> Option<&'static str> {
        Some("productivity")
    }

    fn init(&self, ctx: &AppContext) -> UnifiedResult<()> {

        ai::register_ai_capabilities("workflows", AI_CAPABILITIES);
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
            WORKFLOWS_RUN_REQUESTED,
            "workflows",
            Arc::new(move |ctx: &mut HookContext| {
                on_workflows_run_requested(Arc::clone(&state), ctx)
            }),
        );
    }

    fn configure_routes(&self, cfg: &mut web::ServiceConfig) {
        let state = web::Data::new(self.state.clone());
        cfg.service(
            web::scope("/api/v1/workflows")
                .app_data(state)
                // Literal segments first: they must win over `/{id}`.
                .route("", web::get().to(list_workflows))
                .route("", web::post().to(create_workflow))
                .route("/triggers", web::get().to(list_trigger_subscriptions))
                .route("/hooks/{token}", web::post().to(webhook_trigger))
                .route("/runs/{run_id}", web::get().to(get_workflow_run))
                .route("/runs/{run_id}/retry", web::post().to(retry_workflow_run))
                .route("/{id}", web::get().to(get_workflow))
                .route("/{id}", web::patch().to(update_workflow))
                .route("/{id}", web::delete().to(delete_workflow))
                .route("/{id}/enable", web::post().to(enable_workflow))
                .route("/{id}/disable", web::post().to(disable_workflow))
                .route("/{id}/duplicate", web::post().to(duplicate_workflow))
                .route("/{id}/run", web::post().to(run_workflow))
                .route("/{id}/test", web::post().to(test_workflow))
                .route("/{id}/runs", web::get().to(list_workflow_runs)),
        );
    }

    fn route_info(&self) -> Vec<RouteInfo> {
        vec![
            RouteInfo { method: "GET", path: "/api/v1/workflows", owner: "workflows", description: "List workflows (paginated, filter by is_active, trigger_kind, company_id).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/workflows", owner: "workflows", description: "Create a workflow (trigger, conditions, actions validated at save time; webhook trigger returns its token once).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/workflows/{id}", owner: "workflows", description: "Get one workflow with subscription and run counts.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "PATCH", path: "/api/v1/workflows/{id}", owner: "workflows", description: "Update a workflow (re-subscribes event triggers; rotate_webhook_token rotates the webhook token).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "DELETE", path: "/api/v1/workflows/{id}", owner: "workflows", description: "Delete a workflow with its runs, steps, subscriptions, and webhook token.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/workflows/{id}/enable", owner: "workflows", description: "Enable a workflow (subscribes its event trigger; schedules it when it has a schedule trigger).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/workflows/{id}/disable", owner: "workflows", description: "Disable a workflow (unsubscribes its event trigger; bus events are ignored while disabled).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/workflows/{id}/duplicate", owner: "workflows", description: "Duplicate a workflow (the copy starts disabled with a fresh webhook token).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/workflows/{id}/run", owner: "workflows", description: "Run a workflow manually with a payload (dry_run=true plans without side effects).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/workflows/{id}/test", owner: "workflows", description: "Dry-run evaluation: condition results plus planned actions, no side effects, no run row.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/workflows/{id}/runs", owner: "workflows", description: "List runs of a workflow (paginated, filter by status, trigger_kind, dry_run).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/workflows/runs/{run_id}", owner: "workflows", description: "Get one run with its steps.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/workflows/runs/{run_id}/retry", owner: "workflows", description: "Retry a failed run with a caller-supplied payload (the original payload is not retained).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/workflows/triggers", owner: "workflows", description: "List active bus-event subscriptions (event name -> workflow ids).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/workflows/hooks/{token}", owner: "workflows", description: "TOKEN-AUTHENTICATED: trigger a webhook workflow (no capability check; gated by the API Manager ApiAccessRule for this path).", auth: unified_core::auth::RouteAuth::Public},
        ]
    }

    fn route_version_support(&self) -> RouteVersionSupport {
        RouteVersionSupport::All
    }
}

pub fn register(registry: &ModuleRegistry) {
    registry.register(Arc::new(WorkflowsModule::new()));
}
