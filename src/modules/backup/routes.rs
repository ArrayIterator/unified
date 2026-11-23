// Backup module - module registration and routes.
//
// Split from `src/modules/backup.rs`; the module root re-exports the public API of this module.

use unified_core::context::AppContext;
use unified_core::error::UnifiedResult;
use unified_core::hooks::{HookBus, HookContext};
use unified_core::json;
use unified_core::modules::{Module, ModuleRegistry, RouteInfo};
use crate::modules::ai;
use actix_web::web;
use serde_json::{json, Value};
use std::sync::Arc;

use super::config::{backup_settings, redact_value};
use super::constants::{BACKUP_DELETED, BACKUP_PRUNE_REQUESTED, BACKUP_RUN_REQUESTED, BACKUP_VERIFIED, BACKUP_VERIFY_REQUESTED, STORAGE_SYNC_COMPLETED};
use super::execution::{execute_run, prune_all_jobs, spawn_backup_worker, start_run, verify_run_archive, verify_stale_runs};
use super::handlers::{create_job, delete_job, delete_run, drill_job, drill_run, get_job, get_run, get_summary, list_job_runs, list_jobs, list_runs, list_targets, maintenance_prune, maintenance_verify, restore_job, restore_run, run_job_now, update_job, verify_job, verify_run};
use super::state::{BackupExecutor, BackupState};
use super::types::{BackupRun, BackupStatus, DbSnapshot};
use super::ai_capabilities::AI_CAPABILITIES;

// ---------------------------------------------------------------------------
// Module definition.
// ---------------------------------------------------------------------------

pub struct BackupModule {
    pub(crate) state: Arc<BackupState>,
}

impl BackupModule {
    pub fn new() -> Self {
        Self {
            state: Arc::new(BackupState::new()),
        }
    }
}

impl Default for BackupModule {
    fn default() -> Self {
        Self::new()
    }
}

impl Module for BackupModule {
    fn id(&self) -> &'static str {
        "backup"
    }

    fn name(&self) -> &'static str {
        "Backup Manager"
    }

    fn description(&self) -> &'static str {
        "Backup jobs for database, files, configuration, or full snapshots, stored in a configured storage target with checksum and manifest verification, scheduled integrity sweeps, retention enforcement, restore drills, and restore."
    }

    fn dependencies(&self) -> Vec<&'static str> {
        vec!["scheduler"]
    }

    fn init(&self, ctx: &AppContext) -> UnifiedResult<()> {

        ai::register_ai_capabilities("backup", AI_CAPABILITIES);
        let config = ctx.config.read().expect("config lock poisoned");
        *self
            .state
            .hooks
            .write()
            .expect("backup hooks lock poisoned") = Some(ctx.hooks.clone());
        *self
            .state
            .storage
            .write()
            .expect("backup storage lock poisoned") = Some(ctx.storage_registry.clone());
        *self
            .state
            .settings
            .write()
            .expect("backup settings lock poisoned") = backup_settings(&config);
        *self
            .state
            .db
            .write()
            .expect("backup db lock poisoned") = Some(DbSnapshot::from_config(&config));
        *self
            .state
            .config_snapshot
            .write()
            .expect("backup config lock poisoned") =
            redact_value(serde_json::to_value(&*config).unwrap_or(Value::Null));
        Ok(())
    }

    fn register_hooks(&self, bus: &HookBus) {
        // Run a job when the scheduler dispatches `backup.run`.
        let state = self.state.clone();
        bus.register(
            BACKUP_RUN_REQUESTED,
            "backup",
            Arc::new(move |ctx: &mut HookContext| {
                let job_id = ctx
                    .get("backup_job_id")
                    .or_else(|| ctx.get("job_id"))
                    .and_then(|value| value.as_str())
                    .unwrap_or("")
                    .to_string();
                if job_id.is_empty() {
                    ctx.add_result(json!({
                        "error": "backup.run_requested requires 'backup_job_id'"
                    }));
                    return;
                }
                let executor = match BackupExecutor::from_state(state.clone()) {
                    Ok(executor) => executor,
                    Err(error) => {
                        ctx.add_result(json!({ "error": error.message }));
                        return;
                    }
                };
                match start_run(&executor, &job_id) {
                    Ok(run) => {
                        let run_id = run.id.clone();
                        let job_id = job_id.clone();
                        let (job_id_json, run_id_json) = (job_id.clone(), run_id.clone());
                        spawn_backup_worker(async move {
                            execute_run(executor, job_id, run_id).await;
                        });
                        ctx.add_result(json!({ "started": true, "job_id": job_id_json, "run_id": run_id_json }));
                    }
                    Err(error) => {
                        ctx.add_result(json!({ "error": error.message }));
                    }
                }
            }),
        );

        // Scheduled integrity sweep: verify every stale completed run
        // when the scheduler dispatches `backup.verify`. System actor:
        // platform scope (no company filter).
        let state = self.state.clone();
        bus.register(
            BACKUP_VERIFY_REQUESTED,
            "backup",
            Arc::new(move |ctx: &mut HookContext| {
                let executor = match BackupExecutor::from_state(state.clone()) {
                    Ok(executor) => executor,
                    Err(error) => {
                        ctx.add_result(json!({ "error": error.message }));
                        return;
                    }
                };
                spawn_backup_worker(async move {
                    let view = verify_stale_runs(&executor, &None).await;
                    executor.hooks.emit(
                        BACKUP_VERIFIED,
                        json!({
                            "checked": view.checked,
                            "verified": view.verified,
                            "failed": view.failed,
                        }),
                    );
                });
                ctx.add_result(json!({ "verify_started": true }));
            }),
        );

        // Scheduled retention enforcement: prune every job when the
        // scheduler dispatches `backup.prune`. System actor:
        // platform scope (no company filter).
        let state = self.state.clone();
        bus.register(
            BACKUP_PRUNE_REQUESTED,
            "backup",
            Arc::new(move |ctx: &mut HookContext| {
                let executor = match BackupExecutor::from_state(state.clone()) {
                    Ok(executor) => executor,
                    Err(error) => {
                        ctx.add_result(json!({ "error": error.message }));
                        return;
                    }
                };
                spawn_backup_worker(async move {
                    let view = prune_all_jobs(&executor, &None).await;
                    executor.hooks.emit(
                        BACKUP_DELETED,
                        json!({
                            "action": "backup.prune",
                            "jobs_checked": view.jobs_checked,
                            "runs_pruned": view.runs_pruned,
                            "archives_failed": view.archives_failed,
                        }),
                    );
                });
                ctx.add_result(json!({ "prune_started": true }));
            }),
        );

        // After a storage sync completes, re-verify the latest completed
        // run of every job that uses the synced target.
        let state = self.state.clone();
        bus.register(
            STORAGE_SYNC_COMPLETED,
            "backup",
            Arc::new(move |ctx: &mut HookContext| {
                let target = ctx
                    .get("target")
                    .or_else(|| ctx.get("name"))
                    .and_then(|value| value.as_str())
                    .unwrap_or("")
                    .to_string();
                if target.is_empty() {
                    ctx.add_result(json!({
                        "skipped": true,
                        "reason": "storage.sync.completed carried no target name"
                    }));
                    return;
                }
                let executor = match BackupExecutor::from_state(state.clone()) {
                    Ok(executor) => executor,
                    Err(error) => {
                        ctx.add_result(json!({ "error": error.message }));
                        return;
                    }
                };
                let mut run_ids: Vec<String> = Vec::new();
                let mut job_ids: Vec<String> = state
                    .jobs
                    .iter()
                    .filter(|entry| entry.value().target == target)
                    .map(|entry| entry.key().clone())
                    .collect();
                job_ids.sort();
                for job_id in job_ids {
                    let mut runs: Vec<BackupRun> = state
                        .runs
                        .iter()
                        .map(|entry| entry.value().clone())
                        .filter(|run| {
                            run.job_id == job_id && run.status == BackupStatus::Completed
                        })
                        .collect();
                    runs.sort_by(|a, b| {
                        b.finished_at
                            .unwrap_or(b.started_at)
                            .cmp(&a.finished_at.unwrap_or(a.started_at))
                    });
                    if let Some(run) = runs.into_iter().next() {
                        run_ids.push(run.id);
                    }
                }
                let worker_executor = executor.clone();
                spawn_backup_worker(async move {
                    for run_id in run_ids {
                        let Some(run) = worker_executor
                            .state
                            .runs
                            .get(&run_id)
                            .map(|entry| entry.value().clone())
                        else {
                            continue;
                        };
                        if let Ok(view) = verify_run_archive(&worker_executor, &run).await {
                            worker_executor.hooks.emit(
                                BACKUP_VERIFIED,
                                json!({
                                    "run_id": run.id,
                                    "job_id": run.job_id,
                                    "target": run.target,
                                    "verified": view.verified,
                                }),
                            );
                        }
                    }
                });
                ctx.add_result(json!({ "reverify_started": true, "target": target }));
            }),
        );
    }

    fn configure_routes(&self, cfg: &mut web::ServiceConfig) {
        cfg.app_data(web::Data::new(self.state.clone()));
        cfg.service(
            web::scope("/api/v1/backup")
                .route("", web::get().to(get_summary))
                .route("/targets", web::get().to(list_targets))
                .route("/jobs", web::get().to(list_jobs))
                .route("/jobs", web::post().to(create_job))
                .route("/jobs/{id}", web::get().to(get_job))
                .route("/jobs/{id}", web::put().to(update_job))
                .route("/jobs/{id}", web::delete().to(delete_job))
                .route("/jobs/{id}/run", web::post().to(run_job_now))
                .route("/jobs/{id}/verify", web::post().to(verify_job))
                .route("/jobs/{id}/restore", web::post().to(restore_job))
                .route("/jobs/{id}/drill", web::post().to(drill_job))
                .route("/jobs/{id}/runs", web::get().to(list_job_runs))
                .route("/runs", web::get().to(list_runs))
                .route("/runs/{run_id}", web::get().to(get_run))
                .route("/runs/{run_id}", web::delete().to(delete_run))
                .route("/runs/{run_id}/verify", web::post().to(verify_run))
                .route("/runs/{run_id}/restore", web::post().to(restore_run))
                .route("/runs/{run_id}/drill", web::post().to(drill_run))
                .route("/maintenance/verify", web::post().to(maintenance_verify))
                .route("/maintenance/prune", web::post().to(maintenance_prune)),
        );
    }

    fn route_info(&self) -> Vec<RouteInfo> {
        vec![
            RouteInfo { method: "GET", path: "/api/v1/backup", owner: "backup", description: "Backup module summary: counts, targets, limits.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/backup/targets", owner: "backup", description: "List registered storage target names.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/backup/jobs", owner: "backup", description: "List backup jobs with pagination and filters.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/backup/jobs", owner: "backup", description: "Create a backup job.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/backup/jobs/{id}", owner: "backup", description: "Get one backup job.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "PUT", path: "/api/v1/backup/jobs/{id}", owner: "backup", description: "Update a backup job.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "DELETE", path: "/api/v1/backup/jobs/{id}", owner: "backup", description: "Delete a backup job and its stored archives.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/backup/jobs/{id}/run", owner: "backup", description: "Run a backup job now, in the background.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/backup/jobs/{id}/verify", owner: "backup", description: "Verify the latest completed run of a job.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/backup/jobs/{id}/restore", owner: "backup", description: "Restore from the latest completed run of a job.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/backup/jobs/{id}/drill", owner: "backup", description: "Restore drill for the latest completed run of a job: verify and extract to a throwaway sandbox, then delete it.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/backup/jobs/{id}/runs", owner: "backup", description: "List the run history of one backup job.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/backup/runs", owner: "backup", description: "List all backup runs with pagination and filters.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/backup/runs/{run_id}", owner: "backup", description: "Get one backup run.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "DELETE", path: "/api/v1/backup/runs/{run_id}", owner: "backup", description: "Delete one backup run and its stored archive.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/backup/runs/{run_id}/verify", owner: "backup", description: "Recompute and compare the checksum of a stored backup.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/backup/runs/{run_id}/restore", owner: "backup", description: "Restore from one backup run.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/backup/runs/{run_id}/drill", owner: "backup", description: "Restore drill for one backup run: verify and extract to a throwaway sandbox, then delete it.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/backup/maintenance/verify", owner: "backup", description: "Verify every completed run whose last verification is stale.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/backup/maintenance/prune", owner: "backup", description: "Enforce retention for every backup job.", auth: unified_core::auth::RouteAuth::Authenticated},
        ]
    }
}

pub fn register(registry: &ModuleRegistry) {
    registry.register(Arc::new(BackupModule::new()));
}
