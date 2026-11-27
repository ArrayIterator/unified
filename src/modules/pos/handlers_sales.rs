// Pos module - sale handlers.
// Split from the former single-file module; items keep their original names and behaviour. Private items are `pub(crate)` so sibling files in this module can use them.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::HookBus;
use unified_core::json;
use unified_core::modules::{Module, ModuleRegistry, RouteInfo, RouteVersionSupport};
use unified_core::pagination::{PageResult, PaginationParams};
use actix_web::{web, HttpRequest, HttpResponse};
use chrono::{DateTime, NaiveDate, Utc};
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::sync::{
    atomic::{AtomicU64, Ordering},
    Arc, RwLock,
};
use uuid::Uuid;
use super::*;

// Sale handlers

pub(crate) async fn list_sales(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<PosState>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;

        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_POS_VIEW)?;
        let params = query.into_inner();
        let register_filter = params.filters.get("register_id").cloned();
        let shift_filter = params.filters.get("shift_id").cloned();
        let cashier_filter = params.filters.get("cashier_id").cloned();
        let status_filter = params.filters.get("status").cloned();
        let mut items: Vec<PosSale> = state
            .sales
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|sale| {
                if !in_company_scope(&sale.company_id, &_scope) {
                    return false;
                }
                if let Some(register_id) = &register_filter {
                    if sale.register_id != *register_id {
                        return false;
                    }
                }
                if let Some(shift_id) = &shift_filter {
                    if sale.shift_id.as_deref() != Some(shift_id.as_str()) {
                        return false;
                    }
                }
                if let Some(cashier_id) = &cashier_filter {
                    if sale.cashier_id != *cashier_id {
                        return false;
                    }
                }
                if let Some(status) = &status_filter {
                    if sale.status != *status {
                        return false;
                    }
                }
                true
            })
            .collect();
        items.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        Ok(paginate(items, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn create_sale(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<PosState>,
    req: HttpRequest,
    body: web::Json<CreateSaleRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PosSale> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = require_company_for_write(&_tenant, &req)?;

        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_POS_SELL)?;
        let input = body.into_inner();
        let settings = state.settings();
        let register = load_register(&state, &input.register_id)?;
        if register.status != "active" {
            return Err(UnifiedError::unprocessable(
                "Cannot create a sale on an inactive register",
            ));
        }
        caller_may_sell(&ctx, &state, &register, &caller)?;
        let shift_id = match &input.shift_id {
            Some(raw) => {
                let shift = load_shift(&state, raw)?;
                if shift.register_id != register.id {
                    return Err(UnifiedError::bad_request(
                        "Shift does not belong to this register",
                    ));
                }
                if shift.status != "open" {
                    return Err(UnifiedError::unprocessable(
                        "Cannot create a sale in a closed shift",
                    ));
                }
                if shift.cashier_id != caller.actor
                    && !check_capability(&ctx, &caller.role, PERM_POS_MANAGE)
                {
                    return Err(UnifiedError::forbidden(
                        "Shift belongs to a different cashier",
                    ));
                }
                Some(shift.id)
            }
            None => open_shift_for_register(&state, &register.id).map(|shift| shift.id),
        };
        let currency = match &input.currency {
            Some(raw) => valid_currency(raw)?,
            None => settings.default_currency.clone(),
        };
        let discount_type = valid_discount_type(&input.discount_type)?;
        let discount_value = input.discount_value.unwrap_or(0.0);
        if discount_type.is_some() && (!discount_value.is_finite() || discount_value < 0.0) {
            return Err(UnifiedError::bad_request(
                "discount_value must be zero or greater",
            ));
        }
        let customer_name = optional_text(&input.customer_name, "customer_name", MAX_NAME_LEN)?;
        let notes = optional_text(&input.notes, "notes", MAX_NOTES_LEN)?;
        let company_id = require_company_match(
            &_tenant,
            &req,
            &optional_text(&input.company_id, "company_id", MAX_CODE_LEN)?,
        )?;
        let mut items: Vec<PosSaleItem> = Vec::new();
        if let Some(raw_items) = &input.items {
            for raw in raw_items {
                items.push(build_sale_item(raw, &settings, &currency)?);
            }
        }
        let mut sale = PosSale {
            id: new_id(),
            number: state.next_sale_number(),
            register_id: register.id.clone(),
            shift_id,
            cashier_id: caller.actor.clone(),
            company_id,
            items,
            subtotal: 0,
            discount_type,
            discount_value,
            discount_total: 0,
            tax_total: 0,
            total: 0,
            currency,
            tax_inclusive: settings.tax_inclusive,
            status: "draft".to_string(),
            amount_paid: 0,
            change_due: 0,
            refunded_amount: 0,
            customer_name,
            notes,
            completed_at: None,
            voided_at: None,
            created_at: now(),
            updated_at: now(),
        };
        recompute_sale(&mut sale, &settings)?;
        state.sales.insert(sale.id.clone(), sale.clone());
        state.emit(
            "pos.sale.created",
            json!({
                "sale_id": sale.id,
                "number": sale.number,
                "register_id": sale.register_id,
                "shift_id": sale.shift_id,
                "cashier_id": sale.cashier_id,
                "total": sale.total,
                "currency": sale.currency,
            }),
        );
        Ok(sale)
    })();
    match result {
        Ok(data) => created(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn get_sale(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<PosState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;

        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_POS_VIEW)?;
        let sale = require_sale_in_scope(&state, &path.into_inner(), &_scope)?;
        let payments: Vec<PosPayment> = state
            .payments
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|payment| payment.sale_id == sale.id)
            .collect();
        let refunds: Vec<PosRefund> = state
            .refunds
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|refund| refund.sale_id == sale.id)
            .collect();
        Ok(json!({ "sale": sale, "payments": payments, "refunds": refunds }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

/// A draft sale may be edited by its cashier or a manager.
pub(crate) fn load_editable_sale(
    ctx: &AppContext,
    state: &PosState,
    id: &str,
    caller: &Caller,
    scope: &Option<String>,
) -> UnifiedResult<PosSale> {
    let sale = load_sale(state, id)?;
    require_record_writable_in_company_scope(&sale.company_id, scope)?;
    if !matches!(sale.status.as_str(), "draft" | "held") {
        return Err(UnifiedError::unprocessable(format!(
            "Sale {} cannot be edited (status: {})",
            sale.number, sale.status
        )));
    }
    caller_may_act_on_sale(ctx, &sale, caller)?;
    Ok(sale)
}

pub(crate) async fn update_sale(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<PosState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdateSaleRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PosSale> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = require_company_for_write(&_tenant, &req)?;

        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_POS_SELL)?;
        let id = path.into_inner();
        let input = body.into_inner();
        let settings = state.settings();
        let mut sale = load_editable_sale(&ctx, &state, &id, &caller, &_scope)?;
        if let Some(discount_type) = &input.discount_type {
            sale.discount_type = valid_discount_type(discount_type)?;
        }
        if let Some(discount_value) = input.discount_value {
            if !discount_value.is_finite() || discount_value < 0.0 {
                return Err(UnifiedError::bad_request(
                    "discount_value must be zero or greater",
                ));
            }
            sale.discount_value = discount_value;
        }
        if let Some(customer_name) = &input.customer_name {
            sale.customer_name = match customer_name {
                Some(text) => Some(require_text(text, "customer_name", MAX_NAME_LEN)?),
                None => None,
            };
        }
        if let Some(notes) = &input.notes {
            sale.notes = match notes {
                Some(text) => Some(require_text(text, "notes", MAX_NOTES_LEN)?),
                None => None,
            };
        }
        recompute_sale(&mut sale, &settings)?;
        sale.updated_at = now();
        state.sales.insert(sale.id.clone(), sale.clone());
        state.emit(
            "pos.sale.updated",
            json!({
                "sale_id": sale.id,
                "number": sale.number,
                "total": sale.total,
                "currency": sale.currency,
                "updated_by": caller.actor,
            }),
        );
        Ok(sale)
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn add_sale_item(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<PosState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<NewSaleItemRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PosSale> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = require_company_for_write(&_tenant, &req)?;

        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_POS_SELL)?;
        let id = path.into_inner();
        let settings = state.settings();
        let mut sale = load_editable_sale(&ctx, &state, &id, &caller, &_scope)?;
        let currency = sale.currency.clone();
        let item = build_sale_item(&body.into_inner(), &settings, &currency)?;
        sale.items.push(item);
        recompute_sale(&mut sale, &settings)?;
        sale.updated_at = now();
        state.sales.insert(sale.id.clone(), sale.clone());
        Ok(sale)
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn update_sale_item(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<PosState>,
    req: HttpRequest,
    path: web::Path<(String, String)>,
    body: web::Json<UpdateSaleItemRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PosSale> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = require_company_for_write(&_tenant, &req)?;

        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_POS_SELL)?;
        let (id, item_id) = path.into_inner();
        let input = body.into_inner();
        let settings = state.settings();
        let mut sale = load_editable_sale(&ctx, &state, &id, &caller, &_scope)?;
        let currency = sale.currency.clone();
        let position = sale
            .items
            .iter()
            .position(|item| item.id == item_id)
            .ok_or_else(|| UnifiedError::not_found(format!("Sale line not found: {item_id}")))?;
        let item = &mut sale.items[position];
        if let Some(quantity) = input.quantity {
            if quantity == 0 || quantity > 10_000 {
                return Err(UnifiedError::bad_request(
                    "quantity must be between 1 and 10000",
                ));
            }
            item.quantity = quantity;
        }
        if let Some(unit_price) = input.unit_price {
            item.unit_price = valid_money(unit_price, "unit_price")?;
        }
        if let Some(discount_type) = &input.discount_type {
            item.discount_type = valid_discount_type(discount_type)?;
        }
        if let Some(discount_value) = input.discount_value {
            if !discount_value.is_finite() || discount_value < 0.0 {
                return Err(UnifiedError::bad_request(
                    "discount_value must be zero or greater",
                ));
            }
            item.discount_value = discount_value;
        }
        if let Some(tax_rate) = input.tax_rate {
            if !tax_rate.is_finite() || tax_rate < 0.0 || tax_rate > 10.0 {
                return Err(UnifiedError::bad_request(
                    "tax_rate must be a finite fraction between 0 and 10",
                ));
            }
            item.tax_rate = tax_rate;
        }
        let gross = item.unit_price.saturating_mul(item.quantity as i64);
        item.discount_amount = discount_amount(
            &item.discount_type,
            item.discount_value,
            gross,
            settings.max_discount_percent,
            &currency,
        )?;
        recompute_sale(&mut sale, &settings)?;
        sale.updated_at = now();
        state.sales.insert(sale.id.clone(), sale.clone());
        Ok(sale)
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn remove_sale_item(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<PosState>,
    req: HttpRequest,
    path: web::Path<(String, String)>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PosSale> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = require_company_for_write(&_tenant, &req)?;

        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_POS_SELL)?;
        let (id, item_id) = path.into_inner();
        let settings = state.settings();
        let mut sale = load_editable_sale(&ctx, &state, &id, &caller, &_scope)?;
        let before = sale.items.len();
        sale.items.retain(|item| item.id != item_id);
        if sale.items.len() == before {
            return Err(UnifiedError::not_found(format!(
                "Sale line not found: {item_id}"
            )));
        }
        recompute_sale(&mut sale, &settings)?;
        sale.updated_at = now();
        state.sales.insert(sale.id.clone(), sale.clone());
        Ok(sale)
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}
