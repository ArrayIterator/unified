// Import / Export module — CSV and JSON data exchange.
//
// Routes under `/api/v1/import-export`:
//
//   GET    /entities            entity catalog with columns and types
//   GET    /exports             list export jobs (paginated, filterable)
//   POST   /exports             start an export job (JSON or CSV)
//   GET    /exports/{id}        export job status
//   GET    /exports/{id}/download   download the finished export file
//   GET    /imports             list import jobs (paginated, filterable)
//   POST   /imports             submit a file for validation (dry run)
//   GET    /imports/{id}        import job with per-row results
//   POST   /imports/{id}/confirm   confirm a validated import (stages rows)
//
// Import flow: submit -> dry-run validation (row errors reported,
// nothing written) -> confirm -> rows are staged on the job and an
// `import.completed` hook is emitted carrying the staged rows, so the
// owning module can apply them. The owning modules expose no public
// write/import function today (verified: `users`, `software`, `store`,
// `billing`, and `licensing` only expose their `Module` struct and
// `register`), so this module deliberately does NOT invent a write
// path into their tables. The exact wiring snippet for the integrator
// is in the coordinator report.
//
// Export flow: an export job emits `export.requested` on the hook bus
// and collects rows contributed by owning modules through hook
// results (`{"rows": [...]}` or a bare array). Field selection trims
// the columns. When no module contributes rows the job still
// completes, with zero rows and `data_provider: "none"`, so the gap
// is visible instead of silent.
//
// Safety: file size is capped at 10 MiB, rows at 10,000 per file.
// Columns that look like secrets (`password`, `secret`, `token`,
// `api_key`, `private_key`, `credential`, `hash`, ...) are rejected
// outright: secrets are never imported. Values are type-checked per
// column (string, integer, number, boolean, date, email, uuid).
// Export content is kept in memory behind the job; the download
// endpoint streams it back as a file attachment.

use crate::core::context::AppContext;
use crate::core::error::{UnifiedError, UnifiedResult};
use crate::core::hooks::{HookBus, HookContext};
use crate::core::json;
use crate::core::modules::{Module, ModuleRegistry};
use crate::core::pagination::{PageResult, PaginationParams};
use actix_web::{web, HttpResponse};
use base64::Engine;
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::sync::{Arc, OnceLock};
use uuid::Uuid;

// ---------------------------------------------------------------------------
// Constants.
// ---------------------------------------------------------------------------

/// Maximum accepted import payload in bytes (10 MiB).
const MAX_FILE_BYTES: usize = 10 * 1024 * 1024;
/// Maximum rows per import file.
const MAX_ROWS: usize = 10_000;
/// Maximum rows per export job (also the streaming cap).
const MAX_EXPORT_ROWS: usize = 10_000;
/// Maximum export content kept in memory per job (50 MiB).
const MAX_EXPORT_BYTES: usize = 50 * 1024 * 1024;

/// Well-known events emitted by this module.
pub mod import_export_events {
    pub const IMPORT_COMPLETED: &str = "import.completed";
    pub const IMPORT_FAILED: &str = "import.failed";
    pub const EXPORT_COMPLETED: &str = "export.completed";
    pub const EXPORT_REQUESTED: &str = "export.requested";
}

/// Column-name fragments that are never allowed in an import file.
/// Matching is case-insensitive on the normalized (lowercase,
/// non-alphanumeric-stripped) column name.
const FORBIDDEN_COLUMN_FRAGMENTS: &[&str] = &[
    "password",
    "passwd",
    "secret",
    "token",
    "apikey",
    "api_key",
    "privatekey",
    "private_key",
    "credential",
    "hash",
    "salt",
    "totp",
    "otpsecret",
];

// ---------------------------------------------------------------------------
// Entity catalog.
// ---------------------------------------------------------------------------

/// One column of one importable/exportable entity.
#[derive(Debug, Clone, Serialize)]
pub struct EntityColumn {
    pub name: &'static str,
    /// `string`, `integer`, `number`, `boolean`, `date`, `email`, `uuid`.
    #[serde(rename = "type")]
    pub kind: &'static str,
    pub required: bool,
    /// Short description of the column.
    pub description: &'static str,
}

/// One importable/exportable entity.
#[derive(Debug, Clone, Serialize)]
pub struct EntitySpec {
    pub entity: &'static str,
    pub description: &'static str,
    /// Column used for idempotency when present in the file.
    pub external_id_column: Option<&'static str>,
    pub columns: &'static [EntityColumn],
}

const USERS_COLUMNS: &[EntityColumn] = &[
    EntityColumn { name: "external_id", kind: "string", required: false, description: "Caller-supplied stable id for idempotency" },
    EntityColumn { name: "email", kind: "email", required: true, description: "User email address" },
    EntityColumn { name: "name", kind: "string", required: true, description: "Display name" },
    EntityColumn { name: "is_active", kind: "boolean", required: false, description: "Whether the account is active" },
    EntityColumn { name: "company_id", kind: "uuid", required: false, description: "Owning company id" },
];

const SOFTWARE_COLUMNS: &[EntityColumn] = &[
    EntityColumn { name: "external_id", kind: "string", required: false, description: "Caller-supplied stable id for idempotency" },
    EntityColumn { name: "name", kind: "string", required: true, description: "Software name" },
    EntityColumn { name: "slug", kind: "string", required: true, description: "URL slug, unique" },
    EntityColumn { name: "version", kind: "string", required: false, description: "Current version, semver" },
    EntityColumn { name: "description", kind: "string", required: false, description: "Short description" },
    EntityColumn { name: "status", kind: "string", required: false, description: "draft, published, deprecated, or archived" },
];

const PRODUCTS_COLUMNS: &[EntityColumn] = &[
    EntityColumn { name: "external_id", kind: "string", required: false, description: "Caller-supplied stable id for idempotency" },
    EntityColumn { name: "name", kind: "string", required: true, description: "Product name" },
    EntityColumn { name: "slug", kind: "string", required: true, description: "URL slug, unique" },
    EntityColumn { name: "price", kind: "number", required: true, description: "Price in the store currency" },
    EntityColumn { name: "currency", kind: "string", required: false, description: "ISO currency code, for example USD" },
    EntityColumn { name: "is_active", kind: "boolean", required: false, description: "Whether the product is on sale" },
];

const ORDERS_COLUMNS: &[EntityColumn] = &[
    EntityColumn { name: "external_id", kind: "string", required: false, description: "Caller-supplied stable id for idempotency" },
    EntityColumn { name: "customer_email", kind: "email", required: true, description: "Buyer email address" },
    EntityColumn { name: "product_slug", kind: "string", required: true, description: "Ordered product slug" },
    EntityColumn { name: "quantity", kind: "integer", required: false, description: "Quantity, default 1" },
    EntityColumn { name: "total", kind: "number", required: true, description: "Order total in the store currency" },
    EntityColumn { name: "currency", kind: "string", required: false, description: "ISO currency code, for example USD" },
    EntityColumn { name: "status", kind: "string", required: false, description: "pending, paid, refunded, or cancelled" },
];

const INVOICES_COLUMNS: &[EntityColumn] = &[
    EntityColumn { name: "external_id", kind: "string", required: false, description: "Caller-supplied stable id for idempotency" },
    EntityColumn { name: "order_external_id", kind: "string", required: true, description: "External id of the order this invoice belongs to" },
    EntityColumn { name: "number", kind: "string", required: true, description: "Invoice number, unique" },
    EntityColumn { name: "total", kind: "number", required: true, description: "Invoice total in the store currency" },
    EntityColumn { name: "currency", kind: "string", required: false, description: "ISO currency code, for example USD" },
    EntityColumn { name: "status", kind: "string", required: false, description: "draft, issued, paid, void, or overdue" },
    EntityColumn { name: "due_date", kind: "date", required: false, description: "Due date (RFC 3339 or YYYY-MM-DD)" },
];

const TASKS_COLUMNS: &[EntityColumn] = &[
    EntityColumn { name: "external_id", kind: "string", required: false, description: "Caller-supplied stable id for idempotency" },
    EntityColumn { name: "title", kind: "string", required: true, description: "Task title" },
    EntityColumn { name: "description", kind: "string", required: false, description: "Task description" },
    EntityColumn { name: "status", kind: "string", required: false, description: "open, in_progress, done, or cancelled" },
    EntityColumn { name: "priority", kind: "string", required: false, description: "low, normal, high, or urgent" },
    EntityColumn { name: "assignee_email", kind: "email", required: false, description: "Assignee email address" },
    EntityColumn { name: "due_date", kind: "date", required: false, description: "Due date (RFC 3339 or YYYY-MM-DD)" },
];

const LICENSES_COLUMNS: &[EntityColumn] = &[
    EntityColumn { name: "external_id", kind: "string", required: false, description: "Caller-supplied stable id for idempotency" },
    EntityColumn { name: "software_slug", kind: "string", required: true, description: "Licensed software slug" },
    EntityColumn { name: "owner_email", kind: "email", required: true, description: "License owner email address" },
    EntityColumn { name: "seats", kind: "integer", required: false, description: "Seat count, default 1" },
    EntityColumn { name: "expires_at", kind: "date", required: false, description: "Expiry (RFC 3339 or YYYY-MM-DD)" },
    EntityColumn { name: "status", kind: "string", required: false, description: "active, suspended, or revoked" },
];

const ENTITY_SPECS: &[EntitySpec] = &[
    EntitySpec { entity: "users", description: "User accounts", external_id_column: Some("external_id"), columns: USERS_COLUMNS },
    EntitySpec { entity: "software", description: "Software products and releases", external_id_column: Some("external_id"), columns: SOFTWARE_COLUMNS },
    EntitySpec { entity: "products", description: "Store products", external_id_column: Some("external_id"), columns: PRODUCTS_COLUMNS },
    EntitySpec { entity: "orders", description: "Store orders", external_id_column: Some("external_id"), columns: ORDERS_COLUMNS },
    EntitySpec { entity: "invoices", description: "Billing invoices", external_id_column: Some("external_id"), columns: INVOICES_COLUMNS },
    EntitySpec { entity: "tasks", description: "Tasks", external_id_column: Some("external_id"), columns: TASKS_COLUMNS },
    EntitySpec { entity: "licenses", description: "Software licenses", external_id_column: Some("external_id"), columns: LICENSES_COLUMNS },
];

fn entity_spec(name: &str) -> Option<&'static EntitySpec> {
    ENTITY_SPECS.iter().find(|spec| spec.entity == name)
}

// ---------------------------------------------------------------------------
// State.
// ---------------------------------------------------------------------------

static HOOK_BUS: OnceLock<Arc<HookBus>> = OnceLock::new();
static EXPORT_JOBS: OnceLock<DashMap<Uuid, ExportJob>> = OnceLock::new();
static IMPORT_JOBS: OnceLock<DashMap<Uuid, ImportJob>> = OnceLock::new();

fn export_jobs_store() -> &'static DashMap<Uuid, ExportJob> {
    EXPORT_JOBS.get_or_init(DashMap::new)
}

fn import_jobs_store() -> &'static DashMap<Uuid, ImportJob> {
    IMPORT_JOBS.get_or_init(DashMap::new)
}

fn emit(event: &str, payload: Value) -> Option<HookContext> {
    HOOK_BUS.get().map(|bus| bus.emit(event, payload))
}

// ---------------------------------------------------------------------------
// Shared types.
// ---------------------------------------------------------------------------

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
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    /// Finished export bytes, kept in memory for download.
    #[serde(skip_serializing)]
    pub content: Vec<u8>,
}

// ---------------------------------------------------------------------------
// CSV parsing (hand-rolled: no csv crate is available).
// ---------------------------------------------------------------------------

/// Parse CSV text into rows of fields. Handles quoted fields,
/// doubled-quote escapes, embedded commas/newlines, and CRLF endings.
/// Returns an error naming the first malformed line.
fn parse_csv(text: &str) -> Result<Vec<Vec<String>>, String> {
    let mut rows: Vec<Vec<String>> = Vec::new();
    let mut row: Vec<String> = Vec::new();
    let mut field = String::new();
    let mut in_quotes = false;
    let mut chars = text.chars().peekable();
    let mut line: usize = 1;
    while let Some(ch) = chars.next() {
        match ch {
            '"' => {
                if in_quotes {
                    if chars.peek() == Some(&'"') {
                        chars.next();
                        field.push('"');
                    } else {
                        in_quotes = false;
                    }
                } else if field.is_empty() {
                    in_quotes = true;
                } else {
                    return Err(format!("Line {line}: unexpected quote inside an unquoted field"));
                }
            }
            ',' if !in_quotes => {
                row.push(std::mem::take(&mut field));
            }
            '\r' => {
                if in_quotes {
                    field.push('\r');
                } else if chars.peek() == Some(&'\n') {
                    chars.next();
                    row.push(std::mem::take(&mut field));
                    rows.push(std::mem::take(&mut row));
                    line += 1;
                } else {
                    row.push(std::mem::take(&mut field));
                    rows.push(std::mem::take(&mut row));
                    line += 1;
                }
            }
            '\n' if !in_quotes => {
                row.push(std::mem::take(&mut field));
                rows.push(std::mem::take(&mut row));
                line += 1;
            }
            other => field.push(other),
        }
    }
    if in_quotes {
        return Err(format!("Line {line}: unterminated quoted field"));
    }
    if !field.is_empty() || !row.is_empty() {
        row.push(field);
        rows.push(row);
    }
    // Drop a trailing empty row produced by a final newline.
    while let Some(last) = rows.last() {
        if last.len() == 1 && last[0].is_empty() {
            rows.pop();
        } else {
            break;
        }
    }
    Ok(rows)
}

fn escape_csv_field(field: &str) -> String {
    if field.contains(',') || field.contains('"') || field.contains('\n') || field.contains('\r') {
        format!("\"{}\"", field.replace('"', "\"\""))
    } else {
        field.to_string()
    }
}

fn value_to_csv_cell(value: &Value) -> String {
    match value {
        Value::Null => String::new(),
        Value::Bool(flag) => flag.to_string(),
        Value::Number(number) => number.to_string(),
        Value::String(text) => text.clone(),
        other => serde_json::to_string(other).unwrap_or_default(),
    }
}

// ---------------------------------------------------------------------------
// Validation.
// ---------------------------------------------------------------------------

fn normalized_column(name: &str) -> String {
    name.chars()
        .filter(|ch| ch.is_ascii_alphanumeric())
        .flat_map(|ch| ch.to_lowercase())
        .collect()
}

fn is_forbidden_column(name: &str) -> bool {
    let normalized = normalized_column(name);
    FORBIDDEN_COLUMN_FRAGMENTS
        .iter()
        .any(|fragment| normalized.contains(fragment))
}

fn type_error(row: usize, column: &str, kind: &str, value: &str) -> RowError {
    RowError {
        row,
        column: Some(column.to_string()),
        message: format!("Value \"{value}\" is not a valid {kind}"),
    }
}

/// Validate and coerce one cell value. Returns the JSON value, or a
/// row error. Empty strings become JSON null for optional columns.
fn coerce_value(row: usize, column: &EntityColumn, raw: &str) -> Result<Value, RowError> {
    let raw = raw.trim();
    if raw.is_empty() {
        return Ok(Value::Null);
    }
    match column.kind {
        "string" => Ok(Value::String(raw.to_string())),
        "integer" => raw.parse::<i64>().map(Value::from).map_err(|_| {
            type_error(row, column.name, "integer", raw)
        }),
        "number" => raw
            .parse::<f64>()
            .map_err(|_| type_error(row, column.name, "number", raw))
            .and_then(|number| {
                serde_json::Number::from_f64(number)
                    .map(Value::Number)
                    .ok_or_else(|| type_error(row, column.name, "number", raw))
            }),
        "boolean" => match raw.to_ascii_lowercase().as_str() {
            "true" | "1" | "yes" | "y" => Ok(Value::Bool(true)),
            "false" | "0" | "no" | "n" => Ok(Value::Bool(false)),
            _ => Err(type_error(row, column.name, "boolean", raw)),
        },
        "email" => {
            let parts: Vec<&str> = raw.split('@').collect();
            let valid = parts.len() == 2
                && !parts[0].is_empty()
                && parts[1].contains('.')
                && !parts[1].starts_with('.')
                && !raw.contains(char::is_whitespace);
            if valid {
                Ok(Value::String(raw.to_string()))
            } else {
                Err(type_error(row, column.name, "email", raw))
            }
        }
        "uuid" => raw.parse::<Uuid>().map(|id| Value::String(id.to_string())).map_err(|_| {
            type_error(row, column.name, "uuid", raw)
        }),
        "date" => {
            let parsed = DateTime::parse_from_rfc3339(raw)
                .map(|date| date.with_timezone(&Utc))
                .or_else(|_| {
                    chrono::NaiveDate::parse_from_str(raw, "%Y-%m-%d")
                        .map(|date| date.and_hms_opt(0, 0, 0).expect("midnight is valid").and_utc())
                });
            match parsed {
                Ok(date) => Ok(Value::String(date.to_rfc3339())),
                Err(_) => Err(type_error(row, column.name, "date", raw)),
            }
        }
        _ => Ok(Value::String(raw.to_string())),
    }
}

/// Validate raw rows (each a map of column name to raw string) against
/// an entity spec. Returns validated rows plus every row error found.
fn validate_rows(
    spec: &EntitySpec,
    raw_rows: Vec<HashMap<String, String>>,
) -> (Vec<Value>, Vec<RowError>) {
    let mut valid: Vec<Value> = Vec::new();
    let mut errors: Vec<RowError> = Vec::new();
    for (index, raw_row) in raw_rows.iter().enumerate() {
        let row_number = index + 1;
        let mut object = serde_json::Map::new();
        let mut row_errors: Vec<RowError> = Vec::new();
        for column in spec.columns {
            match raw_row.get(column.name) {
                Some(raw) => match coerce_value(row_number, column, raw) {
                    Ok(value) => {
                        if !value.is_null() {
                            object.insert(column.name.to_string(), value);
                        }
                    }
                    Err(error) => row_errors.push(error),
                },
                None => {
                    if column.required {
                        row_errors.push(RowError {
                            row: row_number,
                            column: Some(column.name.to_string()),
                            message: format!("Missing required column \"{}\"", column.name),
                        });
                    }
                }
            }
        }
        if row_errors.is_empty() {
            valid.push(Value::Object(object));
        } else {
            errors.extend(row_errors);
        }
    }
    (valid, errors)
}

// ---------------------------------------------------------------------------
// Request types.
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct CreateImportRequest {
    entity: String,
    /// `csv` or `json`.
    format: String,
    /// File content: UTF-8 text, or base64 when `encoding` is `base64`.
    content: String,
    /// `utf8` (default) or `base64`.
    #[serde(default)]
    encoding: Option<String>,
    /// Optional rename map: file column name -> entity column name.
    #[serde(default)]
    mapping: Option<HashMap<String, String>>,
}

#[derive(Debug, Deserialize)]
struct CreateExportRequest {
    entity: String,
    /// `csv` or `json` (default `json`).
    #[serde(default)]
    format: Option<String>,
    /// Columns to include, in order. Defaults to every column.
    #[serde(default)]
    fields: Option<Vec<String>>,
    /// Exact-match filters forwarded to the data provider hook.
    #[serde(default)]
    filters: Option<HashMap<String, String>>,
}

fn paginate<T: Serialize + Clone>(items: Vec<T>, params: &PaginationParams) -> PageResult<T> {
    let pagination = params.normalized();
    let total = items.len() as u64;
    let start = ((pagination.page - 1) * pagination.per_page) as usize;
    let end = (start + pagination.per_page as usize).min(items.len());
    let page_items = if start >= items.len() {
        Vec::new()
    } else {
        items[start..end].to_vec()
    };
    PageResult::new(page_items, total, &pagination)
}

fn decode_content(body: &CreateImportRequest) -> UnifiedResult<String> {
    let bytes = match body.encoding.as_deref().unwrap_or("utf8") {
        "base64" => base64::engine::general_purpose::STANDARD
            .decode(body.content.trim())
            .map_err(|_| UnifiedError::bad_request("content is not valid base64"))?,
        "utf8" => body.content.clone().into_bytes(),
        other => {
            return Err(UnifiedError::bad_request(format!(
                "encoding must be \"utf8\" or \"base64\", got \"{other}\""
            )))
        }
    };
    if bytes.len() > MAX_FILE_BYTES {
        return Err(UnifiedError::bad_request(format!(
            "File is too large: maximum is {} bytes",
            MAX_FILE_BYTES
        )));
    }
    String::from_utf8(bytes)
        .map_err(|_| UnifiedError::bad_request("content is not valid UTF-8 text"))
}

/// Turn submitted content into raw rows: each row maps entity column
/// names to raw strings. Applies the optional rename mapping first.
fn content_to_raw_rows(
    spec: &EntitySpec,
    format: &str,
    text: &str,
    mapping: Option<&HashMap<String, String>>,
) -> UnifiedResult<Vec<HashMap<String, String>>> {
    let apply_mapping = |name: &str| -> String {
        mapping
            .and_then(|map| map.get(name))
            .cloned()
            .unwrap_or_else(|| name.to_string())
    };
    match format {
        "csv" => {
            let rows = parse_csv(text)
                .map_err(|error| UnifiedError::bad_request(format!("Invalid CSV: {error}")))?;
            if rows.is_empty() {
                return Err(UnifiedError::bad_request("CSV file is empty"));
            }
            let header: Vec<String> = rows[0].iter().map(|name| apply_mapping(name.trim())).collect();
            if header.iter().any(|name| name.trim().is_empty()) {
                return Err(UnifiedError::bad_request("CSV header contains an empty column name"));
            }
            let mut seen: Vec<String> = Vec::new();
            for name in &header {
                if seen.contains(name) {
                    return Err(UnifiedError::bad_request(format!(
                        "CSV header contains duplicate column \"{name}\""
                    )));
                }
                seen.push(name.clone());
            }
            for name in &header {
                if is_forbidden_column(name) {
                    return Err(UnifiedError::bad_request(format!(
                        "Column \"{name}\" looks like a secret or credential and is never imported"
                    )));
                }
                if !spec.columns.iter().any(|column| column.name == name) {
                    return Err(UnifiedError::bad_request(format!(
                        "Unknown column \"{name}\" for entity \"{}\"",
                        spec.entity
                    )));
                }
            }
            let mut raw_rows = Vec::new();
            for (index, row) in rows.iter().skip(1).enumerate() {
                if row.len() != header.len() {
                    return Err(UnifiedError::bad_request(format!(
                        "CSV row {} has {} fields but the header has {}",
                        index + 2,
                        row.len(),
                        header.len()
                    )));
                }
                let mut map = HashMap::new();
                for (name, value) in header.iter().zip(row.iter()) {
                    map.insert(name.clone(), value.clone());
                }
                raw_rows.push(map);
            }
            Ok(raw_rows)
        }
        "json" => {
            let value: Value = serde_json::from_str(text)
                .map_err(|error| UnifiedError::bad_request(format!("Invalid JSON: {error}")))?;
            let array = value.as_array().ok_or_else(|| {
                UnifiedError::bad_request("JSON import must be an array of objects")
            })?;
            let mut raw_rows = Vec::new();
            for (index, item) in array.iter().enumerate() {
                let object = item.as_object().ok_or_else(|| {
                    UnifiedError::bad_request(format!("JSON row {} is not an object", index + 1))
                })?;
                let mut map = HashMap::new();
                for (key, value) in object {
                    let name = apply_mapping(key);
                    if is_forbidden_column(&name) {
                        return Err(UnifiedError::bad_request(format!(
                            "Column \"{name}\" looks like a secret or credential and is never imported"
                        )));
                    }
                    if !spec.columns.iter().any(|column| column.name == name) {
                        return Err(UnifiedError::bad_request(format!(
                            "Unknown column \"{name}\" for entity \"{}\"",
                            spec.entity
                        )));
                    }
                    let raw = match value {
                        Value::Null => String::new(),
                        Value::String(text) => text.clone(),
                        other => other.to_string(),
                    };
                    map.insert(name, raw);
                }
                raw_rows.push(map);
            }
            Ok(raw_rows)
        }
        other => Err(UnifiedError::bad_request(format!(
            "format must be \"csv\" or \"json\", got \"{other}\""
        ))),
    }
}

// ---------------------------------------------------------------------------
// Handlers: entities and exports.
// ---------------------------------------------------------------------------

async fn list_entities(ctx: web::Data<Arc<AppContext>>) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        Ok(serde_json::json!({
            "entities": ENTITY_SPECS,
            "max_file_bytes": MAX_FILE_BYTES,
            "max_rows": MAX_ROWS,
            "formats": ["csv", "json"],
        }))
    })();
    json::respond(result, debug)
}

async fn create_export(
    ctx: web::Data<Arc<AppContext>>,
    body: web::Json<CreateExportRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<ExportJob> = (|| {
        let spec = entity_spec(&body.entity).ok_or_else(|| {
            UnifiedError::bad_request(format!(
                "Unknown entity \"{}\"; see GET /api/v1/import-export/entities",
                body.entity
            ))
        })?;
        let format = body.format.clone().unwrap_or_else(|| "json".to_string());
        if format != "csv" && format != "json" {
            return Err(UnifiedError::bad_request(
                "format must be \"csv\" or \"json\"",
            ));
        }
        let fields: Vec<String> = match &body.fields {
            Some(fields) => {
                if fields.is_empty() {
                    return Err(UnifiedError::bad_request(
                        "fields must not be empty when provided",
                    ));
                }
                for field in fields {
                    if !spec.columns.iter().any(|column| column.name == field) {
                        return Err(UnifiedError::bad_request(format!(
                            "Unknown field \"{field}\" for entity \"{}\"",
                            spec.entity
                        )));
                    }
                }
                fields.clone()
            }
            None => spec
                .columns
                .iter()
                .map(|column| column.name.to_string())
                .collect(),
        };
        // Ask owning modules for rows through the hook bus. Each
        // handler contributes `{"rows": [...]}` or a bare array.
        let filters = body.filters.clone().unwrap_or_default();
        let mut rows: Vec<Value> = Vec::new();
        let mut provider = "none".to_string();
        if let Some(context) = emit(
            import_export_events::EXPORT_REQUESTED,
            serde_json::json!({
                "entity": spec.entity,
                "fields": fields,
                "filters": filters,
                "limit": MAX_EXPORT_ROWS,
            }),
        ) {
            for result in &context.results {
                let contributed: Option<&Vec<Value>> = result
                    .get("rows")
                    .and_then(Value::as_array)
                    .or_else(|| result.as_array());
                if let Some(list) = contributed {
                    if !list.is_empty() {
                        provider = "hook".to_string();
                    }
                    for row in list {
                        if rows.len() >= MAX_EXPORT_ROWS {
                            break;
                        }
                        rows.push(row.clone());
                    }
                }
            }
        }
        // Trim every row to the requested fields.
        let mut trimmed: Vec<Value> = Vec::new();
        for row in rows {
            if let Some(object) = row.as_object() {
                let mut kept = serde_json::Map::new();
                for field in &fields {
                    if let Some(value) = object.get(field) {
                        kept.insert(field.clone(), value.clone());
                    }
                }
                trimmed.push(Value::Object(kept));
            }
        }
        let content: Vec<u8> = if format == "csv" {
            let mut text = fields
                .iter()
                .map(|field| escape_csv_field(field))
                .collect::<Vec<_>>()
                .join(",");
            text.push('\n');
            for row in &trimmed {
                let cells: Vec<String> = fields
                    .iter()
                    .map(|field| {
                        escape_csv_field(&value_to_csv_cell(
                            row.get(field).unwrap_or(&Value::Null),
                        ))
                    })
                    .collect();
                text.push_str(&cells.join(","));
                text.push('\n');
            }
            text.into_bytes()
        } else {
            serde_json::to_vec_pretty(&trimmed)
                .map_err(|error| UnifiedError::internal(format!("Cannot serialize export: {error}")))?
        };
        if content.len() > MAX_EXPORT_BYTES {
            return Err(UnifiedError::internal(
                "Export is too large to hold in memory; narrow it with filters or field selection",
            ));
        }
        let now = Utc::now();
        let id = Uuid::now_v7();
        let job = ExportJob {
            id,
            entity: spec.entity.to_string(),
            format,
            status: ExportStatus::Completed.as_str().to_string(),
            fields,
            row_count: trimmed.len(),
            data_provider: provider,
            download_url: format!("/api/v1/import-export/exports/{id}/download"),
            error: None,
            created_at: now,
            updated_at: now,
            content,
        };
        let view = job.clone();
        export_jobs_store().insert(id, job);
        emit(
            import_export_events::EXPORT_COMPLETED,
            serde_json::json!({
                "export_id": id,
                "entity": spec.entity,
                "row_count": view.row_count,
                "data_provider": view.data_provider.clone(),
            }),
        );
        Ok(view)
    })();
    match result {
        Ok(view) => json::success_response(view, 201),
        Err(error) => json::error_response(&error, debug),
    }
}

async fn list_exports(
    ctx: web::Data<Arc<AppContext>>,
    params: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let mut items: Vec<ExportJob> = export_jobs_store()
            .iter()
            .map(|entry| entry.value().clone())
            .collect();
        if let Some(entity) = params.filters.get("entity") {
            items.retain(|job| job.entity == *entity);
        }
        if let Some(format) = params.filters.get("format") {
            items.retain(|job| job.format == *format);
        }
        if let Some(status) = params.filters.get("status") {
            items.retain(|job| job.status == *status);
        }
        items.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        if params.normalized().order == "asc" {
            items.reverse();
        }
        Ok(paginate(items, &params).to_json())
    })();
    json::respond(result, debug)
}

async fn get_export(ctx: web::Data<Arc<AppContext>>, id: web::Path<Uuid>) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<ExportJob> = (|| {
        export_jobs_store()
            .get(&id.into_inner())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Export job not found"))
    })();
    json::respond(result, debug)
}

async fn download_export(ctx: web::Data<Arc<AppContext>>, id: web::Path<Uuid>) -> HttpResponse {
    let debug = ctx.is_debug();
    let Some(job) = export_jobs_store()
        .get(&id.into_inner())
        .map(|entry| entry.value().clone())
    else {
        return json::error_response(&UnifiedError::not_found("Export job not found"), debug);
    };
    if job.status != ExportStatus::Completed.as_str() {
        return json::error_response(
            &UnifiedError::conflict(format!(
                "Export job is in status \"{}\"; nothing to download",
                job.status
            )),
            debug,
        );
    }
    let (content_type, filename) = if job.format == "csv" {
        ("text/csv; charset=utf-8", format!("{}-export-{}.csv", job.entity, job.id))
    } else {
        (
            "application/json; charset=utf-8",
            format!("{}-export-{}.json", job.entity, job.id),
        )
    };
    HttpResponse::Ok()
        .content_type(content_type)
        .insert_header((
            "Content-Disposition",
            format!("attachment; filename=\"{filename}\""),
        ))
        .body(job.content)
}

// ---------------------------------------------------------------------------
// Handlers: imports.
// ---------------------------------------------------------------------------

async fn create_import(
    ctx: web::Data<Arc<AppContext>>,
    body: web::Json<CreateImportRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<ImportJob> = (|| {
        let spec = entity_spec(&body.entity).ok_or_else(|| {
            UnifiedError::bad_request(format!(
                "Unknown entity \"{}\"; see GET /api/v1/import-export/entities",
                body.entity
            ))
        })?;
        let format = body.format.trim().to_lowercase();
        if format != "csv" && format != "json" {
            return Err(UnifiedError::bad_request(
                "format must be \"csv\" or \"json\"",
            ));
        }
        let text = decode_content(&body)?;
        let raw_rows = content_to_raw_rows(spec, &format, &text, body.mapping.as_ref())?;
        if raw_rows.len() > MAX_ROWS {
            return Err(UnifiedError::bad_request(format!(
                "File has {} rows; the maximum is {MAX_ROWS}",
                raw_rows.len()
            )));
        }
        let (valid_rows, row_errors) = validate_rows(spec, raw_rows);
        // Row errors are counted per invalid row, not per problem, so
        // a row with three bad columns is still one invalid row.
        let mut invalid_row_numbers: Vec<usize> = row_errors.iter().map(|error| error.row).collect();
        invalid_row_numbers.sort_unstable();
        invalid_row_numbers.dedup();
        let invalid_rows = invalid_row_numbers.len();
        let total = valid_rows.len() + invalid_rows;
        let now = Utc::now();
        let id = Uuid::now_v7();
        let job = ImportJob {
            id,
            entity: spec.entity.to_string(),
            format,
            status: if invalid_rows == 0 {
                ImportStatus::Validated.as_str().to_string()
            } else {
                ImportStatus::Failed.as_str().to_string()
            },
            total_rows: total,
            valid_rows: valid_rows.len(),
            invalid_rows,
            row_errors,
            results: Vec::new(),
            staged_rows: valid_rows,
            created_at: now,
            updated_at: now,
        };
        let view = job.clone();
        import_jobs_store().insert(id, job);
        if invalid_rows > 0 {
            emit(
                import_export_events::IMPORT_FAILED,
                serde_json::json!({
                    "import_id": id,
                    "entity": spec.entity,
                    "invalid_rows": invalid_rows,
                    "total_rows": total,
                }),
            );
        }
        Ok(view)
    })();
    match result {
        Ok(view) => json::success_response(view, 201),
        Err(error) => json::error_response(&error, debug),
    }
}

async fn list_imports(
    ctx: web::Data<Arc<AppContext>>,
    params: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let mut items: Vec<ImportJob> = import_jobs_store()
            .iter()
            .map(|entry| entry.value().clone())
            .collect();
        if let Some(entity) = params.filters.get("entity") {
            items.retain(|job| job.entity == *entity);
        }
        if let Some(status) = params.filters.get("status") {
            items.retain(|job| job.status == *status);
        }
        items.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        if params.normalized().order == "asc" {
            items.reverse();
        }
        Ok(paginate(items, &params).to_json())
    })();
    json::respond(result, debug)
}

async fn get_import(ctx: web::Data<Arc<AppContext>>, id: web::Path<Uuid>) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<ImportJob> = (|| {
        import_jobs_store()
            .get(&id.into_inner())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Import job not found"))
    })();
    json::respond(result, debug)
}

async fn confirm_import(ctx: web::Data<Arc<AppContext>>, id: web::Path<Uuid>) -> HttpResponse {
    let debug = ctx.is_debug();
    let id = id.into_inner();
    let result: UnifiedResult<ImportJob> = (|| {
        let mut job = import_jobs_store()
            .get_mut(&id)
            .ok_or_else(|| UnifiedError::not_found("Import job not found"))?;
        if job.status != ImportStatus::Validated.as_str() {
            return Err(UnifiedError::conflict(format!(
                "Cannot confirm an import in status \"{}\"; only a validated import can be confirmed",
                job.status
            )));
        }
        // Idempotency: within one job, a repeated external id stages
        // the first row and skips later duplicates.
        let mut seen_external_ids: Vec<String> = Vec::new();
        let mut results: Vec<RowResult> = Vec::new();
        for (index, row) in job.staged_rows.iter().enumerate() {
            let external_id = row
                .get("external_id")
                .and_then(Value::as_str)
                .map(str::to_string);
            if let Some(external_id) = &external_id {
                if seen_external_ids.contains(external_id) {
                    results.push(RowResult {
                        row: index + 1,
                        external_id: Some(external_id.clone()),
                        status: "skipped".to_string(),
                        message: Some("Duplicate external_id inside this file".to_string()),
                    });
                    continue;
                }
                seen_external_ids.push(external_id.clone());
            }
            results.push(RowResult {
                row: index + 1,
                external_id,
                status: "staged".to_string(),
                message: None,
            });
        }
        let staged_count = results
            .iter()
            .filter(|result| result.status == "staged")
            .count();
        job.results = results;
        job.status = ImportStatus::Staged.as_str().to_string();
        job.updated_at = Utc::now();
        let staged_rows = job.staged_rows.clone();
        let entity = job.entity.clone();
        let view = job.clone();
        drop(job);
        // Hand the staged rows to the owning module through the hook
        // bus. No owning module consumes them yet; see the wiring
        // snippet in the coordinator report.
        emit(
            import_export_events::IMPORT_COMPLETED,
            serde_json::json!({
                "import_id": id,
                "entity": entity,
                "staged_rows": staged_count,
                "rows": staged_rows,
            }),
        );
        Ok(view)
    })();
    json::respond(result, debug)
}

// ---------------------------------------------------------------------------
// Module definition.
// ---------------------------------------------------------------------------

pub struct ImportExportModule;

impl Module for ImportExportModule {
    fn id(&self) -> &'static str {
        "import_export"
    }

    fn name(&self) -> &'static str {
        "Import / Export"
    }

    fn description(&self) -> &'static str {
        "CSV and JSON import/export for users, software, products, orders, invoices, tasks, and licenses: dry-run validation with per-row errors, confirmed staging with idempotency by external id, and hook-driven exports with field selection."
    }

    fn requires(&self) -> &[&'static str] {
        &["users", "software", "store", "billing", "tasks", "licensing"]
    }

    fn requires_libraries(&self) -> &[&'static str] {
        &["serde_json", "base64"]
    }

    fn init(&self, ctx: &AppContext) -> UnifiedResult<()> {
        let _ = HOOK_BUS.set(Arc::clone(&ctx.hooks));
        Ok(())
    }

    fn configure_routes(&self, cfg: &mut actix_web::web::ServiceConfig) {
        cfg.service(
            web::scope("/api/v1/import-export")
                .route("/entities", web::get().to(list_entities))
                .route("/exports", web::get().to(list_exports))
                .route("/exports", web::post().to(create_export))
                .route("/exports/{id}", web::get().to(get_export))
                .route("/exports/{id}/download", web::get().to(download_export))
                .route("/imports", web::get().to(list_imports))
                .route("/imports", web::post().to(create_import))
                .route("/imports/{id}", web::get().to(get_import))
                .route("/imports/{id}/confirm", web::post().to(confirm_import)),
        );
    }

    fn route_info(&self) -> Vec<crate::core::modules::RouteInfo> {
        use crate::core::modules::RouteInfo;
        vec![
            RouteInfo { method: "GET", path: "/api/v1/import-export/entities", owner: "import_export", description: "Entity catalog with columns and types" },
            RouteInfo { method: "GET", path: "/api/v1/import-export/exports", owner: "import_export", description: "List export jobs (paginated)" },
            RouteInfo { method: "POST", path: "/api/v1/import-export/exports", owner: "import_export", description: "Start an export job (JSON or CSV)" },
            RouteInfo { method: "GET", path: "/api/v1/import-export/exports/{id}", owner: "import_export", description: "Export job status" },
            RouteInfo { method: "GET", path: "/api/v1/import-export/exports/{id}/download", owner: "import_export", description: "Download the finished export file" },
            RouteInfo { method: "GET", path: "/api/v1/import-export/imports", owner: "import_export", description: "List import jobs (paginated)" },
            RouteInfo { method: "POST", path: "/api/v1/import-export/imports", owner: "import_export", description: "Submit a file for dry-run validation" },
            RouteInfo { method: "GET", path: "/api/v1/import-export/imports/{id}", owner: "import_export", description: "Import job with per-row results" },
            RouteInfo { method: "POST", path: "/api/v1/import-export/imports/{id}/confirm", owner: "import_export", description: "Confirm a validated import and stage its rows" },
        ]
    }
}

pub fn register(registry: &ModuleRegistry) {
    registry.register(Arc::new(ImportExportModule));
}
