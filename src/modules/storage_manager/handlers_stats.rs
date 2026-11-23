// Stats handlers.

use unified_core::context::AppContext;
use unified_core::error::UnifiedError;
use unified_core::pagination::PaginationParams;
use actix_web::{web, HttpRequest, HttpResponse};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::Arc;

use super::*;

// ---------------------------------------------------------------------------
// Stats.
// ---------------------------------------------------------------------------

pub(crate) async fn get_stats(
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
    let found = match authorize(&ctx, &req, PERM_READ) {
        Ok(found) => found,
        Err(error) => return err_response(error, debug),
    };
    let is_admin = is_storage_admin(&ctx, &found.role);
    let bucket_filter = params.get("bucket_id").cloned();
    let company_filter = params.get("company_id").cloned();
    let user_filter = params.get("user_id").cloned();
    let module_filter = params.get("module").cloned();
    let objects: Vec<StoredObject> = state
        .objects
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|object| {
            // Tenant scoping: a non-admin counts only objects they
            // own or that belong to their company.
            is_admin
                || object.owner_user == found.actor
                || _scope
                    .as_deref()
                    .is_some_and(|company| object.company_id.as_deref() == Some(company))
        })
        .filter(|object| {
            bucket_filter
                .as_ref()
                .is_none_or(|id| object.bucket_id.to_string() == *id)
                && company_filter
                    .as_ref()
                    .is_none_or(|id| object.company_id.as_deref() == Some(id.as_str()))
                && user_filter
                    .as_ref()
                    .is_none_or(|id| object.owner_user == *id)
                && module_filter
                    .as_ref()
                    .is_none_or(|id| object.owner_module == *id)
        })
        .collect();
    let total_bytes: i64 = objects.iter().map(|object| object.size_bytes).sum();
    let total_objects = objects.len() as i64;
    let mut by_bucket: HashMap<String, (i64, i64)> = HashMap::new();
    let mut by_company: HashMap<String, (i64, i64)> = HashMap::new();
    let mut by_user: HashMap<String, (i64, i64)> = HashMap::new();
    let mut by_module: HashMap<String, (i64, i64)> = HashMap::new();
    for object in &objects {
        let entry = by_bucket
            .entry(object.bucket_id.to_string())
            .or_default();
        entry.0 += object.size_bytes;
        entry.1 += 1;
        let company = object.company_id.clone().unwrap_or_default();
        let entry = by_company.entry(company).or_default();
        entry.0 += object.size_bytes;
        entry.1 += 1;
        let entry = by_user.entry(object.owner_user.clone()).or_default();
        entry.0 += object.size_bytes;
        entry.1 += 1;
        let entry = by_module.entry(object.owner_module.clone()).or_default();
        entry.0 += object.size_bytes;
        entry.1 += 1;
    }
    // `json!` treats a bare identifier key as a literal string, so
    // dynamic dimension names are built as maps by hand.
    let to_list = |map: HashMap<String, (i64, i64)>, key_name: &str| {
        let mut list: Vec<Value> = map
            .into_iter()
            .map(|(key, (bytes, objects))| {
                let mut item = serde_json::Map::new();
                item.insert(key_name.to_string(), Value::String(key));
                item.insert("bytes".to_string(), json!(bytes));
                item.insert("objects".to_string(), json!(objects));
                Value::Object(item)
            })
            .collect();
        list.sort_by(|a, b| {
            b.get("bytes")
                .and_then(Value::as_i64)
                .cmp(&a.get("bytes").and_then(Value::as_i64))
        });
        list
    };
    ok(json!({
        "total_bytes": total_bytes,
        "total_objects": total_objects,
        "buckets": state
            .buckets
            .iter()
            .filter(|entry| bucket_company_visible(&entry.value().company_id, &_scope, is_admin))
            .count(),
        "by_bucket": to_list(by_bucket, "bucket_id"),
        "by_company": to_list(by_company, "company_id"),
        "by_user": to_list(by_user, "user_id"),
        "by_module": to_list(by_module, "module_id"),
    }))
}

pub(crate) async fn get_stats_daily(
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
    let mut items: Vec<UsageDaily> = state
        .usage_daily
        .iter()
        .map(|entry| entry.value().clone())
        .collect();
    // Tenant scoping: a scoped caller sees only their own
    // company's rows; an unresolved caller sees only global rows.
    items.retain(|row| in_company_scope(&row.company_id, &_scope));
    if let Some(from) = params.filters.get("from") {
        items.retain(|row| row.date >= *from);
    }
    if let Some(to) = params.filters.get("to") {
        items.retain(|row| row.date <= *to);
    }
    if let Some(bucket) = params.filters.get("bucket_id") {
        items.retain(|row| row.bucket_id.to_string() == *bucket);
    }
    if let Some(company) = params.filters.get("company_id") {
        items.retain(|row| row.company_id.as_deref() == Some(company.as_str()));
    }
    if let Some(user) = params.filters.get("user_id") {
        items.retain(|row| row.user_id == *user);
    }
    if let Some(module) = params.filters.get("module") {
        items.retain(|row| row.module_id == *module);
    }
    items.sort_by(|a, b| {
        b.date
            .cmp(&a.date)
            .then(a.bucket_id.cmp(&b.bucket_id))
            .then(a.module_id.cmp(&b.module_id))
    });
    ok(paginate(items, &params))
}

pub(crate) async fn get_stats_top(
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
    let found = match authorize(&ctx, &req, PERM_READ) {
        Ok(found) => found,
        Err(error) => return err_response(error, debug),
    };
    let dimension = params.get("by").map(String::as_str).unwrap_or("module");
    if !matches!(dimension, "module" | "user" | "company" | "bucket") {
        return err_response(
            UnifiedError::bad_request(
                "Query parameter \"by\" must be \"module\", \"user\", \"company\", or \"bucket\"",
            ),
            debug,
        );
    }
    let limit: usize = params
        .get("limit")
        .and_then(|raw| raw.parse().ok())
        .unwrap_or(10)
        .clamp(1, 100);
    let is_admin = is_storage_admin(&ctx, &found.role);
    let mut totals: HashMap<String, (i64, i64)> = HashMap::new();
    for entry in state.objects.iter() {
        let object = entry.value();
        // Tenant scoping: a non-admin contributes only objects they
        // own or that belong to their company to the top list.
        if !is_admin
            && object.owner_user != found.actor
            && !_scope
                .as_deref()
                .is_some_and(|company| object.company_id.as_deref() == Some(company))
        {
            continue;
        }
        let key = match dimension {
            "user" => object.owner_user.clone(),
            "company" => object.company_id.clone().unwrap_or_default(),
            "bucket" => object.bucket_id.to_string(),
            _ => object.owner_module.clone(),
        };
        let slot = totals.entry(key).or_default();
        slot.0 += object.size_bytes;
        slot.1 += 1;
    }
    let mut list: Vec<Value> = totals
        .into_iter()
        .map(|(key, (bytes, objects))| {
            let mut item = serde_json::Map::new();
            item.insert(dimension.to_string(), Value::String(key));
            item.insert("bytes".to_string(), json!(bytes));
            item.insert("objects".to_string(), json!(objects));
            Value::Object(item)
        })
        .collect();
    list.sort_by(|a, b| {
        b.get("bytes")
            .and_then(Value::as_i64)
            .cmp(&a.get("bytes").and_then(Value::as_i64))
    });
    list.truncate(limit);
    ok(json!({ "by": dimension, "top": list }))
}
