//! HTTP handlers for contact lists and their contacts:
//! named audiences of normalized E.164 numbers with tags
//! and per-contact template variables. One number per list;
//! a number may appear on many lists.

use actix_web::{HttpRequest, HttpResponse, web};
use chrono::Utc;
use serde_json::{Value, json};
use std::sync::Arc;
use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::pagination::PaginationParams;
use uuid::Uuid;

use super::helpers::{
    caller, caller_company, created, err, load_contact, load_contact_list, ok, optional_text,
    paginate, record_audit, require_caller_company, require_manage, require_phone, require_tenant,
    require_text, require_view, validate_tags, validate_variables,
};
use super::state::SmsMarketingState;
use super::types::{
    CreateContactListRequest, CreateContactRequest, MAX_DESCRIPTION_LEN, MAX_LIST_CONTACTS,
    MAX_NAME_LEN, SmsContact, SmsContactList, UpdateContactListRequest,
};

/// List contact lists (paginated, search by name).
pub(crate) async fn list_contact_lists(
    state: web::Data<SmsMarketingState>,
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let tenant = require_tenant(&req)?;
        let tenant_id = tenant.tenant_id.to_string();
        let _company = caller_company(&tenant, &req)?;
        let caller = caller(&req)?;
        require_view(&ctx, &caller.role)?;
        let params = query.into_inner();
        let search = params.search.clone().map(|needle| needle.to_lowercase());
        let mut items = state.contact_lists_of(&tenant_id);
        if let Some(needle) = &search {
            items.retain(|list| list.name.to_lowercase().contains(needle));
        }
        let views: Vec<Value> = items
            .iter()
            .map(|item| {
                let mut view = serde_json::to_value(item).unwrap_or(Value::Null);
                if let Some(object) = view.as_object_mut() {
                    object.insert(
                        "contact_count".to_string(),
                        Value::from(state.contacts_of_list(&tenant_id, item.id).len() as i64),
                    );
                }
                view
            })
            .collect();
        Ok(paginate(views, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

/// Create a contact list (manager).
pub(crate) async fn create_contact_list(
    state: web::Data<SmsMarketingState>,
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    body: web::Json<CreateContactListRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let tenant = require_tenant(&req)?;
        let tenant_id = tenant.tenant_id.to_string();
        let _company = require_caller_company(&tenant, &req)?;
        let caller = caller(&req)?;
        require_manage(&ctx, &caller.role)?;
        let input = body.into_inner();
        let name = require_text(&input.name, "name", MAX_NAME_LEN)?;
        let description = optional_text(&input.description, "description", MAX_DESCRIPTION_LEN)?;
        let now = Utc::now();
        let list = SmsContactList {
            id: Uuid::new_v4(),
            tenant_id: tenant_id.clone(),
            name,
            description,
            created_by: caller.actor.clone(),
            created_at: now,
            updated_at: now,
        };
        state.contact_lists.insert(list.id, list.clone());
        record_audit(
            &state,
            &tenant_id,
            "contact_list",
            &list.id.to_string(),
            "create",
            &caller.actor,
            None,
            Some(serde_json::to_value(&list).unwrap_or(Value::Null)),
        );
        Ok(serde_json::to_value(&list).unwrap_or(Value::Null))
    })();
    match result {
        Ok(data) => created(data),
        Err(error) => err(error, debug),
    }
}

/// Get one contact list, with its contact count.
pub(crate) async fn get_contact_list(
    state: web::Data<SmsMarketingState>,
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let tenant = require_tenant(&req)?;
        let tenant_id = tenant.tenant_id.to_string();
        let _company = caller_company(&tenant, &req)?;
        let caller = caller(&req)?;
        require_view(&ctx, &caller.role)?;
        let list = load_contact_list(&state, &tenant_id, &path.into_inner())?;
        let mut view = serde_json::to_value(&list).unwrap_or(Value::Null);
        if let Some(object) = view.as_object_mut() {
            object.insert(
                "contact_count".to_string(),
                Value::from(state.contacts_of_list(&tenant_id, list.id).len() as i64),
            );
        }
        Ok(view)
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

/// Update a contact list (manager).
pub(crate) async fn update_contact_list(
    state: web::Data<SmsMarketingState>,
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    path: web::Path<String>,
    body: web::Json<UpdateContactListRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let tenant = require_tenant(&req)?;
        let tenant_id = tenant.tenant_id.to_string();
        let _company = require_caller_company(&tenant, &req)?;
        let caller = caller(&req)?;
        require_manage(&ctx, &caller.role)?;
        let input = body.into_inner();
        let mut list = load_contact_list(&state, &tenant_id, &path.into_inner())?;
        let before = serde_json::to_value(&list).unwrap_or(Value::Null);
        if let Some(name) = &input.name {
            list.name = require_text(name, "name", MAX_NAME_LEN)?;
        }
        if let Some(description) = &input.description {
            list.description = match description {
                Some(text) => Some(require_text(text, "description", MAX_DESCRIPTION_LEN)?),
                None => None,
            };
        }
        list.updated_at = Utc::now();
        state.contact_lists.insert(list.id, list.clone());
        record_audit(
            &state,
            &tenant_id,
            "contact_list",
            &list.id.to_string(),
            "update",
            &caller.actor,
            Some(before),
            Some(serde_json::to_value(&list).unwrap_or(Value::Null)),
        );
        Ok(serde_json::to_value(&list).unwrap_or(Value::Null))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

/// Delete a contact list (manager). Its contacts are
/// deleted with it; campaigns that referenced the list keep
/// their other audiences (a missing list is skipped at send
/// time, never an error).
pub(crate) async fn delete_contact_list(
    state: web::Data<SmsMarketingState>,
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let tenant = require_tenant(&req)?;
        let tenant_id = tenant.tenant_id.to_string();
        let _company = require_caller_company(&tenant, &req)?;
        let caller = caller(&req)?;
        require_manage(&ctx, &caller.role)?;
        let list = load_contact_list(&state, &tenant_id, &path.into_inner())?;
        let contacts = state.contacts_of_list(&tenant_id, list.id);
        for contact in &contacts {
            state.contacts.remove(&contact.id);
        }
        state.contact_lists.remove(&list.id);
        record_audit(
            &state,
            &tenant_id,
            "contact_list",
            &list.id.to_string(),
            "delete",
            &caller.actor,
            Some(serde_json::to_value(&list).unwrap_or(Value::Null)),
            None,
        );
        Ok(json!({
            "deleted": true,
            "contact_list_id": list.id.to_string(),
            "contacts_deleted": contacts.len(),
        }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

/// List contacts of one list (paginated, filter by tag;
/// search by number / display name).
pub(crate) async fn list_contacts(
    state: web::Data<SmsMarketingState>,
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let tenant = require_tenant(&req)?;
        let tenant_id = tenant.tenant_id.to_string();
        let _company = caller_company(&tenant, &req)?;
        let caller = caller(&req)?;
        require_view(&ctx, &caller.role)?;
        let list = load_contact_list(&state, &tenant_id, &path.into_inner())?;
        let params = query.into_inner();
        let tag_filter = params.filters.get("tag").cloned();
        let search = params.search.clone().map(|needle| needle.to_lowercase());
        let mut items = state.contacts_of_list(&tenant_id, list.id);
        items.retain(|contact| {
            if let Some(tag) = &tag_filter
                && !contact.tags.iter().any(|t| t == tag)
            {
                return false;
            }
            if let Some(needle) = &search
                && !contact.phone_number.to_lowercase().contains(needle)
                && !contact
                    .display_name
                    .as_deref()
                    .unwrap_or("")
                    .to_lowercase()
                    .contains(needle)
            {
                return false;
            }
            true
        });
        let views: Vec<Value> = items
            .iter()
            .map(|item| serde_json::to_value(item).unwrap_or(Value::Null))
            .collect();
        Ok(paginate(views, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

/// Add a contact to a list (manager). One number per list;
/// adding the same number again is a 409.
pub(crate) async fn create_contact(
    state: web::Data<SmsMarketingState>,
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    path: web::Path<String>,
    body: web::Json<CreateContactRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let tenant = require_tenant(&req)?;
        let tenant_id = tenant.tenant_id.to_string();
        let _company = require_caller_company(&tenant, &req)?;
        let caller = caller(&req)?;
        require_manage(&ctx, &caller.role)?;
        let list = load_contact_list(&state, &tenant_id, &path.into_inner())?;
        let input = body.into_inner();
        let phone_number = require_phone(&input.phone_number)?;
        let display_name = optional_text(&input.display_name, "display_name", MAX_NAME_LEN)?;
        let tags = validate_tags(&input.tags)?;
        let variables = validate_variables(&input.variables)?;
        let existing = state.contacts_of_list(&tenant_id, list.id);
        if existing.len() >= MAX_LIST_CONTACTS {
            return Err(UnifiedError::bad_request(format!(
                "List already holds {MAX_LIST_CONTACTS} contacts"
            )));
        }
        if existing
            .iter()
            .any(|contact| contact.phone_number == phone_number)
        {
            return Err(UnifiedError::conflict(format!(
                "Number {phone_number} is already on this list"
            )));
        }
        let now = Utc::now();
        let contact = SmsContact {
            id: Uuid::new_v4(),
            tenant_id: tenant_id.clone(),
            list_id: list.id,
            phone_number,
            display_name,
            tags,
            variables,
            created_by: caller.actor.clone(),
            created_at: now,
            updated_at: now,
        };
        state.contacts.insert(contact.id, contact.clone());
        record_audit(
            &state,
            &tenant_id,
            "contact",
            &contact.id.to_string(),
            "create",
            &caller.actor,
            None,
            Some(serde_json::to_value(&contact).unwrap_or(Value::Null)),
        );
        Ok(serde_json::to_value(&contact).unwrap_or(Value::Null))
    })();
    match result {
        Ok(data) => created(data),
        Err(error) => err(error, debug),
    }
}

/// Delete a contact from its list (manager). The contact
/// must belong to the list named in the path; a contact of
/// another list reads as 404.
pub(crate) async fn delete_contact(
    state: web::Data<SmsMarketingState>,
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    path: web::Path<(String, String)>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let tenant = require_tenant(&req)?;
        let tenant_id = tenant.tenant_id.to_string();
        let _company = require_caller_company(&tenant, &req)?;
        let caller = caller(&req)?;
        require_manage(&ctx, &caller.role)?;
        let (list_id, contact_id) = path.into_inner();
        let list = load_contact_list(&state, &tenant_id, &list_id)?;
        let contact = load_contact(&state, &tenant_id, &contact_id)?;
        if contact.list_id != list.id {
            return Err(UnifiedError::not_found(format!(
                "Contact not found: {contact_id}"
            )));
        }
        state.contacts.remove(&contact.id);
        record_audit(
            &state,
            &tenant_id,
            "contact",
            &contact.id.to_string(),
            "delete",
            &caller.actor,
            Some(serde_json::to_value(&contact).unwrap_or(Value::Null)),
            None,
        );
        Ok(json!({ "deleted": true, "contact_id": contact.id.to_string() }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}
