// Health check handlers.

use unified_core::config::AppConfig;
use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::json;
use unified_core::pagination::PaginationParams;
use actix_web::{web, HttpRequest, HttpResponse};
use chrono::Utc;
use serde_json::json;
use std::sync::{Arc, RwLock};
use uuid::Uuid;

use super::*;

// ---------------------------------------------------------------------------
// Health checks.
// ---------------------------------------------------------------------------

/// Run one health check: connectivity plus a write / read / delete
/// probe on a scratch key, then cleanup. The scratch object is
/// removed even when the read fails, so a failed probe never leaves
/// bytes behind. Returns the recorded check.
pub(crate) async fn run_health_check(
    state: &StorageManagerState,
    config: &RwLock<AppConfig>,
    bucket: &StorageBucket,
) -> UnifiedResult<HealthCheck> {
    let started = std::time::Instant::now();
    let outcome: UnifiedResult<()> = (|| async {
        let provider = provider_for_bucket_with(config, bucket)?;
        provider.test_connection().await?;
        let probe_key = format!(".healthcheck-{}/probe.txt", uuid::Uuid::now_v7());
        let probe_body = b"storage-manager-health-probe".to_vec();
        provider.upload(&probe_key, probe_body.clone()).await?;
        let read_back = provider.download(&probe_key).await;
        let delete_result = provider.delete(&probe_key).await;
        let bytes = read_back?;
        if bytes != probe_body {
            // Best-effort cleanup, then report the mismatch.
            let _ = delete_result;
            return Err(UnifiedError::internal(
                "Health probe read back different bytes than were written",
            ));
        }
        delete_result?;
        Ok(())
    })()
    .await;
    let latency_ms = started.elapsed().as_millis() as i64;
    let now = Utc::now();
    let (ok, detail) = match &outcome {
        Ok(()) => (true, "Connectivity and write/read/delete probe passed.".to_string()),
        Err(error) => (false, error.message.clone()),
    };
    let check = HealthCheck {
        id: Uuid::now_v7(),
        bucket_id: bucket.id,
        ok,
        latency_ms,
        detail,
        checked_at: now,
    };
    state.health_checks.insert(check.id, check.clone());
    if ok {
        state.emit(
            events::HEALTH_OK,
            json!({
                "bucket_id": bucket.id.to_string(),
                "name": bucket.name,
                "latency_ms": latency_ms,
                "checked_at": now.to_rfc3339(),
            }),
        );
    } else {
        state.emit(
            events::HEALTH_FAILED,
            json!({
                "bucket_id": bucket.id.to_string(),
                "name": bucket.name,
                "error": check.detail.clone(),
                "latency_ms": latency_ms,
                "checked_at": now.to_rfc3339(),
            }),
        );
    }
    match outcome {
        Ok(()) => Ok(check),
        Err(error) => Err(error),
    }
}

pub(crate) async fn health_bucket(
    state: web::Data<StorageManagerState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    id: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();

    let _tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return err_response(error, debug),
    };
    let _scope = match caller_company(&_tenant, &req) {
        Ok(scope) => scope,
        Err(error) => return err_response(error, debug),
    };
    let found = match authorize(&ctx, &req, PERM_HEALTH) {
        Ok(found) => found,
        Err(error) => return err_response(error, debug),
    };
    let is_admin = is_platform_admin(&ctx, &found.role, &req, PERM_MANAGE_BUCKETS);
    let result: UnifiedResult<HealthCheck> = (|| async {
        let bucket = load_bucket(&state, &id)?;
        check_bucket_access(&bucket, _scope.as_deref(), is_admin)?;
        run_health_check(&state, &ctx.config, &bucket).await
    })()
    .await;
    json::respond(result, debug)
}

pub(crate) async fn list_health(
    state: web::Data<StorageManagerState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    params: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();

    let _tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return err_response(error, debug),
    };
    let _scope = match caller_company(&_tenant, &req) {
        Ok(scope) => scope,
        Err(error) => return err_response(error, debug),
    };
    let found = match authorize(&ctx, &req, PERM_HEALTH) {
        Ok(found) => found,
        Err(error) => return err_response(error, debug),
    };
    let params = params.into_inner();
    let mut items: Vec<HealthCheck> = state
        .health_checks
        .iter()
        .map(|entry| entry.value().clone())
        .collect();
    // Tenant scoping: a non-admin lists only checks for buckets
    // they may access; an admin lists all. Checks for deleted
    // buckets are hidden from non-admins.
    let is_admin = is_platform_admin(&ctx, &found.role, &req, PERM_MANAGE_BUCKETS);
    items.retain(|check| match state.buckets.get(&check.bucket_id) {
        Some(bucket) => bucket_company_visible(&bucket.value().company_id, &_scope, is_admin),
        None => is_admin,
    });
    if let Some(bucket) = params.filters.get("bucket_id") {
        items.retain(|check| check.bucket_id.to_string() == *bucket);
    }
    if let Some(ok) = params.filters.get("ok") {
        let want = ok == "true";
        items.retain(|check| check.ok == want);
    }
    items.sort_by(|a, b| b.checked_at.cmp(&a.checked_at));
    ok(paginate(items, &params))
}

pub(crate) async fn list_bucket_health(
    state: web::Data<StorageManagerState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    id: web::Path<String>,
    params: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();

    let _tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return err_response(error, debug),
    };
    let _scope = match caller_company(&_tenant, &req) {
        Ok(scope) => scope,
        Err(error) => return err_response(error, debug),
    };
    let found = match authorize(&ctx, &req, PERM_HEALTH) {
        Ok(found) => found,
        Err(error) => return err_response(error, debug),
    };
    let is_admin = is_platform_admin(&ctx, &found.role, &req, PERM_MANAGE_BUCKETS);
    let bucket = match load_bucket(&state, &id) {
        Ok(bucket) => bucket,
        Err(error) => return err_response(error, debug),
    };
    if let Err(error) = check_bucket_access(&bucket, _scope.as_deref(), is_admin) {
        return err_response(error, debug);
    }
    let params = params.into_inner();
    let mut items: Vec<HealthCheck> = state
        .health_checks
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|check| check.bucket_id == bucket.id)
        .collect();
    items.sort_by(|a, b| b.checked_at.cmp(&a.checked_at));
    ok(paginate(items, &params))
}
