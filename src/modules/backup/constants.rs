// Backup module - event names, permissions, and limits.
//
// Split from `src/modules/backup.rs`; the module root re-exports the public API of this module.

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
pub const BACKUP_VERIFY_REQUESTED: &str = "backup.verify_requested";
pub const BACKUP_PRUNE_REQUESTED: &str = "backup.prune_requested";
pub const BACKUP_VERIFICATION_FAILED: &str = "backup.verification_failed";
pub const BACKUP_DRILL_COMPLETED: &str = "backup.drill_completed";
pub const BACKUP_DRILL_FAILED: &str = "backup.drill_failed";
pub const NOTIFICATION_REQUESTED: &str = "notification.requested";
pub const STORAGE_SYNC_COMPLETED: &str = "storage.sync.completed";
pub const SCHEDULER_ENQUEUE: &str = "scheduler.enqueue";
pub const BACKUP_RUN_KIND: &str = "backup.run";

/// Permissions checked on every route through the `role.check` hook.
/// Reads need `backup:view`; every write needs `backup:manage`.
pub const PERM_BACKUP_VIEW: &str = "backup:view";
pub const PERM_BACKUP_MANAGE: &str = "backup:manage";

pub(crate) const MANIFEST_NAME: &str = "manifest.json";
pub(crate) const CONFIG_NAME: &str = "config.json";
pub(crate) const DUMP_NAME: &str = "dump.sql";
pub(crate) const MAX_NAME_LEN: usize = 128;
pub(crate) const MAX_PATH_LEN: usize = 1024;
pub(crate) const MAX_STDERR_CHARS: usize = 500;

/// Configuration keys that may be restored from a config snapshot.
/// Secrets are never on this list.
pub(crate) const RESTORABLE_CONFIG_KEYS: &[&str] = &[
    "app.dashboard_path",
    "cache.dir",
    "cache.default_ttl_secs",
    "geo.auto_update",
    "geo.update_interval_hours",
];

/// Key fragments that mark a configuration value as secret. Matching
/// values are replaced with `********` in every snapshot and are never
/// logged or returned by the API.
///
/// Matching is boundary-aware for the short fragments `key`, `auth`
/// and `apikey` (see `config::is_secret_key`): a bare `key` fragment
/// must appear as its own token, so innocent keys such as `monkey`
/// or `keyboard` are not redacted. Longer fragments match as
/// substrings. The `*_url`, `dsn` and `connection_string` fragments
/// cover credential-bearing connection strings.
pub(crate) const SECRET_KEY_FRAGMENTS: &[&str] = &[
    "password", "secret", "token", "salt", "private", "credential", "maxmind", "key",
    "api_key", "access_key", "apikey", "auth", "database_url", "db_url",
    "connection_string", "connectionstring", "dsn",
];
