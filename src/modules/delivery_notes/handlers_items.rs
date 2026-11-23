//! HTTP handlers for delivery note items: list, add, read,
//! update, and remove. Items belong to a draft DO's snapshot:
//! lines may change while the DO is a draft; once issued, only
//! the return endpoint may change a quantity (the returned
//! count), never the ordered or shipped counts.

use actix_web::{web, HttpRequest, HttpResponse};
use chrono::Utc;
use serde::Deserialize;
use serde_json::Value;
use std::sync::Arc;
use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::pagination::PaginationParams;
use uuid::Uuid;

use super::helpers::{
    caller, created, err, load_item, load_note, note_event, ok, paginate, require_label,
    require_notes, require_reference, require_text, require_unit, caller_company,
    in_company_scope_uuid, require_company_match_uuid, require_note_in_company_scope,
    require_tenant,
};
use super::state::DeliveryNoteState;
use super::types::{
    CreateItemRequest, DeliveryNoteItem, DeliveryNoteStatus, UpdateItemRequest, EVENT_UPDATED,
    MAX_NOTES_LEN, MAX_REFERENCE_LEN, PERM_DELIVERY_NOTES_MANAGE, PERM_DELIVERY_NOTES_VIEW,
};

/// Path carrying a delivery note id.
#[derive(Debug, Deserialize)]
struct NotePath {
    note_id: String,
}

/// Path carrying a delivery note id and an item id.
#[derive(Debug, Deserialize)]
struct ItemPath {
    note_id: String,
    item_id: String,
}

/// List the items of one DO (paginated, filter by product_id).
pub(crate) async fn list_items(
    state: web::Data<DeliveryNoteState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    path: web::Path<NotePath>,
    params: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let tenant_id = _tenant.tenant_id.to_string();
        let caller = caller(&req)?;
        super::helpers::require_permission(&ctx, &caller.role, PERM_DELIVERY_NOTES_VIEW)?;
        let note = load_note(&state, &tenant_id, &path.note_id)?;
        require_note_in_company_scope(&note.company_id, &_scope)?;
        let params = params.into_inner();
        let product_filter = params.filters.get("product_id").cloned();
        let mut rows = state.note_items(note.id);
        if let Some(product_id) = product_filter {
            rows.retain(|item| item.product_id.as_deref() == Some(product_id.as_str()));
        }
        Ok(serde_json::to_value(paginate(rows, &params)).unwrap_or(Value::Null))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

/// Add one line to a draft DO. The line joins the snapshot:
/// ordered and shipped both start at the supplied quantity.
pub(crate) async fn add_item(
    state: web::Data<DeliveryNoteState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    path: web::Path<NotePath>,
    body: web::Json<CreateItemRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let tenant_id = _tenant.tenant_id.to_string();
        let caller = caller(&req)?;
        super::helpers::require_permission(&ctx, &caller.role, PERM_DELIVERY_NOTES_MANAGE)?;
        let note = load_note(&state, &tenant_id, &path.note_id)?;
        require_note_in_company_scope(&note.company_id, &_scope)?;
        if note.status != DeliveryNoteStatus::Draft {
            return Err(UnifiedError::conflict(format!(
                "Items can only be added to a draft delivery note (current status: {})",
                note.status.as_str()
            )));
        }
        let input = body.into_inner();
        if input.quantity <= 0 {
            return Err(UnifiedError::bad_request(
                "Item quantity must be above zero",
            ));
        }
        let description = require_label(&input.description)?;
        let unit = require_unit(&input.unit)?;
        let product_id = match &input.product_id {
            Some(id) => Some(require_reference(id, "product_id")?),
            None => None,
        };
        let notes = require_notes(&input.notes)?;
        let now = Utc::now();
        let item = DeliveryNoteItem {
            id: Uuid::new_v4(),
            tenant_id: tenant_id.clone(),
            delivery_note_id: note.id,
            product_id,
            description,
            quantity_ordered: input.quantity,
            quantity_shipped: input.quantity,
            quantity_returned: 0,
            unit,
            notes,
            created_at: now,
            updated_at: now,
        };
        state.items.insert(item.id, item.clone());
        let mut updated = note.clone();
        updated.updated_at = now;
        state.notes.insert(updated.id, updated.clone());
        note_event(&state, EVENT_UPDATED, &updated, &caller.actor, None);
        Ok(serde_json::to_value(item).unwrap_or(Value::Null))
    })();
    match result {
        Ok(data) => created(data),
        Err(error) => err(error, debug),
    }
}

/// Get one item of one DO.
pub(crate) async fn get_item(
    state: web::Data<DeliveryNoteState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    path: web::Path<ItemPath>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let tenant_id = _tenant.tenant_id.to_string();
        let caller = caller(&req)?;
        super::helpers::require_permission(&ctx, &caller.role, PERM_DELIVERY_NOTES_VIEW)?;
        let note = load_note(&state, &tenant_id, &path.note_id)?;
        require_note_in_company_scope(&note.company_id, &_scope)?;
        let item = load_item(&state, &tenant_id, note.id, &path.item_id)?;
        Ok(serde_json::to_value(item).unwrap_or(Value::Null))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

/// Update one line of a draft DO. Ordered and shipped stay in
/// step unless the caller changes one explicitly (a short
/// shipment corrects `quantity_shipped` down); returned is
/// never set here — only the return endpoint moves it.
pub(crate) async fn update_item(
    state: web::Data<DeliveryNoteState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    path: web::Path<ItemPath>,
    body: web::Json<UpdateItemRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let tenant_id = _tenant.tenant_id.to_string();
        let caller = caller(&req)?;
        super::helpers::require_permission(&ctx, &caller.role, PERM_DELIVERY_NOTES_MANAGE)?;
        let note = load_note(&state, &tenant_id, &path.note_id)?;
        require_note_in_company_scope(&note.company_id, &_scope)?;
        if note.status != DeliveryNoteStatus::Draft {
            return Err(UnifiedError::conflict(format!(
                "Items can only be updated on a draft delivery note (current status: {})",
                note.status.as_str()
            )));
        }
        let mut item = load_item(&state, &tenant_id, note.id, &path.item_id)?;
        let input = body.into_inner();
        if let Some(product_id) = input.product_id {
            item.product_id = match product_id {
                Some(id) => Some(require_reference(&id, "product_id")?),
                None => None,
            };
        }
        if let Some(description) = &input.description {
            item.description = require_label(description)?;
        }
        if let Some(quantity_ordered) = input.quantity_ordered {
            if quantity_ordered <= 0 {
                return Err(UnifiedError::bad_request(
                    "quantity_ordered must be above zero",
                ));
            }
            item.quantity_ordered = quantity_ordered;
        }
        if let Some(quantity_shipped) = input.quantity_shipped {
            if quantity_shipped < 0 {
                return Err(UnifiedError::bad_request(
                    "quantity_shipped must not be negative",
                ));
            }
            if quantity_shipped > item.quantity_ordered {
                return Err(UnifiedError::bad_request(
                    "quantity_shipped must not exceed quantity_ordered",
                ));
            }
            item.quantity_shipped = quantity_shipped;
        }
        if let Some(unit) = &input.unit {
            item.unit = require_unit(unit)?;
        }
        if let Some(notes) = input.notes {
            item.notes = match notes {
                Some(text) => Some(require_text(&text, "notes", MAX_NOTES_LEN)?),
                None => None,
            };
        }
        item.updated_at = Utc::now();
        state.items.insert(item.id, item.clone());
        let mut updated = note.clone();
        updated.updated_at = Utc::now();
        state.notes.insert(updated.id, updated.clone());
        note_event(&state, EVENT_UPDATED, &updated, &caller.actor, None);
        Ok(serde_json::to_value(item).unwrap_or(Value::Null))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

/// Remove one line from a draft DO. The last line cannot be
/// removed: a DO with no items is not a document — delete the
/// DO instead.
pub(crate) async fn delete_item(
    state: web::Data<DeliveryNoteState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    path: web::Path<ItemPath>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let tenant_id = _tenant.tenant_id.to_string();
        let caller = caller(&req)?;
        super::helpers::require_permission(&ctx, &caller.role, PERM_DELIVERY_NOTES_MANAGE)?;
        let note = load_note(&state, &tenant_id, &path.note_id)?;
        require_note_in_company_scope(&note.company_id, &_scope)?;
        if note.status != DeliveryNoteStatus::Draft {
            return Err(UnifiedError::conflict(format!(
                "Items can only be removed from a draft delivery note (current status: {})",
                note.status.as_str()
            )));
        }
        let item = load_item(&state, &tenant_id, note.id, &path.item_id)?;
        if state.note_items(note.id).len() <= 1 {
            return Err(UnifiedError::conflict(
                "A delivery note needs at least one item: delete the delivery note instead",
            ));
        }
        state.items.remove(&item.id);
        let mut updated = note.clone();
        updated.updated_at = Utc::now();
        state.notes.insert(updated.id, updated.clone());
        note_event(&state, EVENT_UPDATED, &updated, &caller.actor, None);
        Ok(serde_json::json!({ "deleted": item.id.to_string() }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}
