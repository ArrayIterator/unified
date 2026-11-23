//! analytics - handlers definitions.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::json;
use unified_core::pagination::PaginationParams;
use actix_web::{http::header, web, HttpRequest, HttpResponse};
use chrono::{ DateTime, Utc };
use serde_json::{ json, Value };
use std::sync::Arc;
use super::{events::*, helpers::*, types::*, views::*, inputs::*, state::*, hooks::*, reports::*, handlers_dashboard::*, handlers_runs::*, handlers_scheduled::*, module::*};
use super::helpers::{matches_search};



// ---------------------------------------------------------------------------
// Report definition handlers.
// ---------------------------------------------------------------------------
pub(crate) fn parse_optional_datetime(value: Option<String>, field: &str) -> UnifiedResult<Option<DateTime<Utc>>> {
    match value {
        Some(value) => Ok(Some(parse_datetime(&value, field)?)),
        None => Ok(None),
    }
}


pub(crate) async fn list_reports(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<AnalyticsState>>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let _tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return json::error_response(&error, ctx.is_debug()),
    };
    let _scope = match caller_company(&_tenant, &req) {
        Ok(scope) => scope,
        Err(error) => return json::error_response(&error, ctx.is_debug()),
    };
    if let Err(error) = analytics_authorize(&ctx, &req, PERM_ANALYTICS_VIEW) {
        return json::error_response(&error, ctx.is_debug());
    }
    let params = query.into_inner();
    let mut items: Vec<ReportDefinition> = state
        .definitions
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|definition| {
            matches_search(&format!("{} {}", definition.name, definition.description.clone().unwrap_or_default()), &params)
        })
        .collect();
    items.sort_by(|a, b| a.created_at.cmp(&b.created_at));
    if params.normalized().order == "desc" {
        items.reverse();
    }
    let page = paginate(items, &params);
    json::success_response(page, 200)
}


pub(crate) async fn create_report(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<AnalyticsState>>,
    req: HttpRequest,
    body: web::Json<CreateReportRequest>,
) -> HttpResponse {
    let result: UnifiedResult<ReportDefinition> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = require_company_for_write(&_tenant, &req)?;
        analytics_authorize(&ctx, &req, PERM_ANALYTICS_MANAGE)?;
        let input = body.into_inner();
        let name = require_name(&input.name)?;
        validate_metrics(&input.metrics)?;
        let dimensions = input.dimensions.unwrap_or_default();
        validate_dimensions(&dimensions)?;
        let range_preset = match input.range_preset {
            Some(preset) => {
                if !matches!(preset.as_str(), "7d" | "30d" | "90d" | "365d") {
                    return Err(UnifiedError::bad_request(
                        "range_preset must be one of: 7d, 30d, 90d, 365d",
                    ));
                }
                Some(preset)
            }
            None => None,
        };
        let start = parse_optional_datetime(input.start, "start")?;
        let end = parse_optional_datetime(input.end, "end")?;
        if let (Some(start), Some(end)) = (start, end) {
            if start >= end {
                return Err(UnifiedError::bad_request("start must be before end"));
            }
        }
        let timestamp = now();
        let definition = ReportDefinition {
            id: new_id(),
            name,
            description: input
                .description
                .map(|value| value.trim().to_string())
                .filter(|value| !value.is_empty()),
            metrics: input.metrics,
            dimensions,
            range_preset,
            start,
            end,
            filters: input.filters.unwrap_or(Value::Null),
            created_at: timestamp,
            updated_at: timestamp,
        };
        state
            .definitions
            .insert(definition.id.clone(), definition.clone());
        Ok(definition)
    })();
    respond_created(result, ctx.is_debug())
}


pub(crate) async fn get_report(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<AnalyticsState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let _tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return json::error_response(&error, ctx.is_debug()),
    };
    let _scope = match caller_company(&_tenant, &req) {
        Ok(scope) => scope,
        Err(error) => return json::error_response(&error, ctx.is_debug()),
    };
    if let Err(error) = analytics_authorize(&ctx, &req, PERM_ANALYTICS_VIEW) {
        return json::error_response(&error, ctx.is_debug());
    }
    let result = state.definition(path.as_str());
    json::respond(result, ctx.is_debug())
}


pub(crate) async fn update_report(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<AnalyticsState>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdateReportRequest>,
) -> HttpResponse {
    let result: UnifiedResult<ReportDefinition> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = require_company_for_write(&_tenant, &req)?;
        analytics_authorize(&ctx, &req, PERM_ANALYTICS_MANAGE)?;
        let input = body.into_inner();
        let mut definition = state.definition(path.as_str())?;
        if let Some(name) = input.name {
            definition.name = require_name(&name)?;
        }
        if let Some(description) = input.description {
            definition.description = description
                .map(|value| value.trim().to_string())
                .filter(|value| !value.is_empty());
        }
        if let Some(metrics) = input.metrics {
            validate_metrics(&metrics)?;
            definition.metrics = metrics;
        }
        if let Some(dimensions) = input.dimensions {
            validate_dimensions(&dimensions)?;
            definition.dimensions = dimensions;
        }
        if let Some(range_preset) = input.range_preset {
            definition.range_preset = match range_preset {
                Some(preset) => {
                    if !matches!(preset.as_str(), "7d" | "30d" | "90d" | "365d") {
                        return Err(UnifiedError::bad_request(
                            "range_preset must be one of: 7d, 30d, 90d, 365d",
                        ));
                    }
                    Some(preset)
                }
                None => None,
            };
        }
        if let Some(start) = input.start {
            definition.start = parse_optional_datetime(start, "start")?;
        }
        if let Some(end) = input.end {
            definition.end = parse_optional_datetime(end, "end")?;
        }
        if let (Some(start), Some(end)) = (definition.start, definition.end) {
            if start >= end {
                return Err(UnifiedError::bad_request("start must be before end"));
            }
        }
        if let Some(filters) = input.filters {
            definition.filters = filters;
        }
        definition.updated_at = now();
        state
            .definitions
            .insert(definition.id.clone(), definition.clone());
        Ok(definition)
    })();
    json::respond(result, ctx.is_debug())
}


pub(crate) async fn delete_report(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<AnalyticsState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = require_company_for_write(&_tenant, &req)?;
        analytics_authorize(&ctx, &req, PERM_ANALYTICS_MANAGE)?;
        let definition = state.definition(path.as_str())?;
        state.definitions.remove(&definition.id);
        state
            .runs
            .retain(|_, run| run.definition_id != definition.id);
        state
            .scheduled
            .retain(|_, scheduled| scheduled.definition_id != definition.id);
        Ok(json!({ "deleted": true, "id": definition.id }))
    })();
    json::respond(result, ctx.is_debug())
}
