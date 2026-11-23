//! import export - export.

use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::HookBus;
use unified_core::pagination::{PageResult, PaginationParams};
use crate::modules::privacy::redact_value;
use base64::Engine;
use chrono::Utc;
use serde::Serialize;
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use uuid::Uuid;
use super::types::*;
use super::events::*;
use super::auth::*;
use super::entities::*;
use super::store::*;
use super::jobs::*;
use super::formats::*;
use super::validation::*;

pub(crate) fn paginate<T: Serialize + Clone>(items: Vec<T>, params: &PaginationParams) -> PageResult<T> {
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

pub(crate) fn decode_content(content: &str, encoding: Option<&str>, max_bytes: usize) -> UnifiedResult<String> {
    let bytes = match encoding.unwrap_or("utf8") {
        "base64" => base64::engine::general_purpose::STANDARD
            .decode(content.trim())
            .map_err(|_| UnifiedError::bad_request("content is not valid base64"))?,
        "utf8" => content.to_string().into_bytes(),
        other => {
            return Err(UnifiedError::bad_request(format!(
                "encoding must be \"utf8\" or \"base64\", got \"{other}\""
            )))
        }
    };
    if bytes.len() > max_bytes {
        return Err(UnifiedError::bad_request(format!(
            "Content is too large: maximum is {max_bytes} bytes"
        )));
    }
    String::from_utf8(bytes)
        .map_err(|_| UnifiedError::bad_request("content is not valid UTF-8 text"))
}

/// Resolve the effective rename map for an import: a saved template
/// (tenant-scoped, same entity) merged under a per-request map.
pub(crate) fn resolve_mapping(
    entity: &str,
    mapping: Option<HashMap<String, String>>,
    mapping_id: Option<Uuid>,
    caller_company: Option<Uuid>,
    can_manage: bool,
) -> UnifiedResult<HashMap<String, String>> {
    let mut resolved: HashMap<String, String> = HashMap::new();
    if let Some(id) = mapping_id {
        let template = mappings_store()
            .get(&id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Mapping template not found"))?;
        if !can_view_company(caller_company, template.company_id, can_manage) {
            return Err(UnifiedError::not_found("Mapping template not found"));
        }
        if template.entity != entity {
            return Err(UnifiedError::bad_request(format!(
                "Mapping template is for entity \"{}\" but the import is for \"{entity}\"",
                template.entity
            )));
        }
        resolved = template.mapping;
    }
    if let Some(request_mapping) = mapping {
        for (source, target) in request_mapping {
            resolved.insert(source, target);
        }
    }
    Ok(resolved)
}

/// Turn submitted content into raw rows: each row maps entity column
/// names to raw strings. Applies the optional rename mapping first.
pub(crate) fn content_to_raw_rows(
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
                check_column_name(spec, name).map_err(UnifiedError::bad_request)?;
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
                    check_column_name(spec, &name).map_err(UnifiedError::bad_request)?;
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
        "xml" => {
            let root = parse_xml_document(text)
                .map_err(|error| UnifiedError::bad_request(format!("Invalid XML: {error}")))?;
            if !XML_ROOT_ELEMENTS.contains(&root.name.as_str()) {
                return Err(UnifiedError::bad_request(format!(
                    "XML root element must be one of {}, got \"{}\"",
                    XML_ROOT_ELEMENTS.join(", "),
                    root.name
                )));
            }
            if !root.text.is_empty() {
                return Err(UnifiedError::bad_request(
                    "XML root element must only contain row elements",
                ));
            }
            let mut raw_rows = Vec::new();
            for (index, row_element) in root.children.iter().enumerate() {
                if !XML_ROW_ELEMENTS.contains(&row_element.name.as_str()) {
                    return Err(UnifiedError::bad_request(format!(
                        "XML row {} uses element \"{}\"; expected one of {}",
                        index + 1,
                        row_element.name,
                        XML_ROW_ELEMENTS.join(", ")
                    )));
                }
                let mut map = HashMap::new();
                for column_element in &row_element.children {
                    if !column_element.children.is_empty() {
                        return Err(UnifiedError::bad_request(format!(
                            "XML row {} column \"{}\" must be flat text, not nested elements",
                            index + 1,
                            column_element.name
                        )));
                    }
                    let name = apply_mapping(&column_element.name);
                    check_column_name(spec, &name).map_err(UnifiedError::bad_request)?;
                    if map.contains_key(&name) {
                        return Err(UnifiedError::bad_request(format!(
                            "XML row {} repeats column \"{name}\"",
                            index + 1
                        )));
                    }
                    map.insert(name, column_element.text.clone());
                }
                raw_rows.push(map);
            }
            Ok(raw_rows)
        }
        other => Err(UnifiedError::bad_request(format!(
            "format must be one of csv, json, or xml, got \"{other}\""
        ))),
    }
}

pub(crate) struct ExportSpec {
    pub(crate) entity: String,
    pub(crate) format: String,
    pub(crate) fields: Option<Vec<String>>,
    pub(crate) filters: HashMap<String, String>,
    pub(crate) redact_pii: Option<bool>,
    pub(crate) redact_names: Option<bool>,
    pub(crate) limit: Option<usize>,
    pub(crate) offset: Option<usize>,
}

/// Stable identity of one exported row for dedupe: the row's
/// `external_id` (or `id`) when present, otherwise the canonical
/// JSON encoding of the whole row. Two providers answering the
/// same row therefore collapse to one copy.
pub(crate) fn export_row_key(row: &Value) -> String {
    if let Some(object) = row.as_object() {
        for key in ["external_id", "id"] {
            if let Some(text) = object.get(key).and_then(Value::as_str) {
                if !text.is_empty() {
                    return format!("{key}:{text}");
                }
            }
        }
    }
    serde_json::to_string(row).unwrap_or_default()
}

/// Collect rows from hook results: the first provider that answers
/// with a non-empty row list wins; later providers are ignored, so
/// the same rows are never concatenated twice. Rows are deduped by
/// `export_row_key` and capped at `fetch_limit`.
pub(crate) fn collect_provider_rows(results: &[Value], fetch_limit: usize) -> Vec<Value> {
    let mut rows: Vec<Value> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    for result in results {
        let contributed: Option<&Vec<Value>> = result
            .get("rows")
            .and_then(Value::as_array)
            .or_else(|| result.as_array());
        let Some(list) = contributed else {
            continue;
        };
        if list.is_empty() {
            continue;
        }
        for row in list {
            if rows.len() >= fetch_limit {
                break;
            }
            if seen.insert(export_row_key(row)) {
                rows.push(row.clone());
            }
        }
        // First non-empty provider wins; ignore later providers.
        break;
    }
    rows
}

/// Apply the tenant scope to already-collected export rows.
/// Returns the kept rows and how many were dropped. When the
/// caller belongs to a company, a row is kept only when it carries
/// a `company_id` tag equal to that company: rows tagged with
/// another company are dropped, and so are rows with no usable
/// tag at all. An untagged row must never leak across tenants on
/// the assumption that its provider scoped server-side; providers
/// must tag every row they contribute. With no caller company
/// (platform scope) every row is kept.
pub(crate) fn scope_export_rows(rows: Vec<Value>, company_id: Option<Uuid>) -> (Vec<Value>, usize) {
    let Some(company) = company_id else {
        return (rows, 0);
    };
    let mut excluded = 0usize;
    let kept: Vec<Value> = rows
        .into_iter()
        .filter(|row| {
            let tagged = row
                .get("company_id")
                .and_then(Value::as_str)
                .and_then(|raw| Uuid::parse_str(raw).ok());
            match tagged {
                Some(tagged) if tagged == company => true,
                _ => {
                    excluded += 1;
                    false
                }
            }
        })
        .collect();
    (kept, excluded)
}

/// Build one export job: collect rows from owning modules through
/// the hook bus, scope them to the caller's company, redact personal
/// data through the privacy module, serialize, store, and audit.
/// Unredacted personal data requires `can_export_pii`; anything else
/// forces redaction.
pub(crate) fn run_export(
    actor: &str,
    company_id: Option<Uuid>,
    can_export_pii: bool,
    spec: &ExportSpec,
) -> UnifiedResult<ExportJob> {
    let config = settings();
    let entity = spec.entity.trim();
    let entity_spec = entity_spec(entity).ok_or_else(|| {
        UnifiedError::bad_request(format!(
            "Unknown entity \"{entity}\"; see GET /api/v1/import-export/entities"
        ))
    })?;
    let format = spec.format.trim().to_lowercase();
    if !valid_format(&format, &config.allowed_formats) {
        return Err(UnifiedError::bad_request(format!(
            "format must be one of {}, got \"{format}\"",
            config.allowed_formats.join(", ")
        )));
    }
    let fields: Vec<String> = match &spec.fields {
        Some(fields) => {
            validate_fields(entity_spec, fields)?;
            fields.clone()
        }
        None => entity_spec
            .columns
            .iter()
            .map(|column| column.name.to_string())
            .collect(),
    };
    for key in spec.filters.keys() {
        if !entity_spec.columns.iter().any(|column| column.name == key) {
            return Err(UnifiedError::bad_request(format!(
                "Unknown filter \"{key}\" for entity \"{entity}\""
            )));
        }
    }
    let limit = spec.limit.unwrap_or(config.max_export_rows);
    if limit == 0 || limit > config.max_export_rows {
        return Err(UnifiedError::bad_request(format!(
            "limit must be between 1 and {}",
            config.max_export_rows
        )));
    }
    let offset = spec.offset.unwrap_or(0);
    // Ask owning modules for rows through the hook bus, asking for
    // one row more than the requested window: whether that extra
    // row arrives is the only reliable `has_more` signal (see
    // below). Each handler contributes `{"rows": [...]}` or a bare
    // array.
    let fetch_limit = limit.saturating_add(1);
    let mut rows: Vec<Value> = Vec::new();
    let mut provider = "none".to_string();
    if let Some(context) = emit(
        import_export_events::EXPORT_REQUESTED,
        serde_json::json!({
            "entity": entity_spec.entity,
            "fields": fields,
            "filters": spec.filters,
            "limit": fetch_limit,
            "offset": offset,
            "company_id": company_id.map(|id| id.to_string()),
            "actor": actor,
        }),
    ) {
        rows = collect_provider_rows(&context.results, fetch_limit);
        if !rows.is_empty() {
            provider = "hook".to_string();
        }
    }
    // Tenant scope: keep only rows tagged with the caller's
    // company. Rows tagged with another company are dropped, and
    // so are rows with no usable `company_id` tag at all: an
    // untagged provider row must never leak across tenants on the
    // assumption that its provider scoped server-side. With no
    // caller company (platform scope) every row is kept.
    let (rows, rows_excluded_by_tenant_scope) = scope_export_rows(rows, company_id);
    // `has_more` is decided on the scoped, deduped rows: true only
    // when a row beyond the requested window survived scoping. A
    // window that happens to hold exactly `limit` rows no longer
    // claims more rows exist.
    let has_more = rows.len() > limit;
    let rows: Vec<Value> = rows.into_iter().take(limit).collect();
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
    // PII redaction through the privacy module (reused, never
    // duplicated here). Forced on when the caller may not export
    // personal data unredacted, or when the module default says so.
    let requested_redact = spec.redact_pii.unwrap_or(config.default_redact_pii);
    let redact = requested_redact || !can_export_pii;
    let redact_names = spec.redact_names.unwrap_or(false);
    let redacted_fields: Vec<String> = if redact {
        entity_spec
            .pii_columns
            .iter()
            .filter(|column| fields.iter().any(|field| field == *column))
            .map(|column| column.to_string())
            .collect()
    } else {
        Vec::new()
    };
    if redact {
        for row in &mut trimmed {
            *row = redact_value(row, redact_names);
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
    } else if format == "xml" {
        let mut text = format!(
            "<export entity=\"{}\" generated_at=\"{}\" row_count=\"{}\">",
            escape_xml_text(entity_spec.entity),
            Utc::now().to_rfc3339(),
            trimmed.len()
        );
        for row in &trimmed {
            text.push_str("<row>");
            for field in &fields {
                let cell = value_to_xml_cell(row.get(field).unwrap_or(&Value::Null));
                text.push_str(&format!(
                    "<{field}>{}</{field}>",
                    escape_xml_text(&cell)
                ));
            }
            text.push_str("</row>");
        }
        text.push_str("</export>");
        text.into_bytes()
    } else {
        serde_json::to_vec_pretty(&trimmed)
            .map_err(|error| UnifiedError::internal(format!("Cannot serialize export: {error}")))?
    };
    if content.len() > config.max_export_bytes {
        return Err(UnifiedError::internal(
            "Export is too large to hold in memory; narrow it with filters or field selection",
        ));
    }
    let now = Utc::now();
    let id = Uuid::now_v7();
    let job = ExportJob {
        id,
        entity: entity_spec.entity.to_string(),
        format,
        status: ExportStatus::Completed.as_str().to_string(),
        fields,
        row_count: trimmed.len(),
        data_provider: provider.clone(),
        download_url: format!("/api/v1/import-export/exports/{id}/download"),
        error: None,
        pii_redacted: redact,
        redacted_fields: redacted_fields.clone(),
        limit,
        offset,
        has_more,
        rows_excluded_by_tenant_scope,
        actor: actor.to_string(),
        company_id,
        created_at: now,
        updated_at: now,
        content,
    };
    let view = job.clone();
    export_jobs_store().insert(id, job);
    let resource = format!("export:{id}");
    emit_audit(
        import_export_events::EXPORT_CREATED,
        actor,
        company_id,
        &resource,
        serde_json::json!({
            "entity": entity_spec.entity,
            "format": view.format,
            "row_count": view.row_count,
            "pii_redacted": redact,
            "limit": limit,
            "offset": offset,
        }),
    );
    if redact && !redacted_fields.is_empty() {
        emit_audit(
            import_export_events::EXPORT_PII_REDACTED,
            actor,
            company_id,
            &resource,
            serde_json::json!({
                "entity": entity_spec.entity,
                "redacted_fields": redacted_fields,
                "redact_names": redact_names,
            }),
        );
    }
    emit(
        import_export_events::EXPORT_COMPLETED,
        serde_json::json!({
            "export_id": id,
            "entity": entity_spec.entity,
            "row_count": view.row_count,
            "data_provider": view.data_provider.clone(),
            "actor": actor,
            "actor_id": actor,
            "resource": resource,
            "company_id": company_id.map(|id| id.to_string()),
            "pii_redacted": redact,
        }),
    );
    Ok(view)
}

/// Offer a schedule to the scheduler module through
/// `scheduler.enqueue` with kind `export.run`, the way the backup
/// module offers its jobs. A refusal is recorded on the schedule
/// instead of failing the request, because run-now keeps working.
pub(crate) fn notify_scheduler(hooks: &HookBus, schedule_id: Uuid) -> SchedulingOutcome {
    let context = hooks.emit(
        SCHEDULER_ENQUEUE,
        serde_json::json!({
            "kind": EXPORT_RUN_KIND,
            "payload": { "export_schedule_id": schedule_id, "job_id": schedule_id },
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
                .unwrap_or("Scheduler refused the export schedule.");
            return SchedulingOutcome {
                enqueued: false,
                message: format!(
                    "Scheduler did not accept the schedule ({detail}). Run-now still works; the scheduler needs an \"export.run\" dispatch arm."
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

/// Run one stored schedule through the shared export runner and
/// record the outcome on the schedule. Returns the new export job.
pub(crate) fn run_schedule(schedule: &ExportSchedule) -> UnifiedResult<ExportJob> {
    let spec = ExportSpec {
        entity: schedule.entity.clone(),
        format: schedule.format.clone(),
        fields: Some(schedule.fields.clone()),
        filters: schedule.filters.clone(),
        redact_pii: Some(schedule.redact_pii),
        redact_names: Some(schedule.redact_names),
        limit: None,
        offset: None,
    };
    let job = run_export(
        &schedule.actor,
        schedule.company_id,
        schedule.pii_allowed,
        &spec,
    )?;
    let now = Utc::now();
    if let Some(mut stored) = export_schedules_store().get_mut(&schedule.id) {
        stored.last_run_at = Some(now);
        stored.last_export_id = Some(job.id);
        stored.last_error = None;
        stored.run_count += 1;
        stored.updated_at = now;
    }
    emit_audit(
        import_export_events::EXPORT_SCHEDULE_RUN,
        &schedule.actor,
        schedule.company_id,
        &format!("export_schedule:{}", schedule.id),
        serde_json::json!({
            "entity": schedule.entity,
            "export_id": job.id,
            "row_count": job.row_count,
        }),
    );
    Ok(job)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(external_id: &str, company: Option<Uuid>) -> Value {
        let mut object = serde_json::Map::new();
        object.insert("external_id".to_string(), Value::String(external_id.to_string()));
        object.insert("name".to_string(), Value::String(format!("row {external_id}")));
        if let Some(company) = company {
            object.insert("company_id".to_string(), Value::String(company.to_string()));
        }
        Value::Object(object)
    }

    fn provider_result(rows: Vec<Value>) -> Value {
        serde_json::json!({ "rows": rows })
    }

    #[test]
    fn collect_provider_rows_takes_the_first_non_empty_provider_only() {
        let first = provider_result(vec![row("a", None), row("b", None)]);
        let second = provider_result(vec![row("c", None), row("d", None)]);
        let rows = collect_provider_rows(&[first, second], 10);
        let ids: Vec<&str> = rows
            .iter()
            .filter_map(|row| row.get("external_id").and_then(Value::as_str))
            .collect();
        assert_eq!(ids, vec!["a", "b"]);
    }

    #[test]
    fn collect_provider_rows_skips_empty_providers_and_bare_arrays() {
        let empty = provider_result(vec![]);
        let bare = serde_json::json!([row("x", None)]);
        let rows = collect_provider_rows(&[empty, bare], 10);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].get("external_id").and_then(Value::as_str), Some("x"));
    }

    #[test]
    fn collect_provider_rows_dedupes_by_external_id_and_caps_at_fetch_limit() {
        let result = provider_result(vec![
            row("a", None),
            row("a", None),
            row("b", None),
            row("c", None),
        ]);
        let rows = collect_provider_rows(&[result], 2);
        let ids: Vec<&str> = rows
            .iter()
            .filter_map(|row| row.get("external_id").and_then(Value::as_str))
            .collect();
        assert_eq!(ids, vec!["a", "b"]);
    }

    #[test]
    fn collect_provider_rows_dedupes_identical_rows_without_an_id() {
        let plain = serde_json::json!({ "name": "same", "total": 5 });
        let result = provider_result(vec![plain.clone(), plain, serde_json::json!({ "name": "other" })]);
        let rows = collect_provider_rows(&[result], 10);
        assert_eq!(rows.len(), 2);
    }

    #[test]
    fn scope_export_rows_keeps_only_matching_tags() {
        let own = Uuid::now_v7();
        let other = Uuid::now_v7();
        let rows = vec![row("a", Some(own)), row("b", Some(other)), row("c", None)];
        let (kept, excluded) = scope_export_rows(rows, Some(own));
        assert_eq!(kept.len(), 1);
        assert_eq!(kept[0].get("external_id").and_then(Value::as_str), Some("a"));
        assert_eq!(excluded, 2);
    }

    #[test]
    fn scope_export_rows_drops_untagged_rows_only_under_a_tenant_scope() {
        let own = Uuid::now_v7();
        let untagged = vec![row("a", None)];
        let (kept, excluded) = scope_export_rows(untagged, Some(own));
        assert!(kept.is_empty());
        assert_eq!(excluded, 1);

        let (kept, excluded) = scope_export_rows(vec![row("a", None)], None);
        assert_eq!(kept.len(), 1);
        assert_eq!(excluded, 0);
    }

    #[test]
    fn scope_export_rows_drops_unparseable_company_tags() {
        let own = Uuid::now_v7();
        let mut bad = serde_json::Map::new();
        bad.insert("external_id".to_string(), Value::String("a".to_string()));
        bad.insert("company_id".to_string(), Value::String("not-a-uuid".to_string()));
        let (kept, excluded) = scope_export_rows(vec![Value::Object(bad)], Some(own));
        assert!(kept.is_empty());
        assert_eq!(excluded, 1);
    }
}
