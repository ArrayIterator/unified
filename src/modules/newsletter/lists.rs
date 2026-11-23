// Newsletter module - list handlers.
//
// Split from `src/modules/newsletter.rs`; the module root re-exports the public API of this module.

use unified_core::error::UnifiedError;
use unified_core::json as api_json;
use unified_core::pagination::PaginationParams;
use actix_web::{web, HttpRequest, HttpResponse};
use serde::Deserialize;
use serde_json::json;
use std::sync::Arc;
use uuid::Uuid;

use super::constants::{PERM_NEWSLETTER_MANAGE, PERM_NEWSLETTER_VIEW};
use super::helpers::{matches_search, now_string, paginate, validate_connection_id, validate_email_address};
use super::state::{
    NewsletterState, authorize, caller_company, campaigns_store, lists_store, require_tenant,
    subscribers_store,
};
use super::types::{NewsletterList};

// ---------------------------------------------------------------------------
// List handlers.
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub(crate) struct ListInput {
    pub(crate) name: String,
    pub(crate) description: Option<String>,
    pub(crate) default_connection_id: Option<String>,
    pub(crate) double_opt_in: Option<bool>,
    pub(crate) sender_name: Option<String>,
    pub(crate) sender_email: Option<String>,
    pub(crate) base_url: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct ListUpdateInput {
    pub(crate) name: Option<String>,
    pub(crate) description: Option<String>,
    pub(crate) default_connection_id: Option<String>,
    pub(crate) double_opt_in: Option<bool>,
    pub(crate) sender_name: Option<String>,
    pub(crate) sender_email: Option<String>,
    pub(crate) base_url: Option<String>,
}

pub(crate) async fn list_lists(
    req: HttpRequest,
    state: web::Data<Arc<NewsletterState>>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = state.is_debug();
    let _tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return api_json::error_response(&error, debug),
    };
    if let Err(error) = caller_company(&_tenant, &req) {
        return api_json::error_response(&error, debug);
    }
    if let Err(error) = authorize(&req, &state, PERM_NEWSLETTER_VIEW) {
        return api_json::error_response(&error, debug);
    }
    let params = query.into_inner();
    let mut items: Vec<NewsletterList> = lists_store()
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|list| matches_search(&format!("{} {}", list.name, list.description), &params))
        .collect();
    items.sort_by(|a, b| a.created_at.cmp(&b.created_at));
    if params.normalized().order == "desc" {
        items.reverse();
    }
    let page = paginate(items, &params);
    api_json::success_response(page.to_json(), 200)
}

pub(crate) async fn create_list(
    req: HttpRequest,
    state: web::Data<Arc<NewsletterState>>,
    body: web::Json<ListInput>,
) -> HttpResponse {
    let debug = state.is_debug();
    let _tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return api_json::error_response(&error, debug),
    };
    if let Err(error) = caller_company(&_tenant, &req) {
        return api_json::error_response(&error, debug);
    }
    if let Err(error) = authorize(&req, &state, PERM_NEWSLETTER_MANAGE) {
        return api_json::error_response(&error, debug);
    }
    let input = body.into_inner();
    if input.name.trim().is_empty() {
        return api_json::error_response(
            &UnifiedError::bad_request("List name must not be empty"),
            debug,
        );
    }
    if let Err(error) = validate_connection_id(&input.default_connection_id) {
        return api_json::error_response(&error, debug);
    }
    if let Some(email) = &input.sender_email {
        if let Err(error) = validate_email_address(email) {
            return api_json::error_response(&error, debug);
        }
    }
    let now = now_string();
    let list = NewsletterList {
        id: Uuid::now_v7().to_string(),
        name: input.name.trim().to_string(),
        description: input.description.unwrap_or_default(),
        default_connection_id: input.default_connection_id.filter(|v| !v.trim().is_empty()),
        double_opt_in: input.double_opt_in.unwrap_or(true),
        sender_name: input.sender_name.filter(|v| !v.trim().is_empty()),
        sender_email: input.sender_email.filter(|v| !v.trim().is_empty()),
        base_url: input.base_url.filter(|v| !v.trim().is_empty()),
        created_at: now.clone(),
        updated_at: now,
    };
    lists_store().insert(list.id.clone(), list.clone());
    api_json::success_response(list, 201)
}

pub(crate) async fn get_list(
    req: HttpRequest,
    state: web::Data<Arc<NewsletterState>>,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = state.is_debug();
    let _tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return api_json::error_response(&error, debug),
    };
    if let Err(error) = caller_company(&_tenant, &req) {
        return api_json::error_response(&error, debug);
    }
    if let Err(error) = authorize(&req, &state, PERM_NEWSLETTER_VIEW) {
        return api_json::error_response(&error, debug);
    }
    match lists_store().get(path.as_str()).map(|entry| entry.value().clone()) {
        Some(list) => api_json::success_response(list, 200),
        None => api_json::error_response(&UnifiedError::not_found("Newsletter list not found"), debug),
    }
}

pub(crate) async fn update_list(
    req: HttpRequest,
    state: web::Data<Arc<NewsletterState>>,
    path: web::Path<String>,
    body: web::Json<ListUpdateInput>,
) -> HttpResponse {
    let debug = state.is_debug();
    let _tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return api_json::error_response(&error, debug),
    };
    if let Err(error) = caller_company(&_tenant, &req) {
        return api_json::error_response(&error, debug);
    }
    if let Err(error) = authorize(&req, &state, PERM_NEWSLETTER_MANAGE) {
        return api_json::error_response(&error, debug);
    }
    let input = body.into_inner();
    if let Err(error) = validate_connection_id(&input.default_connection_id) {
        return api_json::error_response(&error, debug);
    }
    if let Some(email) = &input.sender_email {
        if let Err(error) = validate_email_address(email) {
            return api_json::error_response(&error, debug);
        }
    }
    let mut lists = lists_store();
    let Some(mut list) = lists.get_mut(path.as_str()) else {
        return api_json::error_response(&UnifiedError::not_found("Newsletter list not found"), debug);
    };
    if let Some(name) = input.name {
        if name.trim().is_empty() {
            return api_json::error_response(&UnifiedError::bad_request("List name must not be empty"), debug);
        }
        list.name = name.trim().to_string();
    }
    if let Some(description) = input.description {
        list.description = description;
    }
    if let Some(connection_id) = input.default_connection_id {
        list.default_connection_id = if connection_id.trim().is_empty() { None } else { Some(connection_id) };
    }
    if let Some(double_opt_in) = input.double_opt_in {
        list.double_opt_in = double_opt_in;
    }
    if let Some(sender_name) = input.sender_name {
        list.sender_name = if sender_name.trim().is_empty() { None } else { Some(sender_name) };
    }
    if let Some(sender_email) = input.sender_email {
        list.sender_email = if sender_email.trim().is_empty() { None } else { Some(sender_email) };
    }
    if let Some(base_url) = input.base_url {
        list.base_url = if base_url.trim().is_empty() { None } else { Some(base_url) };
    }
    list.updated_at = now_string();
    let list = list.clone();
    drop(lists);
    api_json::success_response(list, 200)
}

pub(crate) async fn delete_list(
    req: HttpRequest,
    state: web::Data<Arc<NewsletterState>>,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = state.is_debug();
    let _tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return api_json::error_response(&error, debug),
    };
    if let Err(error) = caller_company(&_tenant, &req) {
        return api_json::error_response(&error, debug);
    }
    if let Err(error) = authorize(&req, &state, PERM_NEWSLETTER_MANAGE) {
        return api_json::error_response(&error, debug);
    }
    let id = path.into_inner();
    match lists_store().remove(&id) {
        Some(_) => {
            let subscriber_ids: Vec<String> = subscribers_store()
                .iter()
                .filter(|entry| entry.value().list_id == id)
                .map(|entry| entry.key().clone())
                .collect();
            for subscriber_id in &subscriber_ids {
                subscribers_store().remove(subscriber_id);
            }
            let campaign_ids: Vec<String> = campaigns_store()
                .iter()
                .filter(|entry| entry.value().list_id == id)
                .map(|entry| entry.key().clone())
                .collect();
            for campaign_id in &campaign_ids {
                campaigns_store().remove(campaign_id);
            }
            api_json::success_response(
                json!({
                    "deleted": true,
                    "id": id,
                    "removed_subscribers": subscriber_ids.len(),
                    "removed_campaigns": campaign_ids.len(),
                }),
                200,
            )
        }
        None => api_json::error_response(&UnifiedError::not_found("Newsletter list not found"), debug),
    }
}
