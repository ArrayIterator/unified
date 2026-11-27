// Quota handlers.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::json;
use unified_core::pagination::PaginationParams;
use actix_web::{web, HttpRequest, HttpResponse};
use chrono::Utc;
use serde_json::{json, Value};
use std::sync::Arc;
use uuid::Uuid;

use super::*;

// Quotas.

pub(crate) async fn list_quotas(
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
    let found = match authorize(&ctx, &req, PERM_READ) {
        Ok(found) => found,
        Err(error) => return err_response(error, debug),
    };
    let params = params.into_inner();
    let mut items: Vec<StorageQuota> = state
        .quotas
        .iter()
        .map(|entry| entry.value().clone())
        .collect();
    // Tenant scoping: company quotas of the caller's company,
    // the caller's own user quotas, and platform (module) quotas.
    items.retain(|quota| quota_in_scope(quota, &_scope, &found.actor));
    if let Some(scope) = params.filters.get("scope") {
        items.retain(|quota| quota.scope == *scope);
    }
    if let Some(scope_id) = params.filters.get("scope_id") {
        items.retain(|quota| quota.scope_id == *scope_id);
    }
    if let Some(search) = &params.search {
        let needle = search.to_lowercase();
        items.retain(|quota| quota.scope_id.to_lowercase().contains(&needle));
    }
    items.sort_by(|a, b| a.scope.cmp(&b.scope).then(a.scope_id.cmp(&b.scope_id)));
    ok(paginate(items, &params))
}

pub(crate) async fn create_quota(
    state: web::Data<StorageManagerState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    body: web::Json<CreateQuotaRequest>,
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
    let found = match authorize(&ctx, &req, PERM_MANAGE_QUOTAS) {
        Ok(found) => found,
        Err(error) => return err_response(error, debug),
    };
    let result: UnifiedResult<StorageQuota> = (|| {
        let input = body.into_inner();
        let scope = match input.scope.trim() {
            "company" | "user" | "module" => input.scope.trim().to_string(),
            other => {
                return Err(UnifiedError::bad_request(format!(
                    "Quota scope must be \"company\", \"user\", or \"module\", not \"{other}\""
                )));
            }
        };
        let scope_id = if scope == "module" {
            valid_module_id(&input.scope_id)?
        } else {
            valid_scope_id(&input.scope_id)?
        };
        // Tenant scoping, before any existence-revealing check: a
        // company quota must name the caller's own company; a user
        // quota must name the caller; a module quota is platform
        // configuration and only an unresolved caller may create one.
        if scope == "company" {
            require_company_match_for(&_scope, &Some(scope_id.clone()))?;
        } else if scope == "user" && scope_id != found.actor {
            return Err(UnifiedError::forbidden(
                "A user quota may only be created for the caller",
            ));
        } else if scope == "module" && _scope.is_some() {
            return Err(UnifiedError::forbidden(
                "A module quota is platform configuration and may only be created by a platform caller",
            ));
        }
        let bytes_limit = valid_quota_limit(input.bytes_limit, "bytes_limit")?;
        let objects_limit = valid_quota_limit(input.objects_limit, "objects_limit")?;
        let soft_bytes_limit = valid_quota_limit(input.soft_bytes_limit, "soft_bytes_limit")?;
        let soft_objects_limit =
            valid_quota_limit(input.soft_objects_limit, "soft_objects_limit")?;
        if let (Some(soft), Some(hard)) = (soft_bytes_limit, bytes_limit) {
            if soft > hard {
                return Err(UnifiedError::bad_request(
                    "soft_bytes_limit must not exceed bytes_limit",
                ));
            }
        }
        if let (Some(soft), Some(hard)) = (soft_objects_limit, objects_limit) {
            if soft > hard {
                return Err(UnifiedError::bad_request(
                    "soft_objects_limit must not exceed objects_limit",
                ));
            }
        }
        let key = quota_key(&scope, &scope_id);
        if state.quotas.contains_key(&key) {
            return Err(UnifiedError::conflict(format!(
                "A quota already exists for {scope} \"{scope_id}\""
            )));
        }
        let now = Utc::now();
        let quota = StorageQuota {
            id: Uuid::now_v7(),
            scope,
            scope_id,
            bytes_limit,
            objects_limit,
            soft_bytes_limit,
            soft_objects_limit,
            used_bytes: 0,
            used_objects: 0,
            created_at: now,
            updated_at: now,
        };
        state.quotas.insert(key, quota.clone());
        Ok(quota)
    })();
    match result {
        Ok(view) => created(view),
        Err(error) => err_response(error, debug),
    }
}

pub(crate) async fn get_quota(
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
    let found = match authorize(&ctx, &req, PERM_READ) {
        Ok(found) => found,
        Err(error) => return err_response(error, debug),
    };
    let result: UnifiedResult<StorageQuota> = (|| {
        let quota = load_quota(&state, &id)?;
        require_quota_in_scope(&quota, &_scope, &found.actor)?;
        Ok(quota)
    })();
    json::respond(result, debug)
}

pub(crate) async fn update_quota(
    state: web::Data<StorageManagerState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    id: web::Path<String>,
    body: web::Json<UpdateQuotaRequest>,
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
    let found = match authorize(&ctx, &req, PERM_MANAGE_QUOTAS) {
        Ok(found) => found,
        Err(error) => return err_response(error, debug),
    };
    let result: UnifiedResult<StorageQuota> = (|| {
        let input = body.into_inner();
        let mut quota = load_quota(&state, &id)?;
        require_quota_in_scope(&quota, &_scope, &found.actor)?;
        if let Some(limit) = input.bytes_limit {
            quota.bytes_limit = valid_quota_limit(limit, "bytes_limit")?;
        }
        if let Some(limit) = input.objects_limit {
            quota.objects_limit = valid_quota_limit(limit, "objects_limit")?;
        }
        if let Some(limit) = input.soft_bytes_limit {
            quota.soft_bytes_limit = valid_quota_limit(limit, "soft_bytes_limit")?;
        }
        if let Some(limit) = input.soft_objects_limit {
            quota.soft_objects_limit = valid_quota_limit(limit, "soft_objects_limit")?;
        }
        if let (Some(soft), Some(hard)) = (quota.soft_bytes_limit, quota.bytes_limit) {
            if soft > hard {
                return Err(UnifiedError::bad_request(
                    "soft_bytes_limit must not exceed bytes_limit",
                ));
            }
        }
        if let (Some(soft), Some(hard)) = (quota.soft_objects_limit, quota.objects_limit) {
            if soft > hard {
                return Err(UnifiedError::bad_request(
                    "soft_objects_limit must not exceed objects_limit",
                ));
            }
        }
        quota.updated_at = Utc::now();
        let key = quota_key(&quota.scope, &quota.scope_id);
        state.quotas.insert(key, quota.clone());
        Ok(quota)
    })();
    json::respond(result, debug)
}

pub(crate) async fn delete_quota(
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
    let found = match authorize(&ctx, &req, PERM_MANAGE_QUOTAS) {
        Ok(found) => found,
        Err(error) => return err_response(error, debug),
    };
    let result: UnifiedResult<Value> = (|| {
        let quota = load_quota(&state, &id)?;
        require_quota_in_scope(&quota, &_scope, &found.actor)?;
        state
            .quotas
            .remove(&quota_key(&quota.scope, &quota.scope_id));
        Ok(json!({ "quota_id": quota.id, "deleted": true }))
    })();
    json::respond(result, debug)
}
