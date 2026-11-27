// timetracker - HTTP handlers: activity and usage reports, screenshots.

use super::*;
use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::json;
use unified_core::pagination::{PageResult, PaginationParams};
use actix_web::{HttpRequest, HttpResponse, web};
use serde_json::{Value, json};
use std::sync::Arc;

pub(crate) async fn report_activity(
    state: web::Data<TimeTrackerState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    body: web::Json<ReportActivityRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<ActivitySample> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = require_company_for_write(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_WRITE)?;
        record_activity(&state, &caller.actor, body.into_inner())
    })();
    match result {
        Ok(sample) => json::success_response(sample, 201),
        Err(error) => json::error_response(&error, debug),
    }
}

pub(crate) async fn list_activity(
    state: web::Data<TimeTrackerState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PageResult<ActivitySample>> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_READ)?;
        let params = query.into_inner();
        let user_filter = params.filters.get("user_id").cloned();
        if let Some(wanted) = &user_filter {
            can_view_user(&ctx, &caller, wanted)?;
        }
        let mut items: Vec<ActivitySample> = state
            .activity
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|sample| match &user_filter {
                Some(wanted) => &sample.user_id == wanted,
                None => sample.user_id == caller.actor,
            })
            .collect();
        items.sort_by(|a, b| b.interval_start.cmp(&a.interval_start));
        Ok(paginate(items, &params))
    })();
    json::respond(result, debug)
}

pub(crate) async fn report_usage(
    state: web::Data<TimeTrackerState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    body: web::Json<ReportUsageRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<AppUsageRecord> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = require_company_for_write(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_WRITE)?;
        record_usage(&state, &caller.actor, body.into_inner())
    })();
    match result {
        Ok(record) => json::success_response(record, 201),
        Err(error) => json::error_response(&error, debug),
    }
}

pub(crate) async fn list_usage(
    state: web::Data<TimeTrackerState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PageResult<AppUsageRecord>> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_READ)?;
        let params = query.into_inner();
        let user_filter = params.filters.get("user_id").cloned();
        if let Some(wanted) = &user_filter {
            can_view_user(&ctx, &caller, wanted)?;
        }
        let category_filter = params.filters.get("category").cloned();
        let mut items: Vec<AppUsageRecord> = state
            .usage
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|record| match &user_filter {
                Some(wanted) => &record.user_id == wanted,
                None => record.user_id == caller.actor,
            })
            .filter(|record| {
                if let Some(category) = &category_filter {
                    let expected = match category.as_str() {
                        "work" => UsageCategory::Work,
                        "communication" => UsageCategory::Communication,
                        "development" => UsageCategory::Development,
                        "browsing" => UsageCategory::Browsing,
                        "social" => UsageCategory::Social,
                        "entertainment" => UsageCategory::Entertainment,
                        "other" => UsageCategory::Other,
                        _ => return false,
                    };
                    if record.category != expected {
                        return false;
                    }
                }
                true
            })
            .collect();
        items.sort_by(|a, b| b.interval_start.cmp(&a.interval_start));
        Ok(paginate(items, &params))
    })();
    json::respond(result, debug)
}

pub(crate) async fn list_screenshots(
    state: web::Data<TimeTrackerState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PageResult<ScreenshotRecord>> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_READ)?;
        let params = query.into_inner();
        let user_filter = params.filters.get("user_id").cloned();
        if let Some(wanted) = &user_filter {
            can_view_screenshots(&ctx, &caller, wanted)?;
        }
        let mut items: Vec<ScreenshotRecord> = state
            .screenshots
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|shot| match &user_filter {
                Some(wanted) => &shot.user_id == wanted,
                None => shot.user_id == caller.actor,
            })
            .collect();
        items.sort_by(|a, b| b.captured_at.cmp(&a.captured_at));
        Ok(paginate(items, &params))
    })();
    json::respond(result, debug)
}

pub(crate) async fn get_screenshot(
    state: web::Data<TimeTrackerState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();

    let _tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return json::error_response(&error, debug),
    };
    let _scope = match caller_company(&_tenant, &req) {
        Ok(scope) => scope,
        Err(error) => return json::error_response(&error, debug),
    };
    let result: UnifiedResult<Value> = (|| async {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_READ)?;
        let id = parse_uuid(&path.into_inner(), "screenshot")?;
        let shot = state
            .screenshots
            .get(&id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found(format!("Screenshot not found: {id}")))?;
        if shot.user_id != caller.actor {
            can_view_screenshots(&ctx, &caller, &shot.user_id)?;
        }
        let provider = ctx
            .storage_registry
            .get(&shot.storage_target)
            .ok_or_else(|| {
                UnifiedError::new(
                    format!("Storage target \"{}\" is not registered", shot.storage_target),
                    503,
                )
            })?;
        let url = provider
            .signed_url(&shot.storage_key, SCREENSHOT_SIGNED_URL_SECS)
            .await?;
        Ok(json!({
            "screenshot": shot,
            "url": url,
            "expires_secs": SCREENSHOT_SIGNED_URL_SECS,
        }))
    })()
    .await;
    json::respond(result, debug)
}

pub(crate) async fn delete_screenshot(
    state: web::Data<TimeTrackerState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();

    let _tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return json::error_response(&error, debug),
    };
    let _scope = match require_company_for_write(&_tenant, &req) {
        Ok(scope) => scope,
        Err(error) => return json::error_response(&error, debug),
    };
    let result: UnifiedResult<Value> = (|| async {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_WRITE)?;
        let id = parse_uuid(&path.into_inner(), "screenshot")?;
        let shot = state
            .screenshots
            .get(&id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found(format!("Screenshot not found: {id}")))?;
        if shot.user_id != caller.actor {
            require_permission(&ctx, &caller.role, PERM_MANAGE)?;
        }
        // Track whether a storage object was actually deleted: with
        // no provider registered there is nothing to delete, and
        // the event must say so instead of claiming success.
        let mut object_deleted = false;
        if let Some(provider) = ctx.storage_registry.get(&shot.storage_target) {
            provider.delete(&shot.storage_key).await?;
            object_deleted = true;
        }
        state.screenshots.remove(&id);
        state.emit(
            events::SCREENSHOT_DELETED,
            json!({
                "screenshot_id": shot.id.to_string(),
                "user_id": shot.user_id,
                "storage_target": shot.storage_target,
                "storage_key": shot.storage_key,
                "reason": "manual_delete",
                "object_deleted": object_deleted,
            }),
        );
        Ok(json!({ "id": shot.id.to_string(), "deleted": true, "object_deleted": object_deleted }))
    })()
    .await;
    json::respond(result, debug)
}

pub(crate) async fn purge_screenshots(
    state: web::Data<TimeTrackerState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
) -> HttpResponse {
    let debug = ctx.is_debug();

    let _tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return json::error_response(&error, debug),
    };
    let _scope = match require_company_for_write(&_tenant, &req) {
        Ok(scope) => scope,
        Err(error) => return json::error_response(&error, debug),
    };
    let result: UnifiedResult<Value> = (|| async {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_MANAGE)?;
        let (removed, failed) = purge_expired_screenshots(&state, &ctx.storage_registry).await;
        // Best-effort: ask the scheduler to run the purge on a
        // schedule from now on. False today: the scheduler does not
        // know the `timetracker.screenshots.purge` kind yet (see
        // `request_screenshot_purge`).
        let scheduler_queued = request_screenshot_purge(&state);
        Ok(json!({
            "removed": removed,
            "failed": failed,
            "purged_by": caller.actor,
            "scheduler_enqueue_queued": scheduler_queued,
        }))
    })()
    .await;
    json::respond(result, debug)
}
