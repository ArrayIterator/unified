//! import export - jobs.

use chrono::{DateTime, Utc};
use serde::Serialize;
use serde_json::Value;
use std::collections::HashMap;
use uuid::Uuid;

/// One row-level validation problem.
#[derive(Debug, Clone, Serialize)]
pub struct RowError {
    /// 1-based row number in the file (header excluded for CSV).
    pub row: usize,
    pub column: Option<String>,
    pub message: String,
}

/// Outcome of one row after confirmation.
#[derive(Debug, Clone, Serialize)]
pub struct RowResult {
    pub row: usize,
    pub external_id: Option<String>,
    /// `staged`, `skipped` (duplicate external id), or `error`.
    pub status: String,
    pub message: Option<String>,
}

/// Lifecycle of an import job.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImportStatus {
    Validated,
    Failed,
    Staged,
}

impl ImportStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Validated => "validated",
            Self::Failed => "failed",
            Self::Staged => "staged",
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct ImportJob {
    pub id: Uuid,
    pub entity: String,
    pub format: String,
    pub status: String,
    pub total_rows: usize,
    pub valid_rows: usize,
    pub invalid_rows: usize,
    pub row_errors: Vec<RowError>,
    pub results: Vec<RowResult>,
    /// Actor who submitted the import (`X-User-Id` header).
    pub actor: String,
    /// Owning company, when the caller belongs to one.
    pub company_id: Option<Uuid>,
    /// Validated rows, kept so confirmation can stage them. Only the
    /// validated column values are kept, never raw file bytes.
    #[serde(skip_serializing)]
    pub staged_rows: Vec<Value>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Lifecycle of an export job.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExportStatus {
    Completed,
    Failed,
}

impl ExportStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Completed => "completed",
            Self::Failed => "failed",
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct ExportJob {
    pub id: Uuid,
    pub entity: String,
    pub format: String,
    pub status: String,
    pub fields: Vec<String>,
    pub row_count: usize,
    /// `hook` when rows came from an owning module, `none` when no
    /// module contributed rows for this entity.
    pub data_provider: String,
    pub download_url: String,
    pub error: Option<String>,
    /// True when personal data in the rows was redacted (forced when
    /// the caller lacks `import_export:export_pii`).
    pub pii_redacted: bool,
    /// Personal-data columns that were redacted, if any.
    pub redacted_fields: Vec<String>,
    /// Requested row window (chunked export): rows start at `offset`
    /// and at most `limit` rows are returned.
    pub limit: usize,
    pub offset: usize,
    /// True when the provider returned more rows than the requested
    /// window: the runner asks for `limit + 1` rows and reports
    /// more only when that extra row arrives (after dedupe and
    /// tenant scoping), so a window that happens to hold exactly
    /// `limit` rows no longer claims more rows exist.
    pub has_more: bool,
    /// Rows dropped because their `company_id` did not match the
    /// caller's company.
    pub rows_excluded_by_tenant_scope: usize,
    /// Actor who requested the export (`X-User-Id` header).
    pub actor: String,
    /// Owning company, when the caller belongs to one.
    pub company_id: Option<Uuid>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    /// Finished export bytes, kept in memory for download.
    #[serde(skip_serializing)]
    pub content: Vec<u8>,
}

/// One saved field-mapping template: file column name -> entity
/// column name, reusable across imports of the same entity.
#[derive(Debug, Clone, Serialize)]
pub struct MappingTemplate {
    pub id: Uuid,
    pub name: String,
    pub entity: String,
    pub mapping: HashMap<String, String>,
    pub actor: String,
    pub company_id: Option<Uuid>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Outcome of offering a schedule to the scheduler module.
#[derive(Debug, Clone, Serialize)]
pub struct SchedulingOutcome {
    pub enqueued: bool,
    pub message: String,
}

/// One scheduled export: a stored export definition plus its cron
/// schedule. Runs are produced by `run-now`, by the
/// `export.run_requested` hook handler, or -- once the scheduler
/// gains an `export.run` dispatch arm -- by the scheduler.
#[derive(Debug, Clone, Serialize)]
pub struct ExportSchedule {
    pub id: Uuid,
    pub name: String,
    pub entity: String,
    pub format: String,
    pub fields: Vec<String>,
    pub filters: HashMap<String, String>,
    pub redact_pii: bool,
    pub redact_names: bool,
    /// True when the creator held `import_export:export_pii` at
    /// creation time. A scheduled run never exports personal data
    /// unredacted unless this is true; the flag is never widened by
    /// a run, only by a manage-capable update from a caller who holds
    /// the PII capability at that moment.
    pub pii_allowed: bool,
    /// Five cron-like fields, or an interval in seconds (same rule
    /// the scheduler module validates with).
    pub schedule: String,
    pub enabled: bool,
    pub company_id: Option<Uuid>,
    pub actor: String,
    pub last_run_at: Option<DateTime<Utc>>,
    pub last_export_id: Option<Uuid>,
    pub last_error: Option<String>,
    pub run_count: u64,
    pub scheduling: SchedulingOutcome,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// One chunked import session. Each chunk is a self-contained
/// document (CSV chunk repeats its header, JSON chunk is an array,
/// XML chunk is a `<rows>` document) validated on arrival; nothing
/// is written anywhere until the session is completed into an
/// import job and that job is confirmed.
#[derive(Debug, Clone, Serialize)]
pub struct ImportSession {
    pub id: Uuid,
    pub entity: String,
    pub format: String,
    /// `open`, `completed`, or `failed`.
    pub status: String,
    /// Declared chunk count, when the client knows it up front.
    pub total_chunks: Option<usize>,
    pub received_chunks: Vec<usize>,
    pub total_rows: usize,
    pub valid_rows: usize,
    pub invalid_rows: usize,
    pub row_errors: Vec<RowError>,
    /// Resolved rename map for the whole session.
    pub mapping: HashMap<String, String>,
    pub actor: String,
    pub company_id: Option<Uuid>,
    #[serde(skip_serializing)]
    pub staged_rows: Vec<Value>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}
