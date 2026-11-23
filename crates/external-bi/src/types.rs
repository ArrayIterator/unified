//! BI external module (split from lib.rs; moved verbatim).

use super::*;


// ---------------------------------------------------------------------------
// Domain types
// ---------------------------------------------------------------------------

/// A field type in a dataset definition. Datasets declare shape only.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FieldType {
    String,
    Number,
    Boolean,
    Datetime,
}

impl FieldType {
    pub(crate) fn as_str(&self) -> &'static str {
        match self {
            Self::String => "string",
            Self::Number => "number",
            Self::Boolean => "boolean",
            Self::Datetime => "datetime",
        }
    }
}

/// One declared field of a dataset.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FieldDef {
    pub name: String,
    #[serde(rename = "type")]
    pub field_type: FieldType,
}

/// A dataset definition: shape only, never code and never SQL.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Dataset {
    pub id: String,
    pub tenant_id: String,
    pub name: String,
    /// Hook event name or module id the dataset extracts from.
    /// Validated as a non-empty string; never executed.
    pub source: String,
    pub fields: Vec<FieldDef>,
    pub description: String,
    pub owner: String,
    /// Bumped whenever `fields` changes, so snapshots record which
    /// schema version produced them.
    pub schema_version: u32,
    pub created_at: String,
    pub updated_at: String,
}

/// One rejected row from an extract, with the reason it was rejected.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RowRejection {
    pub row_index: usize,
    pub reason: String,
}

/// One extract run.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExtractRun {
    pub id: String,
    pub tenant_id: String,
    pub dataset_id: String,
    /// `completed` or `failed`.
    pub status: String,
    /// `manual` or `schedule`.
    pub trigger: String,
    pub started_at: String,
    pub finished_at: Option<String>,
    pub duration_ms: Option<u64>,
    pub rows_accepted: usize,
    pub rows_rejected: usize,
    pub rejections: Vec<RowRejection>,
    pub error: Option<String>,
}

/// One immutable snapshot: the accepted rows of one extract, plus the
/// schema version and extraction time. Append-only by design.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Snapshot {
    pub id: String,
    pub tenant_id: String,
    pub dataset_id: String,
    pub schema_version: u32,
    pub extracted_at: String,
    pub row_count: usize,
    pub rows: Vec<Value>,
}

/// A destination an extract can be delivered to.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Destination {
    pub id: String,
    pub tenant_id: String,
    /// `webhook` or `file`.
    pub kind: String,
    pub name: String,
    /// Webhook URL. Required for `webhook`, forbidden for `file`.
    pub url: Option<String>,
    /// Relative path below the export base directory. Required for
    /// `file`, forbidden for `webhook`.
    pub path: Option<String>,
    pub created_at: String,
}

/// One delivery attempt to a destination.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeliveryAttempt {
    pub id: String,
    pub tenant_id: String,
    pub destination_id: String,
    pub dataset_id: Option<String>,
    pub snapshot_id: Option<String>,
    /// `delivered`, `failed`, or `deferred` (a schedule tick ran
    /// outside an async runtime, so a webhook delivery could not be
    /// performed; use run-now or the test endpoint instead).
    pub status: String,
    pub status_code: Option<u16>,
    pub latency_ms: Option<u64>,
    pub error: Option<String>,
    pub attempted_at: String,
}

/// A schedule: dataset + cron expression + optional destination.
/// Evaluated only when `bi.schedule.tick` fires.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Schedule {
    pub id: String,
    pub tenant_id: String,
    pub dataset_id: String,
    pub destination_id: Option<String>,
    pub cron: String,
    pub enabled: bool,
    pub last_run_at: Option<String>,
    pub created_at: String,
}

// ---------------------------------------------------------------------------
// Request bodies
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub(crate) struct CreateDatasetRequest {
    pub(crate) tenant_id: Option<String>,
    pub(crate) name: String,
    pub(crate) source: String,
    pub(crate) fields: Vec<FieldDef>,
    pub(crate) description: Option<String>,
    pub(crate) owner: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct UpdateDatasetRequest {
    pub(crate) tenant_id: Option<String>,
    pub(crate) name: Option<String>,
    pub(crate) source: Option<String>,
    pub(crate) fields: Option<Vec<FieldDef>>,
    pub(crate) description: Option<String>,
    pub(crate) owner: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct ExtractRequest {
    pub(crate) tenant_id: Option<String>,
    pub(crate) destination_id: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct CreateDestinationRequest {
    pub(crate) tenant_id: Option<String>,
    pub(crate) kind: String,
    pub(crate) name: String,
    pub(crate) url: Option<String>,
    pub(crate) path: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct CreateScheduleRequest {
    pub(crate) tenant_id: Option<String>,
    pub(crate) dataset_id: String,
    pub(crate) destination_id: Option<String>,
    pub(crate) cron: String,
    pub(crate) enabled: Option<bool>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct QueryFilter {
    pub(crate) field: String,
    pub(crate) op: String,
    pub(crate) value: Value,
}

#[derive(Debug, Deserialize)]
pub(crate) struct AggSpec {
    pub(crate) agg: String,
    pub(crate) field: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct SortSpec {
    pub(crate) field: String,
    pub(crate) direction: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct QueryRequest {
    pub(crate) tenant_id: Option<String>,
    pub(crate) snapshot_id: Option<String>,
    pub(crate) filters: Option<Vec<QueryFilter>>,
    pub(crate) group_by: Option<String>,
    pub(crate) aggregations: Option<Vec<AggSpec>>,
    pub(crate) sort: Option<SortSpec>,
    pub(crate) limit: Option<usize>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct ExportQuery {
    pub(crate) format: Option<String>,
    pub(crate) snapshot_id: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct DeliveryListQuery {
    pub(crate) destination_id: Option<String>,
    pub(crate) dataset_id: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct DatasetPath {
    pub id: String,
}

#[derive(Debug, Deserialize)]
pub(crate) struct DestinationPath {
    pub id: String,
}

#[derive(Debug, Deserialize)]
pub(crate) struct SchedulePath {
    pub id: String,
}
