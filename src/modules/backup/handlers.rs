// Backup module - HTTP handlers.
//
// Split from `src/modules/backup.rs`; the module root re-exports the public API of this module.

use unified_core::config::AppConfig;
use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::json;
use unified_core::pagination::PaginationParams;
use actix_web::{web, HttpRequest, HttpResponse};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;

use super::archive::{audit_payload};
use super::config::{backup_settings};
use super::constants::{BACKUP_CREATED, BACKUP_DELETED, BACKUP_DRILL_COMPLETED, BACKUP_RESTORED, BACKUP_VERIFIED, CONFIG_NAME, DUMP_NAME, MANIFEST_NAME, MAX_NAME_LEN, PERM_BACKUP_MANAGE, PERM_BACKUP_VIEW, RESTORABLE_CONFIG_KEYS};
use super::execution::{drill_run_archive, execute_run, fail_alert, inspect_archive_bytes_with_limits, prune_all_jobs, record_verification, start_run, verify_run_archive, verify_stale_runs};
use super::restore::{FileRestoreOutcome, notify_scheduler, restore_config_blocking, restore_database_blocking, restore_files_blocking, rollback_files_blocking, validate_common, validate_job_fields};
use super::state::{BackupExecutor, BackupState, matches_search, new_id, now, paginate};
use super::types::{BackupJob, BackupRun, BackupStatus, BackupSummary, CreateBackupJob, DrillView, MaintenancePruneView, MaintenanceVerifyView, RestoreRequest, RestoreView, SchedulingOutcome, UpdateBackupJob, VerifyView};
use super::validation::{Caller, caller, caller_company, require_company_for_write, job_visible, require_permission, require_tenant, run_visible, valid_schedule, valid_storage_name, visible_job, visible_run};

// Handlers.

pub(crate) async fn get_summary(
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<BackupState>>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<BackupSummary> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_BACKUP_VIEW)?;
        let company = _scope.clone();
        let config = ctx.config.read().expect("config lock poisoned");
        let settings = backup_settings(&config);
        let mut jobs_by_status: BTreeMap<String, u64> = BTreeMap::new();
        for entry in state.jobs.iter() {
            if !job_visible(entry.value(), &company) {
                continue;
            }
            *jobs_by_status
                .entry(entry.value().status.as_str().to_string())
                .or_default() += 1;
        }
        let mut runs_by_status: BTreeMap<String, u64> = BTreeMap::new();
        for entry in state.runs.iter() {
            if !run_visible(entry.value(), &company) {
                continue;
            }
            *runs_by_status
                .entry(entry.value().status.as_str().to_string())
                .or_default() += 1;
        }
        let jobs_total: u64 = jobs_by_status.values().sum();
        let runs_total: u64 = runs_by_status.values().sum();
        Ok(BackupSummary {
            jobs_total,
            jobs_by_status,
            runs_total,
            runs_by_status,
            targets: ctx.storage_registry.list(),
            base_dir: settings.base_dir,
            drill_dir: settings.drill_dir,
            max_total_bytes: settings.max_total_bytes,
            max_file_bytes: settings.max_file_bytes,
            default_retention_keep_last: settings.default_retention_keep_last,
            default_retention_keep_days: settings.default_retention_keep_days,
            verify_interval_hours: settings.verify_interval_hours,
            // At-rest encryption is not supported; archives are
            // stored unencrypted (see the module header).
            encryption: "none".to_string(),
        })
    })();
    json::respond(result, debug)
}

pub(crate) async fn list_targets(req: HttpRequest, ctx: web::Data<Arc<AppContext>>) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Vec<String>> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_BACKUP_VIEW)?;
        Ok(ctx.storage_registry.list())
    })();
    match result {
        Ok(targets) => json::success_response(json!({ "targets": targets }), 200),
        Err(error) => json::error_response(&error, debug),
    }
}

pub(crate) async fn list_jobs(
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<BackupState>>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let params = query.into_inner();
    let (caller, _scope) = match (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_BACKUP_VIEW)?;
        Ok::<(Caller, Option<String>), UnifiedError>((caller, _scope))
    })() {
        Ok((caller, scope)) => (caller, scope),
        Err(error) => return json::error_response(&error, debug),
    };
    let company = _scope.clone();
    let mut items: Vec<BackupJob> = state
        .jobs
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|job| {
            job_visible(job, &company)
                && matches_search(&format!("{} {}", job.name, job.target), &params)
                && params
                    .filters
                    .get("kind")
                    .is_none_or(|kind| job.kind.as_str() == kind)
                && params
                    .filters
                    .get("status")
                    .is_none_or(|status| job.status.as_str() == status)
                && params
                    .filters
                    .get("target")
                    .is_none_or(|target| job.target == *target)
                && params.filters.get("enabled").is_none_or(|flag| {
                    job.enabled == (*flag == "true" || *flag == "1")
                })
        })
        .collect();
    let sort = params.allowed_sort(&["created_at", "name", "status"], "created_at");
    items.sort_by(|a, b| match sort.as_str() {
        "name" => a.name.cmp(&b.name),
        "status" => a.status.as_str().cmp(b.status.as_str()),
        _ => a.created_at.cmp(&b.created_at),
    });
    if params.normalized().order == "desc" {
        items.reverse();
    }
    let page = paginate(items, &params);
    json::success_response(page, 200)
}

pub(crate) async fn create_job(
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<BackupState>>,
    body: web::Json<CreateBackupJob>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<BackupJob> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = require_company_for_write(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_BACKUP_MANAGE)?;
        let input = body.into_inner();
        let config = ctx.config.read().expect("config lock poisoned");
        let settings = backup_settings(&config);
        let retention_keep_last = input
            .retention_keep_last
            .unwrap_or(settings.default_retention_keep_last);
        let retention_keep_days = input
            .retention_keep_days
            .unwrap_or(settings.default_retention_keep_days);
        let schedule = input.schedule.unwrap_or_default();
        validate_common(
            &input.name,
            &input.target,
            &schedule,
            retention_keep_last,
            retention_keep_days,
        )?;
        let database_name = input.database_name.unwrap_or_default();
        let paths = input.paths.unwrap_or_default();
        let config_keys = input.config_keys.unwrap_or_default();
        validate_job_fields(
            input.kind,
            &database_name,
            &paths,
            &config_keys,
            &settings,
        )?;
        if ctx.storage_registry.get(&input.target).is_none() {
            return Err(UnifiedError::not_found(format!(
                "Storage target \"{}\" is not registered",
                input.target
            )));
        }
        let created_at = now();
        let mut job = BackupJob {
            id: new_id(),
            name: input.name.trim().to_string(),
            company_id: _scope.clone(),
            kind: input.kind,
            database_name: database_name.trim().to_string(),
            paths,
            config_keys,
            target: input.target,
            schedule: schedule.trim().to_string(),
            retention_keep_last,
            retention_keep_days,
            enabled: input.enabled.unwrap_or(true),
            scheduling: SchedulingOutcome {
                enqueued: false,
                message: "Not scheduled yet.".to_string(),
            },
            status: BackupStatus::Pending,
            checksum: None,
            size_bytes: None,
            storage_key: None,
            last_run_id: None,
            started_at: None,
            finished_at: None,
            error: None,
            created_at,
            updated_at: created_at,
        };
        job.scheduling = notify_scheduler(&ctx.hooks, &job);
        state.jobs.insert(job.id.clone(), job.clone());
        ctx.hooks.emit(
            BACKUP_CREATED,
            audit_payload(&job, None, json!({ "action": BACKUP_CREATED })),
        );
        Ok(job)
    })();
    match result {
        Ok(job) => json::success_response(job, 201),
        Err(error) => json::error_response(&error, debug),
    }
}

pub(crate) async fn get_job(
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<BackupState>>,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<BackupJob> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_BACKUP_VIEW)?;
        visible_job(&state, path.as_str(), &_scope)
    })();
    json::respond(result, debug)
}

pub(crate) async fn update_job(
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<BackupState>>,
    path: web::Path<String>,
    body: web::Json<UpdateBackupJob>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<BackupJob> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = require_company_for_write(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_BACKUP_MANAGE)?;
        let input = body.into_inner();
        let config = ctx.config.read().expect("config lock poisoned");
        let settings = backup_settings(&config);
        // Work on a clone so a failed validation never leaves a
        // half-updated job in the store. Cross-company jobs answer
        // 404 here, exactly like get.
        let mut job = visible_job(&state, path.as_str(), &_scope)?;
        if let Some(name) = input.name {
            if name.trim().is_empty() || name.len() > MAX_NAME_LEN {
                return Err(UnifiedError::bad_request(
                    "Job name must be non-empty and at most 128 characters",
                ));
            }
            job.name = name.trim().to_string();
        }
        if let Some(kind) = input.kind {
            job.kind = kind;
        }
        if let Some(database_name) = input.database_name {
            job.database_name = database_name.trim().to_string();
        }
        if let Some(paths) = input.paths {
            job.paths = paths;
        }
        if let Some(config_keys) = input.config_keys {
            job.config_keys = config_keys;
        }
        if let Some(target) = input.target {
            if !valid_storage_name(&target) {
                return Err(UnifiedError::bad_request(
                    "Target must be a registered storage name (letters, digits, '-', '_' or '.')",
                ));
            }
            if ctx.storage_registry.get(&target).is_none() {
                return Err(UnifiedError::not_found(format!(
                    "Storage target \"{target}\" is not registered"
                )));
            }
            job.target = target;
        }
        if let Some(schedule) = input.schedule {
            if !valid_schedule(&schedule) {
                return Err(UnifiedError::bad_request(
                    "Schedule must be empty (manual-only) or a five-field cron expression such as \"0 2 * * *\"",
                ));
            }
            job.schedule = schedule.trim().to_string();
        }
        if let Some(keep_last) = input.retention_keep_last {
            if keep_last == 0 || keep_last > 100 {
                return Err(UnifiedError::bad_request(
                    "retention_keep_last must be between 1 and 100",
                ));
            }
            job.retention_keep_last = keep_last;
        }
        if let Some(keep_days) = input.retention_keep_days {
            if keep_days > 3650 {
                return Err(UnifiedError::bad_request(
                    "retention_keep_days must be between 0 and 3650",
                ));
            }
            job.retention_keep_days = keep_days;
        }
        if let Some(enabled) = input.enabled {
            job.enabled = enabled;
        }
        validate_job_fields(
            job.kind,
            &job.database_name,
            &job.paths,
            &job.config_keys,
            &settings,
        )?;
        job.updated_at = now();
        job.scheduling = notify_scheduler(&ctx.hooks, &job);
        state.jobs.insert(job.id.clone(), job.clone());
        Ok(job)
    })();
    json::respond(result, debug)
}

pub(crate) async fn delete_job(
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<BackupState>>,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| async {
        let _tenant = require_tenant(&req)?;
        let _scope = require_company_for_write(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_BACKUP_MANAGE)?;
        let job_id = path.into_inner();
        let job = visible_job(&state, &job_id, &_scope)?;
        if state.running.contains_key(&job_id) {
            return Err(UnifiedError::conflict(
                "Backup job is running and cannot be deleted yet",
            ));
        }
        let run_ids: Vec<String> = state
            .runs
            .iter()
            .filter(|entry| entry.value().job_id == job_id)
            .map(|entry| entry.key().clone())
            .collect();
        let mut storage_deleted = 0u64;
        let mut storage_failed = 0u64;
        for run_id in &run_ids {
            if let Some(run) = state.runs.get(run_id) {
                if let Some(key) = &run.storage_key {
                    match ctx.storage_registry.get(&job.target) {
                        Some(provider) => match provider.delete(key).await {
                            Ok(()) => storage_deleted += 1,
                            Err(_) => storage_failed += 1,
                        },
                        None => storage_failed += 1,
                    }
                }
                state.runs.remove(run_id);
            }
        }
        state.jobs.remove(&job_id);
        ctx.hooks.emit(
            BACKUP_DELETED,
            audit_payload(&job, None, json!({ "action": BACKUP_DELETED })),
        );
        Ok(json!({
            "deleted": true,
            "id": job_id,
            "runs_removed": run_ids.len(),
            "archives_deleted": storage_deleted,
            "archives_failed": storage_failed,
        }))
    })()
    .await;
    json::respond(result, debug)
}

pub(crate) async fn run_job_now(
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<BackupState>>,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<BackupRun> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = require_company_for_write(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_BACKUP_MANAGE)?;
        let executor = BackupExecutor::from_context(&ctx, state.get_ref().clone())?;
        let job_id = path.into_inner();
        // Cross-company jobs answer 404 before any run starts.
        visible_job(&state, &job_id, &_scope)?;
        let run = start_run(&executor, &job_id)?;
        let run_id = run.id.clone();
        let worker_executor = executor.clone();
        tokio::spawn(async move {
            execute_run(worker_executor, job_id, run_id).await;
        });
        Ok(run)
    })();
    match result {
        Ok(run) => json::success_response(run, 202),
        Err(error) => json::error_response(&error, debug),
    }
}

pub(crate) async fn list_job_runs(
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<BackupState>>,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let _tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return json::error_response(&error, ctx.is_debug()),
    };
    let _scope = match caller_company(&_tenant, &req) {
        Ok(scope) => scope,
        Err(error) => return json::error_response(&error, ctx.is_debug()),
    };
    let params = query.into_inner();
    let job_id = path.into_inner();
    let caller = match caller(&req).and_then(|caller| {
        require_permission(&ctx, &caller.role, PERM_BACKUP_VIEW)?;
        Ok(caller)
    }) {
        Ok(caller) => caller,
        Err(error) => return json::error_response(&error, ctx.is_debug()),
    };
    if visible_job(&state, &job_id, &_scope).is_err() {
        return json::error_response(
            &UnifiedError::not_found("Backup job not found"),
            ctx.is_debug(),
        );
    }
    let mut items: Vec<BackupRun> = state
        .runs
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|run| {
            run.job_id == job_id
                && run_visible(run, &_scope)
                && params
                    .filters
                    .get("status")
                    .is_none_or(|status| run.status.as_str() == status)
        })
        .collect();
    items.sort_by(|a, b| b.started_at.cmp(&a.started_at));
    if params.normalized().order == "asc" {
        items.reverse();
    }
    let page = paginate(items, &params);
    json::success_response(page, 200)
}

pub(crate) async fn list_runs(
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<BackupState>>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let _tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return json::error_response(&error, ctx.is_debug()),
    };
    let _scope = match caller_company(&_tenant, &req) {
        Ok(scope) => scope,
        Err(error) => return json::error_response(&error, ctx.is_debug()),
    };
    let params = query.into_inner();
    let caller = match caller(&req).and_then(|caller| {
        require_permission(&ctx, &caller.role, PERM_BACKUP_VIEW)?;
        Ok(caller)
    }) {
        Ok(caller) => caller,
        Err(error) => return json::error_response(&error, ctx.is_debug()),
    };
    let company = _scope.clone();
    let mut items: Vec<BackupRun> = state
        .runs
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|run| {
            run_visible(run, &company)
                && matches_search(&format!("{} {}", run.job_id, run.target), &params)
                && params
                    .filters
                    .get("job_id")
                    .is_none_or(|job_id| run.job_id == *job_id)
                && params
                    .filters
                    .get("kind")
                    .is_none_or(|kind| run.kind.as_str() == kind)
                && params
                    .filters
                    .get("status")
                    .is_none_or(|status| run.status.as_str() == status)
                && params
                    .filters
                    .get("target")
                    .is_none_or(|target| run.target == *target)
        })
        .collect();
    let sort = params.allowed_sort(&["started_at", "status"], "started_at");
    items.sort_by(|a, b| match sort.as_str() {
        "status" => a.status.as_str().cmp(b.status.as_str()),
        _ => a.started_at.cmp(&b.started_at),
    });
    if params.normalized().order == "desc" {
        items.reverse();
    }
    let page = paginate(items, &params);
    json::success_response(page, 200)
}

pub(crate) async fn get_run(
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<BackupState>>,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<BackupRun> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_BACKUP_VIEW)?;
        visible_run(&state, path.as_str(), &_scope)
    })();
    json::respond(result, debug)
}

pub(crate) async fn verify_run(
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<BackupState>>,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<VerifyView> = (|| async {
        let _tenant = require_tenant(&req)?;
        let _scope = require_company_for_write(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_BACKUP_MANAGE)?;
        let executor = BackupExecutor::from_context(&ctx, state.get_ref().clone())?;
        let run = visible_run(&state, path.as_str(), &_scope)?;
        let view = verify_run_archive(&executor, &run).await?;
        if let Some(job) = state.jobs.get(&run.job_id) {
            ctx.hooks.emit(
                BACKUP_VERIFIED,
                audit_payload(&job, Some(&run), json!({ "verified": view.verified })),
            );
        }
        if !view.verified {
            let detail = if view.files_mismatched.is_empty() {
                "checksum mismatch".to_string()
            } else {
                view.files_mismatched.join("; ")
            };
            fail_alert(&executor, &run, "verification", &detail);
        }
        Ok(view)
    })()
    .await;
    json::respond(result, debug)
}

pub(crate) async fn verify_job(
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<BackupState>>,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<VerifyView> = (|| async {
        let _tenant = require_tenant(&req)?;
        let _scope = require_company_for_write(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_BACKUP_MANAGE)?;
        let executor = BackupExecutor::from_context(&ctx, state.get_ref().clone())?;
        let job_id = path.into_inner();
        visible_job(&state, &job_id, &_scope)?;
        let mut runs: Vec<BackupRun> = state
            .runs
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|run| run.job_id == job_id && run.status == BackupStatus::Completed)
            .collect();
        runs.sort_by(|a, b| {
            b.finished_at
                .unwrap_or(b.started_at)
                .cmp(&a.finished_at.unwrap_or(a.started_at))
        });
        let run = runs
            .into_iter()
            .next()
            .ok_or_else(|| UnifiedError::not_found("Job has no completed run to verify"))?;
        let view = verify_run_archive(&executor, &run).await?;
        if let Some(job) = state.jobs.get(&run.job_id) {
            ctx.hooks.emit(
                BACKUP_VERIFIED,
                audit_payload(&job, Some(&run), json!({ "verified": view.verified })),
            );
        }
        if !view.verified {
            let detail = if view.files_mismatched.is_empty() {
                "checksum mismatch".to_string()
            } else {
                view.files_mismatched.join("; ")
            };
            fail_alert(&executor, &run, "verification", &detail);
        }
        Ok(view)
    })()
    .await;
    json::respond(result, debug)
}

/// Drill the latest completed run of a job: verify it and extract it
/// to a throwaway sandbox that is deleted afterwards.
pub(crate) async fn drill_job(
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<BackupState>>,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<DrillView> = (|| async {
        let _tenant = require_tenant(&req)?;
        let _scope = require_company_for_write(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_BACKUP_MANAGE)?;
        let executor = BackupExecutor::from_context(&ctx, state.get_ref().clone())?;
        let job_id = path.into_inner();
        visible_job(&state, &job_id, &_scope)?;
        let run = resolve_restore_run(&state, &job_id, None)?;
        drill_one(&ctx, &executor, &run).await
    })()
    .await;
    json::respond(result, debug)
}

/// Drill one run: verify it and extract it to a throwaway sandbox
/// that is deleted afterwards. Live data is never touched.
pub(crate) async fn drill_run(
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<BackupState>>,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<DrillView> = (|| async {
        let _tenant = require_tenant(&req)?;
        let _scope = require_company_for_write(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_BACKUP_MANAGE)?;
        let executor = BackupExecutor::from_context(&ctx, state.get_ref().clone())?;
        let run = visible_run(&state, path.as_str(), &_scope)?;
        if run.status != BackupStatus::Completed {
            return Err(UnifiedError::unprocessable(
                "Only completed runs can be drilled",
            ));
        }
        drill_one(&ctx, &executor, &run).await
    })()
    .await;
    json::respond(result, debug)
}

/// Shared drill tail: run the drill, emit the outcome hook, and alert
/// on failure.
pub(crate) async fn drill_one(
    ctx: &AppContext,
    executor: &BackupExecutor,
    run: &BackupRun,
) -> UnifiedResult<DrillView> {
    match drill_run_archive(executor, run).await {
        Ok(view) => {
            if let Some(job) = executor.state.jobs.get(&run.job_id) {
                ctx.hooks.emit(
                    BACKUP_DRILL_COMPLETED,
                    audit_payload(&job, Some(run), json!({ "verified": view.verified })),
                );
            }
            if !view.verified {
                fail_alert(
                    executor,
                    run,
                    "drill",
                    "drill extraction did not fully verify (see manifest mismatches)",
                );
            }
            Ok(view)
        }
        Err(error) => {
            fail_alert(executor, run, "drill", &error.message);
            Err(error)
        }
    }
}

/// Verify every stale completed run (scheduled integrity sweep).
pub(crate) async fn maintenance_verify(
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<BackupState>>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<MaintenanceVerifyView> = (|| async {
        let _tenant = require_tenant(&req)?;
        let _scope = require_company_for_write(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_BACKUP_MANAGE)?;
        let executor = BackupExecutor::from_context(&ctx, state.get_ref().clone())?;
        Ok(verify_stale_runs(&executor, &_scope).await)
    })()
    .await;
    json::respond(result, debug)
}

/// Enforce retention for every job (scheduled retention sweep).
pub(crate) async fn maintenance_prune(
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<BackupState>>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<MaintenancePruneView> = (|| async {
        let _tenant = require_tenant(&req)?;
        let _scope = require_company_for_write(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_BACKUP_MANAGE)?;
        let executor = BackupExecutor::from_context(&ctx, state.get_ref().clone())?;
        Ok(prune_all_jobs(&executor, &_scope).await)
    })()
    .await;
    json::respond(result, debug)
}

/// Resolve the run a restore request refers to: the named run, or the
/// latest completed run of the job.
pub(crate) fn resolve_restore_run(state: &BackupState, job_id: &str, run_id: Option<&str>) -> UnifiedResult<BackupRun> {
    if let Some(run_id) = run_id {
        let run = state
            .runs
            .get(run_id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Backup run not found"))?;
        if run.job_id != job_id {
            return Err(UnifiedError::bad_request(
                "Run does not belong to this backup job",
            ));
        }
        if run.status != BackupStatus::Completed {
            return Err(UnifiedError::unprocessable(
                "Only completed runs can be restored",
            ));
        }
        return Ok(run);
    }
    let mut runs: Vec<BackupRun> = state
        .runs
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|run| run.job_id == job_id && run.status == BackupStatus::Completed)
        .collect();
    runs.sort_by(|a, b| {
        b.finished_at
            .unwrap_or(b.started_at)
            .cmp(&a.finished_at.unwrap_or(a.started_at))
    });
    runs.into_iter()
        .next()
        .ok_or_else(|| UnifiedError::not_found("Job has no completed run to restore"))
}

/// Roll back the file and configuration changes of a restore that
/// failed at a later step. Restored files are reverted from the
/// snapshot taken before the restore, and the previous live
/// configuration is reloaded when it was changed. Returns a
/// human-readable summary of what was undone. The database step has
/// no rollback: a failure there may leave the database partially
/// restored even after files and configuration are rolled back.
async fn rollback_restore(
    ctx: &AppContext,
    base_dir: &str,
    files_outcome: &Option<FileRestoreOutcome>,
    previous_config: &Option<AppConfig>,
    config_was_applied: bool,
) -> String {
    let mut notes: Vec<String> = Vec::new();
    if let Some(outcome) = files_outcome {
        let base = base_dir.to_string();
        let previous = outcome.previous.clone();
        let fallback: Vec<String> = previous
            .iter()
            .map(|(path, _)| path.clone())
            .collect();
        let failed = tokio::task::spawn_blocking(move || {
            rollback_files_blocking(&base, &previous)
        })
        .await
        .unwrap_or(fallback);
        if failed.is_empty() {
            notes.push(format!(
                "rolled back {} restored file(s)",
                outcome.restored.len()
            ));
        } else {
            notes.push(format!(
                "could not roll back file(s): {}",
                failed.join(", ")
            ));
        }
    }
    if config_was_applied {
        match previous_config {
            Some(previous) => match ctx.reload_config(previous.clone()) {
                Ok(()) => {
                    notes.push("restored the previous live configuration".to_string());
                }
                Err(error) => notes.push(format!(
                    "could not restore the previous configuration: {}",
                    error.message
                )),
            },
            None => notes.push(
                "no previous configuration snapshot was available to restore".to_string(),
            ),
        }
    }
    if notes.is_empty() {
        "nothing had been changed yet".to_string()
    } else {
        notes.join("; ")
    }
}

/// Execute (or plan) a restore for one completed run. Database restores
/// execute only with explicit confirmation; everything else runs unless
/// the request is a dry run.
///
/// The steps run in order: files, then configuration, then database.
/// A restore is not fully atomic, but it does not fail silently into
/// a half-restored system either: the files step validates every
/// entry and snapshots previous contents before writing anything, so
/// it rolls itself back on failure; if the configuration or database
/// step then fails, the files are rolled back from that snapshot and
/// the previous live configuration is reloaded, and the error
/// reports what was rolled back. Only the database step cannot be
/// undone: a failure there may leave the database partially restored,
/// and the error says so.
pub(crate) async fn perform_restore(
    ctx: &AppContext,
    state: &Arc<BackupState>,
    run: &BackupRun,
    request: &RestoreRequest,
) -> UnifiedResult<RestoreView> {
    let executor = BackupExecutor::from_context(ctx, state.clone())?;
    let job = state
        .jobs
        .get(&run.job_id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found("Backup job not found"))?;
    let provider = executor.provider_for(&run.target)?;
    let storage_key = run.storage_key.clone().ok_or_else(|| {
        UnifiedError::unprocessable("Run has no stored archive to restore")
    })?;
    let bytes = provider.download(&storage_key).await?;
    // Verify checksum + manifest BEFORE touching live data. A
    // corrupt or foreign archive fails closed here; nothing is
    // written, and the failure is recorded on the run.
    let run_for_inspect = run.clone();
    let max_file_bytes = executor.settings.max_file_bytes;
    let max_total_bytes = executor.settings.max_total_bytes;
    let inspection = tokio::task::spawn_blocking(move || {
        inspect_archive_bytes_with_limits(
            &bytes,
            &run_for_inspect,
            max_file_bytes,
            max_total_bytes,
        )
    })
    .await
    .map_err(|error| UnifiedError::internal(format!("Restore worker failed: {error}")))?;
    let inspection = match inspection {
        Ok(inspection) => inspection,
        Err(error) => {
            record_verification(&state, &run.id, false, false, Some(error.message.clone()));
            return Err(error);
        }
    };
    if !inspection.checksum_verified || !inspection.manifest_verified {
        let detail = if !inspection.checksum_verified {
            "Archive checksum does not match the recorded checksum".to_string()
        } else {
            inspection.files_mismatched.join("; ")
        };
        record_verification(
            &state,
            &run.id,
            false,
            inspection.manifest_verified,
            Some(detail.clone()),
        );
        return Err(UnifiedError::unprocessable(format!(
            "Backup archive failed integrity verification and was not restored: {detail}"
        )));
    }
    record_verification(&state, &run.id, true, true, None);
    let entries = inspection.entries;

    let wants_database = run.kind.includes_database();
    let wants_files = run.kind.includes_files();
    let wants_config = run.kind.includes_config();
    let database_confirmed = request.confirm && !request.dry_run;
    let planned = request.dry_run || (wants_database && !database_confirmed);

    let mut steps: Vec<String> = Vec::new();
    if wants_files {
        let count = entries
            .iter()
            .filter(|(path, _)| {
                path != MANIFEST_NAME && path != CONFIG_NAME && path != DUMP_NAME
            })
            .count();
        steps.push(format!("Restore {count} file(s) under the allowlisted base directory"));
    }
    if wants_config {
        steps.push(format!(
            "Apply restorable configuration keys ({}) from the snapshot; secrets are never applied",
            RESTORABLE_CONFIG_KEYS.join(", ")
        ));
    }
    if wants_database {
        if database_confirmed {
            steps.push(format!(
                "Restore database \"{}\" from the archive dump with psql",
                executor.db.database
            ));
        } else {
            steps.push(format!(
                "Restore database \"{}\" from the archive dump with psql (requires confirm=true)",
                executor.db.database
            ));
        }
    }
    steps.push(
        "If a later step fails, restored files are rolled back and the previous configuration is restored; a database restore cannot be rolled back"
            .to_string(),
    );

    let mut view = RestoreView {
        run_id: run.id.clone(),
        job_id: run.job_id.clone(),
        kind: run.kind.as_str().to_string(),
        dry_run: request.dry_run,
        planned,
        steps,
        restored_files: Vec::new(),
        applied_config_keys: Vec::new(),
        database_restored: false,
        database_plan: None,
    };

    if planned {
        if wants_database {
            view.database_plan = Some(json!({
                "database": executor.db.database,
                "driver": executor.db.driver,
                "host": executor.db.host,
                "port": executor.db.port,
                "username": executor.db.username,
                "command": format!(
                    "psql -h {} -p {} -U {} -d {} < dump.sql",
                    executor.db.host, executor.db.port, executor.db.username, executor.db.database
                ),
                "confirm_required": true,
            }));
        }
        return Ok(view);
    }

    // State tracked so a later failed step can roll back the earlier
    // ones (see the doc comment on this function).
    let mut files_outcome: Option<FileRestoreOutcome> = None;
    let mut previous_config: Option<AppConfig> = None;
    let mut config_applied = false;

    if wants_files {
        let base_dir = executor.settings.base_dir.clone();
        let entries_for_files = entries.clone();
        let outcome = match tokio::task::spawn_blocking(move || {
            restore_files_blocking(&entries_for_files, &base_dir)
        })
        .await
        {
            Ok(Ok(outcome)) => outcome,
            Ok(Err(error)) => return Err(error),
            Err(join_error) => {
                return Err(UnifiedError::internal(format!(
                    "Restore worker failed: {join_error}"
                )));
            }
        };
        view.restored_files = outcome.restored.clone();
        files_outcome = Some(outcome);
    }
    if wants_config {
        // Snapshot the live configuration before changing it, so a
        // later database failure can reload exactly this version.
        let before = ctx.config.read().expect("config lock poisoned").clone();
        let entries_for_config = entries.clone();
        // `ctx` is borrowed from the request; the blocking restore needs
        // an owned handle, so apply the configuration on this task.
        match restore_config_blocking(&entries_for_config, ctx) {
            Ok(applied) => {
                view.applied_config_keys = applied;
                previous_config = Some(before);
                config_applied = true;
            }
            Err(error) => {
                let rollback = rollback_restore(
                    ctx,
                    &executor.settings.base_dir,
                    &files_outcome,
                    &Some(before),
                    false,
                )
                .await;
                return Err(UnifiedError::unprocessable(format!(
                    "Restore failed at the configuration step after restoring {} file(s): {error}. Rollback: {rollback}.",
                    view.restored_files.len()
                )));
            }
        }
    }
    if wants_database {
        let db = executor.db.clone();
        let psql_bin = executor.settings.psql_bin.clone();
        let entries_for_db = entries.clone();
        let restored = match tokio::task::spawn_blocking(move || {
            restore_database_blocking(&entries_for_db, &db, &psql_bin)
        })
        .await
        {
            Ok(Ok(restored)) => restored,
            Ok(Err(error)) => {
                let rollback = rollback_restore(
                    ctx,
                    &executor.settings.base_dir,
                    &files_outcome,
                    &previous_config,
                    config_applied,
                )
                .await;
                return Err(UnifiedError::unprocessable(format!(
                    "Restore failed at the database step after restoring {} file(s) and applying {} configuration key(s): {error}. Rollback: {rollback}. The database itself cannot be rolled back and may be partially restored.",
                    view.restored_files.len(),
                    view.applied_config_keys.len()
                )));
            }
            Err(join_error) => {
                let rollback = rollback_restore(
                    ctx,
                    &executor.settings.base_dir,
                    &files_outcome,
                    &previous_config,
                    config_applied,
                )
                .await;
                return Err(UnifiedError::internal(format!(
                    "Restore worker failed: {join_error}. Rollback: {rollback}. The database itself cannot be rolled back and may be partially restored."
                )));
            }
        };
        view.database_restored = true;
        view.database_plan = Some(json!({ "command": restored, "executed": true }));
    }

    ctx.hooks.emit(
        BACKUP_RESTORED,
        audit_payload(
            &job,
            Some(run),
            json!({
                "action": BACKUP_RESTORED,
                "restored_files": view.restored_files.len(),
                "applied_config_keys": view.applied_config_keys,
                "database_restored": view.database_restored,
            }),
        ),
    );
    Ok(view)
}

pub(crate) async fn restore_job(
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<BackupState>>,
    path: web::Path<String>,
    body: web::Json<RestoreRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<RestoreView> = (|| async {
        let _tenant = require_tenant(&req)?;
        let _scope = require_company_for_write(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_BACKUP_MANAGE)?;
        let job_id = path.into_inner();
        visible_job(&state, &job_id, &_scope)?;
        let request = body.into_inner();
        let run = resolve_restore_run(&state, &job_id, request.run_id.as_deref())?;
        perform_restore(&ctx, &state, &run, &request).await
    })()
    .await;
    json::respond(result, debug)
}

pub(crate) async fn restore_run(
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<BackupState>>,
    path: web::Path<String>,
    body: web::Json<RestoreRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<RestoreView> = (|| async {
        let _tenant = require_tenant(&req)?;
        let _scope = require_company_for_write(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_BACKUP_MANAGE)?;
        let request = body.into_inner();
        let run = visible_run(&state, path.as_str(), &_scope)?;
        if run.status != BackupStatus::Completed {
            return Err(UnifiedError::unprocessable(
                "Only completed runs can be restored",
            ));
        }
        perform_restore(&ctx, &state, &run, &request).await
    })()
    .await;
    json::respond(result, debug)
}

pub(crate) async fn delete_run(
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<BackupState>>,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| async {
        let _tenant = require_tenant(&req)?;
        let _scope = require_company_for_write(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_BACKUP_MANAGE)?;
        let run_id = path.into_inner();
        let run = visible_run(&state, &run_id, &_scope)?;
        if run.status == BackupStatus::Running {
            return Err(UnifiedError::conflict(
                "Backup run is in progress and cannot be deleted yet",
            ));
        }
        let mut storage_deleted = false;
        if let Some(key) = &run.storage_key {
            if let Some(provider) = ctx.storage_registry.get(&run.target) {
                if provider.delete(key).await.is_ok() {
                    storage_deleted = true;
                }
            }
        }
        state.runs.remove(&run_id);
        Ok(json!({
            "deleted": true,
            "id": run_id,
            "storage_deleted": storage_deleted,
        }))
    })()
    .await;
    json::respond(result, debug)
}
