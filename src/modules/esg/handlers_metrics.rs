//! HTTP handlers for metric definitions: admin-defined
//! sustainability metrics (name, category, unit, data type,
//! reporting frequency, framework tags).

use actix_web::{HttpRequest, HttpResponse, web};
use chrono::Utc;
use serde_json::{Value, json};
use std::sync::Arc;
use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::pagination::PaginationParams;
use uuid::Uuid;

use super::helpers::{
    caller, created, err, load_metric, ok, paginate, require_manage, require_text, require_view,
    require_tenant, resolve_tenant, validate_framework_ids, validate_metric_input,
};
use super::state::EsgState;
use super::types::{
    Category, CreateMetricRequest, EVENT_METRIC_DEFINED, MAX_DESCRIPTION_LEN, MAX_FREQUENCY_LEN,
    MAX_NAME_LEN, MAX_UNIT_LEN, Metric, MetricDataType, UpdateMetricRequest,
};

/// List metrics (paginated, filter by category, framework,
/// data type, active; search by name/description/unit).
pub(crate) async fn list_metrics(
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
        let category_filter = params
            .filters
            .get("category")
            .and_then(|raw| Category::from_str(raw));
        let framework_filter = params.filters.get("framework").cloned();
        let data_type_filter = params
            .filters
            .get("data_type")
            .and_then(|raw| MetricDataType::from_str(raw));
        let active_filter = params
            .filters
            .get("active")
            .and_then(|raw| match raw.as_str() {
                "true" => Some(true),
                "false" => Some(false),
                _ => None,
            });
        let search = params.search.clone().map(|needle| needle.to_lowercase());
        let mut items = state.metrics_of(&tenant_id);
        items.retain(|metric| {
            if let Some(category) = category_filter {
                if metric.category != category {
                    return false;
                }
            }
            if let Some(framework) = &framework_filter {
                if !metric.framework_ids.iter().any(|id| id == framework) {
                    return false;
                }
            }
            if let Some(data_type) = data_type_filter {
                if metric.data_type != data_type {
                    return false;
                }
            }
            if let Some(active) = active_filter {
                if metric.active != active {
                    return false;
                }
            }
            if let Some(needle) = &search {
                if !metric.name.to_lowercase().contains(needle)
                    && !metric
                        .description
                        .as_deref()
                        .unwrap_or("")
                        .to_lowercase()
                        .contains(needle)
                    && !metric.unit.to_lowercase().contains(needle)
                {
                    return false;
                }
            }
            true
        });
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

/// Create a metric definition (manager).
pub(crate) async fn create_metric(
    state: web::Data<EsgState>,
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    body: web::Json<CreateMetricRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let tenant_id = resolve_tenant(&req)?;
        let caller = caller(&req)?;
        require_manage(&ctx, &caller.role)?;
        let input = body.into_inner();
        let (name, description, category, unit, data_type, frequency, framework_ids) =
            validate_metric_input(
                &input.name,
                &input.description,
                &input.category,
                &input.unit,
                &input.data_type,
                &input.frequency,
                &input.framework_ids,
            )?;
        let now = Utc::now();
        let metric = Metric {
            id: Uuid::new_v4(),
            tenant_id: tenant_id.clone(),
            name,
            description,
            category,
            unit,
            data_type,
            frequency,
            framework_ids,
            active: true,
            created_by: caller.actor.clone(),
            created_at: now,
            updated_at: now,
        };
        state.metrics.insert(metric.id, metric.clone());
        state.emit(
            EVENT_METRIC_DEFINED,
            json!({
                "tenant_id": tenant_id,
                "metric_id": metric.id.to_string(),
                "name": metric.name,
                "category": metric.category.as_str(),
                "unit": metric.unit,
                "data_type": metric.data_type.as_str(),
                "created_by": caller.actor,
            }),
        );
        Ok(serde_json::to_value(&metric).unwrap_or(Value::Null))
    })();
    match result {
        Ok(data) => created(data),
        Err(error) => err(error, debug),
    }
}

/// Get one metric definition.
pub(crate) async fn get_metric(
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
        let metric = load_metric(&state, &tenant_id, &path.into_inner())?;
        Ok(serde_json::to_value(&metric).unwrap_or(Value::Null))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

/// Update a metric definition (manager). Changing the data
/// type does not rewrite existing entries: values recorded
/// under the old type stay as recorded, and new values must
/// match the new type.
pub(crate) async fn update_metric(
    state: web::Data<EsgState>,
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    path: web::Path<String>,
    body: web::Json<UpdateMetricRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let tenant_id = resolve_tenant(&req)?;
        let caller = caller(&req)?;
        require_manage(&ctx, &caller.role)?;
        let input = body.into_inner();
        let mut metric = load_metric(&state, &tenant_id, &path.into_inner())?;
        if let Some(name) = &input.name {
            metric.name = require_text(name, "name", MAX_NAME_LEN)?;
        }
        if let Some(description) = &input.description {
            metric.description = match description {
                Some(text) => Some(require_text(text, "description", MAX_DESCRIPTION_LEN)?),
                None => None,
            };
        }
        if let Some(category) = &input.category {
            metric.category = Category::from_str(category.trim()).ok_or_else(|| {
                UnifiedError::bad_request(
                    "category must be environmental, social, or governance".to_string(),
                )
            })?;
        }
        if let Some(unit) = &input.unit {
            metric.unit = require_text(unit, "unit", MAX_UNIT_LEN)?;
        }
        if let Some(data_type) = &input.data_type {
            metric.data_type = MetricDataType::from_str(data_type.trim()).ok_or_else(|| {
                UnifiedError::bad_request("data_type must be number, text, or boolean".to_string())
            })?;
        }
        if let Some(frequency) = &input.frequency {
            metric.frequency = require_text(frequency, "frequency", MAX_FREQUENCY_LEN)?;
        }
        if let Some(framework_ids) = &input.framework_ids {
            metric.framework_ids = validate_framework_ids(framework_ids)?;
        }
        if let Some(active) = input.active {
            metric.active = active;
        }
        metric.updated_at = Utc::now();
        state.metrics.insert(metric.id, metric.clone());
        Ok(serde_json::to_value(&metric).unwrap_or(Value::Null))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

/// Delete a metric definition (manager). Entries, history,
/// and targets that reference the metric are kept: the
/// reporting history stays auditable, and the summary skips
/// metrics that no longer exist.
pub(crate) async fn delete_metric(
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
        let metric = load_metric(&state, &tenant_id, &path.into_inner())?;
        state.metrics.remove(&metric.id);
        Ok(json!({ "deleted": true, "metric_id": metric.id.to_string() }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}
