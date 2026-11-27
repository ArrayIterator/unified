//! Cloudflare connector module. - Handlers: accounts and zones.
//!
//! Split from `src/modules/cloudflare.rs`; behaviour unchanged.

use super::cache_handlers::*;
use super::client::*;
use super::constants::*;
use super::dns_handlers::*;
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

pub(super) async fn list_accounts(
    state: web::Data<CloudflareState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    params: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| async {
        authorize(&ctx, &req, PERM_CLOUDFLARE_VIEW)?;
        // The accounts endpoint supports page/per_page only, so a
        // free-text search is applied to the returned page.
        let envelope = cf_call(&state, Method::GET, "/accounts", None, &cf_query(&params)).await?;
        let mut items = cf_result(&envelope)
            .as_array()
            .cloned()
            .unwrap_or_default();
        if let Some(search) = &params.search {
            let needle = search.to_lowercase();
            items.retain(|account| {
                account
                    .get("name")
                    .and_then(Value::as_str)
                    .map(|name| name.to_lowercase().contains(&needle))
                    .unwrap_or(false)
            });
        }
        Ok(cf_page(&envelope, items, &params))
    })()
    .await;
    json::respond(result, debug)
}

pub(super) async fn list_zones(
    state: web::Data<CloudflareState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    params: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| async {
        authorize(&ctx, &req, PERM_CLOUDFLARE_VIEW)?;
        let mut query = cf_query(&params);
        // Cloudflare matches `name` exactly; `status` and `account.id`
        // are exact filters too.
        if let Some(search) = &params.search {
            query.push(("name".to_string(), search.clone()));
        }
        if let Some(status) = params.filters.get("status") {
            if !matches!(status.as_str(), "active" | "pending" | "initializing" | "moved" | "deleted" | "deactivated") {
                return Err(UnifiedError::bad_request(format!(
                    "Unknown zone status \"{status}\""
                )));
            }
            query.push(("status".to_string(), status.clone()));
        }
        if let Some(account_id) = params.filters.get("account_id") {
            validate_cf_id(account_id, "account")?;
            query.push(("account.id".to_string(), account_id.clone()));
        }
        let sort = params.allowed_sort(&["name", "status"], "name");
        query.push(("order".to_string(), sort));
        query.push((
            "direction".to_string(),
            params.normalized().order.clone(),
        ));
        let envelope = cf_call(&state, Method::GET, "/zones", None, &query).await?;
        let items = cf_result(&envelope)
            .as_array()
            .cloned()
            .unwrap_or_default();
        Ok(cf_page(&envelope, items, &params))
    })()
    .await;
    json::respond(result, debug)
}

pub(super) async fn get_zone(
    state: web::Data<CloudflareState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    zone_id: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| async {
        authorize(&ctx, &req, PERM_CLOUDFLARE_VIEW)?;
        let zone_id = zone_id.into_inner();
        validate_cf_id(&zone_id, "zone")?;
        let envelope = cf_call(
            &state,
            Method::GET,
            &format!("/zones/{zone_id}"),
            None,
            &[],
        )
        .await?;
        Ok(cf_result(&envelope))
    })()
    .await;
    json::respond(result, debug)
}
