// crm — handlers contacts.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::HookBus;
use unified_core::json;
use unified_core::modules::{Module, ModuleRegistry, RouteInfo};
use unified_core::pagination::{PageResult, PaginationParams};
use actix_web::{web, HttpRequest, HttpResponse};
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::sync::{Arc, RwLock};
use uuid::Uuid;
use super::*;
use super::types::*;
use super::store::*;
use super::common::*;
use super::handlers_leads::*;
use super::handlers_activities::*;
use super::module::*;

// ---------------------------------------------------------------------------
// Contact handlers
// ---------------------------------------------------------------------------

pub(crate) async fn list_contacts(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<CrmState>>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return json::error_response(&error, ctx.is_debug()),
    };
    let _company = match caller_company(&tenant, &req) {
        Ok(company) => company,
        Err(error) => return json::error_response(&error, ctx.is_debug()),
    };
    if let Err(error) = authorize(&ctx, &req, PERM_CRM_READ) {
        return json::error_response(&error, ctx.is_debug());
    }
    let params = query.into_inner();
    let mut items: Vec<Contact> = state
        .contacts
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|contact| {
            matches_search(
                &format!(
                    "{} {} {}",
                    contact.name,
                    contact.email,
                    contact.company.clone().unwrap_or_default()
                ),
                &params,
            ) && params.filters.get("company").is_none_or(|company| {
                contact.company.as_deref() == Some(company.as_str())
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

pub(crate) async fn create_contact(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<CrmState>>,
    req: HttpRequest,
    body: web::Json<CreateContact>,
) -> HttpResponse {
    let tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return json::error_response(&error, ctx.is_debug()),
    };
    let _company = match require_caller_company(&tenant, &req) {
        Ok(company) => company,
        Err(error) => return json::error_response(&error, ctx.is_debug()),
    };
    if let Err(error) = authorize(&ctx, &req, PERM_CRM_MANAGE) {
        return json::error_response(&error, ctx.is_debug());
    }
    let result: UnifiedResult<Contact> = (|| {
        let input = body.into_inner();
        if input.name.trim().is_empty() {
            return Err(UnifiedError::bad_request("Contact name must not be empty"));
        }
        if !valid_email(&input.email) {
            return Err(UnifiedError::bad_request("Contact email is not valid"));
        }
        let contact = Contact {
            id: new_id(),
            name: input.name.trim().to_string(),
            email: input.email.trim().to_string(),
            company: clean_optional(input.company),
            phone: clean_optional(input.phone),
            created_at: now(),
            updated_at: now(),
        };
        state.contacts.insert(contact.id.clone(), contact.clone());
        Ok(contact)
    })();
    respond_created(result)
}

pub(crate) async fn get_contact(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<CrmState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return json::error_response(&error, ctx.is_debug()),
    };
    let _company = match caller_company(&tenant, &req) {
        Ok(company) => company,
        Err(error) => return json::error_response(&error, ctx.is_debug()),
    };
    if let Err(error) = authorize(&ctx, &req, PERM_CRM_READ) {
        return json::error_response(&error, ctx.is_debug());
    }
    let result: UnifiedResult<Contact> = state
        .contacts
        .get(path.as_str())
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found("Contact not found"));
    json::respond(result, ctx.is_debug())
}

pub(crate) async fn update_contact(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<CrmState>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdateContact>,
) -> HttpResponse {
    let tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return json::error_response(&error, ctx.is_debug()),
    };
    let _company = match require_caller_company(&tenant, &req) {
        Ok(company) => company,
        Err(error) => return json::error_response(&error, ctx.is_debug()),
    };
    if let Err(error) = authorize(&ctx, &req, PERM_CRM_MANAGE) {
        return json::error_response(&error, ctx.is_debug());
    }
    let result: UnifiedResult<Contact> = (|| {
        let input = body.into_inner();
        let mut contact = state
            .contacts
            .get_mut(path.as_str())
            .ok_or_else(|| UnifiedError::not_found("Contact not found"))?;
        if let Some(name) = input.name {
            if name.trim().is_empty() {
                return Err(UnifiedError::bad_request("Contact name must not be empty"));
            }
            contact.name = name.trim().to_string();
        }
        if let Some(email) = input.email {
            if !valid_email(&email) {
                return Err(UnifiedError::bad_request("Contact email is not valid"));
            }
            contact.email = email.trim().to_string();
        }
        if let Some(company) = input.company {
            // Empty/whitespace-only clears the field back to `None`.
            contact.company = clean_optional(Some(company));
        }
        if let Some(phone) = input.phone {
            contact.phone = clean_optional(Some(phone));
        }
        contact.updated_at = now();
        Ok(contact.clone())
    })();
    json::respond(result, ctx.is_debug())
}

pub(crate) async fn delete_contact(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<CrmState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return json::error_response(&error, ctx.is_debug()),
    };
    let _company = match require_caller_company(&tenant, &req) {
        Ok(company) => company,
        Err(error) => return json::error_response(&error, ctx.is_debug()),
    };
    if let Err(error) = authorize(&ctx, &req, PERM_CRM_MANAGE) {
        return json::error_response(&error, ctx.is_debug());
    }
    let result: UnifiedResult<Value> = (|| {
        state
            .contacts
            .remove(path.as_str())
            .map(|_| {
                // Cascade: no orphaned activities on a deleted record.
                delete_activities_for(&state, path.as_str());
                json!({ "deleted": true, "id": path.as_str() })
            })
            .ok_or_else(|| UnifiedError::not_found("Contact not found"))
    })();
    json::respond(result, ctx.is_debug())
}
