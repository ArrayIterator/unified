// Backup Manager module.
//
// Backup jobs that capture the database, files, configuration, or all of
// them, store the resulting archive in a configured storage target, and
// support verification, retention pruning, and restore.
//
// Routes under `/api/v1/backup`:
//
//   GET    /                        module summary (counts, targets, limits)
//   GET    /targets                 storage target names from the registry
//   GET    /jobs                    list jobs (pagination, kind/status/target)
//   POST   /jobs                    create a job
//   GET    /jobs/{id}               get one job
//   PUT    /jobs/{id}               update a job
//   DELETE /jobs/{id}               delete a job and its stored archives
//   POST   /jobs/{id}/run           run a job now (202, runs in background)
//   POST   /jobs/{id}/verify        verify the latest completed run
//   POST   /jobs/{id}/restore       restore from the latest completed run
//   GET    /jobs/{id}/runs          run history for one job
//   GET    /runs                    list all runs (pagination, filters)
//   GET    /runs/{run_id}           get one run
//   POST   /runs/{run_id}/verify    recompute and compare the checksum
//   POST   /runs/{run_id}/restore   restore from one run
//   DELETE /runs/{run_id}           delete one run and its stored archive
//
// Archives are gzip-compressed tar files containing `manifest.json` plus
// the payload (`dump.sql` for database backups, the captured files for
// file backups, `config.json` for configuration backups). The archive
// key is `backups/<job-id>/<timestamp>-<run>-<kind>.tar.gz`, so every
// upload lands under a unique name.
//
// Storage comes from the shared `StorageRegistry` on the application
// context (`ctx.storage_registry`). A job whose target is not registered
// is rejected at creation time and fails honestly at run time.
//
// Scheduling: creating or updating a job with a cron schedule emits
// `scheduler.enqueue` with kind `backup.run`. The scheduler needs a
// `backup.run` dispatch arm (see the integrator notes in the module
// report); until then the enqueue is refused and the fact is recorded
// on the job, while `run-now` keeps working. The module also handles
// `backup.run_requested` (emitted by the scheduler once it dispatches
// `backup.run`) and re-verifies the latest run of a target when
// `storage.sync.completed` reports that target.
//
// Hooks emitted: `backup.started`, `backup.completed`, `backup.failed`,
// `backup.restored`, `backup.verified`, plus `backup.created` and
// `backup.deleted` for the audit log.

use crate::core::config::AppConfig;
use crate::core::context::AppContext;
use crate::core::error::{UnifiedError, UnifiedResult};
use crate::core::hooks::{HookBus, HookContext};
use crate::core::json;
use crate::core::modules::{Module, ModuleRegistry, RouteInfo};
use crate::core::pagination::{PageResult, PaginationParams};
use crate::core::storage::StorageRegistry;
use actix_web::{web, HttpResponse};
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use flate2::read::GzDecoder;
use flate2::write::GzEncoder;
use flate2::Compression;
use path_clean::PathClean;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, RwLock};
use tar::{Archive, Builder, Header};
use uuid::Uuid;
use walkdir::WalkDir;

// ---------------------------------------------------------------------------
// Events and limits.
// ---------------------------------------------------------------------------

pub const BACKUP_STARTED: &str = "backup.started";
pub const BACKUP_COMPLETED: &str = "backup.completed";
pub const BACKUP_FAILED: &str = "backup.failed";
pub const BACKUP_RESTORED: &str = "backup.restored";
pub const BACKUP_VERIFIED: &str = "backup.verified";
pub const BACKUP_CREATED: &str = "backup.created";
pub const BACKUP_DELETED: &str = "backup.deleted";
pub const BACKUP_RUN_REQUESTED: &str = "backup.run_requested";
pub const STORAGE_SYNC_COMPLETED: &str = "storage.sync.completed";
pub const SCHEDULER_ENQUEUE: &str = "scheduler.enqueue";
pub const BACKUP_RUN_KIND: &str = "backup.run";

const MANIFEST_NAME: &str = "manifest.json";
const CONFIG_NAME: &str = "config.json";
const DUMP_NAME: &str = "dump.sql";
const MAX_NAME_LEN: usize = 128;
const MAX_PATH_LEN: usize = 1024;
const MAX_STDERR_CHARS: usize = 500;

/// Configuration keys that may be restored from a config snapshot.
/// Secrets are never on this list.
const RESTORABLE_CONFIG_KEYS: &[&str] = &[
    "app.dashboard_path",
    "cache.dir",
    "cache.default_ttl_secs",
    "geo.auto_update",
    "geo.update_interval_hours",
];

/// Key fragments that mark a configuration value as secret. Matching
/// values are replaced with `********` in every snapshot and are never
/// logged or returned by the API.
const SECRET_KEY_FRAGMENTS: &[&str] = &[
    "password", "secret", "token", "salt", "private", "credential", "maxmind",
];

// ---------------------------------------------------------------------------
// Types.
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum BackupKind {
    Database,
    Files,
    Config,
    Full,
}

impl BackupKind {
    fn as_str(&self) -> &'static str {
        match self {
            BackupKind::Database => "database",
            BackupKind::Files => "files",
            BackupKind::Config => "config",
            BackupKind::Full => "full",
        }
    }

    fn includes_database(&self) -> bool {
        matches!(self, BackupKind::Database | BackupKind::Full)
    }

    fn includes_files(&self) -> bool {
        matches!(self, BackupKind::Files | BackupKind::Full)
    }

    fn includes_config(&self) -> bool {
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
    fn as_str(&self) -> &'static str {
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
    pub started_at: DateTime<Utc>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub finished_at: Option<DateTime<Utc>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize)]
struct BackupSummary {
    jobs_total: u64,
    jobs_by_status: BTreeMap<String, u64>,
    runs_total: u64,
    runs_by_status: BTreeMap<String, u64>,
    targets: Vec<String>,
    base_dir: String,
    max_total_bytes: u64,
    max_file_bytes: u64,
    default_retention_keep_last: u32,
    default_retention_keep_days: u32,
}

#[derive(Debug, Clone, Serialize)]
struct VerifyView {
    run_id: String,
    job_id: String,
    target: String,
    verified: bool,
    expected_checksum: Option<String>,
    actual_checksum: String,
    size_bytes: u64,
}

#[derive(Debug, Clone, Serialize)]
struct RestoreView {
    run_id: String,
    job_id: String,
    kind: String,
    dry_run: bool,
    planned: bool,
    steps: Vec<String>,
    restored_files: Vec<String>,
    applied_config_keys: Vec<String>,
    database_restored: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    database_plan: Option<Value>,
}

#[derive(Debug, Clone, Deserialize)]
struct CreateBackupJob {
    name: String,
    kind: BackupKind,
    #[serde(default)]
    database_name: Option<String>,
    #[serde(default)]
    paths: Option<Vec<String>>,
    #[serde(default)]
    config_keys: Option<Vec<String>>,
    target: String,
    #[serde(default)]
    schedule: Option<String>,
    #[serde(default)]
    retention_keep_last: Option<u32>,
    #[serde(default)]
    retention_keep_days: Option<u32>,
    #[serde(default)]
    enabled: Option<bool>,
}

#[derive(Debug, Clone, Deserialize)]
struct UpdateBackupJob {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    kind: Option<BackupKind>,
    #[serde(default)]
    database_name: Option<String>,
    #[serde(default)]
    paths: Option<Vec<String>>,
    #[serde(default)]
    config_keys: Option<Vec<String>>,
    #[serde(default)]
    target: Option<String>,
    #[serde(default)]
    schedule: Option<String>,
    #[serde(default)]
    retention_keep_last: Option<u32>,
    #[serde(default)]
    retention_keep_days: Option<u32>,
    #[serde(default)]
    enabled: Option<bool>,
}

#[derive(Debug, Clone, Deserialize)]
struct RestoreRequest {
    #[serde(default)]
    run_id: Option<String>,
    /// Database restores execute only when this is true; otherwise a
    /// plan is returned and nothing is changed.
    #[serde(default)]
    confirm: bool,
    /// When true, only a plan is returned and nothing is changed.
    #[serde(default)]
    dry_run: bool,
}

/// Backup settings, read from the `backup` section of the application
/// configuration with safe defaults when the section is absent.
#[derive(Debug, Clone)]
struct BackupSettings {
    base_dir: String,
    max_total_bytes: u64,
    max_file_bytes: u64,
    default_retention_keep_last: u32,
    default_retention_keep_days: u32,
    pg_dump_bin: String,
    psql_bin: String,
}

impl Default for BackupSettings {
    fn default() -> Self {
        Self {
            base_dir: "data/backups".to_string(),
            max_total_bytes: 256 * 1024 * 1024,
            max_file_bytes: 64 * 1024 * 1024,
            default_retention_keep_last: 7,
            default_retention_keep_days: 30,
            pg_dump_bin: "pg_dump".to_string(),
            psql_bin: "psql".to_string(),
        }
    }
}

/// Database connection details needed by `pg_dump` / `psql`. The
/// password lives only in memory here; it is passed to the child
/// process through the environment and is never logged or returned.
#[derive(Debug, Clone)]
struct DbSnapshot {
    driver: String,
    host: String,
    port: u16,
    username: String,
    password: String,
    database: String,
}

impl DbSnapshot {
    fn from_config(config: &AppConfig) -> Self {
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

// ---------------------------------------------------------------------------
// State.
// ---------------------------------------------------------------------------

pub struct BackupState {
    jobs: DashMap<String, BackupJob>,
    runs: DashMap<String, BackupRun>,
    /// Job ids currently executing, to reject overlapping runs.
    running: DashMap<String, ()>,
    hooks: RwLock<Option<Arc<HookBus>>>,
    storage: RwLock<Option<Arc<StorageRegistry>>>,
    settings: RwLock<BackupSettings>,
    db: RwLock<Option<DbSnapshot>>,
    /// Redacted full configuration snapshot taken at startup, so
    /// hook-driven runs can snapshot configuration without a request
    /// context. Refreshed only on restart; HTTP handlers always use
    /// the live configuration instead.
    config_snapshot: RwLock<Value>,
}

impl BackupState {
    fn new() -> Self {
        Self {
            jobs: DashMap::new(),
            runs: DashMap::new(),
            running: DashMap::new(),
            hooks: RwLock::new(None),
            storage: RwLock::new(None),
            settings: RwLock::new(BackupSettings::default()),
            db: RwLock::new(None),
            config_snapshot: RwLock::new(Value::Null),
        }
    }

    fn hooks(&self) -> Option<Arc<HookBus>> {
        self.hooks.read().expect("backup hooks lock poisoned").clone()
    }

    fn settings_snapshot(&self) -> BackupSettings {
        self.settings
            .read()
            .expect("backup settings lock poisoned")
            .clone()
    }

    fn db_snapshot(&self) -> Option<DbSnapshot> {
        self.db.read().expect("backup db lock poisoned").clone()
    }

    fn storage(&self) -> Option<Arc<StorageRegistry>> {
        self.storage
            .read()
            .expect("backup storage lock poisoned")
            .clone()
    }

    fn config_snapshot(&self) -> Value {
        self.config_snapshot
            .read()
            .expect("backup config lock poisoned")
            .clone()
    }
}

/// Everything one backup execution needs, gathered either from the live
/// application context (HTTP handlers) or from the module state snapshot
/// (hook-driven runs, which have no request context).
#[derive(Clone)]
struct BackupExecutor {
    state: Arc<BackupState>,
    storage: Arc<StorageRegistry>,
    settings: BackupSettings,
    db: DbSnapshot,
    hooks: Arc<HookBus>,
    /// Redacted full configuration document, used to build config
    /// snapshots for archives.
    config_snapshot: Value,
}

impl BackupExecutor {
    fn from_context(ctx: &AppContext, state: Arc<BackupState>) -> UnifiedResult<Self> {
        let config = ctx.config.read().expect("config lock poisoned");
        Ok(Self {
            state,
            storage: ctx.storage_registry.clone(),
            settings: backup_settings(&config),
            db: DbSnapshot::from_config(&config),
            hooks: ctx.hooks.clone(),
            config_snapshot: redact_value(serde_json::to_value(&*config).unwrap_or(Value::Null)),
        })
    }

    fn from_state(state: Arc<BackupState>) -> UnifiedResult<Self> {
        let storage = state
            .storage()
            .ok_or_else(|| UnifiedError::internal("Backup storage registry is not initialized"))?;
        let db = state
            .db_snapshot()
            .ok_or_else(|| UnifiedError::internal("Backup database settings are not initialized"))?;
        let hooks = state
            .hooks()
            .ok_or_else(|| UnifiedError::internal("Backup hook bus is not initialized"))?;
        Ok(Self {
            state: state.clone(),
            storage,
            settings: state.settings_snapshot(),
            db,
            hooks,
            config_snapshot: state.config_snapshot(),
        })
    }

    fn provider_for(&self, target: &str) -> UnifiedResult<Arc<dyn crate::core::storage::StorageProvider>> {
        self.storage.get(target).ok_or_else(|| {
            UnifiedError::not_found(format!("Storage target \"{target}\" is not registered"))
        })
    }
}

// ---------------------------------------------------------------------------
// Helpers: ids, hashing, cron validation.
// ---------------------------------------------------------------------------

fn new_id() -> String {
    Uuid::new_v4().to_string()
}

fn now() -> DateTime<Utc> {
    Utc::now()
}

fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn paginate<T: Serialize + Clone>(items: Vec<T>, params: &PaginationParams) -> PageResult<T> {
    let pagination = params.normalized();
    let total = items.len() as u64;
    let start = ((pagination.page - 1) * pagination.per_page) as usize;
    let page_items: Vec<T> = items
        .into_iter()
        .skip(start)
        .take(pagination.per_page as usize)
        .collect();
    PageResult::new(page_items, total, &pagination)
}

fn matches_search(haystack: &str, params: &PaginationParams) -> bool {
    match &params.search {
        Some(needle) if !needle.trim().is_empty() => haystack
            .to_lowercase()
            .contains(&needle.trim().to_lowercase()),
        _ => true,
    }
}

/// Validate one field of a five-field cron expression against its
/// numeric bounds. Accepts `*`, `*/N`, `N`, `N-M`, `N-M/S`, and
/// comma-separated lists of those.
fn valid_cron_field(field: &str, min: u32, max: u32) -> bool {
    if field.is_empty() {
        return false;
    }
    field.split(',').all(|item| {
        if item.is_empty() {
            return false;
        }
        let (base, step) = match item.split_once('/') {
            Some((base, step)) => (base, Some(step)),
            None => (item, None),
        };
        if let Some(step) = step {
            let Ok(step_value) = step.parse::<u32>() else {
                return false;
            };
            if step_value == 0 {
                return false;
            }
        }
        if base == "*" {
            return true;
        }
        if let Some((low, high)) = base.split_once('-') {
            let (Ok(low_value), Ok(high_value)) =
                (low.parse::<u32>(), high.parse::<u32>())
            else {
                return false;
            };
            return low_value >= min && high_value <= max && low_value <= high_value;
        }
        match base.parse::<u32>() {
            Ok(value) => value >= min && value <= max,
            Err(_) => false,
        }
    })
}

/// A schedule is empty (manual-only) or a five-field cron expression:
/// minute hour day-of-month month day-of-week.
fn valid_schedule(schedule: &str) -> bool {
    let schedule = schedule.trim();
    if schedule.is_empty() {
        return true;
    }
    let fields: Vec<&str> = schedule.split_whitespace().collect();
    if fields.len() != 5 {
        return false;
    }
    let bounds = [(0, 59), (0, 23), (1, 31), (1, 12), (0, 7)];
    fields
        .iter()
        .zip(bounds)
        .all(|(field, (min, max))| valid_cron_field(field, min, max))
}

fn valid_storage_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= MAX_NAME_LEN
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.')
}

fn valid_database_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 63
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

fn valid_config_key(key: &str) -> bool {
    !key.is_empty()
        && key.len() <= MAX_PATH_LEN
        && key.split('.').all(|segment| {
            !segment.is_empty()
                && segment
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
        })
}

// ---------------------------------------------------------------------------
// Helpers: settings, redaction, paths.
// ---------------------------------------------------------------------------

/// Read the `backup` section of the configuration, falling back to
/// defaults for every missing key so the module keeps working before
/// the section is added to the configuration file.
fn backup_settings(config: &AppConfig) -> BackupSettings {
    let defaults = BackupSettings::default();
    let Ok(value) = serde_json::to_value(config) else {
        return defaults;
    };
    let Some(section) = value.get("backup") else {
        return defaults;
    };
    let get_string = |key: &str, fallback: &str| {
        section
            .get(key)
            .and_then(|value| value.as_str())
            .map(str::to_string)
            .unwrap_or_else(|| fallback.to_string())
    };
    let get_u64 = |key: &str, fallback: u64| {
        section
            .get(key)
            .and_then(|value| value.as_u64())
            .unwrap_or(fallback)
    };
    let get_u32 = |key: &str, fallback: u32| {
        section
            .get(key)
            .and_then(|value| value.as_u64())
            .map(|value| value as u32)
            .unwrap_or(fallback)
    };
    BackupSettings {
        base_dir: get_string("base_dir", &defaults.base_dir),
        max_total_bytes: get_u64("max_total_bytes", defaults.max_total_bytes),
        max_file_bytes: get_u64("max_file_bytes", defaults.max_file_bytes),
        default_retention_keep_last: get_u32(
            "default_retention_keep_last",
            defaults.default_retention_keep_last,
        ),
        default_retention_keep_days: get_u32(
            "default_retention_keep_days",
            defaults.default_retention_keep_days,
        ),
        pg_dump_bin: get_string("pg_dump_bin", &defaults.pg_dump_bin),
        psql_bin: get_string("psql_bin", &defaults.psql_bin),
    }
}

/// Replace every secret value in a configuration document with
/// `********`. A key is secret when its name contains one of the
/// secret fragments or is exactly `key`.
fn redact_value(value: Value) -> Value {
    match value {
        Value::Object(map) => {
            let mut redacted = serde_json::Map::new();
            for (key, child) in map {
                let lowered = key.to_lowercase();
                let is_secret = lowered == "key"
                    || SECRET_KEY_FRAGMENTS
                        .iter()
                        .any(|fragment| lowered.contains(fragment));
                if is_secret {
                    redacted.insert(key, Value::String("********".to_string()));
                } else {
                    redacted.insert(key, redact_value(child));
                }
            }
            Value::Object(redacted)
        }
        Value::Array(items) => Value::Array(items.into_iter().map(redact_value).collect()),
        other => other,
    }
}

/// Build the redacted configuration snapshot for an archive from an
/// already-redacted full configuration document. When `keys` is empty
/// the whole configuration is snapshotted; otherwise only the
/// requested top-level sections or dotted paths are kept.
fn snapshot_config(full: &Value, keys: &[String]) -> Value {
    if keys.is_empty() {
        return full.clone();
    }
    let mut out = serde_json::Map::new();
    for key in keys {
        let segments: Vec<&str> = key.split('.').collect();
        if let Some(found) = get_dotted_value(&full, &segments) {
            set_dotted_value(&mut out, &segments, found.clone());
        }
    }
    Value::Object(out)
}

fn get_dotted_value<'a>(value: &'a Value, segments: &[&str]) -> Option<&'a Value> {
    let mut current = value;
    for segment in segments {
        current = current.get(*segment)?;
    }
    Some(current)
}

fn set_dotted_value(map: &mut serde_json::Map<String, Value>, segments: &[&str], value: Value) {
    if segments.is_empty() {
        return;
    }
    if segments.len() == 1 {
        map.insert(segments[0].to_string(), value);
        return;
    }
    let entry = map
        .entry(segments[0].to_string())
        .or_insert_with(|| Value::Object(serde_json::Map::new()));
    if let Value::Object(child) = entry {
        set_dotted_value(child, &segments[1..], value);
    }
}

/// Resolve a possibly relative path against the working directory and
/// clean it lexically (no filesystem access).
fn absolute_clean(path: &str) -> PathBuf {
    let raw = Path::new(path);
    let joined = if raw.is_absolute() {
        raw.to_path_buf()
    } else {
        std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("."))
            .join(raw)
    };
    joined.clean()
}

/// Lexical containment check used at job creation time: the candidate
/// path must be the base directory itself or live inside it.
fn path_inside_base(candidate: &str, base_dir: &str) -> bool {
    let base = absolute_clean(base_dir);
    let candidate = absolute_clean(candidate);
    candidate == base || candidate.starts_with(&base)
}

// ---------------------------------------------------------------------------
// Helpers: external binaries.
// ---------------------------------------------------------------------------

/// Find an executable by name on `PATH`, or use it directly when it is
/// already a path. Returns `None` when no usable file is found.
fn find_binary(name: &str) -> Option<PathBuf> {
    let candidate = Path::new(name);
    if name.contains('/') {
        if candidate.is_file() {
            return Some(candidate.to_path_buf());
        }
        return None;
    }
    let path_var = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path_var) {
        let candidate = dir.join(name);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

fn truncate_stderr(stderr: &[u8]) -> String {
    let text = String::from_utf8_lossy(stderr);
    let text = text.trim();
    if text.chars().count() > MAX_STDERR_CHARS {
        text.chars().take(MAX_STDERR_CHARS).collect()
    } else {
        text.to_string()
    }
}

// ---------------------------------------------------------------------------
// Archive building (blocking work).
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
struct ManifestFile {
    path: String,
    size_bytes: u64,
    sha256: String,
}

#[derive(Debug, Clone, Serialize)]
struct Manifest {
    job_id: String,
    kind: String,
    created_at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    database: Option<Value>,
    files: Vec<ManifestFile>,
    #[serde(skip_serializing_if = "Option::is_none")]
    config: Option<Value>,
    file_count: u64,
    total_bytes: u64,
}

struct ArchiveInput {
    job_id: String,
    kind: BackupKind,
    database_name: String,
    db: DbSnapshot,
    paths: Vec<String>,
    config_snapshot: Value,
    settings: BackupSettings,
}

struct ArchiveOutput {
    bytes: Vec<u8>,
    file_count: u64,
    total_bytes: u64,
}

/// Run `pg_dump` for one database and return the plain-format dump.
/// The password is passed through the environment only, and no part of
/// the command line or the error output ever contains it.
fn run_pg_dump(db: &DbSnapshot, pg_dump_bin: &str) -> UnifiedResult<Vec<u8>> {
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
    let output = command.output()?;
    if !output.status.success() {
        let detail = truncate_stderr(&output.stderr);
        return Err(UnifiedError::internal(format!(
            "pg_dump failed for database \"{}\"{}{}",
            db.database,
            if detail.is_empty() { "" } else { ": " },
            detail
        )));
    }
    Ok(output.stdout)
}

/// Walk the configured paths and read every regular file under the
/// allowlisted base directory. Symlinks are never followed, and any
/// path that resolves outside the base directory is rejected.
fn collect_files(
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
fn build_archive(input: ArchiveInput) -> UnifiedResult<ArchiveOutput> {
    let mut manifest = Manifest {
        job_id: input.job_id.clone(),
        kind: input.kind.as_str().to_string(),
        created_at: now().to_rfc3339(),
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
        let dump = run_pg_dump(&dump_db, &input.settings.pg_dump_bin)?;
        if dump.len() as u64 > input.settings.max_total_bytes {
            return Err(UnifiedError::unprocessable(format!(
                "Database dump exceeds the total size limit of {} bytes",
                input.settings.max_total_bytes
            )));
        }
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
    let encoder = builder.finish()?;
    let bytes = encoder.finish()?;
    Ok(ArchiveOutput {
        bytes,
        file_count: manifest.file_count,
        total_bytes: manifest.total_bytes,
    })
}

/// Read every entry of a tar.gz archive into memory.
fn extract_archive(bytes: &[u8]) -> UnifiedResult<Vec<(String, Vec<u8>)>> {
    let decoder = GzDecoder::new(bytes);
    let mut archive = Archive::new(decoder);
    let mut out: Vec<(String, Vec<u8>)> = Vec::new();
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
        let mut data = Vec::new();
        entry.read_to_end(&mut data)?;
        out.push((path, data));
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// Execution.
// ---------------------------------------------------------------------------

fn audit_payload(job: &BackupJob, run: Option<&BackupRun>, extra: Value) -> Value {
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

/// Create the run record and mark the job running. The returned run is
/// then executed in the background by `execute_run`.
fn start_run(executor: &BackupExecutor, job_id: &str) -> UnifiedResult<BackupRun> {
    let job = executor
        .state
        .jobs
        .get(job_id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found("Backup job not found"))?;
    if executor.state.running.contains_key(job_id) {
        return Err(UnifiedError::conflict(format!(
            "Backup job \"{}\" is already running",
            job.name
        )));
    }
    if !job.enabled {
        return Err(UnifiedError::unprocessable(format!(
            "Backup job \"{}\" is disabled",
            job.name
        )));
    }
    let started_at = now();
    let run = BackupRun {
        id: new_id(),
        job_id: job.id.clone(),
        job_name: job.name.clone(),
        kind: job.kind,
        target: job.target.clone(),
        storage_key: None,
        status: BackupStatus::Running,
        checksum: None,
        size_bytes: None,
        file_count: None,
        started_at,
        finished_at: None,
        error: None,
        created_at: started_at,
    };
    executor.state.running.insert(job_id.to_string(), ());
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

fn fail_run(executor: &BackupExecutor, run_id: &str, job_id: &str, message: String) {
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

/// Delete completed runs beyond the retention policy: keep the newest
/// `retention_keep_last` runs and drop runs older than
/// `retention_keep_days`. Stored archives are deleted best-effort.
async fn prune_retention(executor: &BackupExecutor, job: &BackupJob) {
    let mut runs: Vec<BackupRun> = executor
        .state
        .runs
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|run| run.job_id == job.id && run.status == BackupStatus::Completed)
        .collect();
    runs.sort_by(|a, b| {
        b.finished_at
            .unwrap_or(b.started_at)
            .cmp(&a.finished_at.unwrap_or(a.started_at))
    });
    let cutoff = if job.retention_keep_days > 0 {
        Some(now() - chrono::Duration::days(job.retention_keep_days as i64))
    } else {
        None
    };
    let mut stale: Vec<BackupRun> = Vec::new();
    for (index, run) in runs.iter().enumerate() {
        let too_many = index as u32 >= job.retention_keep_last;
        let too_old = cutoff.is_some_and(|cutoff| {
            run.finished_at.unwrap_or(run.started_at) < cutoff
        });
        if too_many || too_old {
            stale.push(run.clone());
        }
    }
    if stale.is_empty() {
        return;
    }
    let provider = executor.provider_for(&job.target);
    for run in stale {
        if let (Some(key), Ok(provider)) = (&run.storage_key, &provider) {
            let _ = provider.delete(key).await;
        }
        executor.state.runs.remove(&run.id);
    }
}

async fn execute_run_inner(
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
        kind: job.kind,
        target: job.target.clone(),
        storage_key: Some(storage_key.clone()),
        status: BackupStatus::Completed,
        checksum: Some(checksum.clone()),
        size_bytes: Some(size_bytes),
        file_count: Some(output.file_count),
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
async fn execute_run(executor: BackupExecutor, job_id: String, run_id: String) {
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
fn spawn_backup_worker<F>(future: F)
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

/// Recompute the checksum of a stored archive and compare it with the
/// recorded one. The archive bytes are downloaded in full; the size is
/// bounded by the backup size cap enforced at creation time.
async fn verify_run_archive(
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
    let actual = sha256_hex(&bytes);
    Ok(VerifyView {
        run_id: run.id.clone(),
        job_id: run.job_id.clone(),
        target: run.target.clone(),
        verified: run.checksum.as_deref() == Some(actual.as_str()),
        expected_checksum: run.checksum.clone(),
        actual_checksum: actual,
        size_bytes: bytes.len() as u64,
    })
}

// ---------------------------------------------------------------------------
// Restore (blocking parts).
// ---------------------------------------------------------------------------

/// Write archived files back under the allowlisted base directory.
/// Archive entries with absolute paths or parent-directory components
/// are rejected, and every destination is re-checked after its parent
/// directories are created.
fn restore_files_blocking(
    entries: &[(String, Vec<u8>)],
    base_dir: &str,
) -> UnifiedResult<Vec<String>> {
    let base = std::fs::canonicalize(base_dir).map_err(|error| {
        UnifiedError::bad_request(format!(
            "Backup base directory \"{base_dir}\" is not accessible: {error}"
        ))
    })?;
    let mut restored: Vec<String> = Vec::new();
    for (path, bytes) in entries {
        if path == MANIFEST_NAME || path == CONFIG_NAME || path == DUMP_NAME {
            continue;
        }
        let relative = Path::new(path);
        if relative.is_absolute()
            || relative.components().any(|component| {
                matches!(component, Component::ParentDir | Component::RootDir)
            })
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
        std::fs::write(&destination, bytes)?;
        restored.push(path.clone());
    }
    restored.sort();
    Ok(restored)
}

/// Apply the allowlisted, non-secret configuration keys from a
/// snapshot to the live configuration. Secret values are never
/// applied, and the whole reload is validated before it takes effect.
fn restore_config_blocking(
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
fn restore_database_blocking(
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
        stdin.write_all(dump)?;
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

// ---------------------------------------------------------------------------
// Scheduling bridge.
// ---------------------------------------------------------------------------

/// Tell the scheduler about a job with a cron schedule. Returns whether
/// the scheduler accepted the job; a refusal is recorded on the job
/// instead of failing the request, because run-now keeps working.
fn notify_scheduler(hooks: &HookBus, job: &BackupJob) -> SchedulingOutcome {
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

// ---------------------------------------------------------------------------
// Validation of job input.
// ---------------------------------------------------------------------------

fn validate_job_fields(
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

fn validate_common(
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

// ---------------------------------------------------------------------------
// Handlers.
// ---------------------------------------------------------------------------

async fn get_summary(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<BackupState>>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<BackupSummary> = (|| {
        let config = ctx.config.read().expect("config lock poisoned");
        let settings = backup_settings(&config);
        let mut jobs_by_status: BTreeMap<String, u64> = BTreeMap::new();
        for entry in state.jobs.iter() {
            *jobs_by_status
                .entry(entry.value().status.as_str().to_string())
                .or_default() += 1;
        }
        let mut runs_by_status: BTreeMap<String, u64> = BTreeMap::new();
        for entry in state.runs.iter() {
            *runs_by_status
                .entry(entry.value().status.as_str().to_string())
                .or_default() += 1;
        }
        Ok(BackupSummary {
            jobs_total: state.jobs.len() as u64,
            jobs_by_status,
            runs_total: state.runs.len() as u64,
            runs_by_status,
            targets: ctx.storage_registry.list(),
            base_dir: settings.base_dir,
            max_total_bytes: settings.max_total_bytes,
            max_file_bytes: settings.max_file_bytes,
            default_retention_keep_last: settings.default_retention_keep_last,
            default_retention_keep_days: settings.default_retention_keep_days,
        })
    })();
    json::respond(result, debug)
}

async fn list_targets(ctx: web::Data<Arc<AppContext>>) -> HttpResponse {
    let targets = ctx.storage_registry.list();
    json::success_response(json!({ "targets": targets }), 200)
}

async fn list_jobs(
    state: web::Data<Arc<BackupState>>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let params = query.into_inner();
    let mut items: Vec<BackupJob> = state
        .jobs
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|job| {
            matches_search(&format!("{} {}", job.name, job.target), &params)
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

async fn create_job(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<BackupState>>,
    body: web::Json<CreateBackupJob>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<BackupJob> = (|| {
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

async fn get_job(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<BackupState>>,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<BackupJob> = state
        .jobs
        .get(path.as_str())
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found("Backup job not found"));
    json::respond(result, debug)
}

async fn update_job(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<BackupState>>,
    path: web::Path<String>,
    body: web::Json<UpdateBackupJob>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<BackupJob> = (|| {
        let input = body.into_inner();
        let config = ctx.config.read().expect("config lock poisoned");
        let settings = backup_settings(&config);
        // Work on a clone so a failed validation never leaves a
        // half-updated job in the store.
        let mut job = state
            .jobs
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Backup job not found"))?;
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

async fn delete_job(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<BackupState>>,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| async {
        let job_id = path.into_inner();
        let job = state
            .jobs
            .get(&job_id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Backup job not found"))?;
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

async fn run_job_now(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<BackupState>>,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<BackupRun> = (|| {
        let executor = BackupExecutor::from_context(&ctx, state.clone().into_inner())?;
        let job_id = path.into_inner();
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

async fn list_job_runs(
    state: web::Data<Arc<BackupState>>,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let params = query.into_inner();
    let job_id = path.into_inner();
    if !state.jobs.contains_key(&job_id) {
        return json::error_response(&UnifiedError::not_found("Backup job not found"), false);
    }
    let mut items: Vec<BackupRun> = state
        .runs
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|run| {
            run.job_id == job_id
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

async fn list_runs(
    state: web::Data<Arc<BackupState>>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let params = query.into_inner();
    let mut items: Vec<BackupRun> = state
        .runs
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|run| {
            matches_search(&format!("{} {}", run.job_id, run.target), &params)
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

async fn get_run(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<BackupState>>,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<BackupRun> = state
        .runs
        .get(path.as_str())
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found("Backup run not found"));
    json::respond(result, debug)
}

async fn verify_run(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<BackupState>>,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<VerifyView> = (|| async {
        let executor = BackupExecutor::from_context(&ctx, state.clone().into_inner())?;
        let run = state
            .runs
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Backup run not found"))?;
        let view = verify_run_archive(&executor, &run).await?;
        if let Some(job) = state.jobs.get(&run.job_id) {
            ctx.hooks.emit(
                BACKUP_VERIFIED,
                audit_payload(&job, Some(&run), json!({ "verified": view.verified })),
            );
        }
        Ok(view)
    })()
    .await;
    json::respond(result, debug)
}

async fn verify_job(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<BackupState>>,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<VerifyView> = (|| async {
        let executor = BackupExecutor::from_context(&ctx, state.clone().into_inner())?;
        let job_id = path.into_inner();
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
        Ok(view)
    })()
    .await;
    json::respond(result, debug)
}

/// Resolve the run a restore request refers to: the named run, or the
/// latest completed run of the job.
fn resolve_restore_run(state: &BackupState, job_id: &str, run_id: Option<&str>) -> UnifiedResult<BackupRun> {
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

/// Execute (or plan) a restore for one completed run. Database restores
/// execute only with explicit confirmation; everything else runs unless
/// the request is a dry run.
async fn perform_restore(
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
    let entries = tokio::task::spawn_blocking(move || extract_archive(&bytes))
        .await
        .map_err(|error| UnifiedError::internal(format!("Restore worker failed: {error}")))?;

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

    if wants_files {
        let base_dir = executor.settings.base_dir.clone();
        let entries_for_files = entries.clone();
        let files = tokio::task::spawn_blocking(move || {
            restore_files_blocking(&entries_for_files, &base_dir)
        })
        .await
        .map_err(|error| UnifiedError::internal(format!("Restore worker failed: {error}")))?;
        view.restored_files = files;
    }
    if wants_config {
        let entries_for_config = entries.clone();
        // `ctx` is borrowed from the request; the blocking restore needs
        // an owned handle, so apply the configuration on this task.
        let applied = restore_config_blocking(&entries_for_config, ctx)?;
        view.applied_config_keys = applied;
    }
    if wants_database {
        let db = executor.db.clone();
        let psql_bin = executor.settings.psql_bin.clone();
        let entries_for_db = entries.clone();
        let restored = tokio::task::spawn_blocking(move || {
            restore_database_blocking(&entries_for_db, &db, &psql_bin)
        })
        .await
        .map_err(|error| UnifiedError::internal(format!("Restore worker failed: {error}")))?;
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

async fn restore_job(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<BackupState>>,
    path: web::Path<String>,
    body: web::Json<RestoreRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<RestoreView> = (|| async {
        let job_id = path.into_inner();
        if !state.jobs.contains_key(&job_id) {
            return Err(UnifiedError::not_found("Backup job not found"));
        }
        let request = body.into_inner();
        let run = resolve_restore_run(&state, &job_id, request.run_id.as_deref())?;
        perform_restore(&ctx, &state, &run, &request).await
    })()
    .await;
    json::respond(result, debug)
}

async fn restore_run(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<BackupState>>,
    path: web::Path<String>,
    body: web::Json<RestoreRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<RestoreView> = (|| async {
        let request = body.into_inner();
        let run = state
            .runs
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Backup run not found"))?;
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

async fn delete_run(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<BackupState>>,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| async {
        let run_id = path.into_inner();
        let run = state
            .runs
            .get(&run_id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Backup run not found"))?;
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

// ---------------------------------------------------------------------------
// Module definition.
// ---------------------------------------------------------------------------

pub struct BackupModule {
    state: Arc<BackupState>,
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
        "Backup jobs for database, files, configuration, or full snapshots, stored in a configured storage target with verification, retention, and restore."
    }

    fn dependencies(&self) -> Vec<&'static str> {
        vec!["scheduler"]
    }

    fn init(&self, ctx: &AppContext) -> UnifiedResult<()> {
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
                        spawn_backup_worker(async move {
                            execute_run(executor, job_id, run_id).await;
                        });
                        ctx.add_result(json!({ "started": true, "job_id": job_id, "run_id": run_id }));
                    }
                    Err(error) => {
                        ctx.add_result(json!({ "error": error.message }));
                    }
                }
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
                .route("/jobs/{id}/runs", web::get().to(list_job_runs))
                .route("/runs", web::get().to(list_runs))
                .route("/runs/{run_id}", web::get().to(get_run))
                .route("/runs/{run_id}", web::delete().to(delete_run))
                .route("/runs/{run_id}/verify", web::post().to(verify_run))
                .route("/runs/{run_id}/restore", web::post().to(restore_run)),
        );
    }

    fn route_info(&self) -> Vec<RouteInfo> {
        vec![
            RouteInfo { method: "GET", path: "/api/v1/backup", owner: "backup", description: "Backup module summary: counts, targets, limits." },
            RouteInfo { method: "GET", path: "/api/v1/backup/targets", owner: "backup", description: "List registered storage target names." },
            RouteInfo { method: "GET", path: "/api/v1/backup/jobs", owner: "backup", description: "List backup jobs with pagination and filters." },
            RouteInfo { method: "POST", path: "/api/v1/backup/jobs", owner: "backup", description: "Create a backup job." },
            RouteInfo { method: "GET", path: "/api/v1/backup/jobs/{id}", owner: "backup", description: "Get one backup job." },
            RouteInfo { method: "PUT", path: "/api/v1/backup/jobs/{id}", owner: "backup", description: "Update a backup job." },
            RouteInfo { method: "DELETE", path: "/api/v1/backup/jobs/{id}", owner: "backup", description: "Delete a backup job and its stored archives." },
            RouteInfo { method: "POST", path: "/api/v1/backup/jobs/{id}/run", owner: "backup", description: "Run a backup job now, in the background." },
            RouteInfo { method: "POST", path: "/api/v1/backup/jobs/{id}/verify", owner: "backup", description: "Verify the latest completed run of a job." },
            RouteInfo { method: "POST", path: "/api/v1/backup/jobs/{id}/restore", owner: "backup", description: "Restore from the latest completed run of a job." },
            RouteInfo { method: "GET", path: "/api/v1/backup/jobs/{id}/runs", owner: "backup", description: "List the run history of one backup job." },
            RouteInfo { method: "GET", path: "/api/v1/backup/runs", owner: "backup", description: "List all backup runs with pagination and filters." },
            RouteInfo { method: "GET", path: "/api/v1/backup/runs/{run_id}", owner: "backup", description: "Get one backup run." },
            RouteInfo { method: "DELETE", path: "/api/v1/backup/runs/{run_id}", owner: "backup", description: "Delete one backup run and its stored archive." },
            RouteInfo { method: "POST", path: "/api/v1/backup/runs/{run_id}/verify", owner: "backup", description: "Recompute and compare the checksum of a stored backup." },
            RouteInfo { method: "POST", path: "/api/v1/backup/runs/{run_id}/restore", owner: "backup", description: "Restore from one backup run." },
        ]
    }
}

pub fn register(registry: &ModuleRegistry) {
    registry.register(Arc::new(BackupModule::new()));
}
