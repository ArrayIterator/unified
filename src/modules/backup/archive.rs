// Backup module - archive building and extraction.
//
// Split from `src/modules/backup.rs`; the module root re-exports the public API of this module.

use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::json;
use flate2::read::GzDecoder;
use flate2::write::GzEncoder;
use flate2::Compression;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use tar::{Archive, Builder, Header};
use walkdir::WalkDir;

use super::config::{find_binary, truncate_stderr};
use super::constants::{CONFIG_NAME, DUMP_NAME, MANIFEST_NAME};
use super::state::{now, sha256_hex};
use super::types::{BackupJob, BackupKind, BackupRun, BackupSettings, DbSnapshot};

// ---------------------------------------------------------------------------
// Archive building (blocking work).
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct ManifestFile {
    pub(crate) path: String,
    pub(crate) size_bytes: u64,
    pub(crate) sha256: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct Manifest {
    pub(crate) job_id: String,
    pub(crate) kind: String,
    pub(crate) created_at: String,
    #[serde(default)]
    pub(crate) encryption: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) database: Option<Value>,
    pub(crate) files: Vec<ManifestFile>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) config: Option<Value>,
    pub(crate) file_count: u64,
    pub(crate) total_bytes: u64,
}

pub(crate) struct ArchiveInput {
    pub(crate) job_id: String,
    pub(crate) kind: BackupKind,
    pub(crate) database_name: String,
    pub(crate) db: DbSnapshot,
    pub(crate) paths: Vec<String>,
    pub(crate) config_snapshot: Value,
    pub(crate) settings: BackupSettings,
}

pub(crate) struct ArchiveOutput {
    pub(crate) bytes: Vec<u8>,
    pub(crate) file_count: u64,
    pub(crate) total_bytes: u64,
}

/// Run `pg_dump` for one database and return the plain-format dump.
/// The password is passed through the environment only, and no part of
/// the command line or the error output ever contains it. The dump is
/// streamed from the child in chunks and aborted as soon as it
/// exceeds `max_bytes`, so an unexpectedly large database never sits
/// fully buffered in memory before the size check.
pub(crate) fn run_pg_dump(
    db: &DbSnapshot,
    pg_dump_bin: &str,
    max_bytes: u64,
) -> UnifiedResult<Vec<u8>> {
    if db.driver != "postgresql" {
        return Err(UnifiedError::unprocessable(format!(
            "Database backups require the postgresql driver, got \"{}\"",
            db.driver
        )));
    }
    let binary = find_binary(pg_dump_bin).ok_or_else(|| {
        UnifiedError::new(
            format!(
                "pg_dump binary \"{pg_dump_bin}\" was not found on PATH; install PostgreSQL client tools or set backup.pg_dump_bin"
            ),
            503,
        )
    })?;
    let mut command = Command::new(binary);
    command
        .arg("-h")
        .arg(&db.host)
        .arg("-p")
        .arg(db.port.to_string())
        .arg("-U")
        .arg(&db.username)
        .arg("-d")
        .arg(&db.database)
        .arg("--no-password")
        .arg("--format=plain");
    if !db.password.is_empty() {
        command.env("PGPASSWORD", &db.password);
    }
    let mut child = command
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let mut stdout = child.stdout.take().ok_or_else(|| {
        UnifiedError::internal("pg_dump stdout was not captured".to_string())
    })?;
    let mut dump: Vec<u8> = Vec::new();
    let mut chunk = [0u8; 8192];
    loop {
        let read = stdout.read(&mut chunk)?;
        if read == 0 {
            break;
        }
        if dump.len() as u64 + read as u64 > max_bytes {
            // Stop the child before it can produce more output, then
            // reap it so no process is left behind.
            let _ = child.kill();
            let _ = child.wait();
            return Err(UnifiedError::unprocessable(format!(
                "Database dump exceeds the total size limit of {max_bytes} bytes"
            )));
        }
        dump.extend_from_slice(&chunk[..read]);
    }
    drop(stdout);
    let output = child.wait_with_output()?;
    if !output.status.success() {
        let detail = truncate_stderr(&output.stderr);
        return Err(UnifiedError::internal(format!(
            "pg_dump failed for database \"{}\"{}{}",
            db.database,
            if detail.is_empty() { "" } else { ": " },
            detail
        )));
    }
    Ok(dump)
}

/// Walk the configured paths and read every regular file under the
/// allowlisted base directory. A path passed directly that is itself a
/// symlink is rejected: symlinks are never followed. Directory walks
/// do not follow symlinks either. Path components below the argument
/// may still be resolved by canonicalization, but the resolved path
/// must stay inside the base directory or the source is rejected.
pub(crate) fn collect_files(
    paths: &[String],
    settings: &BackupSettings,
) -> UnifiedResult<Vec<(String, Vec<u8>, ManifestFile)>> {
    let base = std::fs::canonicalize(&settings.base_dir).map_err(|error| {
        UnifiedError::bad_request(format!(
            "Backup base directory \"{}\" is not accessible: {error}",
            settings.base_dir
        ))
    })?;
    let mut collected: Vec<(String, Vec<u8>, ManifestFile)> = Vec::new();
    let mut total: u64 = 0;
    for input in paths {
        let joined = if Path::new(input).is_absolute() {
            PathBuf::from(input)
        } else {
            base.join(input)
        };
        // Reject a directly-passed symlink before canonicalization
        // resolves it: the target would otherwise be read as if the
        // link itself had been allowlisted.
        let link_meta = std::fs::symlink_metadata(&joined).map_err(|_| {
            UnifiedError::not_found(format!("Backup source \"{input}\" does not exist"))
        })?;
        if link_meta.file_type().is_symlink() {
            return Err(UnifiedError::forbidden(format!(
                "Backup source \"{input}\" is a symlink; symlinks are never followed"
            )));
        }
        let canonical = std::fs::canonicalize(&joined).map_err(|_| {
            UnifiedError::not_found(format!("Backup source \"{input}\" does not exist"))
        })?;
        if !canonical.starts_with(&base) {
            return Err(UnifiedError::forbidden(format!(
                "Backup source \"{input}\" escapes the allowlisted base directory"
            )));
        }
        let mut files: Vec<PathBuf> = Vec::new();
        if canonical.is_file() {
            files.push(canonical.clone());
        } else if canonical.is_dir() {
            for entry in WalkDir::new(&canonical).follow_links(false) {
                let entry = entry.map_err(|error| {
                    UnifiedError::internal(format!("Failed to walk backup source: {error}"))
                })?;
                let file_type = entry.file_type();
                if file_type.is_file() && !file_type.is_symlink() {
                    files.push(entry.path().to_path_buf());
                }
            }
        } else {
            return Err(UnifiedError::bad_request(format!(
                "Backup source \"{input}\" is neither a file nor a directory"
            )));
        }
        for file in files {
            let bytes = std::fs::read(&file)?;
            let size = bytes.len() as u64;
            if size > settings.max_file_bytes {
                return Err(UnifiedError::unprocessable(format!(
                    "File \"{}\" exceeds the per-file limit of {} bytes",
                    file.display(),
                    settings.max_file_bytes
                )));
            }
            total = total.saturating_add(size);
            if total > settings.max_total_bytes {
                return Err(UnifiedError::unprocessable(format!(
                    "Backup sources exceed the total size limit of {} bytes",
                    settings.max_total_bytes
                )));
            }
            let relative = file
                .strip_prefix(&base)
                .map_err(|_| {
                    UnifiedError::internal("Backup file escaped the base directory")
                })?
                .to_string_lossy()
                .replace('\\', "/");
            let manifest_file = ManifestFile {
                path: relative.clone(),
                size_bytes: size,
                sha256: sha256_hex(&bytes),
            };
            collected.push((relative, bytes, manifest_file));
        }
    }
    collected.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(collected)
}

/// Build the tar.gz archive for one backup run. Runs on a blocking
/// thread: it walks the filesystem and invokes `pg_dump`.
pub(crate) fn build_archive(input: ArchiveInput) -> UnifiedResult<ArchiveOutput> {
    let mut manifest = Manifest {
        job_id: input.job_id.clone(),
        kind: input.kind.as_str().to_string(),
        created_at: now().to_rfc3339(),
        // Archives are stored unencrypted (at-rest encryption is
        // not supported — see the module header).
        encryption: "none".to_string(),
        database: None,
        files: Vec::new(),
        config: None,
        file_count: 0,
        total_bytes: 0,
    };
    // (archive path, bytes) pairs, manifest excluded.
    let mut entries: Vec<(String, Vec<u8>)> = Vec::new();
    let mut total: u64 = 0;

    if input.kind.includes_database() {
        let dump_db = DbSnapshot {
            database: input.database_name.clone(),
            ..input.db.clone()
        };
        // The dump is streamed with the size cap enforced while it is
        // produced (see `run_pg_dump`); it never sits in memory
        // unchecked, so no second size check is needed here.
        let dump = run_pg_dump(
            &dump_db,
            &input.settings.pg_dump_bin,
            input.settings.max_total_bytes,
        )?;
        total = total.saturating_add(dump.len() as u64);
        manifest.database = Some(json!({
            "database": dump_db.database,
            "driver": dump_db.driver,
            "host": dump_db.host,
            "bytes": dump.len(),
            "format": "pg_dump plain",
        }));
        entries.push((DUMP_NAME.to_string(), dump));
    }

    if input.kind.includes_files() {
        let files = collect_files(&input.paths, &input.settings)?;
        for (relative, bytes, manifest_file) in files {
            total = total.saturating_add(bytes.len() as u64);
            manifest.files.push(manifest_file);
            entries.push((relative, bytes));
        }
    }

    if input.kind.includes_config() {
        manifest.config = Some(input.config_snapshot.clone());
        let config_bytes = serde_json::to_vec_pretty(&input.config_snapshot)
            .map_err(|error| UnifiedError::internal(format!("Failed to encode config snapshot: {error}")))?;
        total = total.saturating_add(config_bytes.len() as u64);
        entries.push((CONFIG_NAME.to_string(), config_bytes));
    }

    if total > input.settings.max_total_bytes {
        return Err(UnifiedError::unprocessable(format!(
            "Backup payload exceeds the total size limit of {} bytes",
            input.settings.max_total_bytes
        )));
    }
    manifest.file_count = manifest.files.len() as u64;
    manifest.total_bytes = total;

    let manifest_bytes = serde_json::to_vec_pretty(&manifest)
        .map_err(|error| UnifiedError::internal(format!("Failed to encode manifest: {error}")))?;
    let encoder = GzEncoder::new(Vec::new(), Compression::default());
    let mut builder = Builder::new(encoder);
    let mut append = |builder: &mut Builder<GzEncoder<Vec<u8>>>,
                      path: &str,
                      bytes: &[u8]|
     -> UnifiedResult<()> {
        let mut header = Header::new_gnu();
        header.set_path(path)?;
        header.set_size(bytes.len() as u64);
        header.set_mode(0o644);
        header.set_mtime(now().timestamp() as u64);
        header.set_cksum();
        builder.append_data(&mut header, path, bytes)?;
        Ok(())
    };
    append(&mut builder, MANIFEST_NAME, &manifest_bytes)?;
    for (path, bytes) in &entries {
        append(&mut builder, path, bytes)?;
    }
    let encoder = builder.into_inner()?;
    let bytes = encoder.finish()?;
    Ok(ArchiveOutput {
        bytes,
        file_count: manifest.file_count,
        total_bytes: manifest.total_bytes,
    })
}

/// Read every entry of a tar.gz archive into memory, enforcing the
/// same per-file and total size caps as the build path. A tampered or
/// foreign archive cannot turn into a decompression bomb: any entry
/// larger than `max_file_bytes`, or a cumulative decompressed size
/// above `max_total_bytes`, is rejected before more is read.
pub(crate) fn extract_archive_with_limits(
    bytes: &[u8],
    max_file_bytes: u64,
    max_total_bytes: u64,
) -> UnifiedResult<Vec<(String, Vec<u8>)>> {
    let decoder = GzDecoder::new(bytes);
    let mut archive = Archive::new(decoder);
    let mut out: Vec<(String, Vec<u8>)> = Vec::new();
    let mut total: u64 = 0;
    let entries = archive.entries()?;
    for entry in entries {
        let mut entry = entry?;
        if entry.header().entry_type().is_dir() {
            continue;
        }
        let path = entry
            .path()?
            .to_string_lossy()
            .replace('\\', "/");
        // Read at most one byte past the per-file cap: the extra byte
        // proves the entry is too large without buffering the rest.
        let mut data = Vec::new();
        let mut capped = entry.take(max_file_bytes.saturating_add(1));
        capped.read_to_end(&mut data)?;
        if data.len() as u64 > max_file_bytes {
            return Err(UnifiedError::unprocessable(format!(
                "Archive entry \"{path}\" exceeds the per-file limit of {max_file_bytes} bytes"
            )));
        }
        total = total.saturating_add(data.len() as u64);
        if total > max_total_bytes {
            return Err(UnifiedError::unprocessable(format!(
                "Archive entries exceed the total size limit of {max_total_bytes} bytes"
            )));
        }
        out.push((path, data));
    }
    Ok(out)
}

/// Read every entry of a tar.gz archive into memory with the default
/// size caps. Prefer [`extract_archive_with_limits`] with the
/// configured limits when they are available.
pub(crate) fn extract_archive(bytes: &[u8]) -> UnifiedResult<Vec<(String, Vec<u8>)>> {
    let defaults = BackupSettings::default();
    extract_archive_with_limits(bytes, defaults.max_file_bytes, defaults.max_total_bytes)
}

// ---------------------------------------------------------------------------
// Execution.
// ---------------------------------------------------------------------------

pub(crate) fn audit_payload(job: &BackupJob, run: Option<&BackupRun>, extra: Value) -> Value {
    let mut payload = json!({
        "actor": "system",
        "action": "backup",
        "resource": format!("backup:{}", job.id),
        "id": job.id,
        "job_id": job.id,
        "job_name": job.name,
        "kind": job.kind.as_str(),
        "target": job.target,
    });
    if let Some(run) = run {
        payload["run_id"] = Value::String(run.id.clone());
    }
    if let Value::Object(extra_map) = extra {
        if let Value::Object(map) = &mut payload {
            for (key, value) in extra_map {
                map.insert(key, value);
            }
        }
    }
    payload
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_dir(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("backup-archive-test-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create test dir");
        dir
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
    fn extract_archive_accepts_entries_within_limits() {
        let bytes = make_tar_gz(&[("a.txt", b"hello"), ("b.txt", b"world")]);
        let out = extract_archive_with_limits(&bytes, 10, 100).expect("extract");
        assert_eq!(out.len(), 2);
        assert_eq!(out[0], ("a.txt".to_string(), b"hello".to_vec()));
        // The plain wrapper enforces the default caps.
        assert_eq!(extract_archive(&bytes).expect("extract defaults").len(), 2);
    }

    #[test]
    fn extract_archive_rejects_oversized_file() {
        let bytes = make_tar_gz(&[("big.txt", b"0123456789A")]);
        let error = extract_archive_with_limits(&bytes, 10, 100).expect_err("must reject");
        assert!(error.message.contains("per-file limit"), "got: {}", error.message);
    }

    #[test]
    fn extract_archive_rejects_excess_total() {
        let bytes = make_tar_gz(&[("a.txt", b"012345"), ("b.txt", b"012345")]);
        let error = extract_archive_with_limits(&bytes, 10, 10).expect_err("must reject");
        assert!(error.message.contains("total size limit"), "got: {}", error.message);
    }

    #[test]
    fn collect_files_rejects_direct_symlink_argument() {
        let base = test_dir("symlink");
        std::fs::write(base.join("real.txt"), b"data").expect("write real file");
        #[cfg(unix)]
        std::os::unix::fs::symlink(base.join("real.txt"), base.join("link.txt"))
            .expect("create symlink");
        let settings = BackupSettings {
            base_dir: base.to_string_lossy().to_string(),
            ..Default::default()
        };
        let error = collect_files(&["link.txt".to_string()], &settings).expect_err("must reject");
        assert!(error.message.contains("symlink"), "got: {}", error.message);
        // The real file itself still collects fine.
        let collected = collect_files(&["real.txt".to_string()], &settings).expect("collect");
        assert_eq!(collected.len(), 1);
        assert_eq!(collected[0].0, "real.txt");
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn collect_files_skips_symlinks_inside_directories() {
        let base = test_dir("walk-symlink");
        let sub = base.join("sub");
        std::fs::create_dir_all(&sub).expect("create subdir");
        std::fs::write(sub.join("real.txt"), b"data").expect("write real file");
        #[cfg(unix)]
        std::os::unix::fs::symlink(sub.join("real.txt"), sub.join("link.txt"))
            .expect("create symlink");
        let settings = BackupSettings {
            base_dir: base.to_string_lossy().to_string(),
            ..Default::default()
        };
        let collected = collect_files(&["sub".to_string()], &settings).expect("collect");
        assert_eq!(collected.len(), 1);
        assert_eq!(collected[0].0, "sub/real.txt");
        let _ = std::fs::remove_dir_all(&base);
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

    fn fake_pg_dump(name: &str, script: &str) -> PathBuf {
        let dir = test_dir(name);
        let path = dir.join("pg_dump");
        std::fs::write(&path, script).expect("write fake pg_dump");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))
                .expect("chmod fake pg_dump");
        }
        path
    }

    #[test]
    fn run_pg_dump_rejects_non_postgres_driver() {
        let db = DbSnapshot {
            driver: "sqlite".to_string(),
            ..pg_db()
        };
        let error = run_pg_dump(&db, "pg_dump", 1024).expect_err("must reject");
        assert!(error.message.contains("postgresql"), "got: {}", error.message);
    }

    #[test]
    fn run_pg_dump_streams_small_output() {
        let bin = fake_pg_dump("small", "#!/bin/sh\nprintf 'hello\\n'");
        let out = run_pg_dump(&pg_db(), &bin.to_string_lossy(), 1024).expect("dump");
        assert_eq!(out, b"hello\n");
        let _ = std::fs::remove_dir_all(bin.parent().expect("parent"));
    }

    #[test]
    fn run_pg_dump_aborts_oversized_output_while_streaming() {
        let bin = fake_pg_dump("big", "#!/bin/sh\nhead -c 100000 /dev/zero");
        let error = run_pg_dump(&pg_db(), &bin.to_string_lossy(), 1024).expect_err("must reject");
        assert!(
            error.message.contains("exceeds the total size limit"),
            "got: {}",
            error.message
        );
        let _ = std::fs::remove_dir_all(bin.parent().expect("parent"));
    }
}
