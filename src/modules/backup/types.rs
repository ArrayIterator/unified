// Backup module - shared types and request/response shapes.
//
// Split from `src/modules/backup.rs`; the module root re-exports the public API of this module.

use unified_core::config::AppConfig;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;

// Types.

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum BackupKind {
    Database,
    Files,
    Config,
    Full,
}

impl BackupKind {
    pub(crate) fn as_str(&self) -> &'static str {
        match self {
            BackupKind::Database => "database",
            BackupKind::Files => "files",
            BackupKind::Config => "config",
            BackupKind::Full => "full",
        }
    }

    pub(crate) fn includes_database(&self) -> bool {
        matches!(self, BackupKind::Database | BackupKind::Full)
    }

    pub(crate) fn includes_files(&self) -> bool {
        matches!(self, BackupKind::Files | BackupKind::Full)
    }

    pub(crate) fn includes_config(&self) -> bool {
        matches!(self, BackupKind::Config | BackupKind::Full)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum BackupStatus {
    Pending,
    Running,
    Completed,
    Failed,
}

impl BackupStatus {
    pub(crate) fn as_str(&self) -> &'static str {
        match self {
            BackupStatus::Pending => "pending",
            BackupStatus::Running => "running",
            BackupStatus::Completed => "completed",
            BackupStatus::Failed => "failed",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SchedulingOutcome {
    pub enqueued: bool,
    pub message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BackupJob {
    pub id: String,
    pub name: String,
    pub kind: BackupKind,
    /// Owning company id (UUID string) from the verified caller
    /// and the tenant context at creation. `None` means a global
    /// job (created without a company); global jobs are visible
    /// only to callers with no resolvable company, never to a
    /// company caller.
    #[serde(default)]
    pub company_id: Option<String>,
    /// Database name to dump. Empty means the configured database.
    #[serde(default)]
    pub database_name: String,
    /// Filesystem paths to capture, relative to `backup.base_dir` or
    /// absolute but inside it.
    #[serde(default)]
    pub paths: Vec<String>,
    /// Configuration keys to snapshot (top-level sections or dotted
    /// paths). Empty means the whole configuration.
    #[serde(default)]
    pub config_keys: Vec<String>,
    /// Name of the storage target that receives the archive.
    pub target: String,
    /// Cron expression with five fields, or empty for manual-only jobs.
    #[serde(default)]
    pub schedule: String,
    /// Keep at most this many completed runs for the job.
    pub retention_keep_last: u32,
    /// Delete completed runs older than this many days (0 keeps them).
    pub retention_keep_days: u32,
    pub enabled: bool,
    /// Outcome of the last `scheduler.enqueue` attempt for this job.
    pub scheduling: SchedulingOutcome,
    /// Last-run summary, denormalized from the latest run.
    pub status: BackupStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub checksum: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub size_bytes: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub storage_key: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_run_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub started_at: Option<DateTime<Utc>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub finished_at: Option<DateTime<Utc>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BackupRun {
    pub id: String,
    pub job_id: String,
    pub job_name: String,
    /// Owning company id, copied from the job at run creation.
    #[serde(default)]
    pub company_id: Option<String>,
    pub kind: BackupKind,
    pub target: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub storage_key: Option<String>,
    pub status: BackupStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub checksum: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub size_bytes: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file_count: Option<u64>,
    /// Outcome of the last integrity verification: `None` means the
    /// run has never been verified.
    #[serde(default)]
    pub verified: Option<bool>,
    #[serde(default)]
    pub last_verified_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub manifest_verified: Option<bool>,
    #[serde(default)]
    pub verification_error: Option<String>,
    /// Outcome of the last restore drill (`None` = never drilled).
    #[serde(default)]
    pub last_drill_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub drill_verified: Option<bool>,
    /// Encryption mode recorded at run time. Archives are stored
    /// unencrypted (at-rest encryption is not supported - see the
    /// module header), so the recorded value is always `none`.
    /// The field stays so a future encrypted format can be told
    /// apart from today's plain archives.
    #[serde(default)]
    pub encryption: Option<String>,
    pub started_at: DateTime<Utc>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub finished_at: Option<DateTime<Utc>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct BackupSummary {
    pub(crate) jobs_total: u64,
    pub(crate) jobs_by_status: BTreeMap<String, u64>,
    pub(crate) runs_total: u64,
    pub(crate) runs_by_status: BTreeMap<String, u64>,
    pub(crate) targets: Vec<String>,
    pub(crate) base_dir: String,
    pub(crate) drill_dir: String,
    pub(crate) max_total_bytes: u64,
    pub(crate) max_file_bytes: u64,
    pub(crate) default_retention_keep_last: u32,
    pub(crate) default_retention_keep_days: u32,
    pub(crate) verify_interval_hours: u64,
    pub(crate) encryption: String,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct VerifyView {
    pub(crate) run_id: String,
    pub(crate) job_id: String,
    pub(crate) target: String,
    pub(crate) verified: bool,
    pub(crate) expected_checksum: Option<String>,
    pub(crate) actual_checksum: String,
    pub(crate) size_bytes: u64,
    pub(crate) manifest_verified: bool,
    pub(crate) manifest_job_matches: bool,
    pub(crate) files_checked: u64,
    pub(crate) files_mismatched: Vec<String>,
    pub(crate) dump_present: bool,
    pub(crate) config_present: bool,
}

/// Outcome of a restore drill: the archive was verified and
/// extracted to a throwaway sandbox directory that was deleted
/// afterwards. Live data is never written.
#[derive(Debug, Clone, Serialize)]
pub(crate) struct DrillView {
    pub(crate) run_id: String,
    pub(crate) job_id: String,
    pub(crate) target: String,
    pub(crate) verified: bool,
    pub(crate) checksum_verified: bool,
    pub(crate) manifest_verified: bool,
    pub(crate) files_checked: u64,
    pub(crate) files_mismatched: Vec<String>,
    pub(crate) dump_present: bool,
    pub(crate) config_present: bool,
    pub(crate) sandbox_removed: bool,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct MaintenanceVerifyView {
    pub(crate) checked: u64,
    pub(crate) verified: u64,
    pub(crate) failed: u64,
    pub(crate) failures: Vec<Value>,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct MaintenancePruneView {
    pub(crate) jobs_checked: u64,
    pub(crate) runs_pruned: u64,
    pub(crate) archives_failed: u64,
    pub(crate) per_job: Vec<Value>,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct RestoreView {
    pub(crate) run_id: String,
    pub(crate) job_id: String,
    pub(crate) kind: String,
    pub(crate) dry_run: bool,
    pub(crate) planned: bool,
    pub(crate) steps: Vec<String>,
    pub(crate) restored_files: Vec<String>,
    pub(crate) applied_config_keys: Vec<String>,
    pub(crate) database_restored: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) database_plan: Option<Value>,
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct CreateBackupJob {
    pub(crate) name: String,
    pub(crate) kind: BackupKind,
    #[serde(default)]
    pub(crate) database_name: Option<String>,
    #[serde(default)]
    pub(crate) paths: Option<Vec<String>>,
    #[serde(default)]
    pub(crate) config_keys: Option<Vec<String>>,
    pub(crate) target: String,
    #[serde(default)]
    pub(crate) schedule: Option<String>,
    #[serde(default)]
    pub(crate) retention_keep_last: Option<u32>,
    #[serde(default)]
    pub(crate) retention_keep_days: Option<u32>,
    #[serde(default)]
    pub(crate) enabled: Option<bool>,
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct UpdateBackupJob {
    #[serde(default)]
    pub(crate) name: Option<String>,
    #[serde(default)]
    pub(crate) kind: Option<BackupKind>,
    #[serde(default)]
    pub(crate) database_name: Option<String>,
    #[serde(default)]
    pub(crate) paths: Option<Vec<String>>,
    #[serde(default)]
    pub(crate) config_keys: Option<Vec<String>>,
    #[serde(default)]
    pub(crate) target: Option<String>,
    #[serde(default)]
    pub(crate) schedule: Option<String>,
    #[serde(default)]
    pub(crate) retention_keep_last: Option<u32>,
    #[serde(default)]
    pub(crate) retention_keep_days: Option<u32>,
    #[serde(default)]
    pub(crate) enabled: Option<bool>,
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct RestoreRequest {
    #[serde(default)]
    pub(crate) run_id: Option<String>,
    /// Database restores execute only when this is true; otherwise a
    /// plan is returned and nothing is changed.
    #[serde(default)]
    pub(crate) confirm: bool,
    /// When true, only a plan is returned and nothing is changed.
    #[serde(default)]
    pub(crate) dry_run: bool,
}

/// Backup settings, read from the `backup` section of the application
/// configuration with safe defaults when the section is absent.
/// NOTE: the section is only honoured once `AppConfig` gains a
/// `backup` field; until then every value is the default below.
#[derive(Debug, Clone)]
pub(crate) struct BackupSettings {
    pub(crate) base_dir: String,
    /// Throwaway parent directory for restore drills. Deliberately
    /// separate from `base_dir`: drill output must never land where
    /// a restore could read it as live data.
    pub(crate) drill_dir: String,
    pub(crate) max_total_bytes: u64,
    pub(crate) max_file_bytes: u64,
    pub(crate) default_retention_keep_last: u32,
    pub(crate) default_retention_keep_days: u32,
    /// Hours between scheduled integrity verifications of the same
    /// run; runs verified more recently are skipped by the sweep.
    pub(crate) verify_interval_hours: u64,
    pub(crate) pg_dump_bin: String,
    pub(crate) psql_bin: String,
}

impl Default for BackupSettings {
    fn default() -> Self {
        Self {
            base_dir: "data/backups".to_string(),
            drill_dir: "data/backup-drills".to_string(),
            max_total_bytes: 256 * 1024 * 1024,
            max_file_bytes: 64 * 1024 * 1024,
            default_retention_keep_last: 7,
            default_retention_keep_days: 30,
            verify_interval_hours: 24,
            pg_dump_bin: "pg_dump".to_string(),
            psql_bin: "psql".to_string(),
        }
    }
}

/// Database connection details needed by `pg_dump` / `psql`. The
/// password lives only in memory here; it is passed to the child
/// process through the environment and is never logged or returned.
#[derive(Debug, Clone)]
pub(crate) struct DbSnapshot {
    pub(crate) driver: String,
    pub(crate) host: String,
    pub(crate) port: u16,
    pub(crate) username: String,
    pub(crate) password: String,
    pub(crate) database: String,
}

impl DbSnapshot {
    pub(crate) fn from_config(config: &AppConfig) -> Self {
        Self {
            driver: config.database.driver.clone(),
            host: config.database.host.clone(),
            port: config.database.port,
            username: config.database.username.clone(),
            password: config.database.password.clone(),
            database: config.database.database.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backup_kind_includes_parts() {
        assert_eq!(BackupKind::Database.as_str(), "database");
        assert_eq!(BackupKind::Files.as_str(), "files");
        assert_eq!(BackupKind::Config.as_str(), "config");
        assert_eq!(BackupKind::Full.as_str(), "full");
        assert!(BackupKind::Database.includes_database());
        assert!(!BackupKind::Database.includes_files());
        assert!(!BackupKind::Database.includes_config());
        assert!(BackupKind::Files.includes_files());
        assert!(BackupKind::Config.includes_config());
        assert!(BackupKind::Full.includes_database());
        assert!(BackupKind::Full.includes_files());
        assert!(BackupKind::Full.includes_config());
    }

    #[test]
    fn backup_status_names() {
        assert_eq!(BackupStatus::Pending.as_str(), "pending");
        assert_eq!(BackupStatus::Running.as_str(), "running");
        assert_eq!(BackupStatus::Completed.as_str(), "completed");
        assert_eq!(BackupStatus::Failed.as_str(), "failed");
    }
}
