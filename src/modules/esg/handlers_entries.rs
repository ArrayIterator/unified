//! HTTP handlers for metric values (entries): one recorded
//! value per (metric, period, scope), with source / evidence
//! notes, document references, and a full value history.
//! Recording a second value for the same combination updates
//! the entry and appends a history row instead of creating a
//! duplicate.

use actix_web::{HttpRequest, HttpResponse, web};
use chrono::Utc;
use serde_json::{Value, json};
use std::sync::Arc;
use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::pagination::PaginationParams;
use uuid::Uuid;

use super::helpers::{
    caller, created, err, load_entry, load_metric, load_period, normalize_value, ok, paginate,
    record_audit, require_manage, require_period_open, require_text, require_view, require_tenant, resolve_tenant,
    validate_document_ids, validate_scope,
};
use super::state::EsgState;
use super::types::{
    ACHIEVED_BPS, CreateEntryRequest, Decimal, EVENT_ENTRY_RECORDED, EVENT_ENTRY_UPDATED,
    EVENT_TARGET_ACHIEVED, Entry, EntryHistory, MAX_NOTE_LEN, MetricDataType, UpdateEntryRequest,
    attainment_bps,
};

/// List entries (paginated, filter by metric, period, scope,
/// entered_by; search value / source note).
pub(crate) async fn list_entries(
    state: web::Data<EsgState>,
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let tenant_id = resolve_tenant(&req)?;
        let caller = caller(&req)?;
        require_view(&ctx, &caller.role)?;
        let params = query.into_inner();
        let metric_filter = params.filters.get("metric_id").cloned();
        let period_filter = params.filters.get("period_id").cloned();
        let scope_type_filter = params.filters.get("scope_type").cloned();
        let scope_filter = params.filters.get("scope_id").cloned();
        let entered_by_filter = params.filters.get("entered_by").cloned();
        let search = params.search.clone().map(|needle| needle.to_lowercase());
        let mut items: Vec<Entry> = state
            .entries
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|entry| entry.tenant_id == tenant_id)
            .collect();
        items.retain(|entry| {
            if let Some(metric_id) = &metric_filter {
                if entry.metric_id.to_string() != *metric_id {
                    return false;
                }
            }
            if let Some(period_id) = &period_filter {
                if entry.period_id.to_string() != *period_id {
                    return false;
                }
            }
            if let Some(scope_type) = &scope_type_filter {
                if entry.scope_type != *scope_type {
                    return false;
                }
            }
            if let Some(scope_id) = &scope_filter {
                if entry.scope_id != *scope_id {
                    return false;
                }
            }
            if let Some(entered_by) = &entered_by_filter {
                if entry.entered_by != *entered_by {
                    return false;
                }
            }
            if let Some(needle) = &search {
                if !entry.value_text.to_lowercase().contains(needle)
                    && !entry
                        .source_note
                        .as_deref()
                        .unwrap_or("")
                        .to_lowercase()
                        .contains(needle)
                {
                    return false;
                }
            }
            true
        });
        items.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        let views: Vec<Value> = items
            .iter()
            .map(|item| serde_json::to_value(item).unwrap_or(Value::Null))
            .collect();
        Ok(paginate(views, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

/// Record a metric value (manager). When a value already
/// exists for the same (metric, period, scope) combination,
/// the value is updated instead of duplicated — the update
/// appends a history row and emits `esg.entry.updated`.
pub(crate) async fn create_entry(
    state: web::Data<EsgState>,
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    body: web::Json<CreateEntryRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<(Value, bool)> = (|| {
        let _tenant = require_tenant(&req)?;
        let tenant_id = resolve_tenant(&req)?;
        let caller = caller(&req)?;
        require_manage(&ctx, &caller.role)?;
        let input = body.into_inner();
        let metric = load_metric(&state, &tenant_id, &input.metric_id)?;
        if !metric.active {
            return Err(UnifiedError::bad_request(format!(
                "Metric {} is inactive; entries cannot be recorded for it",
                metric.name
            )));
        }
        let period = load_period(&state, &tenant_id, &input.period_id)?;
        require_period_open(&period)?;
        let (scope_type, scope_id) = validate_scope(&input.scope_type, &input.scope_id)?;
        let value = normalize_value(&metric, &input.value, "value")?;
        let source_note = match &input.source_note {
            Some(note) => Some(require_text(note, "source_note", MAX_NOTE_LEN)?),
            None => None,
        };
        let document_ids = validate_document_ids(&input.document_ids)?;
        let now = Utc::now();
        if let Some(existing) =
            state.entry_for(&tenant_id, metric.id, period.id, &scope_type, &scope_id)
        {
            let (entry, _) = apply_entry_update(
                &state,
                &tenant_id,
                existing,
                value,
                source_note,
                Some(document_ids),
                &caller.actor,
                now,
            )?;
            return Ok((serde_json::to_value(&entry).unwrap_or(Value::Null), false));
        }
        let entry = Entry {
            id: Uuid::new_v4(),
            tenant_id: tenant_id.clone(),
            metric_id: metric.id,
            period_id: period.id,
            scope_type: scope_type.clone(),
            scope_id: scope_id.clone(),
            value_text: value.clone(),
            source_note: source_note.clone(),
            document_ids,
            entered_by: caller.actor.clone(),
            created_at: now,
            updated_at: now,
        };
        state.entries.insert(entry.id, entry.clone());
        append_history(
            &state,
            &tenant_id,
            &entry,
            &value,
            &source_note,
            &caller.actor,
            now,
        );
        record_audit(
            &state,
            &tenant_id,
            "entry",
            &entry.id.to_string(),
            "created",
            &caller.actor,
            None,
            serde_json::to_value(&entry).ok(),
        );
        state.emit(
            EVENT_ENTRY_RECORDED,
            json!({
                "tenant_id": tenant_id,
                "entry_id": entry.id.to_string(),
                "metric_id": metric.id.to_string(),
                "period_id": period.id.to_string(),
                "scope_type": scope_type,
                "scope_id": scope_id,
                "value": value,
                "entered_by": caller.actor,
            }),
        );
        check_target_achieved(&state, &tenant_id, &metric, &entry, None, &caller.actor);
        Ok((serde_json::to_value(&entry).unwrap_or(Value::Null), true))
    })();
    match result {
        Ok((data, is_new)) => {
            if is_new {
                created(data)
            } else {
                ok(data)
            }
        }
        Err(error) => err(error, debug),
    }
}

/// Get one entry.
pub(crate) async fn get_entry(
    state: web::Data<EsgState>,
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let tenant_id = resolve_tenant(&req)?;
        let caller = caller(&req)?;
        require_view(&ctx, &caller.role)?;
        let entry = load_entry(&state, &tenant_id, &path.into_inner())?;
        Ok(serde_json::to_value(&entry).unwrap_or(Value::Null))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

/// Update an entry's value, source note, or document
/// references (manager). Appends a history row and writes an
/// audit row with before / after values. The period must be
/// open.
pub(crate) async fn update_entry(
    state: web::Data<EsgState>,
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    path: web::Path<String>,
    body: web::Json<UpdateEntryRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let tenant_id = resolve_tenant(&req)?;
        let caller = caller(&req)?;
        require_manage(&ctx, &caller.role)?;
        let input = body.into_inner();
        let entry = load_entry(&state, &tenant_id, &path.into_inner())?;
        let period = load_period(&state, &tenant_id, &entry.period_id.to_string())?;
        require_period_open(&period)?;
        // The metric may have been deleted since the entry was
        // recorded; in that case the stored value is kept as
        // written and only notes / references may change.
        let metric = state
            .metrics
            .get(&entry.metric_id)
            .map(|row| row.value().clone())
            .filter(|metric| metric.tenant_id == tenant_id);
        let current_value = entry.value_text.clone();
        let current_note = entry.source_note.clone();
        let value = match (&input.value, &metric) {
            (Some(raw), Some(metric)) => normalize_value(metric, raw, "value")?,
            (Some(raw), None) => require_text(raw, "value", MAX_NOTE_LEN)?,
            (None, _) => current_value,
        };
        let source_note = match &input.source_note {
            Some(note) => match note {
                Some(text) => Some(require_text(text, "source_note", MAX_NOTE_LEN)?),
                None => None,
            },
            None => current_note,
        };
        let document_ids = match &input.document_ids {
            Some(ids) => Some(validate_document_ids(ids)?),
            None => None,
        };
        let now = Utc::now();
        let (entry, _) = apply_entry_update(
            &state,
            &tenant_id,
            entry,
            value,
            source_note,
            document_ids,
            &caller.actor,
            now,
        )?;
        Ok(serde_json::to_value(&entry).unwrap_or(Value::Null))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

/// Delete an entry (manager). The value history and audit
/// rows are kept: the reporting history stays auditable after
/// the entry is gone. The period must be open.
pub(crate) async fn delete_entry(
    state: web::Data<EsgState>,
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let tenant_id = resolve_tenant(&req)?;
        let caller = caller(&req)?;
        require_manage(&ctx, &caller.role)?;
        let entry = load_entry(&state, &tenant_id, &path.into_inner())?;
        let period = load_period(&state, &tenant_id, &entry.period_id.to_string())?;
        require_period_open(&period)?;
        state.entries.remove(&entry.id);
        record_audit(
            &state,
            &tenant_id,
            "entry",
            &entry.id.to_string(),
            "deleted",
            &caller.actor,
            serde_json::to_value(&entry).ok(),
            None,
        );
        Ok(json!({ "deleted": true, "entry_id": entry.id.to_string() }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

/// List the value history of one entry, oldest first
/// (paginated).
pub(crate) async fn list_entry_history(
    state: web::Data<EsgState>,
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let tenant_id = resolve_tenant(&req)?;
        let caller = caller(&req)?;
        require_view(&ctx, &caller.role)?;
        let entry = load_entry(&state, &tenant_id, &path.into_inner())?;
        let params = query.into_inner();
        let items = state.history_of(entry.id);
        let views: Vec<Value> = items
            .iter()
            .map(|item| serde_json::to_value(item).unwrap_or(Value::Null))
            .collect();
        Ok(paginate(views, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

// ---------------------------------------------------------------------------
// Shared write path
// ---------------------------------------------------------------------------

/// Append one value-history row for an entry.
fn append_history(
    state: &EsgState,
    tenant_id: &str,
    entry: &Entry,
    value: &str,
    source_note: &Option<String>,
    actor: &str,
    now: chrono::DateTime<Utc>,
) {
    let row = EntryHistory {
        id: Uuid::new_v4(),
        tenant_id: tenant_id.to_string(),
        entry_id: entry.id,
        metric_id: entry.metric_id,
        period_id: entry.period_id,
        value_text: value.to_string(),
        source_note: source_note.clone(),
        entered_by: actor.to_string(),
        created_at: now,
    };
    state.entry_history.insert(row.id, row);
}

/// Apply a value update to an existing entry: store the new
/// value, append a history row, write an audit row, emit
/// `esg.entry.updated`, and check target achievement.
/// Returns the updated entry plus the previous value.
fn apply_entry_update(
    state: &EsgState,
    tenant_id: &str,
    entry: Entry,
    value: String,
    source_note: Option<String>,
    document_ids: Option<Vec<String>>,
    actor: &str,
    now: chrono::DateTime<Utc>,
) -> UnifiedResult<(Entry, Option<String>)> {
    let previous_value = entry.value_text.clone();
    let before = serde_json::to_value(&entry).ok();
    let mut updated = entry.clone();
    updated.value_text = value.clone();
    updated.source_note = source_note.clone();
    if let Some(ids) = document_ids {
        updated.document_ids = ids;
    }
    updated.entered_by = actor.to_string();
    updated.updated_at = now;
    state.entries.insert(updated.id, updated.clone());
    append_history(state, tenant_id, &updated, &value, &source_note, actor, now);
    record_audit(
        state,
        tenant_id,
        "entry",
        &updated.id.to_string(),
        "updated",
        actor,
        before,
        serde_json::to_value(&updated).ok(),
    );
    state.emit(
        EVENT_ENTRY_UPDATED,
        json!({
            "tenant_id": tenant_id,
            "entry_id": updated.id.to_string(),
            "metric_id": updated.metric_id.to_string(),
            "period_id": updated.period_id.to_string(),
            "scope_type": updated.scope_type,
            "scope_id": updated.scope_id,
            "value": value,
            "previous_value": previous_value,
            "entered_by": actor,
        }),
    );
    if let Some(metric) = state
        .metrics
        .get(&updated.metric_id)
        .map(|row| row.value().clone())
        .filter(|metric| metric.tenant_id == tenant_id)
    {
        check_target_achieved(
            state,
            tenant_id,
            &metric,
            &updated,
            Some(previous_value.clone()),
            actor,
        );
    }
    Ok((updated, Some(previous_value)))
}

/// Check whether recording this value achieved the target
/// for the entry's (metric, period, scope). Emits
/// `esg.target.achieved` only on the crossing: the new
/// attainment is at 100% or more while the previous one was
/// below it (or there was no previous value). Boolean metrics
/// achieve when the recorded value equals the target value;
/// text metrics have no attainment and never achieve.
fn check_target_achieved(
    state: &EsgState,
    tenant_id: &str,
    metric: &super::types::Metric,
    entry: &Entry,
    previous_value: Option<String>,
    actor: &str,
) {
    let Some(target) = state.target_for(
        tenant_id,
        entry.metric_id,
        entry.period_id,
        &entry.scope_type,
        &entry.scope_id,
    ) else {
        return;
    };
    let achieved_now = match metric.data_type {
        MetricDataType::Boolean => entry.value_text == target.target_value,
        MetricDataType::Number => {
            let actual = Decimal::parse(&entry.value_text);
            let target_value = Decimal::parse(&target.target_value);
            match (actual, target_value) {
                (Some(actual), Some(target_value)) => {
                    attainment_bps(&actual, &target_value, target.direction)
                        .map(|bps| bps >= ACHIEVED_BPS)
                        .unwrap_or(false)
                }
                _ => false,
            }
        }
        MetricDataType::Text => false,
    };
    if !achieved_now {
        return;
    }
    let achieved_before = match (metric.data_type, previous_value) {
        (MetricDataType::Boolean, Some(previous)) => previous == target.target_value,
        (MetricDataType::Number, Some(previous)) => {
            let actual = Decimal::parse(&previous);
            let target_value = Decimal::parse(&target.target_value);
            match (actual, target_value) {
                (Some(actual), Some(target_value)) => {
                    attainment_bps(&actual, &target_value, target.direction)
                        .map(|bps| bps >= ACHIEVED_BPS)
                        .unwrap_or(false)
                }
                _ => false,
            }
        }
        _ => false,
    };
    if achieved_before {
        return;
    }
    state.emit(
        EVENT_TARGET_ACHIEVED,
        json!({
            "tenant_id": tenant_id,
            "target_id": target.id.to_string(),
            "metric_id": entry.metric_id.to_string(),
            "period_id": entry.period_id.to_string(),
            "scope_type": entry.scope_type,
            "scope_id": entry.scope_id,
            "target_value": target.target_value,
            "actual_value": entry.value_text,
            "entered_by": actor,
        }),
    );
}
