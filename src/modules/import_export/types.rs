//! import export - types.

use serde::{Deserialize, Serialize};
use std::sync::{OnceLock, RwLock};

/// Maximum accepted import payload in bytes (10 MiB).
pub(crate) const DEFAULT_MAX_FILE_BYTES: usize = 10 * 1024 * 1024;

/// Maximum rows per single-shot import file.
pub(crate) const DEFAULT_MAX_ROWS_PER_FILE: usize = 10_000;

/// Maximum bytes per chunked-import chunk (2 MiB).
pub(crate) const DEFAULT_MAX_CHUNK_BYTES: usize = 2 * 1024 * 1024;

/// Maximum rows across all chunks of one import session.
pub(crate) const DEFAULT_MAX_ROWS_PER_SESSION: usize = 50_000;

/// Maximum rows per export job (also the streaming cap).
pub(crate) const DEFAULT_MAX_EXPORT_ROWS: usize = 10_000;

/// Maximum export content kept in memory per job (50 MiB).
pub(crate) const DEFAULT_MAX_EXPORT_BYTES: usize = 50 * 1024 * 1024;

/// Default formats. Admin-editable through the settings endpoint.
pub(crate) const DEFAULT_ALLOWED_FORMATS: &[&str] = &["csv", "json", "xml"];

/// Effective limits and defaults for this module. These are the
/// configurable defaults: an admin may change them through
/// `PATCH /api/v1/import-export/settings`; nothing else in this file
/// hard-codes a limit.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImportExportSettings {
    pub max_file_bytes: usize,
    pub max_rows_per_file: usize,
    pub max_chunk_bytes: usize,
    pub max_rows_per_session: usize,
    pub max_export_rows: usize,
    pub max_export_bytes: usize,
    /// Redact personal data on exports unless the caller explicitly
    /// opts out (which still requires `import_export:export_pii`).
    pub default_redact_pii: bool,
    pub allowed_formats: Vec<String>,
}

impl Default for ImportExportSettings {
    fn default() -> Self {
        Self {
            max_file_bytes: DEFAULT_MAX_FILE_BYTES,
            max_rows_per_file: DEFAULT_MAX_ROWS_PER_FILE,
            max_chunk_bytes: DEFAULT_MAX_CHUNK_BYTES,
            max_rows_per_session: DEFAULT_MAX_ROWS_PER_SESSION,
            max_export_rows: DEFAULT_MAX_EXPORT_ROWS,
            max_export_bytes: DEFAULT_MAX_EXPORT_BYTES,
            default_redact_pii: false,
            allowed_formats: DEFAULT_ALLOWED_FORMATS
                .iter()
                .map(|format| format.to_string())
                .collect(),
        }
    }
}

pub(crate) static SETTINGS: OnceLock<RwLock<ImportExportSettings>> = OnceLock::new();

pub(crate) fn settings_store() -> &'static RwLock<ImportExportSettings> {
    SETTINGS.get_or_init(|| RwLock::new(ImportExportSettings::default()))
}

pub(crate) fn settings() -> ImportExportSettings {
    settings_store()
        .read()
        .map(|guard| guard.clone())
        .unwrap_or_default()
}
