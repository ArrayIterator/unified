// Backup module - backup run execution, verification, and drills.
//
// Split from `src/modules/backup.rs`; the module root re-exports the public API of this module.

use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::json;
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use serde_json::{json, Value};
use std::path::{Component, Path, PathBuf};
use tar::Builder;

use super::archive::{ArchiveInput, Manifest, audit_payload, build_archive, extract_archive_with_limits};
use super::config::{snapshot_config};
use super::constants::{BACKUP_COMPLETED, BACKUP_DRILL_FAILED, BACKUP_FAILED, BACKUP_STARTED, BACKUP_VERIFICATION_FAILED, CONFIG_NAME, DUMP_NAME, MANIFEST_NAME};
use super::restore::reject_symlink_destination;
use super::state::{BackupExecutor, BackupState, new_id, now, sha256_hex};
use super::types::{BackupJob, BackupRun, BackupSettings, BackupStatus, DrillView, MaintenancePruneView, MaintenanceVerifyView, VerifyView};
use super::validation::{job_visible, notify_system, run_visible};

/// Atomically claim the running guard for a job. A single `insert`
/// both tests and sets, so two concurrent `start_run` calls cannot
/// both pass the check: the loser sees the previous entry and gets
/// `false`. Returns `true` when this call won the claim.
pub(crate) fn try_claim_running(running: &DashMap<String, ()>, job_id: &str) -> bool {
    running.insert(job_id.to_string(), ()).is_none()
}

/// Create the run record and mark the job running. The returned run is
/// then executed in the background by `execute_run`.
pub(crate) fn start_run(executor: &BackupExecutor, job_id: &str) -> UnifiedResult<BackupRun> {
    let job = executor
        .state
        .jobs
        .get(job_id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found("Backup job not found"))?;
    if !job.enabled {
        return Err(UnifiedError::unprocessable(format!(
            "Backup job \"{}\" is disabled",
            job.name
        )));
    }
    if !try_claim_running(&executor.state.running, job_id) {
        return Err(UnifiedError::conflict(format!(
            "Backup job \"{}\" is already running",
            job.name
        )));
    }
    let started_at = now();
    let run = BackupRun {
        id: new_id(),
        job_id: job.id.clone(),
        job_name: job.name.clone(),
        company_id: job.company_id.clone(),
        kind: job.kind,
        target: job.target.clone(),
        storage_key: None,
        status: BackupStatus::Running,
        checksum: None,
        size_bytes: None,
        file_count: None,
        verified: None,
        last_verified_at: None,
        manifest_verified: None,
        verification_error: None,
        last_drill_at: None,
        drill_verified: None,
        // Archives are stored unencrypted (at-rest encryption is
        // not supported — see the module header); the recorded mode
        // is always `none`.
        encryption: Some("none".to_string()),
        started_at,
        finished_at: None,
        error: None,
        created_at: started_at,
    };
    // The running guard was already claimed atomically by
    // `try_claim_running` above; it is released by `execute_run`.
    executor.state.runs.insert(run.id.clone(), run.clone());
    if let Some(mut stored) = executor.state.jobs.get_mut(job_id) {
        stored.status = BackupStatus::Running;
        stored.started_at = Some(started_at);
        stored.finished_at = None;
        stored.error = None;
        stored.last_run_id = Some(run.id.clone());
        stored.updated_at = started_at;
    }
    Ok(run)
}

pub(crate) fn fail_run(executor: &BackupExecutor, run_id: &str, job_id: &str, message: String) {
    let finished_at = now();
    if let Some(mut run) = executor.state.runs.get_mut(run_id) {
        run.status = BackupStatus::Failed;
        run.finished_at = Some(finished_at);
        run.error = Some(message.clone());
    }
    if let Some(mut job) = executor.state.jobs.get_mut(job_id) {
        job.status = BackupStatus::Failed;
        job.finished_at = Some(finished_at);
        job.error = Some(message.clone());
        job.updated_at = finished_at;
    }
}

/// Select the runs a retention pass should delete, without touching
/// state. Completed and failed runs are each subject to the keep-last
/// / keep-days policy; the single newest failed run is always kept
/// for diagnosis, so a job that keeps failing never loses its latest
/// failure. Running or pending runs are never selected.
pub(crate) fn select_stale_runs(
    runs: &[BackupRun],
    keep_last: u32,
    cutoff: Option<DateTime<Utc>>,
) -> Vec<BackupRun> {
    let mut ordered: Vec<&BackupRun> = runs
        .iter()
        .filter(|run| matches!(run.status, BackupStatus::Completed | BackupStatus::Failed))
        .collect();
    ordered.sort_by(|a, b| {
        b.finished_at
            .unwrap_or(b.started_at)
            .cmp(&a.finished_at.unwrap_or(a.started_at))
    });
    let mut completed_seen: u32 = 0;
    let mut failed_seen: u32 = 0;
    let mut stale: Vec<BackupRun> = Vec::new();
    for run in ordered {
        let (index, is_newest_failure) = match run.status {
            BackupStatus::Completed => {
                let index = completed_seen;
                completed_seen += 1;
                (index, false)
            }
            BackupStatus::Failed => {
                let index = failed_seen;
                failed_seen += 1;
                (index, index == 0)
            }
            _ => continue,
        };
        if is_newest_failure {
            continue;
        }
        let too_many = index >= keep_last;
        let too_old = cutoff.is_some_and(|cutoff| {
            run.finished_at.unwrap_or(run.started_at) < cutoff
        });
        if too_many || too_old {
            stale.push((*run).clone());
        }
    }
    stale
}

/// Delete completed and failed runs beyond the retention policy: keep
/// the newest `retention_keep_last` runs and drop runs older than
/// `retention_keep_days`. The newest failed run is always kept for
/// diagnosis. Stored archives are deleted best-effort.
/// Returns `(runs_pruned, archive_deletes_failed)`.
pub(crate) async fn prune_retention(executor: &BackupExecutor, job: &BackupJob) -> (u64, u64) {
    let runs: Vec<BackupRun> = executor
        .state
        .runs
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|run| {
            run.job_id == job.id
                && matches!(run.status, BackupStatus::Completed | BackupStatus::Failed)
        })
        .collect();
    let cutoff = if job.retention_keep_days > 0 {
        Some(now() - chrono::Duration::days(job.retention_keep_days as i64))
    } else {
        None
    };
    let stale = select_stale_runs(&runs, job.retention_keep_last, cutoff);
    if stale.is_empty() {
        return (0, 0);
    }
    let provider = executor.provider_for(&job.target);
    let mut pruned: u64 = 0;
    let mut failed: u64 = 0;
    for run in stale {
        if let (Some(key), Ok(provider)) = (&run.storage_key, &provider) {
            if provider.delete(key).await.is_err() {
                failed += 1;
            }
        }
        executor.state.runs.remove(&run.id);
        pruned += 1;
    }
    (pruned, failed)
}

pub(crate) async fn execute_run_inner(
    executor: &BackupExecutor,
    job_id: &str,
    run_id: &str,
) -> UnifiedResult<BackupRun> {
    let job = executor
        .state
        .jobs
        .get(job_id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found("Backup job not found"))?;
    executor.hooks.emit(
        BACKUP_STARTED,
        audit_payload(&job, None, json!({ "run_id": run_id })),
    );

    let database_name = if job.database_name.trim().is_empty() {
        executor.db.database.clone()
    } else {
        job.database_name.clone()
    };
    let config_snapshot = snapshot_config(&executor.config_snapshot, &job.config_keys);
    let input = ArchiveInput {
        job_id: job.id.clone(),
        kind: job.kind,
        database_name,
        db: executor.db.clone(),
        paths: job.paths.clone(),
        config_snapshot,
        settings: executor.settings.clone(),
    };

    let built = tokio::task::spawn_blocking(move || build_archive(input)).await;
    let output = match built {
        Ok(Ok(output)) => output,
        Ok(Err(error)) => {
            fail_run(executor, run_id, job_id, error.message.clone());
            executor.hooks.emit(
                BACKUP_FAILED,
                audit_payload(&job, None, json!({ "run_id": run_id, "error": error.message })),
            );
            return Err(error);
        }
        Err(join_error) => {
            let message = format!("Backup worker failed: {join_error}");
            fail_run(executor, run_id, job_id, message.clone());
            executor.hooks.emit(
                BACKUP_FAILED,
                audit_payload(&job, None, json!({ "run_id": run_id, "error": message })),
            );
            return Err(UnifiedError::internal(message));
        }
    };

    let timestamp = now().format("%Y%m%dT%H%M%SZ");
    let run_short: String = run_id.chars().take(8).collect();
    let storage_key = format!(
        "backups/{}/{timestamp}-{run_short}-{}.tar.gz",
        job.id,
        job.kind.as_str()
    );
    let provider = match executor.provider_for(&job.target) {
        Ok(provider) => provider,
        Err(error) => {
            fail_run(executor, run_id, job_id, error.message.clone());
            executor.hooks.emit(
                BACKUP_FAILED,
                audit_payload(&job, None, json!({ "run_id": run_id, "error": error.message })),
            );
            return Err(error);
        }
    };
    let checksum = sha256_hex(&output.bytes);
    let size_bytes = output.bytes.len() as u64;
    if let Err(error) = provider.upload(&storage_key, output.bytes).await {
        fail_run(executor, run_id, job_id, error.message.clone());
        executor.hooks.emit(
            BACKUP_FAILED,
            audit_payload(&job, None, json!({ "run_id": run_id, "error": error.message })),
        );
        return Err(error);
    }

    let finished_at = now();
    let run = BackupRun {
        id: run_id.to_string(),
        job_id: job.id.clone(),
        job_name: job.name.clone(),
        company_id: job.company_id.clone(),
        kind: job.kind,
        target: job.target.clone(),
        storage_key: Some(storage_key.clone()),
        status: BackupStatus::Completed,
        checksum: Some(checksum.clone()),
        size_bytes: Some(size_bytes),
        file_count: Some(output.file_count),
        verified: None,
        last_verified_at: None,
        manifest_verified: None,
        verification_error: None,
        last_drill_at: None,
        drill_verified: None,
        // Archives are stored unencrypted (at-rest encryption is
        // not supported — see the module header); the recorded mode
        // is always `none`.
        encryption: Some("none".to_string()),
        started_at: job.started_at.unwrap_or(now()),
        finished_at: Some(finished_at),
        error: None,
        created_at: finished_at,
    };
    executor.state.runs.insert(run.id.clone(), run.clone());
    if let Some(mut stored) = executor.state.jobs.get_mut(job_id) {
        stored.status = BackupStatus::Completed;
        stored.checksum = Some(checksum);
        stored.size_bytes = Some(size_bytes);
        stored.storage_key = Some(storage_key);
        stored.finished_at = Some(finished_at);
        stored.error = None;
        stored.updated_at = finished_at;
    }
    executor.hooks.emit(
        BACKUP_COMPLETED,
        audit_payload(
            &job,
            Some(&run),
            json!({ "size_bytes": size_bytes, "file_count": output.file_count }),
        ),
    );
    prune_retention(executor, &job).await;
    Ok(run)
}

/// Execute one run to completion, always releasing the running guard.
pub(crate) async fn execute_run(executor: BackupExecutor, job_id: String, run_id: String) {
    let result = execute_run_inner(&executor, &job_id, &run_id).await;
    executor.state.running.remove(&job_id);
    if let Err(error) = result {
        // The failure is already recorded on the run and the job; the
        // message is kept out of the background task's logs.
        let _ = error;
    }
}

/// Spawn a backup worker the way the scheduler does: on the current
/// Tokio runtime when there is one, otherwise on a dedicated thread
/// with its own runtime (hook handlers are synchronous).
pub(crate) fn spawn_backup_worker<F>(future: F)
where
    F: std::future::Future<Output = ()> + Send + 'static,
{
    if tokio::runtime::Handle::try_current().is_ok() {
        tokio::spawn(future);
    } else {
        std::thread::spawn(move || {
            let Ok(runtime) = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            else {
                return;
            };
            runtime.block_on(future);
        });
    }
}

/// Result of inspecting one stored archive: the checksum comparison
/// plus the manifest cross-check (job id, kind, and the SHA-256 of
/// every listed file). `entries` is the extracted archive, reused by
/// restore and drill so the archive is downloaded and parsed once.
pub(crate) struct ArchiveInspection {
    pub(crate) actual_checksum: String,
    pub(crate) checksum_verified: bool,
    pub(crate) manifest_verified: bool,
    pub(crate) manifest_job_matches: bool,
    pub(crate) files_checked: u64,
    pub(crate) files_mismatched: Vec<String>,
    pub(crate) dump_present: bool,
    pub(crate) config_present: bool,
    pub(crate) entries: Vec<(String, Vec<u8>)>,
}

/// Inspect archive bytes: recompute the checksum, parse `manifest.json`,
/// and verify every file the manifest lists against the archive
/// entries. Blocking work: run it on a blocking thread. Extraction
/// uses the default size caps; prefer
/// [`inspect_archive_bytes_with_limits`] with the configured caps
/// when they are available.
pub(crate) fn inspect_archive_bytes(bytes: &[u8], run: &BackupRun) -> UnifiedResult<ArchiveInspection> {
    let defaults = BackupSettings::default();
    inspect_archive_bytes_with_limits(
        bytes,
        run,
        defaults.max_file_bytes,
        defaults.max_total_bytes,
    )
}

/// Inspect archive bytes with explicit extraction caps (see
/// `extract_archive_with_limits`): a tampered archive cannot expand
/// past `max_total_bytes` decompressed bytes while being inspected.
pub(crate) fn inspect_archive_bytes_with_limits(
    bytes: &[u8],
    run: &BackupRun,
    max_file_bytes: u64,
    max_total_bytes: u64,
) -> UnifiedResult<ArchiveInspection> {
    let actual = sha256_hex(bytes);
    let checksum_verified = run.checksum.as_deref() == Some(actual.as_str());
    let entries = extract_archive_with_limits(bytes, max_file_bytes, max_total_bytes)?;
    let mut mismatched: Vec<String> = Vec::new();
    let dump_present = entries.iter().any(|(path, _)| path == DUMP_NAME);
    let config_present = entries.iter().any(|(path, _)| path == CONFIG_NAME);
    let Some((_, manifest_bytes)) = entries.iter().find(|(path, _)| path == MANIFEST_NAME)
    else {
        return Err(UnifiedError::unprocessable(
            "Archive manifest is missing: the archive is not a backup produced by this module",
        ));
    };
    let manifest: Manifest = serde_json::from_slice(manifest_bytes).map_err(|error| {
        UnifiedError::unprocessable(format!("Archive manifest is unreadable: {error}"))
    })?;
    let manifest_job_matches =
        manifest.job_id == run.job_id && manifest.kind == run.kind.as_str();
    if !manifest_job_matches {
        mismatched.push(format!(
            "manifest belongs to job \"{}\" (kind \"{}\"), not this run",
            manifest.job_id, manifest.kind
        ));
    }
    if run.kind.includes_database() && !dump_present {
        mismatched.push(format!("{DUMP_NAME} is missing from the archive"));
    }
    if run.kind.includes_config() && !config_present {
        mismatched.push(format!("{CONFIG_NAME} is missing from the archive"));
    }
    for listed in &manifest.files {
        match entries.iter().find(|(path, _)| *path == listed.path) {
            None => mismatched.push(format!("missing file: {}", listed.path)),
            Some((_, data)) => {
                if data.len() as u64 != listed.size_bytes {
                    mismatched.push(format!("size mismatch: {}", listed.path));
                } else if sha256_hex(data) != listed.sha256 {
                    mismatched.push(format!("checksum mismatch: {}", listed.path));
                }
            }
        }
    }
    for (path, _) in &entries {
        if path == MANIFEST_NAME || path == CONFIG_NAME || path == DUMP_NAME {
            continue;
        }
        if !manifest.files.iter().any(|listed| &listed.path == path) {
            mismatched.push(format!("unexpected entry: {path}"));
        }
    }
    // Cross-check the manifest's own totals against the archive: a
    // manifest that lists files correctly can still lie about how
    // many files and bytes the backup holds.
    if manifest.file_count != manifest.files.len() as u64 {
        mismatched.push(format!(
            "manifest file_count {} does not match {} listed files",
            manifest.file_count,
            manifest.files.len()
        ));
    }
    let actual_file_entries = entries
        .iter()
        .filter(|(path, _)| *path != MANIFEST_NAME && *path != CONFIG_NAME && *path != DUMP_NAME)
        .count() as u64;
    if manifest.file_count != actual_file_entries {
        mismatched.push(format!(
            "manifest file_count {} does not match {} file entries in the archive",
            manifest.file_count, actual_file_entries
        ));
    }
    let actual_total_bytes: u64 = entries
        .iter()
        .filter(|(path, _)| *path != MANIFEST_NAME)
        .map(|(_, data)| data.len() as u64)
        .sum();
    if manifest.total_bytes != actual_total_bytes {
        mismatched.push(format!(
            "manifest total_bytes {} does not match {} bytes in the archive",
            manifest.total_bytes, actual_total_bytes
        ));
    }
    let manifest_verified = manifest_job_matches && mismatched.is_empty();
    Ok(ArchiveInspection {
        actual_checksum: actual,
        checksum_verified,
        manifest_verified,
        manifest_job_matches,
        files_checked: manifest.files.len() as u64,
        files_mismatched: mismatched,
        dump_present,
        config_present,
        entries,
    })
}

/// Record a verification outcome on the run so the scheduled sweep
/// can skip freshly verified runs.
pub(crate) fn record_verification(
    state: &BackupState,
    run_id: &str,
    verified: bool,
    manifest_verified: bool,
    error: Option<String>,
) {
    if let Some(mut run) = state.runs.get_mut(run_id) {
        run.verified = Some(verified);
        run.last_verified_at = Some(now());
        run.manifest_verified = Some(manifest_verified);
        run.verification_error = error;
    }
}

/// Recompute the checksum of a stored archive and verify its
/// manifest. The archive bytes are downloaded in full; the
/// decompressed size is bounded by the extraction caps enforced
/// while the archive is parsed, so a tampered archive cannot expand
/// without limit during verification.
pub(crate) async fn verify_run_archive(
    executor: &BackupExecutor,
    run: &BackupRun,
) -> UnifiedResult<VerifyView> {
    let storage_key = run.storage_key.clone().ok_or_else(|| {
        UnifiedError::unprocessable("Run has no stored archive to verify")
    })?;
    if run.status != BackupStatus::Completed {
        return Err(UnifiedError::unprocessable(
            "Only completed runs can be verified",
        ));
    }
    let provider = executor.provider_for(&run.target)?;
    let bytes = provider.download(&storage_key).await?;
    let archive_len = bytes.len() as u64;
    let run_for_inspect = run.clone();
    let max_file_bytes = executor.settings.max_file_bytes;
    let max_total_bytes = executor.settings.max_total_bytes;
    let inspection = tokio::task::spawn_blocking(move || {
        inspect_archive_bytes_with_limits(&bytes, &run_for_inspect, max_file_bytes, max_total_bytes)
    })
    .await
    .map_err(|error| UnifiedError::internal(format!("Verification worker failed: {error}")))?;
    let inspection = match inspection {
        Ok(inspection) => inspection,
        Err(error) => {
            record_verification(
                &executor.state,
                &run.id,
                false,
                false,
                Some(error.message.clone()),
            );
            return Err(error);
        }
    };
    let verified = inspection.checksum_verified && inspection.manifest_verified;
    let error = if verified {
        None
    } else if !inspection.checksum_verified {
        Some("Archive checksum does not match the recorded checksum".to_string())
    } else {
        Some(inspection.files_mismatched.join("; "))
    };
    record_verification(
        &executor.state,
        &run.id,
        verified,
        inspection.manifest_verified,
        error,
    );
    Ok(VerifyView {
        run_id: run.id.clone(),
        job_id: run.job_id.clone(),
        target: run.target.clone(),
        verified,
        expected_checksum: run.checksum.clone(),
        actual_checksum: inspection.actual_checksum,
        size_bytes: archive_len,
        manifest_verified: inspection.manifest_verified,
        manifest_job_matches: inspection.manifest_job_matches,
        files_checked: inspection.files_checked,
        files_mismatched: inspection.files_mismatched,
        dump_present: inspection.dump_present,
        config_present: inspection.config_present,
    })
}

// ---------------------------------------------------------------------------
// Restore drill (blocking parts).
// ---------------------------------------------------------------------------

/// Extract an archive into a throwaway sandbox directory, check the
/// payload is usable (dump non-empty, config parses), then delete
/// the sandbox. Returns `(dump_present, config_present,
/// sandbox_removed)`. Any failure best-effort removes the sandbox
/// before the error propagates, so a drill never leaves data behind.
pub(crate) fn drill_extract_blocking(
    entries: &[(String, Vec<u8>)],
    drill_dir: &str,
    run_id: &str,
) -> UnifiedResult<(bool, bool, bool)> {
    let sandbox = PathBuf::from(drill_dir).join(format!("drill-{run_id}"));
    let result: UnifiedResult<(bool, bool)> = (|| {
        std::fs::create_dir_all(&sandbox)?;
        let canonical_sandbox = std::fs::canonicalize(&sandbox)?;
        let mut dump_present = false;
        let mut config_present = false;
        for (path, bytes) in entries {
            if path == DUMP_NAME {
                if bytes.is_empty() {
                    return Err(UnifiedError::unprocessable(
                        "Archive database dump is empty",
                    ));
                }
                dump_present = true;
                continue;
            }
            if path == CONFIG_NAME {
                serde_json::from_slice::<Value>(bytes).map_err(|error| {
                    UnifiedError::unprocessable(format!(
                        "Archive configuration snapshot is unreadable: {error}"
                    ))
                })?;
                config_present = true;
                continue;
            }
            if path == MANIFEST_NAME {
                continue;
            }
            let relative = Path::new(path);
            if relative.is_absolute()
                || relative.components().any(|component| {
                    matches!(component, Component::ParentDir | Component::RootDir)
                })
            {
                return Err(UnifiedError::forbidden(format!(
                    "Archive entry \"{path}\" escapes the drill sandbox"
                )));
            }
            let destination = canonical_sandbox.join(relative);
            if let Some(parent) = destination.parent() {
                std::fs::create_dir_all(parent)?;
                let canonical_parent = std::fs::canonicalize(parent)?;
                if !canonical_parent.starts_with(&canonical_sandbox) {
                    return Err(UnifiedError::forbidden(format!(
                        "Archive entry \"{path}\" escapes the drill sandbox"
                    )));
                }
            }
            // A pre-existing symlink at the destination would be
            // followed by the write; reject it like the restore path.
            reject_symlink_destination(&destination, path)?;
            std::fs::write(&destination, bytes)?;
        }
        Ok((dump_present, config_present))
    })();
    // The sandbox is always removed, on success and on failure.
    let removed = std::fs::remove_dir_all(&sandbox).is_ok() || !sandbox.exists();
    let (dump_present, config_present) = result?;
    Ok((dump_present, config_present, removed))
}

/// Run a restore drill for one completed run: verify checksum +
/// manifest, then extract to a throwaway sandbox and delete it.
/// Live data is never touched. The outcome is recorded on the run.
pub(crate) async fn drill_run_archive(
    executor: &BackupExecutor,
    run: &BackupRun,
) -> UnifiedResult<DrillView> {
    let storage_key = run.storage_key.clone().ok_or_else(|| {
        UnifiedError::unprocessable("Run has no stored archive to drill")
    })?;
    if run.status != BackupStatus::Completed {
        return Err(UnifiedError::unprocessable(
            "Only completed runs can be drilled",
        ));
    }
    let provider = executor.provider_for(&run.target)?;
    let bytes = provider.download(&storage_key).await?;
    let run_for_drill = run.clone();
    let drill_dir = executor.settings.drill_dir.clone();
    let max_file_bytes = executor.settings.max_file_bytes;
    let max_total_bytes = executor.settings.max_total_bytes;
    let outcome = tokio::task::spawn_blocking(move || {
        let inspection = inspect_archive_bytes_with_limits(
            &bytes,
            &run_for_drill,
            max_file_bytes,
            max_total_bytes,
        )?;
        let (dump_present, config_present, sandbox_removed) =
            drill_extract_blocking(&inspection.entries, &drill_dir, &run_for_drill.id)?;
        Ok::<_, UnifiedError>((
            inspection.checksum_verified,
            inspection.manifest_verified,
            inspection.files_checked,
            inspection.files_mismatched,
            dump_present,
            config_present,
            sandbox_removed,
        ))
    })
    .await
    .map_err(|error| UnifiedError::internal(format!("Drill worker failed: {error}")))?;
    let outcome = match outcome {
        Ok(outcome) => outcome,
        Err(error) => {
            if let Some(mut stored) = executor.state.runs.get_mut(&run.id) {
                stored.last_drill_at = Some(now());
                stored.drill_verified = Some(false);
            }
            return Err(error);
        }
    };
    let (
        checksum_verified,
        manifest_verified,
        files_checked,
        files_mismatched,
        dump_present,
        config_present,
        sandbox_removed,
    ) = outcome;
    let verified = checksum_verified && manifest_verified && sandbox_removed;
    if let Some(mut stored) = executor.state.runs.get_mut(&run.id) {
        stored.last_drill_at = Some(now());
        stored.drill_verified = Some(verified);
    }
    Ok(DrillView {
        run_id: run.id.clone(),
        job_id: run.job_id.clone(),
        target: run.target.clone(),
        verified,
        checksum_verified,
        manifest_verified,
        files_checked,
        files_mismatched,
        dump_present,
        config_present,
        sandbox_removed,
    })
}

/// Verify every completed run whose last verification is older than
/// the configured interval (or never verified). Failures emit
/// `backup.verification_failed` and a `system` alert.
pub(crate) async fn verify_stale_runs(
    executor: &BackupExecutor,
    company: &Option<String>,
) -> MaintenanceVerifyView {
    let interval = chrono::Duration::hours(executor.settings.verify_interval_hours as i64);
    let cutoff = now() - interval;
    let mut runs: Vec<BackupRun> = executor
        .state
        .runs
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|run| {
            run.status == BackupStatus::Completed
                && run_visible(run, company)
                && run
                    .last_verified_at
                    .is_none_or(|verified_at| verified_at < cutoff)
        })
        .collect();
    runs.sort_by(|a, b| a.started_at.cmp(&b.started_at));
    let mut checked: u64 = 0;
    let mut verified_count: u64 = 0;
    let mut failed: u64 = 0;
    let mut failures: Vec<Value> = Vec::new();
    for run in runs {
        checked += 1;
        match verify_run_archive(executor, &run).await {
            Ok(view) if view.verified => verified_count += 1,
            Ok(view) => {
                failed += 1;
                let detail = if view.files_mismatched.is_empty() {
                    "checksum mismatch".to_string()
                } else {
                    view.files_mismatched.join("; ")
                };
                failures.push(json!({
                    "run_id": run.id,
                    "job_id": run.job_id,
                    "error": detail,
                }));
                fail_alert(executor, &run, "verification", &detail);
            }
            Err(error) => {
                failed += 1;
                failures.push(json!({
                    "run_id": run.id,
                    "job_id": run.job_id,
                    "error": error.message,
                }));
                fail_alert(executor, &run, "verification", &error.message);
            }
        }
    }
    MaintenanceVerifyView {
        checked,
        verified: verified_count,
        failed,
        failures,
    }
}

/// Emit the failure hook and the `system` alert for a verification
/// or drill failure.
pub(crate) fn fail_alert(executor: &BackupExecutor, run: &BackupRun, what: &str, detail: &str) {
    let event = if what == "drill" {
        BACKUP_DRILL_FAILED
    } else {
        BACKUP_VERIFICATION_FAILED
    };
    executor.hooks.emit(
        event,
        json!({
            "run_id": run.id,
            "job_id": run.job_id,
            "job_name": run.job_name,
            "target": run.target,
            "error": detail,
        }),
    );
    let kind = format!("backup.{what}_failed");
    notify_system(
        &executor.hooks,
        &kind,
        format!("Backup {what} failed: {}", run.job_name),
        format!(
            "Backup {what} for job {} ({}) failed: {detail}",
            run.job_name, run.job_id
        ),
    );
}

/// Enforce retention for every job the caller may see. Returns the
/// per-job outcome.
pub(crate) async fn prune_all_jobs(
    executor: &BackupExecutor,
    company: &Option<String>,
) -> MaintenancePruneView {
    let mut jobs: Vec<BackupJob> = executor
        .state
        .jobs
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|job| job_visible(job, company))
        .collect();
    jobs.sort_by(|a, b| a.name.cmp(&b.name));
    let mut runs_pruned: u64 = 0;
    let mut archives_failed: u64 = 0;
    let mut per_job: Vec<Value> = Vec::new();
    for job in &jobs {
        let (pruned, failed) = prune_retention(executor, job).await;
        runs_pruned += pruned;
        archives_failed += failed;
        per_job.push(json!({
            "job_id": job.id,
            "job_name": job.name,
            "runs_pruned": pruned,
            "archives_failed": failed,
        }));
    }
    MaintenancePruneView {
        jobs_checked: jobs.len() as u64,
        runs_pruned,
        archives_failed,
        per_job,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration;
    use flate2::Compression;
    use flate2::write::GzEncoder;
    use tar::Header;

    fn make_run(id: &str, status: BackupStatus, days_ago: i64) -> BackupRun {
        let at = now() - Duration::days(days_ago);
        BackupRun {
            id: id.to_string(),
            job_id: "job-1".to_string(),
            job_name: "job".to_string(),
            company_id: None,
            kind: BackupKind::Files,
            target: "local".to_string(),
            storage_key: None,
            status,
            checksum: None,
            size_bytes: None,
            file_count: None,
            verified: None,
            last_verified_at: None,
            manifest_verified: None,
            verification_error: None,
            last_drill_at: None,
            drill_verified: None,
            encryption: None,
            started_at: at,
            finished_at: Some(at),
            error: None,
            created_at: at,
        }
    }

    fn make_tar_gz(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let encoder = GzEncoder::new(Vec::new(), Compression::default());
        let mut builder = Builder::new(encoder);
        for (path, bytes) in entries {
            let mut header = Header::new_gnu();
            header.set_path(path).expect("set tar path");
            header.set_size(bytes.len() as u64);
            header.set_mode(0o644);
            header.set_cksum();
            builder
                .append_data(&mut header, path, *bytes)
                .expect("append tar entry");
        }
        builder.finish().expect("finish tar").finish().expect("finish gzip")
    }

    #[test]
    fn try_claim_running_is_single_winner() {
        let running = DashMap::new();
        assert!(try_claim_running(&running, "job-1"));
        assert!(!try_claim_running(&running, "job-1"));
        assert!(try_claim_running(&running, "job-2"));
        running.remove("job-1");
        assert!(try_claim_running(&running, "job-1"));
    }

    #[test]
    fn select_stale_runs_prunes_completed_beyond_keep_last() {
        let runs = vec![
            make_run("c1", BackupStatus::Completed, 0),
            make_run("c2", BackupStatus::Completed, 1),
            make_run("c3", BackupStatus::Completed, 2),
            make_run("c4", BackupStatus::Completed, 3),
        ];
        let stale = select_stale_runs(&runs, 2, None);
        let ids: Vec<&str> = stale.iter().map(|run| run.id.as_str()).collect();
        assert_eq!(ids, vec!["c3", "c4"]);
    }

    #[test]
    fn select_stale_runs_prunes_failed_but_keeps_newest_failure() {
        let runs = vec![
            make_run("f1", BackupStatus::Failed, 0),
            make_run("f2", BackupStatus::Failed, 5),
            make_run("f3", BackupStatus::Failed, 9),
        ];
        // Same keep-last policy, but the newest failure is kept even
        // when older failures are pruned by age.
        let cutoff = Some(now() - Duration::days(1));
        let stale = select_stale_runs(&runs, 7, cutoff);
        let ids: Vec<&str> = stale.iter().map(|run| run.id.as_str()).collect();
        assert_eq!(ids, vec!["f2", "f3"]);
        // With no age cutoff and a generous keep-last, the older
        // failures are kept too; only the newest-failure rule matters.
        let stale = select_stale_runs(&runs, 7, None);
        assert!(stale.is_empty());
        // With keep_last 1, every failure but the newest is stale.
        let stale = select_stale_runs(&runs, 1, None);
        let ids: Vec<&str> = stale.iter().map(|run| run.id.as_str()).collect();
        assert_eq!(ids, vec!["f2", "f3"]);
    }

    #[test]
    fn select_stale_runs_never_prunes_running_runs() {
        let runs = vec![
            make_run("r1", BackupStatus::Running, 30),
            make_run("c1", BackupStatus::Completed, 30),
        ];
        let stale = select_stale_runs(&runs, 1, None);
        let ids: Vec<&str> = stale.iter().map(|run| run.id.as_str()).collect();
        assert_eq!(ids, vec!["c1"]);
    }

    fn manifest_archive(file_count: u64, total_bytes: u64) -> Vec<u8> {
        let manifest = json!({
            "job_id": "job-1",
            "kind": "files",
            "created_at": now().to_rfc3339(),
            "encryption": "none",
            "files": [{
                "path": "a.txt",
                "size_bytes": 5,
                "sha256": sha256_hex(b"hello"),
            }],
            "file_count": file_count,
            "total_bytes": total_bytes,
        });
        let manifest_bytes = serde_json::to_vec(&manifest).expect("encode manifest");
        make_tar_gz(&[("manifest.json", &manifest_bytes), ("a.txt", b"hello")])
    }

    #[test]
    fn inspect_flags_manifest_count_and_total_mismatch() {
        let run = make_run("run-1", BackupStatus::Completed, 0);
        let bytes = manifest_archive(99, 9999);
        let inspection = inspect_archive_bytes(&bytes, &run).expect("inspect");
        assert!(!inspection.manifest_verified);
        assert!(
            inspection
                .files_mismatched
                .iter()
                .any(|message| message.contains("file_count 99")),
            "mismatches: {:?}",
            inspection.files_mismatched
        );
        assert!(
            inspection
                .files_mismatched
                .iter()
                .any(|message| message.contains("total_bytes 9999")),
            "mismatches: {:?}",
            inspection.files_mismatched
        );
    }

    #[test]
    fn inspect_accepts_consistent_manifest_counts() {
        let run = make_run("run-1", BackupStatus::Completed, 0);
        let bytes = manifest_archive(1, 5);
        let inspection = inspect_archive_bytes(&bytes, &run).expect("inspect");
        assert!(inspection.files_mismatched.is_empty());
        assert!(inspection.manifest_verified);
    }

    #[test]
    fn drill_extract_rejects_symlinked_destination() {
        let drill_dir = std::env::temp_dir().join(format!(
            "backup-drill-test-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&drill_dir);
        std::fs::create_dir_all(&drill_dir).expect("create drill dir");
        let outside = drill_dir.join("outside.txt");
        std::fs::write(&outside, b"original").expect("write outside");
        let sandbox = drill_dir.join("drill-run-1");
        std::fs::create_dir_all(&sandbox).expect("create sandbox");
        #[cfg(unix)]
        std::os::unix::fs::symlink(&outside, sandbox.join("evil.txt")).expect("create symlink");
        let entries = vec![("evil.txt".to_string(), b"pwned".to_vec())];
        let error = drill_extract_blocking(&entries, &drill_dir.to_string_lossy(), "run-1")
            .expect_err("must reject");
        assert!(error.message.contains("symlink"), "got: {}", error.message);
        assert_eq!(std::fs::read(&outside).expect("read outside"), b"original");
        assert!(!sandbox.exists(), "drill sandbox must be removed after failure");
        let _ = std::fs::remove_dir_all(&drill_dir);
    }
}
