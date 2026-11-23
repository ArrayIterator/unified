//! Cloudflare connector module. — Handlers: DNS records.
//!
//! Split from `src/modules/cloudflare.rs`; behaviour unchanged.

use super::accounts_zones::*;
use super::cache_handlers::*;
use super::client::*;
use super::constants::*;
use super::dns_validation::*;
use super::helpers::*;
use super::module::*;
use super::requests::*;
use super::state::*;
use super::status_handlers::*;

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::json;
use unified_core::pagination::PaginationParams;
use actix_web::{web, HttpRequest, HttpResponse};
use reqwest::Method;
use serde_json::json;
use serde_json::Value;
use std::sync::Arc;

pub(super) async fn list_dns(
    state: web::Data<CloudflareState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    zone_id: web::Path<String>,
    params: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| async {
        authorize(&ctx, &req, PERM_CLOUDFLARE_VIEW)?;
        let zone_id = zone_id.into_inner();
        validate_cf_id(&zone_id, "zone")?;
        let mut query = cf_query(&params);
        if let Some(record_type) = params.filters.get("type") {
            let record_type = record_type.trim().to_uppercase();
            if !CF_RECORD_TYPES.contains(&record_type.as_str()) {
                return Err(UnifiedError::bad_request(format!(
                    "Unsupported record type \"{record_type}\""
                )));
            }
            query.push(("type".to_string(), record_type));
        }
        if let Some(search) = &params.search {
            query.push(("name".to_string(), search.clone()));
        }
        if let Some(name) = params.filters.get("name") {
            query.push(("name".to_string(), name.clone()));
        }
        if let Some(content) = params.filters.get("content") {
            query.push(("content".to_string(), content.clone()));
        }
        if let Some(proxied) = params.filters.get("proxied") {
            if !matches!(proxied.as_str(), "true" | "false") {
                return Err(UnifiedError::bad_request(
                    "proxied filter must be \"true\" or \"false\"",
                ));
            }
            query.push(("proxied".to_string(), proxied.clone()));
        }
        query.push(("match".to_string(), "all".to_string()));
        let sort = params.allowed_sort(&["type", "name", "content", "ttl", "proxied"], "name");
        query.push(("order".to_string(), sort));
        query.push((
            "direction".to_string(),
            params.normalized().order.clone(),
        ));
        let envelope = cf_call(
            &state,
            Method::GET,
            &format!("/zones/{zone_id}/dns_records"),
            None,
            &query,
        )
        .await?;
        let items = cf_result(&envelope)
            .as_array()
            .cloned()
            .unwrap_or_default();
        Ok(cf_page(&envelope, items, &params))
    })()
    .await;
    json::respond(result, debug)
}

pub(super) async fn create_dns(
    state: web::Data<CloudflareState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    zone_id: web::Path<String>,
    input: web::Json<DnsRecordInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| async {
        authorize(&ctx, &req, PERM_CLOUDFLARE_MANAGE)?;
        let zone_id = zone_id.into_inner();
        validate_cf_id(&zone_id, "zone")?;
        let input = input.into_inner();
        let body = dns_record_body(
            &input.record_type,
            &input.name,
            &input.content,
            input.ttl,
            input.proxied,
            input.priority,
            &input.comment,
        )?;
        let record_type = body["type"].as_str().unwrap_or("").to_string();
        let name = body["name"].as_str().unwrap_or("").to_string();
        let envelope = cf_call(
            &state,
            Method::POST,
            &format!("/zones/{zone_id}/dns_records"),
            Some(body),
            &[],
        )
        .await?;
        let created = cf_result(&envelope);
        let record_id = created
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        ctx.hooks.emit(
            "cloudflare.dns.changed",
            json!({
                "action": "created",
                "zone_id": zone_id,
                "record_id": record_id,
                "type": record_type,
                "name": name,
            }),
        );
        Ok(created)
    })()
    .await;
    match result {
        Ok(record) => json::success_response(record, 201),
        Err(error) => json::error_response(&error, debug),
    }
}

pub(super) async fn get_dns(
    state: web::Data<CloudflareState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    path: web::Path<(String, String)>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| async {
        authorize(&ctx, &req, PERM_CLOUDFLARE_VIEW)?;
        let (zone_id, record_id) = path.into_inner();
        validate_cf_id(&zone_id, "zone")?;
        validate_cf_id(&record_id, "record")?;
        let envelope = cf_call(
            &state,
            Method::GET,
            &format!("/zones/{zone_id}/dns_records/{record_id}"),
            None,
            &[],
        )
        .await?;
        Ok(cf_result(&envelope))
    })()
    .await;
    json::respond(result, debug)
}

pub(super) async fn update_dns(
    state: web::Data<CloudflareState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    path: web::Path<(String, String)>,
    input: web::Json<UpdateDnsRecordInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| async {
        authorize(&ctx, &req, PERM_CLOUDFLARE_MANAGE)?;
        let (zone_id, record_id) = path.into_inner();
        validate_cf_id(&zone_id, "zone")?;
        validate_cf_id(&record_id, "record")?;
        let input = input.into_inner();
        // Read the current record first so omitted fields keep their
        // values, and so a missing record returns 404 before any
        // validation of the update body.
        let current_envelope = cf_call(
            &state,
            Method::GET,
            &format!("/zones/{zone_id}/dns_records/{record_id}"),
            None,
            &[],
        )
        .await?;
        let current = cf_result(&current_envelope);
        let record_type = input
            .record_type
            .clone()
            .or_else(|| current.get("type").and_then(Value::as_str).map(str::to_string))
            .unwrap_or_default();
        let name = input
            .name
            .clone()
            .or_else(|| current.get("name").and_then(Value::as_str).map(str::to_string))
            .unwrap_or_default();
        let content = input
            .content
            .clone()
            .or_else(|| current.get("content").and_then(Value::as_str).map(str::to_string))
            .unwrap_or_default();
        let ttl = input.ttl.or_else(|| {
            current
                .get("ttl")
                .and_then(Value::as_u64)
                .map(|ttl| ttl as u32)
        }).unwrap_or(1);
        let proxied = input.proxied.or_else(|| {
            current.get("proxied").and_then(Value::as_bool)
        }).unwrap_or(false);
        let priority = input.priority.or_else(|| {
            current
                .get("priority")
                .and_then(Value::as_u64)
                .map(|priority| priority as u16)
        });
        let comment = input.comment.or_else(|| {
            current
                .get("comment")
                .and_then(Value::as_str)
                .map(str::to_string)
        });
        let body = dns_record_body(
            &record_type,
            &name,
            &content,
            ttl,
            proxied,
            priority,
            &comment,
        )?;
        let envelope = cf_call(
            &state,
            Method::PUT,
            &format!("/zones/{zone_id}/dns_records/{record_id}"),
            Some(body),
            &[],
        )
        .await?;
        let updated = cf_result(&envelope);
        ctx.hooks.emit(
            "cloudflare.dns.changed",
            json!({
                "action": "updated",
                "zone_id": zone_id,
                "record_id": record_id,
                "type": record_type,
                "name": name,
            }),
        );
        Ok(updated)
    })()
    .await;
    json::respond(result, debug)
}

pub(super) async fn delete_dns(
    state: web::Data<CloudflareState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    path: web::Path<(String, String)>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| async {
        authorize(&ctx, &req, PERM_CLOUDFLARE_MANAGE)?;
        let (zone_id, record_id) = path.into_inner();
        validate_cf_id(&zone_id, "zone")?;
        validate_cf_id(&record_id, "record")?;
        // Read the record first so the change event carries its type
        // and name, and so a missing record returns 404.
        let current_envelope = cf_call(
            &state,
            Method::GET,
            &format!("/zones/{zone_id}/dns_records/{record_id}"),
            None,
            &[],
        )
        .await?;
        let current = cf_result(&current_envelope);
        let record_type = current
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let name = current
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        cf_call(
            &state,
            Method::DELETE,
            &format!("/zones/{zone_id}/dns_records/{record_id}"),
            None,
            &[],
        )
        .await?;
        ctx.hooks.emit(
            "cloudflare.dns.changed",
            json!({
                "action": "deleted",
                "zone_id": zone_id,
                "record_id": record_id,
                "type": record_type,
                "name": name,
            }),
        );
        Ok(json!({ "deleted": true, "id": record_id }))
    })()
    .await;
    json::respond(result, debug)
}
