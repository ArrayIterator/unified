// procurement -- Purchase order handlers.

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

// Purchase order handlers

pub(crate) async fn list_purchase_orders(
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
        let vendor_filter = params.filters.get("vendor_id").cloned();
        let category_filter = params.filters.get("category").cloned();
        let search = params.search.clone().unwrap_or_default().to_lowercase();
        let mut items: Vec<Value> = state
            .purchase_orders
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|po| {
                if let Some(status) = &status_filter {
                    if po.status.as_str() != status.as_str() {
                        return false;
                    }
                }
                if let Some(vendor_id) = &vendor_filter {
                    if po.vendor_id.to_string() != *vendor_id {
                        return false;
                    }
                }
                if let Some(category) = &category_filter {
                    if po.category.as_deref() != Some(category.as_str()) {
                        return false;
                    }
                }
                if !search.is_empty() {
                    let haystack = format!("{} {}", po.po_number, po.title).to_lowercase();
                    if !haystack.contains(&search) {
                        return false;
                    }
                }
                true
            })
            .map(|po| po_view(&state, &po))
            .collect();
        let sort = params.allowed_sort(&["po_number", "total", "created_at"], "created_at");
        let descending = params.normalized().order == "desc";
        items.sort_by(|a, b| {
            let ordering = match sort.as_str() {
                "po_number" => a
                    .get("po_number")
                    .and_then(Value::as_str)
                    .cmp(&b.get("po_number").and_then(Value::as_str)),
                "total" => a
                    .get("total")
                    .and_then(Value::as_f64)
                    .partial_cmp(&b.get("total").and_then(Value::as_f64))
                    .unwrap_or(std::cmp::Ordering::Equal),
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

pub(crate) fn validate_line_input(
    input: &CreateLineRequest,
) -> UnifiedResult<(String, f64, i64, f64)> {
    let description = require_text(&input.description, "description", MAX_DESCRIPTION_LEN)?;
    let quantity = valid_quantity(input.quantity)?;
    // `unit_price` arrives as integer minor units (the request
    // JSON carries major units, converted by `serde_major`); only
    // the non-negative check is left here.
    let unit_price = require_non_negative_minor(input.unit_price, "unit_price")?;
    let tax_rate = valid_tax_rate(input.tax_rate)?;
    Ok((description, quantity, unit_price, tax_rate))
}

pub(crate) async fn create_purchase_order(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ProcurementState>,
    req: HttpRequest,
    body: web::Json<CreatePurchaseOrderRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = require_caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PROCUREMENT_MANAGE)?;
        let input = body.into_inner();
        let vendor = state
            .vendors
            .get(&input.vendor_id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| {
                UnifiedError::not_found(format!("Vendor not found: {}", input.vendor_id))
            })?;
        if vendor.status == VendorStatus::Suspended {
            return Err(UnifiedError::conflict(format!(
                "Vendor \"{}\" is suspended and cannot receive new purchase orders",
                vendor.name
            )));
        }
        let title = require_text(&input.title, "title", MAX_TITLE_LEN)?;
        let category = match &input.category {
            Some(raw) => Some(require_text(raw, "category", MAX_CATEGORY_LEN)?),
            None => None,
        };
        let currency = valid_currency(input.currency.as_deref().unwrap_or("USD"))?;
        if input.lines.len() > MAX_LINES_PER_PO {
            return Err(UnifiedError::bad_request(format!(
                "at most {MAX_LINES_PER_PO} lines per purchase order"
            )));
        }
        let mut validated_lines: Vec<(String, f64, i64, f64)> = Vec::new();
        for line in &input.lines {
            validated_lines.push(validate_line_input(line)?);
        }
        let now = Utc::now();
        let mut po = PurchaseOrder {
            id: Uuid::now_v7(),
            po_number: state.next_po_number(),
            vendor_id: vendor.id,
            title,
            category,
            currency,
            status: PurchaseOrderStatus::Draft,
            subtotal: 0,
            tax_total: 0,
            total: 0,
            promised_date: input.promised_date,
            ordered_at: None,
            received_at: None,
            cancel_reason: None,
            requested_by: caller.actor.clone(),
            approved_by: None,
            created_at: now,
            updated_at: now,
        };
        state.purchase_orders.insert(po.id, po.clone());
        for (description, quantity, unit_price, tax_rate) in validated_lines {
            let line = PurchaseOrderLine {
                id: Uuid::now_v7(),
                purchase_order_id: po.id,
                description,
                quantity,
                unit_price,
                tax_rate,
                line_total: line_total_minor(quantity, unit_price, tax_rate),
                received_quantity: 0.0,
                created_at: now,
                updated_at: now,
            };
            state.lines.insert(line.id, line);
        }
        recompute_po_totals(&state, &mut po);
        state.purchase_orders.insert(po.id, po.clone());
        state.emit(
            "procurement.po.created",
            json!({
                "purchase_order_id": po.id.to_string(),
                "po_number": po.po_number,
                "vendor_id": po.vendor_id.to_string(),
                "total": minor_to_major(po.total, &po.currency),
                "currency": po.currency,
                "status": "draft",
                "requested_by": caller.actor,
            }),
        );
        Ok(po_view(&state, &po))
    })();
    match result {
        Ok(data) => created(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn get_purchase_order(
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
        let po = load_po(&state, &path.into_inner())?;
        Ok(po_view(&state, &po))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn update_purchase_order(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ProcurementState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdatePurchaseOrderRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = require_caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PROCUREMENT_MANAGE)?;
        let mut po = load_po(&state, &path.into_inner())?;
        if po.status != PurchaseOrderStatus::Draft {
            return Err(UnifiedError::conflict(format!(
                "Only a draft purchase order can be edited (current status \"{}\")",
                po.status.as_str(),
            )));
        }
        let input = body.into_inner();
        if let Some(title) = &input.title {
            po.title = require_text(title, "title", MAX_TITLE_LEN)?;
        }
        if let Some(category) = &input.category {
            po.category = match category {
                Some(raw) => Some(require_text(raw, "category", MAX_CATEGORY_LEN)?),
                None => None,
            };
        }
        if let Some(currency) = &input.currency {
            po.currency = valid_currency(currency)?;
        }
        if let Some(promised_date) = input.promised_date {
            po.promised_date = promised_date;
        }
        po.updated_at = Utc::now();
        state.purchase_orders.insert(po.id, po.clone());
        Ok(po_view(&state, &po))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn delete_purchase_order(
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
        let po = load_po(&state, &path.into_inner())?;
        if po.status != PurchaseOrderStatus::Draft {
            return Err(UnifiedError::conflict(format!(
                "Only a draft purchase order can be deleted (current status \"{}\"). Cancel it instead.",
                po.status.as_str(),
            )));
        }
        for line in lines_of(&state, po.id) {
            state.lines.remove(&line.id);
        }
        state
            .approvals
            .retain(|_, entry| entry.purchase_order_id != po.id);
        state
            .receipts
            .retain(|_, entry| entry.purchase_order_id != po.id);
        state.purchase_orders.remove(&po.id);
        state.emit(
            "procurement.po.deleted",
            json!({
                "purchase_order_id": po.id.to_string(),
                "po_number": po.po_number,
                "deleted_by": caller.actor,
            }),
        );
        Ok(json!({ "deleted": true, "purchase_order_id": po.id.to_string() }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn submit_purchase_order(
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
        let mut po = load_po(&state, &path.into_inner())?;
        transition(&po, PurchaseOrderStatus::PendingApproval, "submit")?;
        if lines_of(&state, po.id).is_empty() {
            return Err(UnifiedError::bad_request(
                "Add at least one line before submitting a purchase order",
            ));
        }
        po.status = PurchaseOrderStatus::PendingApproval;
        po.updated_at = Utc::now();
        state.purchase_orders.insert(po.id, po.clone());
        state.emit(
            "procurement.po.submitted",
            json!({
                "purchase_order_id": po.id.to_string(),
                "po_number": po.po_number,
                "total": minor_to_major(po.total, &po.currency),
                "submitted_by": caller.actor,
            }),
        );
        Ok(po_view(&state, &po))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn approve_purchase_order(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ProcurementState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<ApproveRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = require_caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PROCUREMENT_MANAGE)?;
        let mut po = load_po(&state, &path.into_inner())?;
        // Segregation of duties: the requester cannot approve
        // their own purchase order.
        check_not_self_approval(&po, &caller.actor)?;
        transition(&po, PurchaseOrderStatus::Approved, "approve")?;
        let now = Utc::now();
        let reason = match &body.reason {
            Some(raw) => Some(require_text(raw, "reason", MAX_REASON_LEN)?),
            None => None,
        };
        let approval = PurchaseOrderApproval {
            id: Uuid::now_v7(),
            purchase_order_id: po.id,
            approver_id: caller.actor.clone(),
            approver_role: caller.role.clone(),
            decision: ApprovalDecision::Approved,
            reason: reason.clone(),
            decided_at: now,
        };
        state.approvals.insert(approval.id, approval);
        po.status = PurchaseOrderStatus::Approved;
        po.approved_by = Some(caller.actor.clone());
        po.updated_at = now;
        state.purchase_orders.insert(po.id, po.clone());
        // Over-budget warning (advisory only -- never blocks).
        let warnings = budget_warnings(&state, &po);
        warn_over_budget(&state, &po, &warnings);
        state.emit(
            "procurement.po.approved",
            json!({
                "purchase_order_id": po.id.to_string(),
                "po_number": po.po_number,
                "total": minor_to_major(po.total, &po.currency),
                "approved_by": caller.actor,
                "approver_role": caller.role,
                "budget_warnings": warnings.len(),
            }),
        );
        Ok(po_view(&state, &po))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn reject_purchase_order(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ProcurementState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<RejectRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = require_caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PROCUREMENT_MANAGE)?;
        let mut po = load_po(&state, &path.into_inner())?;
        transition(&po, PurchaseOrderStatus::Draft, "reject")?;
        let reason = require_text(&body.reason, "reason", MAX_REASON_LEN)?;
        let now = Utc::now();
        let approval = PurchaseOrderApproval {
            id: Uuid::now_v7(),
            purchase_order_id: po.id,
            approver_id: caller.actor.clone(),
            approver_role: caller.role.clone(),
            decision: ApprovalDecision::Rejected,
            reason: Some(reason.clone()),
            decided_at: now,
        };
        state.approvals.insert(approval.id, approval);
        // Rejection returns the PO to draft so it can be corrected
        // and resubmitted; the rejection stays on the record.
        po.status = PurchaseOrderStatus::Draft;
        po.approved_by = None;
        po.updated_at = now;
        state.purchase_orders.insert(po.id, po.clone());
        state.emit(
            "procurement.po.rejected",
            json!({
                "purchase_order_id": po.id.to_string(),
                "po_number": po.po_number,
                "rejected_by": caller.actor,
                "approver_role": caller.role,
                "reason": reason,
            }),
        );
        Ok(po_view(&state, &po))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn order_purchase_order(
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
        let mut po = load_po(&state, &path.into_inner())?;
        transition(&po, PurchaseOrderStatus::Ordered, "order")?;
        let now = Utc::now();
        po.status = PurchaseOrderStatus::Ordered;
        po.ordered_at = Some(now);
        po.updated_at = now;
        state.purchase_orders.insert(po.id, po.clone());
        let warnings = budget_warnings(&state, &po);
        warn_over_budget(&state, &po, &warnings);
        state.emit(
            "procurement.po.ordered",
            json!({
                "purchase_order_id": po.id.to_string(),
                "po_number": po.po_number,
                "total": minor_to_major(po.total, &po.currency),
                "ordered_by": caller.actor,
                "budget_warnings": warnings.len(),
            }),
        );
        Ok(po_view(&state, &po))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn receive_purchase_order(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ProcurementState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<ReceiveRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = require_caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PROCUREMENT_MANAGE)?;
        let mut po = load_po(&state, &path.into_inner())?;
        if !matches!(
            po.status,
            PurchaseOrderStatus::Ordered | PurchaseOrderStatus::PartiallyReceived
        ) {
            return Err(UnifiedError::conflict(format!(
                "Goods can only be received for an ordered or partially received purchase order (current status \"{}\")",
                po.status.as_str(),
            )));
        }
        let input = body.into_inner();
        let mut line = load_line(&state, po.id, &input.line_id.to_string())?;
        let quantity = valid_quantity(input.quantity)?;
        // Over-receipt is rejected: received + new must not exceed
        // the ordered quantity. The sum is rounded first (see
        // `would_over_receive`): raw f64 addition would reject
        // exact fills such as 0.1 + 0.2 against an order of 0.3.
        if would_over_receive(line.received_quantity, quantity, line.quantity) {
            return Err(UnifiedError::bad_request(format!(
                "Over-receipt rejected: line ordered {:.4}, already received {:.4}, attempted {:.4}",
                line.quantity, line.received_quantity, quantity
            )));
        }
        let quality_note = match &input.quality_note {
            Some(raw) => Some(require_text(raw, "quality_note", MAX_NOTE_LEN)?),
            None => None,
        };
        let now = Utc::now();
        // Quantities stay `f64`, rounded to 4 decimals as before.
        line.received_quantity = round_quantity(line.received_quantity + quantity);
        line.updated_at = now;
        state.lines.insert(line.id, line.clone());
        let receipt = GoodsReceipt {
            id: Uuid::now_v7(),
            purchase_order_id: po.id,
            line_id: line.id,
            quantity_received: quantity,
            quality_note,
            received_by: caller.actor.clone(),
            received_at: now,
            created_at: now,
        };
        state.receipts.insert(receipt.id, receipt.clone());
        let previous_status = po.status;
        derive_receipt_status(&state, &mut po);
        state.purchase_orders.insert(po.id, po.clone());
        state.emit(
            "procurement.po.received",
            json!({
                "purchase_order_id": po.id.to_string(),
                "po_number": po.po_number,
                "receipt_id": receipt.id.to_string(),
                "line_id": line.id.to_string(),
                "quantity_received": quantity,
                "previous_status": previous_status.as_str(),
                "status": po.status.as_str(),
                "received_by": caller.actor,
            }),
        );
        Ok(json!({
            "purchase_order": po_view(&state, &po),
            "receipt": receipt,
        }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn close_purchase_order(
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
        let mut po = load_po(&state, &path.into_inner())?;
        transition(&po, PurchaseOrderStatus::Closed, "close")?;
        po.status = PurchaseOrderStatus::Closed;
        po.updated_at = Utc::now();
        state.purchase_orders.insert(po.id, po.clone());
        state.emit(
            "procurement.po.closed",
            json!({
                "purchase_order_id": po.id.to_string(),
                "po_number": po.po_number,
                "closed_by": caller.actor,
            }),
        );
        Ok(po_view(&state, &po))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn cancel_purchase_order(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ProcurementState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<CancelRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = require_caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PROCUREMENT_MANAGE)?;
        let mut po = load_po(&state, &path.into_inner())?;
        transition(&po, PurchaseOrderStatus::Cancelled, "cancel")?;
        let reason = match &body.reason {
            Some(raw) => Some(require_text(raw, "reason", MAX_REASON_LEN)?),
            None => None,
        };
        po.status = PurchaseOrderStatus::Cancelled;
        po.cancel_reason = reason.clone();
        po.updated_at = Utc::now();
        state.purchase_orders.insert(po.id, po.clone());
        state.emit(
            "procurement.po.cancelled",
            json!({
                "purchase_order_id": po.id.to_string(),
                "po_number": po.po_number,
                "cancelled_by": caller.actor,
                "reason": reason,
            }),
        );
        Ok(po_view(&state, &po))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn list_po_approvals(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ProcurementState>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PROCUREMENT_VIEW)?;
        let po = load_po(&state, &path.into_inner())?;
        let params = query.into_inner();
        let decision_filter = params.filters.get("decision").cloned();
        let items: Vec<Value> = approvals_of(&state, po.id)
            .into_iter()
            .filter(|approval| {
                if let Some(decision) = &decision_filter {
                    if approval.decision.as_str() != decision.as_str() {
                        return false;
                    }
                }
                true
            })
            .map(|approval| json!(approval))
            .collect();
        Ok(paginate(items, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn list_po_receipts(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ProcurementState>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PROCUREMENT_VIEW)?;
        let po = load_po(&state, &path.into_inner())?;
        let params = query.into_inner();
        let items: Vec<Value> = receipts_of_po(&state, po.id)
            .into_iter()
            .map(|receipt| json!(receipt))
            .collect();
        Ok(paginate(items, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}
