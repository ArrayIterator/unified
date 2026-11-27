// Backup module - restore workers.
//
// Split from `src/modules/backup.rs`; the module root re-exports the public API of this module.

use unified_core::config::AppConfig;
use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::HookBus;
use unified_core::json;
use serde_json::{json, Value};
use std::io::Write;
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Stdio};

use super::config::{find_binary, get_dotted_value, path_inside_base, set_dotted_value, truncate_stderr};
use super::constants::{BACKUP_RUN_KIND, CONFIG_NAME, DUMP_NAME, MANIFEST_NAME, MAX_NAME_LEN, MAX_PATH_LEN, RESTORABLE_CONFIG_KEYS, SCHEDULER_ENQUEUE};
use super::types::{BackupJob, BackupKind, BackupSettings, DbSnapshot, SchedulingOutcome};
use super::validation::{valid_config_key, valid_database_name, valid_schedule, valid_storage_name};

// Restore (blocking parts).

/// Outcome of restoring files: the restored relative paths plus the
/// previous contents of each destination (`None` when the file did
/// not exist before the restore), so a later failed restore step can
/// roll the file changes back.
pub(crate) struct FileRestoreOutcome {
    pub(crate) restored: Vec<String>,
    pub(crate) previous: Vec<(String, Option<Vec<u8>>)>,
}

/// Reject a restore destination that is a symlink. Writing through
/// it would follow the link out of the base directory, so a
/// pre-existing link at the destination is never trusted, even when
/// it happens to point inside the base directory.
pub(crate) fn reject_symlink_destination(destination: &Path, path: &str) -> UnifiedResult<()> {
    if let Ok(metadata) = std::fs::symlink_metadata(destination) {
        if metadata.file_type().is_symlink() {
            return Err(UnifiedError::forbidden(format!(
                "Archive entry \"{path}\" points at a symlinked destination; refusing to follow it"
            )));
        }
    }
    Ok(())
}

/// Undo a file restore from a snapshot taken by
/// [`restore_files_blocking`]: delete files that did not exist
/// before and rewrite the previous bytes of files that did. Returns
/// the relative paths that could not be rolled back (empty means the
/// rollback succeeded fully).
pub(crate) fn rollback_files_blocking(
    base_dir: &str,
    previous: &[(String, Option<Vec<u8>>)],
) -> Vec<String> {
    let Ok(base) = std::fs::canonicalize(base_dir) else {
        return previous.iter().map(|(path, _)| path.clone()).collect();
    };
    let mut failed: Vec<String> = Vec::new();
    for (path, prior) in previous {
        let relative = Path::new(path);
        if relative.is_absolute()
            || relative
                .components()
                .any(|component| matches!(component, Component::ParentDir | Component::RootDir))
        {
            failed.push(path.clone());
            continue;
        }
        let destination = base.join(relative);
        let rolled_back = match prior {
            None => std::fs::remove_file(&destination)
                .or_else(|error| {
                    if error.kind() == std::io::ErrorKind::NotFound {
                        Ok(())
                    } else {
                        Err(error)
                    }
                })
                .is_ok(),
            Some(bytes) => {
                reject_symlink_destination(&destination, path).is_ok()
                    && std::fs::write(&destination, bytes).is_ok()
            }
        };
        if !rolled_back {
            failed.push(path.clone());
        }
    }
    failed
}

/// Write archived files back under the allowlisted base directory.
/// Archive entries with absolute paths or parent-directory components
/// are rejected, and every destination is re-checked after its parent
/// directories are created. A pre-existing symlink at a destination
/// is rejected rather than followed.
///
/// The restore runs in two passes: first every entry is validated
/// and the current contents of each destination are snapshotted,
/// then the files are written. A validation failure therefore writes
/// nothing, and a write failure rolls back the files already written
/// in this call, so the files step does not leave a half-restored
/// directory behind. The returned snapshot lets the caller roll the
/// files back if a later restore step (configuration, database)
/// fails; see `perform_restore`.
pub(crate) fn restore_files_blocking(
    entries: &[(String, Vec<u8>)],
    base_dir: &str,
) -> UnifiedResult<FileRestoreOutcome> {
    let base = std::fs::canonicalize(base_dir).map_err(|error| {
        UnifiedError::bad_request(format!(
            "Backup base directory \"{base_dir}\" is not accessible: {error}"
        ))
    })?;
    // Validation + snapshot pass: nothing is written yet.
    struct Planned<'a> {
        path: &'a str,
        bytes: &'a [u8],
        destination: PathBuf,
        previous: Option<Vec<u8>>,
    }
    let mut planned: Vec<Planned<'_>> = Vec::new();
    for (path, bytes) in entries {
        if path == MANIFEST_NAME || path == CONFIG_NAME || path == DUMP_NAME {
            continue;
        }
        let relative = Path::new(path);
        if relative.is_absolute()
            || relative
                .components()
                .any(|component| matches!(component, Component::ParentDir | Component::RootDir))
        {
            return Err(UnifiedError::forbidden(format!(
                "Archive entry \"{path}\" escapes the allowlisted base directory"
            )));
        }
        let destination = base.join(relative);
        if let Some(parent) = destination.parent() {
            std::fs::create_dir_all(parent)?;
            let canonical_parent = std::fs::canonicalize(parent)?;
            if !canonical_parent.starts_with(&base) {
                return Err(UnifiedError::forbidden(format!(
                    "Archive entry \"{path}\" escapes the allowlisted base directory"
                )));
            }
        }
        reject_symlink_destination(&destination, path)?;
        let previous = match std::fs::read(&destination) {
            Ok(bytes) => Some(bytes),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => return Err(error.into()),
        };
        planned.push(Planned {
            path,
            bytes,
            destination,
            previous,
        });
    }
    // Write pass: on failure, roll back what this call already wrote.
    let mut restored: Vec<String> = Vec::new();
    let mut previous: Vec<(String, Option<Vec<u8>>)> = Vec::new();
    for item in planned {
        // Re-check right before the write: the destination must still
        // not be a symlink, even if it was fine a moment ago.
        if let Err(error) = reject_symlink_destination(&item.destination, item.path) {
            let _ = rollback_files_blocking(
                base_dir,
                &previous
                    .iter()
                    .map(|(path, prior)| (path.clone(), prior.clone()))
                    .collect::<Vec<_>>(),
            );
            return Err(error);
        }
        if let Err(error) = std::fs::write(&item.destination, item.bytes) {
            let _ = rollback_files_blocking(
                base_dir,
                &previous
                    .iter()
                    .map(|(path, prior)| (path.clone(), prior.clone()))
                    .collect::<Vec<_>>(),
            );
            return Err(error.into());
        }
        restored.push(item.path.to_string());
        previous.push((item.path.to_string(), item.previous));
    }
    restored.sort();
    Ok(FileRestoreOutcome { restored, previous })
}

/// Apply the allowlisted, non-secret configuration keys from a
/// snapshot to the live configuration. Secret values are never
/// applied, and the whole reload is validated before it takes effect.
pub(crate) fn restore_config_blocking(
    entries: &[(String, Vec<u8>)],
    ctx: &AppContext,
) -> UnifiedResult<Vec<String>> {
    let Some((_, config_bytes)) = entries.iter().find(|(path, _)| path == CONFIG_NAME) else {
        return Err(UnifiedError::not_found(
            "Archive contains no configuration snapshot",
        ));
    };
    let snapshot: Value = serde_json::from_slice(config_bytes)?;
    let current = ctx.config.read().expect("config lock poisoned");
    let mut merged = serde_json::to_value(&*current).unwrap_or(Value::Null);
    drop(current);
    let mut applied: Vec<String> = Vec::new();
    for key in RESTORABLE_CONFIG_KEYS {
        let segments: Vec<&str> = key.split('.').collect();
        if let Some(value) = get_dotted_value(&snapshot, &segments) {
            if value.as_str() == Some("********") {
                continue;
            }
            if let Value::Object(map) = &mut merged {
                set_dotted_value(map, &segments, value.clone());
                applied.push(key.to_string());
            }
        }
    }
    if applied.is_empty() {
        return Err(UnifiedError::not_found(
            "Archive configuration snapshot has no restorable keys",
        ));
    }
    let new_config: AppConfig = serde_json::from_value(merged).map_err(|error| {
        UnifiedError::bad_request(format!("Restored configuration is invalid: {error}"))
    })?;
    ctx.reload_config(new_config)?;
    Ok(applied)
}

/// Restore a database from the `dump.sql` entry of an archive by
/// piping it into `psql`. Only runs with explicit confirmation.
pub(crate) fn restore_database_blocking(
    entries: &[(String, Vec<u8>)],
    db: &DbSnapshot,
    psql_bin: &str,
) -> UnifiedResult<String> {
    if db.driver != "postgresql" {
        return Err(UnifiedError::unprocessable(format!(
            "Database restores require the postgresql driver, got \"{}\"",
            db.driver
        )));
    }
    let Some((_, dump)) = entries.iter().find(|(path, _)| path == DUMP_NAME) else {
        return Err(UnifiedError::not_found(
            "Archive contains no database dump",
        ));
    };
    let binary = find_binary(psql_bin).ok_or_else(|| {
        UnifiedError::new(
            format!(
                "psql binary \"{psql_bin}\" was not found on PATH; install PostgreSQL client tools or set backup.psql_bin"
            ),
            503,
        )
    })?;
    let mut child = Command::new(binary)
        .arg("-h")
        .arg(&db.host)
        .arg("-p")
        .arg(db.port.to_string())
        .arg("-U")
        .arg(&db.username)
        .arg("-d")
        .arg(&db.database)
        .arg("--no-password")
        .arg("-v")
        .arg("ON_ERROR_STOP=1")
        .arg("-q")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .env("PGPASSWORD", db.password.as_str())
        .spawn()?;
    if let Some(mut stdin) = child.stdin.take() {
        // A failed write must not leak the child: kill it and reap it
        // before returning, otherwise `psql` keeps running with a
        // broken stdin pipe and becomes a zombie.
        if let Err(error) = stdin.write_all(dump) {
            let _ = child.kill();
            let _ = child.wait();
            return Err(error.into());
        }
    }
    let output = child.wait_with_output()?;
    if !output.status.success() {
        let detail = truncate_stderr(&output.stderr);
        return Err(UnifiedError::internal(format!(
            "psql restore failed for database \"{}\"{}{}",
            db.database,
            if detail.is_empty() { "" } else { ": " },
            detail
        )));
    }
    Ok(format!(
        "psql -h {} -p {} -U {} -d {} < dump.sql",
        db.host, db.port, db.username, db.database
    ))
}

// Scheduling bridge.

/// Tell the scheduler about a job with a cron schedule. Returns whether
/// the scheduler accepted the job; a refusal is recorded on the job
/// instead of failing the request, because run-now keeps working.
pub(crate) fn notify_scheduler(hooks: &HookBus, job: &BackupJob) -> SchedulingOutcome {
    if job.schedule.trim().is_empty() || !job.enabled {
        return SchedulingOutcome {
            enqueued: false,
            message: "Manual-only job: no cron schedule configured or the job is disabled."
                .to_string(),
        };
    }
    let context = hooks.emit(
        SCHEDULER_ENQUEUE,
        json!({
            "kind": BACKUP_RUN_KIND,
            "payload": { "backup_job_id": job.id, "job_id": job.id },
        }),
    );
    for result in &context.results {
        if let Some(queued) = result.get("queued").and_then(|value| value.as_bool()) {
            if queued {
                return SchedulingOutcome {
                    enqueued: true,
                    message: "Scheduled with the scheduler queue.".to_string(),
                };
            }
            let detail = result
                .get("error")
                .and_then(|value| value.as_str())
                .unwrap_or("Scheduler refused the backup job.");
            return SchedulingOutcome {
                enqueued: false,
                message: format!(
                    "Scheduler did not accept the job ({detail}). Run-now still works; the scheduler needs a \"backup.run\" dispatch arm."
                ),
            };
        }
    }
    SchedulingOutcome {
        enqueued: false,
        message: "No scheduler.enqueue handler is registered; the scheduler module may be disabled."
            .to_string(),
    }
}

// Validation of job input.

pub(crate) fn validate_job_fields(
    kind: BackupKind,
    database_name: &str,
    paths: &[String],
    config_keys: &[String],
    settings: &BackupSettings,
) -> UnifiedResult<()> {
    if kind.includes_database() && !database_name.trim().is_empty() && !valid_database_name(database_name.trim()) {
        return Err(UnifiedError::bad_request(
            "database_name must contain only letters, digits, '-' or '_' (max 63 characters)",
        ));
    }
    if kind.includes_files() {
        if paths.is_empty() {
            return Err(UnifiedError::bad_request(
                "File and full backups require at least one source path",
            ));
        }
        for path in paths {
            if path.trim().is_empty() || path.len() > MAX_PATH_LEN {
                return Err(UnifiedError::bad_request(
                    "Backup source paths must be non-empty and at most 1024 characters",
                ));
            }
            if !path_inside_base(path, &settings.base_dir) {
                return Err(UnifiedError::forbidden(format!(
                    "Backup source \"{path}\" escapes the allowlisted base directory \"{}\"",
                    settings.base_dir
                )));
            }
        }
    }
    for key in config_keys {
        if !valid_config_key(key) {
            return Err(UnifiedError::bad_request(format!(
                "Invalid configuration key \"{key}\": use top-level sections or dotted paths"
            )));
        }
    }
    Ok(())
}

pub(crate) fn validate_common(
    name: &str,
    target: &str,
    schedule: &str,
    retention_keep_last: u32,
    retention_keep_days: u32,
) -> UnifiedResult<()> {
    if name.trim().is_empty() || name.len() > MAX_NAME_LEN {
        return Err(UnifiedError::bad_request(
            "Job name must be non-empty and at most 128 characters",
        ));
    }
    if !valid_storage_name(target) {
        return Err(UnifiedError::bad_request(
            "Target must be a registered storage name (letters, digits, '-', '_' or '.')",
        ));
    }
    if !valid_schedule(schedule) {
        return Err(UnifiedError::bad_request(
            "Schedule must be empty (manual-only) or a five-field cron expression such as \"0 2 * * *\"",
        ));
    }
    if retention_keep_last == 0 || retention_keep_last > 100 {
        return Err(UnifiedError::bad_request(
            "retention_keep_last must be between 1 and 100",
        ));
    }
    if retention_keep_days > 3650 {
        return Err(UnifiedError::bad_request(
            "retention_keep_days must be between 0 and 3650",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_dir(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("backup-restore-test-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create test dir");
        dir
    }

    fn pg_db() -> DbSnapshot {
        DbSnapshot {
            driver: "postgresql".to_string(),
            host: "127.0.0.1".to_string(),
            port: 5432,
            username: "user".to_string(),
            password: String::new(),
            database: "db".to_string(),
        }
    }

    #[test]
    fn restore_files_rejects_symlinked_destination() {
        let base = test_dir("symlink-dest");
        let outside = test_dir("symlink-outside");
        std::fs::write(outside.join("target.txt"), b"original").expect("write target");
        #[cfg(unix)]
        std::os::unix::fs::symlink(outside.join("target.txt"), base.join("evil.txt"))
            .expect("create symlink");
        let entries = vec![("evil.txt".to_string(), b"pwned".to_vec())];
        let error =
            restore_files_blocking(&entries, &base.to_string_lossy()).expect_err("must reject");
        assert!(error.message.contains("symlink"), "got: {}", error.message);
        assert_eq!(
            std::fs::read(outside.join("target.txt")).expect("read target"),
            b"original"
        );
        let _ = std::fs::remove_dir_all(&base);
        let _ = std::fs::remove_dir_all(&outside);
    }

    #[test]
    fn restore_files_writes_nothing_when_a_later_entry_is_invalid() {
        let base = test_dir("validate-first");
        let entries = vec![
            ("good.txt".to_string(), b"good".to_vec()),
            ("../escape.txt".to_string(), b"bad".to_vec()),
        ];
        let error =
            restore_files_blocking(&entries, &base.to_string_lossy()).expect_err("must reject");
        assert!(error.message.contains("escapes"), "got: {}", error.message);
        assert!(!base.join("good.txt").exists());
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn rollback_files_restores_previous_contents() {
        let base = test_dir("rollback");
        std::fs::write(base.join("existing.txt"), b"old").expect("write existing");
        let entries = vec![
            ("existing.txt".to_string(), b"new".to_vec()),
            ("added.txt".to_string(), b"added".to_vec()),
        ];
        let outcome =
            restore_files_blocking(&entries, &base.to_string_lossy()).expect("restore");
        assert_eq!(outcome.restored, vec!["added.txt", "existing.txt"]);
        assert_eq!(
            std::fs::read(base.join("existing.txt")).expect("read"),
            b"new"
        );
        let failed = rollback_files_blocking(&base.to_string_lossy(), &outcome.previous);
        assert!(failed.is_empty(), "rollback failed for: {failed:?}");
        assert_eq!(
            std::fs::read(base.join("existing.txt")).expect("read"),
            b"old"
        );
        assert!(!base.join("added.txt").exists());
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn restore_database_returns_error_without_leaking_child_on_broken_pipe() {
        // `/usr/bin/true` exits immediately without reading stdin, so
        // writing a dump larger than the pipe buffer fails with a
        // broken pipe. The call must return (not hang) and reap the
        // child instead of leaking it.
        let dump = vec![b'x'; 8 * 1024 * 1024];
        let entries = vec![(DUMP_NAME.to_string(), dump)];
        let error = restore_database_blocking(&entries, &pg_db(), "/usr/bin/true")
            .expect_err("must fail on broken pipe");
        assert!(
            error.message.contains("Broken pipe") || error.message.contains("I/O error"),
            "got: {}",
            error.message
        );
    }
}
