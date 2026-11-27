// procurement -- Purchase order line handlers (draft POs only).

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::pagination::PaginationParams;
use actix_web::{HttpRequest, HttpResponse, web};
use chrono::Utc;
use serde_json::{Value, json};
use std::sync::Arc;
use uuid::Uuid;

use super::handlers_orders::*;
use super::helpers::*;
use super::state::*;
use super::types::*;

// Purchase order line handlers (draft POs only)

pub(crate) async fn list_po_lines(
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
        let items: Vec<Value> = lines_of(&state, po.id)
            .into_iter()
            .map(|line| json!(line))
            .collect();
        Ok(paginate(items, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn add_po_line(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ProcurementState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<CreateLineRequest>,
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
                "Lines can only be added to a draft purchase order (current status \"{}\")",
                po.status.as_str(),
            )));
        }
        if lines_of(&state, po.id).len() >= MAX_LINES_PER_PO {
            return Err(UnifiedError::bad_request(format!(
                "at most {MAX_LINES_PER_PO} lines per purchase order"
            )));
        }
        let (description, quantity, unit_price, tax_rate) =
            validate_line_input(&body.into_inner())?;
        let now = Utc::now();
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
        state.lines.insert(line.id, line.clone());
        recompute_po_totals(&state, &mut po);
        po.updated_at = now;
        state.purchase_orders.insert(po.id, po.clone());
        Ok(po_view(&state, &po))
    })();
    match result {
        Ok(data) => created(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn update_po_line(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ProcurementState>,
    req: HttpRequest,
    path: web::Path<(String, String)>,
    body: web::Json<UpdateLineRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = require_caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PROCUREMENT_MANAGE)?;
        let (po_id, line_id) = path.into_inner();
        let mut po = load_po(&state, &po_id)?;
        if po.status != PurchaseOrderStatus::Draft {
            return Err(UnifiedError::conflict(format!(
                "Lines can only be edited on a draft purchase order (current status \"{}\")",
                po.status.as_str(),
            )));
        }
        let mut line = load_line(&state, po.id, &line_id)?;
        let input = body.into_inner();
        if let Some(description) = &input.description {
            line.description = require_text(description, "description", MAX_DESCRIPTION_LEN)?;
        }
        if let Some(quantity) = input.quantity {
            line.quantity = valid_quantity(quantity)?;
        }
        if let Some(unit_price) = input.unit_price {
            line.unit_price = require_non_negative_minor(unit_price, "unit_price")?;
        }
        if let Some(tax_rate) = input.tax_rate {
            line.tax_rate = valid_tax_rate(tax_rate)?;
        }
        line.line_total = line_total_minor(line.quantity, line.unit_price, line.tax_rate);
        line.updated_at = Utc::now();
        state.lines.insert(line.id, line);
        recompute_po_totals(&state, &mut po);
        po.updated_at = Utc::now();
        state.purchase_orders.insert(po.id, po.clone());
        Ok(po_view(&state, &po))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn delete_po_line(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ProcurementState>,
    req: HttpRequest,
    path: web::Path<(String, String)>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = require_caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PROCUREMENT_MANAGE)?;
        let (po_id, line_id) = path.into_inner();
        let mut po = load_po(&state, &po_id)?;
        if po.status != PurchaseOrderStatus::Draft {
            return Err(UnifiedError::conflict(format!(
                "Lines can only be removed from a draft purchase order (current status \"{}\")",
                po.status.as_str(),
            )));
        }
        let line = load_line(&state, po.id, &line_id)?;
        state.lines.remove(&line.id);
        recompute_po_totals(&state, &mut po);
        po.updated_at = Utc::now();
        state.purchase_orders.insert(po.id, po.clone());
        Ok(po_view(&state, &po))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}
