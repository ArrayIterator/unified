//! HTTP handlers for targets: one per-metric, per-period,
//! per-scope target value with a direction and a deadline.
//! Setting a target again replaces it and writes an audit
//! row instead of creating a duplicate.

use actix_web::{HttpRequest, HttpResponse, web};
use chrono::Utc;
use serde_json::{Value, json};
use std::sync::Arc;
use unified_core::context::AppContext;
use unified_core::error::UnifiedResult;
use unified_core::pagination::PaginationParams;
use uuid::Uuid;

use super::helpers::{
    caller, created, err, load_metric, load_period, load_target, normalize_value, ok, paginate,
    record_audit, require_manage, require_period_open, require_view, require_tenant, resolve_tenant,
    validate_deadline, validate_direction, validate_scope,
};
use super::state::EsgState;
use super::types::{CreateTargetRequest, EVENT_TARGET_SET, Target, UpdateTargetRequest};

/// List targets (paginated, filter by metric, period, scope,
/// direction).
pub(crate) async fn list_targets(
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
        let direction_filter = params.filters.get("direction").cloned();
        let mut items: Vec<Target> = state
            .targets
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|target| target.tenant_id == tenant_id)
            .collect();
        items.retain(|target| {
            if let Some(metric_id) = &metric_filter {
                if target.metric_id.to_string() != *metric_id {
                    return false;
                }
            }
            if let Some(period_id) = &period_filter {
                if target.period_id.to_string() != *period_id {
                    return false;
                }
            }
            if let Some(scope_type) = &scope_type_filter {
                if target.scope_type != *scope_type {
                    return false;
                }
            }
            if let Some(scope_id) = &scope_filter {
                if target.scope_id != *scope_id {
                    return false;
                }
            }
            if let Some(direction) = &direction_filter {
                if target.direction.as_str() != *direction {
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

/// Set a target (manager). When a target already exists for
/// the same (metric, period, scope) combination, it is
/// replaced: the old row is kept in the audit trail and the
/// new row takes its place.
pub(crate) async fn create_target(
    state: web::Data<EsgState>,
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    body: web::Json<CreateTargetRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<(Value, bool)> = (|| {
        let _tenant = require_tenant(&req)?;
        let tenant_id = resolve_tenant(&req)?;
        let caller = caller(&req)?;
        require_manage(&ctx, &caller.role)?;
        let input = body.into_inner();
        let metric = load_metric(&state, &tenant_id, &input.metric_id)?;
        let period = load_period(&state, &tenant_id, &input.period_id)?;
        require_period_open(&period)?;
        let (scope_type, scope_id) = validate_scope(&input.scope_type, &input.scope_id)?;
        let target_value = normalize_value(&metric, &input.target_value, "target_value")?;
        let direction = validate_direction(&input.direction)?;
        let deadline = validate_deadline(&input.deadline)?;
        let now = Utc::now();
        if let Some(existing) =
            state.target_for(&tenant_id, metric.id, period.id, &scope_type, &scope_id)
        {
            let before = serde_json::to_value(&existing).ok();
            let mut updated = existing.clone();
            updated.target_value = target_value.clone();
            updated.direction = direction;
            updated.deadline = deadline;
            updated.created_by = caller.actor.clone();
            updated.updated_at = now;
            state.targets.insert(updated.id, updated.clone());
            record_audit(
                &state,
                &tenant_id,
                "target",
                &updated.id.to_string(),
                "updated",
                &caller.actor,
                before,
                serde_json::to_value(&updated).ok(),
            );
            state.emit(
                EVENT_TARGET_SET,
                json!({
                    "tenant_id": tenant_id,
                    "target_id": updated.id.to_string(),
                    "metric_id": metric.id.to_string(),
                    "period_id": period.id.to_string(),
                    "scope_type": scope_type,
                    "scope_id": scope_id,
                    "target_value": target_value,
                    "direction": direction.as_str(),
                    "deadline": deadline.format("%Y-%m-%d").to_string(),
                    "created_by": caller.actor,
                }),
            );
            return Ok((serde_json::to_value(&updated).unwrap_or(Value::Null), false));
        }
        let target = Target {
            id: Uuid::new_v4(),
            tenant_id: tenant_id.clone(),
            metric_id: metric.id,
            period_id: period.id,
            scope_type: scope_type.clone(),
            scope_id: scope_id.clone(),
            target_value: target_value.clone(),
            direction,
            deadline,
            created_by: caller.actor.clone(),
            created_at: now,
            updated_at: now,
        };
        state.targets.insert(target.id, target.clone());
        record_audit(
            &state,
            &tenant_id,
            "target",
            &target.id.to_string(),
            "created",
            &caller.actor,
            None,
            serde_json::to_value(&target).ok(),
        );
        state.emit(
            EVENT_TARGET_SET,
            json!({
                "tenant_id": tenant_id,
                "target_id": target.id.to_string(),
                "metric_id": metric.id.to_string(),
                "period_id": period.id.to_string(),
                "scope_type": scope_type,
                "scope_id": scope_id,
                "target_value": target_value,
                "direction": direction.as_str(),
                "deadline": deadline.format("%Y-%m-%d").to_string(),
                "created_by": caller.actor,
            }),
        );
        Ok((serde_json::to_value(&target).unwrap_or(Value::Null), true))
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

/// Get one target.
pub(crate) async fn get_target(
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
        let target = load_target(&state, &tenant_id, &path.into_inner())?;
        Ok(serde_json::to_value(&target).unwrap_or(Value::Null))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

/// Update a target's value, direction, or deadline
/// (manager). The period must be open. Writes an audit row
/// with before / after values.
pub(crate) async fn update_target(
    state: web::Data<EsgState>,
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    path: web::Path<String>,
    body: web::Json<UpdateTargetRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let tenant_id = resolve_tenant(&req)?;
        let caller = caller(&req)?;
        require_manage(&ctx, &caller.role)?;
        let input = body.into_inner();
        let target = load_target(&state, &tenant_id, &path.into_inner())?;
        let period = load_period(&state, &tenant_id, &target.period_id.to_string())?;
        require_period_open(&period)?;
        // The metric may have been deleted since the target was
        // set; in that case the target value is stored as
        // written and only direction / deadline may change.
        let metric = state
            .metrics
            .get(&target.metric_id)
            .map(|row| row.value().clone())
            .filter(|metric| metric.tenant_id == tenant_id);
        let before = serde_json::to_value(&target).ok();
        let mut updated = target.clone();
        if let Some(raw) = &input.target_value {
            updated.target_value = match &metric {
                Some(metric) => normalize_value(metric, raw, "target_value")?,
                None => {
                    super::helpers::require_text(raw, "target_value", super::types::MAX_NOTE_LEN)?
                }
            };
        }
        if let Some(raw) = &input.direction {
            updated.direction = validate_direction(raw)?;
        }
        if let Some(raw) = &input.deadline {
            updated.deadline = validate_deadline(raw)?;
        }
        updated.updated_at = Utc::now();
        state.targets.insert(updated.id, updated.clone());
        record_audit(
            &state,
            &tenant_id,
            "target",
            &updated.id.to_string(),
            "updated",
            &caller.actor,
            before,
            serde_json::to_value(&updated).ok(),
        );
        Ok(serde_json::to_value(&updated).unwrap_or(Value::Null))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

/// Delete a target (manager). The audit rows are kept. The
/// period must be open.
pub(crate) async fn delete_target(
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
        let target = load_target(&state, &tenant_id, &path.into_inner())?;
        let period = load_period(&state, &tenant_id, &target.period_id.to_string())?;
        require_period_open(&period)?;
        state.targets.remove(&target.id);
        record_audit(
            &state,
            &tenant_id,
            "target",
            &target.id.to_string(),
            "deleted",
            &caller.actor,
            serde_json::to_value(&target).ok(),
            None,
        );
        Ok(json!({ "deleted": true, "target_id": target.id.to_string() }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}
