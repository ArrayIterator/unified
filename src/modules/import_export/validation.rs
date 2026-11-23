//! import export - validation.

use unified_core::error::{UnifiedError, UnifiedResult};
use chrono::{DateTime, Utc};
use serde::Deserialize;
use serde_json::Value;
use std::collections::HashMap;
use uuid::Uuid;
use super::auth::*;
use super::entities::*;
use super::jobs::*;

pub(crate) fn normalized_column(name: &str) -> String {
    name.chars()
        .filter(|ch| ch.is_ascii_alphanumeric())
        .flat_map(|ch| ch.to_lowercase())
        .collect()
}

pub(crate) fn is_forbidden_column(name: &str) -> bool {
    let normalized = normalized_column(name);
    FORBIDDEN_COLUMN_FRAGMENTS
        .iter()
        .any(|fragment| normalized.contains(fragment))
}

pub(crate) fn type_error(row: usize, column: &str, kind: &str, value: &str) -> RowError {
    RowError {
        row,
        column: Some(column.to_string()),
        message: format!("Value \"{value}\" is not a valid {kind}"),
    }
}

/// Validate and coerce one cell value. Returns the JSON value, or a
/// row error. Empty strings become JSON null for optional columns.
pub(crate) fn coerce_value(row: usize, column: &EntityColumn, raw: &str) -> Result<Value, RowError> {
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
/// A row whose `company_id` disagrees with the caller's company
/// fails validation: a tenant can only import its own rows.
pub(crate) fn validate_rows(
    spec: &EntitySpec,
    raw_rows: Vec<HashMap<String, String>>,
    caller_company: Option<Uuid>,
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
                            if column.name == "company_id" {
                                if let (Some(company), Some(text)) =
                                    (caller_company, value.as_str())
                                {
                                    if text != company.to_string() {
                                        row_errors.push(RowError {
                                            row: row_number,
                                            column: Some(column.name.to_string()),
                                            message: "company_id does not match the caller's company"
                                                .to_string(),
                                        });
                                        continue;
                                    }
                                }
                            }
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

/// Shared column-name gate for every import format: reject secrets,
/// reject unknown columns.
pub(crate) fn check_column_name(spec: &EntitySpec, name: &str) -> Result<(), String> {
    if is_forbidden_column(name) {
        return Err(format!(
            "Column \"{name}\" looks like a secret or credential and is never imported"
        ));
    }
    if !spec.columns.iter().any(|column| column.name == name) {
        return Err(format!(
            "Unknown column \"{name}\" for entity \"{}\"",
            spec.entity
        ));
    }
    Ok(())
}

/// Validate a rename map: every target must be a known, non-secret
/// column of the entity, and every source name must be non-empty.
pub(crate) fn validate_mapping(spec: &EntitySpec, mapping: &HashMap<String, String>) -> UnifiedResult<()> {
    for (source, target) in mapping {
        if source.trim().is_empty() {
            return Err(UnifiedError::bad_request(
                "Mapping source column names must not be empty",
            ));
        }
        check_column_name(spec, target).map_err(UnifiedError::bad_request)?;
    }
    Ok(())
}

/// Validate an export field list against an entity spec.
pub(crate) fn validate_fields(spec: &EntitySpec, fields: &[String]) -> UnifiedResult<()> {
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
    Ok(())
}

pub(crate) fn require_text(value: &str, field: &str, max: usize) -> UnifiedResult<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err(UnifiedError::bad_request(format!("{field} must not be empty")));
    }
    if trimmed.len() > max {
        return Err(UnifiedError::bad_request(format!(
            "{field} must be at most {max} characters"
        )));
    }
    Ok(trimmed.to_string())
}

pub(crate) fn valid_format(format: &str, allowed: &[String]) -> bool {
    allowed.iter().any(|entry| entry == format)
}

#[derive(Debug, Deserialize)]
pub(crate) struct CreateImportRequest {
    pub(crate) entity: String,
    /// `csv`, `json`, or `xml`.
    pub(crate) format: String,
    /// File content: UTF-8 text, or base64 when `encoding` is `base64`.
    pub(crate) content: String,
    /// `utf8` (default) or `base64`.
    #[serde(default)]
    pub(crate) encoding: Option<String>,
    /// Optional rename map: file column name -> entity column name.
    #[serde(default)]
    pub(crate) mapping: Option<HashMap<String, String>>,
    /// Optional saved mapping template id. A per-request `mapping`
    /// wins over the template on conflicting keys.
    #[serde(default)]
    pub(crate) mapping_id: Option<Uuid>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct CreateExportRequest {
    pub(crate) entity: String,
    /// `csv`, `json`, or `xml` (default `json`).
    #[serde(default)]
    pub(crate) format: Option<String>,
    /// Columns to include, in order. Defaults to every column.
    #[serde(default)]
    pub(crate) fields: Option<Vec<String>>,
    /// Exact-match filters forwarded to the data provider hook.
    #[serde(default)]
    pub(crate) filters: Option<HashMap<String, String>>,
    /// Redact personal data in the exported rows. Defaults to the
    /// module setting; forced on when the caller lacks
    /// `import_export:export_pii`.
    #[serde(default)]
    pub(crate) redact_pii: Option<bool>,
    /// Also redact personal names when redacting (default false:
    /// names stay readable, emails and phones are still redacted).
    #[serde(default)]
    pub(crate) redact_names: Option<bool>,
    /// Chunked export window: at most this many rows (default: the
    /// configured maximum). Must not exceed the configured maximum.
    #[serde(default)]
    pub(crate) limit: Option<usize>,
    /// Chunked export window: rows start at this offset (default 0).
    /// Forwarded to the data provider hook.
    #[serde(default)]
    pub(crate) offset: Option<usize>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct CreateMappingRequest {
    pub(crate) name: String,
    pub(crate) entity: String,
    pub(crate) mapping: HashMap<String, String>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct CreateScheduleRequest {
    pub(crate) name: String,
    pub(crate) entity: String,
    #[serde(default)]
    pub(crate) format: Option<String>,
    #[serde(default)]
    pub(crate) fields: Option<Vec<String>>,
    #[serde(default)]
    pub(crate) filters: Option<HashMap<String, String>>,
    #[serde(default)]
    pub(crate) redact_pii: Option<bool>,
    #[serde(default)]
    pub(crate) redact_names: Option<bool>,
    pub(crate) schedule: String,
    #[serde(default)]
    pub(crate) enabled: Option<bool>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct UpdateScheduleRequest {
    #[serde(default)]
    pub(crate) name: Option<String>,
    #[serde(default)]
    pub(crate) format: Option<String>,
    #[serde(default)]
    pub(crate) fields: Option<Vec<String>>,
    #[serde(default)]
    pub(crate) filters: Option<HashMap<String, String>>,
    #[serde(default)]
    pub(crate) redact_pii: Option<bool>,
    #[serde(default)]
    pub(crate) redact_names: Option<bool>,
    #[serde(default)]
    pub(crate) schedule: Option<String>,
    #[serde(default)]
    pub(crate) enabled: Option<bool>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct UpdateSettingsRequest {
    #[serde(default)]
    pub(crate) max_file_bytes: Option<usize>,
    #[serde(default)]
    pub(crate) max_rows_per_file: Option<usize>,
    #[serde(default)]
    pub(crate) max_chunk_bytes: Option<usize>,
    #[serde(default)]
    pub(crate) max_rows_per_session: Option<usize>,
    #[serde(default)]
    pub(crate) max_export_rows: Option<usize>,
    #[serde(default)]
    pub(crate) max_export_bytes: Option<usize>,
    #[serde(default)]
    pub(crate) default_redact_pii: Option<bool>,
    #[serde(default)]
    pub(crate) allowed_formats: Option<Vec<String>>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct CreateSessionRequest {
    pub(crate) entity: String,
    /// `csv`, `json`, or `xml`.
    pub(crate) format: String,
    /// Optional rename map for the whole session.
    #[serde(default)]
    pub(crate) mapping: Option<HashMap<String, String>>,
    /// Optional saved mapping template id (same merge rule as imports).
    #[serde(default)]
    pub(crate) mapping_id: Option<Uuid>,
    /// Declared chunk count, when the client knows it up front.
    #[serde(default)]
    pub(crate) total_chunks: Option<usize>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct AppendChunkRequest {
    /// 0-based chunk index. Each index may be sent only once.
    pub(crate) chunk_index: usize,
    /// Chunk content: UTF-8 text, or base64 when `encoding` is `base64`.
    pub(crate) content: String,
    /// `utf8` (default) or `base64`.
    #[serde(default)]
    pub(crate) encoding: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalized_column_strips_punctuation_and_lowercases() {
        assert_eq!(normalized_column("First Name!"), "firstname");
        assert_eq!(normalized_column("Email-Address"), "emailaddress");
    }

    #[test]
    fn is_forbidden_column_flags_secret_columns() {
        assert!(is_forbidden_column("password_hash"));
        assert!(is_forbidden_column("user_password"));
        assert!(is_forbidden_column("api_key"));
        assert!(!is_forbidden_column("email"));
        assert!(!is_forbidden_column("name"));
    }

    /// Small fixture so the tests call the real 3-argument
    /// `coerce_value(row, column, raw)` signature.
    fn column(kind: &'static str) -> EntityColumn {
        EntityColumn { name: "x", kind, required: false, description: "" }
    }

    #[test]
    fn coerce_value_parses_integers_and_rejects_junk() {
        assert_eq!(coerce_value(1, &column("integer"), "42").unwrap(), serde_json::json!(42));
        assert_eq!(coerce_value(2, &column("integer"), " 7 ").unwrap(), serde_json::json!(7));
        let error = coerce_value(3, &column("integer"), "x").expect_err("junk is rejected");
        assert_eq!(error.row, 3);
        assert_eq!(error.column.as_deref(), Some("x"));
    }

    #[test]
    fn coerce_value_parses_booleans() {
        assert_eq!(coerce_value(1, &column("boolean"), "yes").unwrap(), serde_json::json!(true));
        assert_eq!(coerce_value(1, &column("boolean"), "0").unwrap(), serde_json::json!(false));
        assert!(coerce_value(1, &column("boolean"), "maybe").is_err());
    }

    #[test]
    fn coerce_value_validates_email_and_uuid() {
        assert_eq!(coerce_value(1, &column("email"), "a@b.co").unwrap(), serde_json::json!("a@b.co"));
        assert!(coerce_value(1, &column("email"), "bad").is_err());
        assert!(coerce_value(1, &column("uuid"), "550e8400-e29b-41d4-a716-446655440000").is_ok());
        assert!(coerce_value(1, &column("uuid"), "nope").is_err());
    }

    #[test]
    fn coerce_value_parses_dates() {
        let value = coerce_value(1, &column("date"), "2026-01-02").expect("date parses");
        assert!(value.as_str().unwrap().starts_with("2026-01-02"));
        assert!(coerce_value(1, &column("date"), "not-a-date").is_err());
    }

    #[test]
    fn coerce_value_empty_raw_is_null_for_every_kind() {
        assert_eq!(coerce_value(1, &column("integer"), "").unwrap(), Value::Null);
        assert_eq!(coerce_value(1, &column("boolean"), "").unwrap(), Value::Null);
    }

    #[test]
    fn coerce_value_parses_numbers_and_passes_strings_through() {
        assert_eq!(coerce_value(1, &column("number"), "12.5").unwrap(), serde_json::json!(12.5));
        assert!(coerce_value(1, &column("number"), "x").is_err());
        assert_eq!(coerce_value(1, &column("string"), " hello ").unwrap(), serde_json::json!("hello"));
        assert_eq!(coerce_value(1, &column("unknown-kind"), "raw").unwrap(), serde_json::json!("raw"));
    }

    #[test]
    fn valid_format_checks_the_allowlist() {
        assert!(valid_format("csv", &["csv", "json"]));
        assert!(!valid_format("xml", &["csv", "json"]));
    }

    #[test]
    fn check_column_name_accepts_known_and_rejects_secrets_and_unknown() {
        let spec = entity_spec("users").expect("users spec exists");
        assert!(check_column_name(spec, "email").is_ok());
        assert!(check_column_name(spec, "password").is_err());
        assert!(check_column_name(spec, "nope").is_err());
    }
}
