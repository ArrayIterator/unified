// Binding handlers.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::json;
use unified_core::pagination::PaginationParams;
use actix_web::{web, HttpRequest, HttpResponse};
use chrono::Utc;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::Arc;
use uuid::Uuid;

use super::*;

// ---------------------------------------------------------------------------
// Bindings.
// ---------------------------------------------------------------------------

pub(crate) async fn list_bindings(
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
    if let Err(error) = authorize(&ctx, &req, PERM_READ) {
        return err_response(error, debug);
    }
    let params = params.into_inner();
    let mut items: Vec<StorageBinding> = state
        .bindings
        .iter()
        .map(|entry| entry.value().clone())
        .collect();
    // Tenant scoping: a scoped caller lists only their own
    // company's bindings; an unresolved caller lists only global
    // bindings.
    items.retain(|binding| in_company_scope(&binding.company_id, &_scope));
    if let Some(module) = params.filters.get("module_id") {
        items.retain(|binding| binding.module_id == *module);
    }
    if let Some(bucket) = params.filters.get("bucket_id") {
        items.retain(|binding| binding.bucket_id.to_string() == *bucket);
    }
    if let Some(company) = params.filters.get("company_id") {
        items.retain(|binding| binding.company_id.as_deref() == Some(company.as_str()));
    }
    if let Some(enabled) = params.filters.get("enabled") {
        let want = enabled == "true";
        items.retain(|binding| binding.enabled == want);
    }
    if let Some(search) = &params.search {
        let needle = search.to_lowercase();
        items.retain(|binding| binding.module_id.to_lowercase().contains(&needle));
    }
    items.sort_by(|a, b| {
        a.module_id
            .cmp(&b.module_id)
            .then(a.company_id.cmp(&b.company_id))
    });
    ok(paginate(items, &params))
}

pub(crate) async fn create_binding(
    state: web::Data<StorageManagerState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    body: web::Json<CreateBindingRequest>,
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
    let found = match authorize(&ctx, &req, PERM_MANAGE_BINDINGS) {
        Ok(found) => found,
        Err(error) => return err_response(error, debug),
    };
    let is_admin = is_platform_admin(&ctx, &found.role, &req, PERM_MANAGE_BUCKETS);
    let result: UnifiedResult<StorageBinding> = (|| {
        let input = body.into_inner();
        let module_id = valid_module_id(&input.module_id)?;
        let bucket = state
            .buckets
            .get(&input.bucket_id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| {
                UnifiedError::not_found(format!(
                    "Storage bucket not found: {}",
                    input.bucket_id
                ))
            })?;
        check_bucket_access(&bucket, _scope.as_deref(), is_admin)?;
        if !bucket.enabled {
            return Err(UnifiedError::conflict(format!(
                "Storage bucket \"{}\" is disabled and cannot be bound",
                bucket.name
            )));
        }
        let key_prefix = match &input.key_prefix {
            Some(raw) => valid_prefix(raw)?,
            None => String::new(),
        };
        // Tenant scoping: the binding's company is verified against
        // the caller's scope — a scoped caller binds for their own
        // company (an omitted company adopts it); an unresolved
        // caller may only create a global binding.
        let body_company = match &input.company_id {
            Some(raw) => Some(valid_scope_id(raw)?),
            None => None,
        };
        let company_id = require_company_match_for(&_scope, &body_company)?;
        if state.bindings.iter().any(|entry| {
            let binding = entry.value();
            binding.enabled
                && binding.module_id == module_id
                && binding.company_id == company_id
        }) {
            return Err(UnifiedError::conflict(format!(
                "Module \"{module_id}\" already has an enabled binding{}; update or delete it first",
                match &company_id {
                    Some(company) => format!(" for company \"{company}\""),
                    None => String::new(),
                }
            )));
        }
        let now = Utc::now();
        let binding = StorageBinding {
            id: Uuid::now_v7(),
            module_id,
            bucket_id: bucket.id,
            key_prefix,
            company_id,
            enabled: true,
            created_at: now,
            updated_at: now,
        };
        state.bindings.insert(binding.id, binding.clone());
        state.emit(
            events::BINDING_CHANGED,
            json!({
                "binding_id": binding.id.to_string(),
                "module_id": binding.module_id,
                "bucket_id": binding.bucket_id.to_string(),
                "key_prefix": binding.key_prefix,
                "company_id": binding.company_id,
                "action": "created",
                "changed_by": found.actor,
            }),
        );
        Ok(binding)
    })();
    match result {
        Ok(view) => created(view),
        Err(error) => err_response(error, debug),
    }
}

pub(crate) async fn get_binding(
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
    if let Err(error) = authorize(&ctx, &req, PERM_READ) {
        return err_response(error, debug);
    }
    let result: UnifiedResult<StorageBinding> = (|| {
        let binding = load_binding(&state, &id)?;
        require_record_in_company_scope(&binding.company_id, &_scope)?;
        Ok(binding)
    })();
    json::respond(result, debug)
}

pub(crate) async fn update_binding(
    state: web::Data<StorageManagerState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    id: web::Path<String>,
    body: web::Json<UpdateBindingRequest>,
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
    let found = match authorize(&ctx, &req, PERM_MANAGE_BINDINGS) {
        Ok(found) => found,
        Err(error) => return err_response(error, debug),
    };
    let is_admin = is_platform_admin(&ctx, &found.role, &req, PERM_MANAGE_BUCKETS);
    let result: UnifiedResult<StorageBinding> = (|| {
        let input = body.into_inner();
        let mut binding = load_binding(&state, &id)?;
        require_record_writable_in_company_scope(&binding.company_id, &_scope)?;
        if let Some(bucket_id) = input.bucket_id {
            let bucket = state
                .buckets
                .get(&bucket_id)
                .map(|entry| entry.value().clone())
                .ok_or_else(|| {
                    UnifiedError::not_found(format!("Storage bucket not found: {bucket_id}"))
                })?;
            check_bucket_access(&bucket, _scope.as_deref(), is_admin)?;
            if !bucket.enabled {
                return Err(UnifiedError::conflict(format!(
                    "Storage bucket \"{}\" is disabled and cannot be bound",
                    bucket.name
                )));
            }
            binding.bucket_id = bucket_id;
        }
        if let Some(raw) = &input.key_prefix {
            binding.key_prefix = valid_prefix(raw)?;
        }
        if let Some(enabled) = input.enabled {
            binding.enabled = enabled;
        }
        binding.updated_at = Utc::now();
        state.bindings.insert(binding.id, binding.clone());
        state.emit(
            events::BINDING_CHANGED,
            json!({
                "binding_id": binding.id.to_string(),
                "module_id": binding.module_id,
                "bucket_id": binding.bucket_id.to_string(),
                "key_prefix": binding.key_prefix,
                "company_id": binding.company_id,
                "enabled": binding.enabled,
                "action": "updated",
                "changed_by": found.actor,
            }),
        );
        Ok(binding)
    })();
    json::respond(result, debug)
}

pub(crate) async fn delete_binding(
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
    let found = match authorize(&ctx, &req, PERM_MANAGE_BINDINGS) {
        Ok(found) => found,
        Err(error) => return err_response(error, debug),
    };
    let result: UnifiedResult<Value> = (|| {
        let binding = load_binding(&state, &id)?;
        require_record_writable_in_company_scope(&binding.company_id, &_scope)?;
        state.bindings.remove(&binding.id);
        state.emit(
            events::BINDING_CHANGED,
            json!({
                "binding_id": binding.id.to_string(),
                "module_id": binding.module_id,
                "bucket_id": binding.bucket_id.to_string(),
                "company_id": binding.company_id,
                "action": "deleted",
                "changed_by": found.actor,
            }),
        );
        Ok(json!({ "binding_id": binding.id, "deleted": true }))
    })();
    json::respond(result, debug)
}

pub(crate) async fn resolve_binding_endpoint(
    state: web::Data<StorageManagerState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    params: web::Query<HashMap<String, String>>,
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
    if let Err(error) = authorize(&ctx, &req, PERM_READ) {
        return err_response(error, debug);
    }
    let result: UnifiedResult<Value> = (|| {
        let module_raw = params.get("module").ok_or_else(|| {
            UnifiedError::bad_request("Query parameter \"module\" is required")
        })?;
        let module_id = valid_module_id(module_raw)?;
        // Tenant scoping (read rule, C3 task 320): the queried
        // company is verified against the caller's scope — a scoped
        // caller resolves for their own company (an omitted company
        // adopts it); an unresolved caller may only resolve the
        // global binding. This is a GET, so the write helper
        // `require_company_match_for` (C3 task 315) does not apply
        // here: it would 403 an unresolved caller resolving the
        // global binding.
        let body_company = params
            .get("company_id")
            .map(|raw| valid_scope_id(raw))
            .transpose()?;
        let resolved = unified_core::tenancy::CallerScope::from_company(_scope)
            .scoped_read_company(body_company.as_deref().unwrap_or(""))?;
        let company_id = if resolved.is_empty() {
            None
        } else {
            Some(resolved)
        };
        match resolve_binding(&state, &module_id, company_id.as_deref()) {
            Some((bucket, prefix, source)) => Ok(json!({
                "module_id": module_id,
                "company_id": company_id,
                "resolved": true,
                "source": source,
                "bucket_id": bucket.id,
                "bucket_name": bucket.name,
                "provider_kind": bucket.provider_kind.as_str(),
                "key_prefix": prefix,
            })),
            None => Ok(json!({
                "module_id": module_id,
                "company_id": company_id,
                "resolved": false,
                "source": "none",
                "bucket_id": Value::Null,
                "bucket_name": Value::Null,
                "provider_kind": Value::Null,
                "key_prefix": "",
            })),
        }
    })();
    json::respond(result, debug)
}
