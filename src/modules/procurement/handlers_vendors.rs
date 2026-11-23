// procurement -- Vendor handlers.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::pagination::PaginationParams;
use actix_web::{HttpRequest, HttpResponse, web};
use chrono::Utc;
use serde_json::{Value, json};
use std::sync::Arc;
use uuid::Uuid;

use super::helpers::*;
use super::state::*;
use super::types::*;

// ---------------------------------------------------------------------------
// Vendor handlers
// ---------------------------------------------------------------------------

pub(crate) async fn list_vendors(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ProcurementState>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PROCUREMENT_VIEW)?;
        let params = query.into_inner();
        let status_filter = params.filters.get("status").cloned();
        let category_filter = params.filters.get("category").cloned();
        let search = params.search.clone().unwrap_or_default().to_lowercase();
        let mut items: Vec<Value> = state
            .vendors
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|vendor| {
                if let Some(status) = &status_filter {
                    if vendor.status.as_str() != status.as_str() {
                        return false;
                    }
                }
                if let Some(category) = &category_filter {
                    if !vendor.categories.iter().any(|entry| entry == category) {
                        return false;
                    }
                }
                if !search.is_empty() {
                    let haystack = format!(
                        "{} {} {}",
                        vendor.name,
                        vendor.contact_name.clone().unwrap_or_default(),
                        vendor.email.clone().unwrap_or_default()
                    )
                    .to_lowercase();
                    if !haystack.contains(&search) {
                        return false;
                    }
                }
                true
            })
            .map(|vendor| vendor_view(&vendor))
            .collect();
        let sort = params.allowed_sort(&["name", "created_at"], "created_at");
        let descending = params.normalized().order == "desc";
        items.sort_by(|a, b| {
            let ordering = match sort.as_str() {
                "name" => a
                    .get("name")
                    .and_then(Value::as_str)
                    .cmp(&b.get("name").and_then(Value::as_str)),
                _ => a
                    .get("created_at")
                    .and_then(Value::as_str)
                    .cmp(&b.get("created_at").and_then(Value::as_str)),
            };
            if descending {
                ordering.reverse()
            } else {
                ordering
            }
        });
        Ok(paginate(items, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn create_vendor(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ProcurementState>,
    req: HttpRequest,
    body: web::Json<CreateVendorRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = require_caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PROCUREMENT_MANAGE)?;
        let input = body.into_inner();
        let name = require_text(&input.name, "name", MAX_NAME_LEN)?;
        let contact_name = match &input.contact_name {
            Some(raw) => Some(require_text(raw, "contact_name", MAX_NAME_LEN)?),
            None => None,
        };
        let email = match &input.email {
            Some(raw) => {
                let cleaned = require_text(raw, "email", MAX_EMAIL_LEN)?;
                if !valid_email(&cleaned) {
                    return Err(UnifiedError::bad_request(
                        "email must be a valid email address",
                    ));
                }
                Some(cleaned)
            }
            None => None,
        };
        let phone = match &input.phone {
            Some(raw) => Some(require_text(raw, "phone", MAX_PHONE_LEN)?),
            None => None,
        };
        let address = match &input.address {
            Some(raw) => Some(require_text(raw, "address", MAX_ADDRESS_LEN)?),
            None => None,
        };
        let categories = validate_categories(&input.categories)?;
        let tags = validate_tags(&input.tags)?;
        let payment_reference = match &input.payment_reference {
            Some(raw) => Some(require_text(raw, "payment_reference", MAX_REFERENCE_LEN)?),
            None => None,
        };
        let notes = match &input.notes {
            Some(raw) => Some(require_text(raw, "notes", MAX_NOTE_LEN)?),
            None => None,
        };
        let rating = match input.rating {
            Some(value) => Some(validate_rating(value)?),
            None => None,
        };
        let now = Utc::now();
        let vendor = Vendor {
            id: Uuid::now_v7(),
            name,
            contact_name,
            email,
            phone,
            address,
            categories,
            tags,
            status: input.status.unwrap_or_default(),
            payment_reference,
            notes,
            rating,
            created_by: caller.actor.clone(),
            created_at: now,
            updated_at: now,
        };
        state.vendors.insert(vendor.id, vendor.clone());
        state.emit(
            "procurement.vendor.created",
            json!({
                "vendor_id": vendor.id.to_string(),
                "name": vendor.name,
                "status": vendor.status.as_str(),
                "created_by": caller.actor,
            }),
        );
        Ok(vendor_view(&vendor))
    })();
    match result {
        Ok(data) => created(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn get_vendor(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ProcurementState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PROCUREMENT_VIEW)?;
        let vendor = load_vendor(&state, &path.into_inner())?;
        Ok(vendor_view(&vendor))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn update_vendor(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ProcurementState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdateVendorRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = require_caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PROCUREMENT_MANAGE)?;
        let mut vendor = load_vendor(&state, &path.into_inner())?;
        let input = body.into_inner();
        if let Some(name) = &input.name {
            vendor.name = require_text(name, "name", MAX_NAME_LEN)?;
        }
        if let Some(contact_name) = &input.contact_name {
            vendor.contact_name = match contact_name {
                Some(raw) => Some(require_text(raw, "contact_name", MAX_NAME_LEN)?),
                None => None,
            };
        }
        if let Some(email) = &input.email {
            vendor.email = match email {
                Some(raw) => {
                    let cleaned = require_text(raw, "email", MAX_EMAIL_LEN)?;
                    if !valid_email(&cleaned) {
                        return Err(UnifiedError::bad_request(
                            "email must be a valid email address",
                        ));
                    }
                    Some(cleaned)
                }
                None => None,
            };
        }
        if let Some(phone) = &input.phone {
            vendor.phone = match phone {
                Some(raw) => Some(require_text(raw, "phone", MAX_PHONE_LEN)?),
                None => None,
            };
        }
        if let Some(address) = &input.address {
            vendor.address = match address {
                Some(raw) => Some(require_text(raw, "address", MAX_ADDRESS_LEN)?),
                None => None,
            };
        }
        if let Some(categories) = &input.categories {
            vendor.categories = validate_categories(categories)?;
        }
        if let Some(tags) = &input.tags {
            vendor.tags = validate_tags(tags)?;
        }
        if let Some(status) = input.status {
            vendor.status = status;
        }
        if let Some(payment_reference) = &input.payment_reference {
            vendor.payment_reference = match payment_reference {
                Some(raw) => Some(require_text(raw, "payment_reference", MAX_REFERENCE_LEN)?),
                None => None,
            };
        }
        if let Some(notes) = &input.notes {
            vendor.notes = match notes {
                Some(raw) => Some(require_text(raw, "notes", MAX_NOTE_LEN)?),
                None => None,
            };
        }
        if let Some(rating) = &input.rating {
            vendor.rating = match rating {
                Some(value) => Some(validate_rating(*value)?),
                None => None,
            };
        }
        vendor.updated_at = Utc::now();
        state.vendors.insert(vendor.id, vendor.clone());
        state.emit(
            "procurement.vendor.updated",
            json!({
                "vendor_id": vendor.id.to_string(),
                "name": vendor.name,
                "updated_by": caller.actor,
            }),
        );
        Ok(vendor_view(&vendor))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn delete_vendor(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ProcurementState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = require_caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PROCUREMENT_MANAGE)?;
        let vendor = load_vendor(&state, &path.into_inner())?;
        if state
            .purchase_orders
            .iter()
            .any(|entry| entry.value().vendor_id == vendor.id)
        {
            return Err(UnifiedError::conflict(
                "Vendor has purchase orders and cannot be deleted; set status to inactive instead",
            ));
        }
        if vendor_has_budgets(&state, vendor.id) {
            return Err(UnifiedError::conflict(
                "Vendor has budgets and cannot be deleted; set status to inactive instead",
            ));
        }
        state.vendors.remove(&vendor.id);
        state.emit(
            "procurement.vendor.deleted",
            json!({
                "vendor_id": vendor.id.to_string(),
                "name": vendor.name,
                "deleted_by": caller.actor,
            }),
        );
        Ok(json!({ "deleted": true, "vendor_id": vendor.id.to_string() }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

/// Vendor performance: on-time delivery rate computed from PO
/// promised dates vs receipt dates, total committed spend, PO
/// counts, and recent quality notes from receipts.
pub(crate) async fn vendor_performance(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ProcurementState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PROCUREMENT_VIEW)?;
        let vendor = load_vendor(&state, &path.into_inner())?;
        let pos: Vec<PurchaseOrder> = state
            .purchase_orders
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|po| po.vendor_id == vendor.id)
            .collect();
        let total_pos = pos.len() as i64;
        let committed_pos = pos.iter().filter(|po| is_committed(po.status)).count() as i64;
        // Committed spend is integer minor units. Vendor spend
        // spans currencies, as before, so the JSON carries major
        // units with the standard two-decimal fallback.
        let total_spend: i64 = pos
            .iter()
            .filter(|po| is_committed(po.status))
            .map(|po| po.total)
            .sum();
        // On-time rate: receipts of this vendor's POs that have a
        // promised date, where the receipt date is on or before it.
        let mut on_time = 0i64;
        let mut measured = 0i64;
        let mut quality_notes: Vec<Value> = Vec::new();
        for po in &pos {
            let Some(promised) = po.promised_date else {
                continue;
            };
            for receipt in receipts_of_po(&state, po.id) {
                measured += 1;
                if receipt.received_at.date_naive() <= promised {
                    on_time += 1;
                }
                if let Some(note) = &receipt.quality_note {
                    quality_notes.push(json!({
                        "purchase_order_id": po.id.to_string(),
                        "po_number": po.po_number,
                        "receipt_id": receipt.id.to_string(),
                        "note": note,
                        "received_at": receipt.received_at.to_rfc3339(),
                    }));
                }
            }
        }
        quality_notes.sort_by(|a, b| {
            b.get("received_at")
                .and_then(Value::as_str)
                .cmp(&a.get("received_at").and_then(Value::as_str))
        });
        quality_notes.truncate(20);
        let on_time_rate = if measured > 0 {
            (on_time as f64 / measured as f64 * 10_000.0).round() / 10_000.0
        } else {
            0.0
        };
        Ok(json!({
            "vendor_id": vendor.id.to_string(),
            "name": vendor.name,
            "status": vendor.status.as_str(),
            "rating": vendor.rating,
            "total_purchase_orders": total_pos,
            "committed_purchase_orders": committed_pos,
            "total_committed_spend": minor_to_major(total_spend, "USD"),
            "receipts_measured": measured,
            "on_time_receipts": on_time,
            "on_time_rate": on_time_rate,
            "quality_notes": quality_notes,
        }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}
