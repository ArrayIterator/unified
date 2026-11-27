//! analytics - handlers scheduled.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::json;
use unified_core::pagination::PaginationParams;
use actix_web::{http::header, web, HttpRequest, HttpResponse};
use serde_json::{ json, Value };
use std::sync::Arc;
use super::{events::*, helpers::*, types::*, views::*, inputs::*, state::*, hooks::*, reports::*, handlers_dashboard::*, handlers_definitions::*, handlers_runs::*, module::*};



// Scheduled report handlers.
pub(crate) async fn list_scheduled(
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
    let mut items: Vec<ScheduledReport> = state
        .scheduled
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|scheduled| {
            params
                .filters
                .get("definition_id")
                .is_none_or(|id| scheduled.definition_id == *id)
                && params.filters.get("enabled").is_none_or(|flag| {
                    scheduled.enabled == (flag == "true" || flag == "1")
                })
        })
        .collect();
    items.sort_by(|a, b| a.created_at.cmp(&b.created_at));
    if params.normalized().order == "desc" {
        items.reverse();
    }
    let page = paginate(items, &params);
    json::success_response(page, 200)
}


pub(crate) async fn create_scheduled(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<AnalyticsState>>,
    req: HttpRequest,
    body: web::Json<CreateScheduledRequest>,
) -> HttpResponse {
    let result: UnifiedResult<ScheduledReport> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = require_company_for_write(&_tenant, &req)?;
        analytics_authorize(&ctx, &req, PERM_ANALYTICS_MANAGE)?;
        let input = body.into_inner();
        let definition = state.definition(&input.definition_id)?;
        let schedule = input.schedule.trim().to_string();
        if !valid_schedule(&schedule) {
            return Err(UnifiedError::bad_request(
                "schedule must be an interval in seconds or a cron-like expression such as \"*/5 * * * *\"",
            ));
        }
        let enabled = input.enabled.unwrap_or(true);
        let timestamp = now();
        let scheduled = ScheduledReport {
            id: new_id(),
            definition_id: definition.id,
            schedule,
            enabled,
            next_run_at: if enabled { next_run_at(&input.schedule, timestamp) } else { None },
            last_run_at: None,
            created_at: timestamp,
            updated_at: timestamp,
        };
        state
            .scheduled
            .insert(scheduled.id.clone(), scheduled.clone());
        Ok(scheduled)
    })();
    respond_created(result, ctx.is_debug())
}


pub(crate) async fn get_scheduled(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<AnalyticsState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<ScheduledReport> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        analytics_authorize(&ctx, &req, PERM_ANALYTICS_VIEW)?;
        state
            .scheduled
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found(format!("Scheduled report \"{}\" not found", path.as_str())))
    })();
    json::respond(result, ctx.is_debug())
}


pub(crate) async fn update_scheduled(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<AnalyticsState>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdateScheduledRequest>,
) -> HttpResponse {
    let result: UnifiedResult<ScheduledReport> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = require_company_for_write(&_tenant, &req)?;
        analytics_authorize(&ctx, &req, PERM_ANALYTICS_MANAGE)?;
        let input = body.into_inner();
        let mut scheduled = state
            .scheduled
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found(format!("Scheduled report \"{}\" not found", path.as_str())))?;
        if let Some(schedule) = input.schedule {
            let schedule = schedule.trim().to_string();
            if !valid_schedule(&schedule) {
                return Err(UnifiedError::bad_request(
                    "schedule must be an interval in seconds or a cron-like expression such as \"*/5 * * * *\"",
                ));
            }
            scheduled.schedule = schedule;
        }
        if let Some(enabled) = input.enabled {
            scheduled.enabled = enabled;
        }
        scheduled.updated_at = now();
        scheduled.next_run_at = if scheduled.enabled {
            next_run_at(&scheduled.schedule, now())
        } else {
            None
        };
        state
            .scheduled
            .insert(scheduled.id.clone(), scheduled.clone());
        Ok(scheduled)
    })();
    json::respond(result, ctx.is_debug())
}


pub(crate) async fn delete_scheduled(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<AnalyticsState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = require_company_for_write(&_tenant, &req)?;
        analytics_authorize(&ctx, &req, PERM_ANALYTICS_MANAGE)?;
        state
            .scheduled
            .remove(path.as_str())
            .map(|_| json!({ "deleted": true, "id": path.as_str() }))
            .ok_or_else(|| UnifiedError::not_found(format!("Scheduled report \"{}\" not found", path.as_str())))
    })();
    json::respond(result, ctx.is_debug())
}


pub(crate) async fn run_scheduled_now(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<AnalyticsState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<RunNowView> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        analytics_authorize(&ctx, &req, PERM_ANALYTICS_MANAGE)?;
        let mut scheduled = state
            .scheduled
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found(format!("Scheduled report \"{}\" not found", path.as_str())))?;
        let run = run_report(&state, &scheduled.definition_id, &_scope)?;
        ctx.hooks.emit(
            ANALYTICS_REPORT_GENERATED,
            serde_json::to_value(&run).unwrap_or(Value::Null),
        );
        // Ask the scheduler to queue the same work. The scheduler only
        // knows mail.send, geo.update, ssl.renew and cleanup today, so
        // it refuses kind "report.generate" until it gains that kind;
        // the refusal is reported, not hidden, and the report above
        // already ran synchronously either way.
        let hook_context = ctx.hooks.emit(
            SCHEDULER_ENQUEUE,
            json!({
                "kind": "report.generate",
                "payload": {
                    "definition_id": scheduled.definition_id,
                    "scheduled_report_id": scheduled.id,
                },
            }),
        );
        let mut scheduler_queued = false;
        let mut scheduler_error: Option<String> = None;
        for outcome in &hook_context.results {
            if outcome.get("queued").and_then(|value| value.as_bool()) == Some(true) {
                scheduler_queued = true;
            }
            if let Some(error) = outcome.get("error").and_then(|value| value.as_str()) {
                scheduler_error = Some(error.to_string());
            }
        }
        scheduled.last_run_at = Some(now());
        if scheduled.enabled {
            scheduled.next_run_at = next_run_at(&scheduled.schedule, now());
        }
        scheduled.updated_at = now();
        state
            .scheduled
            .insert(scheduled.id.clone(), scheduled.clone());
        Ok(RunNowView {
            run,
            scheduler_queued,
            scheduler_error,
            scheduler_note: "Scheduler queueing needs the scheduler to accept job kind \"report.generate\"; see the coordinator report for the snippet. The report itself ran synchronously regardless.".to_string(),
        })
    })();
    json::respond(result, ctx.is_debug())
}
