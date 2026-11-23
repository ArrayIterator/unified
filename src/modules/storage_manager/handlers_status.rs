// Status and defaults handlers.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::json;
use unified_core::storage::StorageKind;
use actix_web::{web, HttpRequest, HttpResponse};
use serde_json::{json, Value};
use std::sync::Arc;

use super::*;

// ---------------------------------------------------------------------------
// Status + defaults.
// ---------------------------------------------------------------------------

pub(crate) async fn get_status(
    state: web::Data<StorageManagerState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
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
    let _ = found;
    let buckets = state.buckets.len();
    let enabled_buckets = state
        .buckets
        .iter()
        .filter(|entry| entry.value().enabled)
        .count();
    let kinds: Vec<String> = StorageKind::all()
        .iter()
        .map(|kind| kind.as_str().to_string())
        .collect();
    let global_default = *state
        .global_default
        .read()
        .expect("global default lock poisoned");
    ok(json!({
        "module": "storage_manager",
        "kinds": kinds,
        "buckets": buckets,
        "enabled_buckets": enabled_buckets,
        "bindings": state.bindings.len(),
        "quotas": state.quotas.len(),
        "recorded_objects": state.objects.len(),
        "lifecycle_rules": state.lifecycle_rules.len(),
        "issued_signed_urls": state.signed_urls.len(),
        "plans": state.plans.len(),
        "plan_assignments": state.plan_assignments.len(),
        "metering_reports": state.metering_reports.len(),
        "byo_connections": state.byo_connections.len(),
        "byo_terms_versions": state.byo_terms.len(),
        "current_byo_terms_version": current_byo_terms(&state).map(|terms| terms.version),
        "byo_allowed_provider_kinds": state
            .byo_settings
            .read()
            .expect("BYO settings lock poisoned")
            .allowed_provider_kinds
            .clone(),
        "global_default_bucket_id": global_default,
        "company_defaults": state.company_defaults.len(),
        "available_as_product": true,
        "product_category": "infrastructure",
    }))
}

pub(crate) async fn get_defaults(
    state: web::Data<StorageManagerState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
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
    let global_default = *state
        .global_default
        .read()
        .expect("global default lock poisoned");
    // Tenant scoping: a scoped caller sees only their own
    // company's default; an unresolved caller sees none (the global
    // default above is platform configuration, visible to all).
    let company_defaults: Vec<Value> = state
        .company_defaults
        .iter()
        .filter(|entry| in_company_scope(&Some(entry.key().clone()), &_scope))
        .map(|entry| {
            json!({ "company_id": entry.key(), "bucket_id": entry.value() })
        })
        .collect();
    ok(json!({
        "global_default_bucket_id": global_default,
        "company_defaults": company_defaults,
    }))
}

pub(crate) async fn set_default(
    state: web::Data<StorageManagerState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    body: web::Json<SetDefaultRequest>,
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
    let result: UnifiedResult<Value> = (|| {
        let input = body.into_inner();
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
        if !bucket.enabled {
            return Err(UnifiedError::conflict(format!(
                "Storage bucket \"{}\" is disabled and cannot be a default",
                bucket.name
            )));
        }
        check_bucket_access(&bucket, _scope.as_deref(), is_admin)?;
        match &input.company_id {
            Some(company) => {
                let company = valid_scope_id(company)?;
                require_company_match_for(&_scope, &Some(company.clone()))?;
                state.company_defaults.insert(company.clone(), bucket.id);
                state.emit(
                    events::BINDING_CHANGED,
                    json!({
                        "scope": "company_default",
                        "company_id": company,
                        "bucket_id": bucket.id.to_string(),
                        "changed_by": found.actor,
                    }),
                );
                Ok(json!({
                    "company_id": company,
                    "bucket_id": bucket.id,
                    "bucket_name": bucket.name,
                }))
            }
            None => {
                *state
                    .global_default
                    .write()
                    .expect("global default lock poisoned") = Some(bucket.id);
                state.emit(
                    events::BINDING_CHANGED,
                    json!({
                        "scope": "global_default",
                        "bucket_id": bucket.id.to_string(),
                        "changed_by": found.actor,
                    }),
                );
                Ok(json!({
                    "company_id": Value::Null,
                    "bucket_id": bucket.id,
                    "bucket_name": bucket.name,
                }))
            }
        }
    })();
    json::respond(result, debug)
}
