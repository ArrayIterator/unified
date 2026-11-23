// Pos module — checkout, void, refund, and hold handlers.
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

pub(crate) async fn complete_sale(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<PosState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<CompleteSaleRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = require_company_for_write(&_tenant, &req)?;

        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_POS_SELL)?;
        let id = path.into_inner();
        let input = body.into_inner();
        let settings = state.settings();
        state.seed_payment_methods();
        let mut sale = require_sale_in_scope(&state, &id, &_scope)?;
        if !matches!(sale.status.as_str(), "draft" | "held") {
            return Err(UnifiedError::unprocessable(format!(
                "Sale {} cannot be completed (status: {})",
                sale.number, sale.status
            )));
        }
        caller_may_act_on_sale(&ctx, &sale, &caller)?;
        if sale.items.is_empty() {
            return Err(UnifiedError::unprocessable(
                "Cannot complete a sale with no lines",
            ));
        }
        if let Some(shift_id) = &sale.shift_id {
            let shift = load_shift(&state, shift_id)?;
            if shift.status != "open" {
                return Err(UnifiedError::unprocessable(
                    "Cannot complete a sale in a closed shift",
                ));
            }
        }
        if input.payments.is_empty() {
            return Err(UnifiedError::bad_request(
                "At least one payment is required",
            ));
        }
        if !settings.allow_split_payment && input.payments.len() > 1 {
            return Err(UnifiedError::unprocessable(
                "Split payment is disabled in the POS settings",
            ));
        }
        recompute_sale(&mut sale, &settings)?;
        // Validate every payment against the stored method
        // definitions, then require the applied amounts to sum to
        // the sale total. Change is the tendered excess, and only
        // cash methods may tender more than the payment amount:
        // a card/bank/e-wallet overpayment is rejected outright
        // (see `validate_non_cash_tendered`).
        let mut payments: Vec<PosPayment> = Vec::new();
        let mut applied_total = 0i64;
        let mut tendered_total = 0i64;
        for raw in &input.payments {
            let method = state
                .payment_methods
                .get(&raw.method_id)
                .map(|entry| entry.value().clone())
                .ok_or_else(|| {
                    UnifiedError::not_found(format!(
                        "Payment method not found: {}",
                        raw.method_id
                    ))
                })?;
            if !method.active {
                return Err(UnifiedError::unprocessable(format!(
                    "Payment method is disabled: {}",
                    method.label
                )));
            }
            let amount = valid_money(raw.amount, "payment amount")?;
            if amount <= 0 {
                return Err(UnifiedError::bad_request(
                    "payment amount must be greater than zero",
                ));
            }
            let tendered = match raw.tendered {
                Some(value) => valid_money(value, "tendered amount")?,
                None => amount,
            };
            if tendered < amount {
                return Err(UnifiedError::bad_request(
                    "tendered amount must not be less than the payment amount",
                ));
            }
            validate_non_cash_tendered(method.is_cash, amount, tendered)?;
            if method.requires_reference {
                let reference = raw
                    .reference
                    .as_deref()
                    .map(str::trim)
                    .filter(|value| !value.is_empty());
                if reference.is_none() {
                    return Err(UnifiedError::bad_request(format!(
                        "Payment method \"{}\" requires a reference",
                        method.label
                    )));
                }
            }
            applied_total += amount;
            tendered_total += tendered;
            payments.push(PosPayment {
                id: new_id(),
                sale_id: sale.id.clone(),
                method_id: method.id.clone(),
                method_code: method.code.clone(),
                amount,
                tendered,
                reference: optional_text(&raw.reference, "reference", MAX_CODE_LEN)?,
                recorded_by: caller.actor.clone(),
                recorded_at: now(),
            });
        }
        if applied_total != sale.total {
            return Err(UnifiedError::unprocessable(format!(
                "Payments total {} {} but the sale total is {} {}",
                minor_to_major(applied_total, &sale.currency),
                sale.currency,
                minor_to_major(sale.total, &sale.currency),
                sale.currency
            )));
        }
        let change_due = tendered_total - applied_total;
        let completed_at = now();
        sale.status = "completed".to_string();
        sale.amount_paid = applied_total;
        sale.change_due = change_due;
        sale.completed_at = Some(completed_at);
        if let Some(notes) = optional_text(&input.notes, "notes", MAX_NOTES_LEN)? {
            sale.notes = Some(notes);
        }
        sale.updated_at = completed_at;
        for payment in &payments {
            state
                .payments
                .insert(payment.id.clone(), payment.clone());
        }
        state.sales.insert(sale.id.clone(), sale.clone());
        // Receipt: tax grouped by rate, plus the structured print
        // lines for the printer.
        let mut tax_breakdown: Vec<TaxBreakdownLine> = Vec::new();
        for item in &sale.items {
            if let Some(line) = tax_breakdown
                .iter_mut()
                .find(|line| (line.tax_rate - item.tax_rate).abs() < f64::EPSILON)
            {
                line.tax_amount += item.tax_amount;
            } else {
                tax_breakdown.push(TaxBreakdownLine {
                    tax_rate: item.tax_rate,
                    tax_amount: item.tax_amount,
                });
            }
        }
        tax_breakdown.sort_by(|a, b| {
            a.tax_rate
                .partial_cmp(&b.tax_rate)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        let register = load_register(&state, &sale.register_id)?;
        let mut receipt = PosReceipt {
            id: new_id(),
            number: state.next_receipt_number(),
            sale_id: sale.id.clone(),
            sale_number: sale.number.clone(),
            register_id: sale.register_id.clone(),
            register_code: Some(register.code.clone()),
            cashier_id: sale.cashier_id.clone(),
            store_name: settings.store_name.clone(),
            store_address: settings.store_address.clone(),
            items: sale.items.clone(),
            subtotal: sale.subtotal,
            discount_total: sale.discount_total,
            tax_total: sale.tax_total,
            total: sale.total,
            currency: sale.currency.clone(),
            tax_breakdown,
            payments: payments.clone(),
            amount_paid: applied_total,
            change_due,
            print_lines: Vec::new(),
            footer: settings.receipt_footer.clone(),
            issued_at: completed_at,
            created_at: completed_at,
        };
        receipt.print_lines = build_print_lines(&receipt, &settings);
        state.receipts.insert(receipt.id.clone(), receipt.clone());
        // Events. The store-shaped `order.created` payload is what
        // the store, billing, and tax modules consume; the POS
        // events carry the full sale for POS listeners.
        let order_items: Vec<Value> = sale
            .items
            .iter()
            .map(|item| {
                json!({
                    "product_id": item.product_id,
                    "name": item.name,
                    "unit_price": minor_to_major(item.unit_price, &sale.currency),
                    "quantity": item.quantity,
                    "line_total": minor_to_major(item.line_total, &sale.currency),
                    "license_product": false,
                })
            })
            .collect();
        state.emit(
            "order.created",
            order_created_payload(&sale, order_items, settings.country_code.as_deref()),
        );
        state.emit(
            EVENT_SALE_COMPLETED,
            json!({
                "sale_id": sale.id,
                "number": sale.number,
                "register_id": sale.register_id,
                "shift_id": sale.shift_id,
                "cashier_id": sale.cashier_id,
                "total": minor_to_major(sale.total, &sale.currency),
                "currency": sale.currency,
                "amount_paid": minor_to_major(applied_total, &sale.currency),
                "change_due": minor_to_major(change_due, &sale.currency),
                "receipt_id": receipt.id,
                "receipt_number": receipt.number,
                "items": serde_json::to_value(&sale.items).unwrap_or(Value::Null),
                "payments": serde_json::to_value(&payments).unwrap_or(Value::Null),
            }),
        );
        state.emit(
            "pos.receipt.created",
            json!({
                "receipt_id": receipt.id,
                "receipt_number": receipt.number,
                "sale_id": sale.id,
                "register_id": sale.register_id,
                "total": minor_to_major(sale.total, &sale.currency),
                "currency": sale.currency,
            }),
        );
        // Stock moves are requested, never applied here.
        for item in &sale.items {
            state.emit(
                EVENT_STOCK_DECREMENT_REQUESTED,
                json!({
                    "asset_id": item.product_id,
                    "product_id": item.product_id,
                    "sku": item.sku,
                    "barcode": item.barcode,
                    "quantity": item.quantity,
                    "reason": format!("POS sale {}", sale.number),
                    "sale_id": sale.id,
                    "register_id": sale.register_id,
                }),
            );
        }
        Ok(json!({
            "sale": sale,
            "payments": payments,
            "receipt": receipt,
            "change_due": minor_to_major(change_due, &sale.currency),
        }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn void_sale(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<PosState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PosSale> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = require_company_for_write(&_tenant, &req)?;

        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_POS_SELL)?;
        let id = path.into_inner();
        let mut sale = require_sale_in_scope(&state, &id, &_scope)?;
        if !matches!(sale.status.as_str(), "draft" | "held") {
            return Err(UnifiedError::unprocessable(format!(
                "Sale {} cannot be voided (status: {}); use refund for completed sales",
                sale.number, sale.status
            )));
        }
        caller_may_act_on_sale(&ctx, &sale, &caller)?;
        sale.status = "voided".to_string();
        sale.voided_at = Some(now());
        sale.updated_at = now();
        state.sales.insert(sale.id.clone(), sale.clone());
        state.emit(
            "pos.sale.voided",
            json!({
                "sale_id": sale.id,
                "number": sale.number,
                "register_id": sale.register_id,
                "shift_id": sale.shift_id,
                "cashier_id": sale.cashier_id,
                "voided_by": caller.actor,
            }),
        );
        Ok(sale)
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn refund_sale(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<PosState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<RefundSaleRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = require_company_for_write(&_tenant, &req)?;

        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_POS_REFUND)?;
        let id = path.into_inner();
        let input = body.into_inner();
        state.seed_payment_methods();
        let mut sale = require_sale_in_scope(&state, &id, &_scope)?;
        if !matches!(
            sale.status.as_str(),
            "completed" | "partially_refunded"
        ) {
            return Err(UnifiedError::unprocessable(format!(
                "Sale {} cannot be refunded (status: {})",
                sale.number, sale.status
            )));
        }
        let reason = require_text(
            input.reason.as_deref().unwrap_or("Customer return"),
            "reason",
            MAX_REASON_LEN,
        )?;
        let remaining = sale.total - sale.refunded_amount;
        if remaining <= 0 {
            return Err(UnifiedError::unprocessable(
                "Sale has already been fully refunded",
            ));
        }
        // Refund amount: by line items when given, otherwise the
        // plain amount. A line refund returns the pro-rata share of
        // the line total (including its tax) per unit.
        let mut refund_items: Vec<PosRefundItem> = Vec::new();
        let mut amount = 0i64;
        if let Some(raw_items) = &input.items {
            if raw_items.is_empty() {
                return Err(UnifiedError::bad_request(
                    "items must not be empty when given",
                ));
            }
            for raw in raw_items {
                if raw.quantity == 0 {
                    return Err(UnifiedError::bad_request(
                        "refund quantity must be greater than zero",
                    ));
                }
            }
            // Aggregate first: the same line listed twice must be
            // validated against the available quantity as one
            // combined quantity, or the second entry would
            // over-refund the line and emit the stock event twice.
            for (item_id, quantity) in aggregate_refund_quantities(raw_items) {
                let item = sale
                    .items
                    .iter()
                    .find(|item| item.id == item_id)
                    .ok_or_else(|| {
                        UnifiedError::not_found(format!("Sale line not found: {item_id}"))
                    })?;
                let available = item.quantity - item.refunded_quantity;
                if quantity > available {
                    return Err(UnifiedError::unprocessable(format!(
                        "Only {available} unit(s) of \"{}\" are left to refund",
                        item.name
                    )));
                }
                let unit_total = div_rounded(item.line_total, item.quantity as i64);
                let line_amount = unit_total.saturating_mul(quantity as i64);
                amount += line_amount;
                refund_items.push(PosRefundItem {
                    item_id: item.id.clone(),
                    product_id: item.product_id.clone(),
                    quantity,
                    amount: line_amount,
                });
            }
        } else if let Some(raw_amount) = input.amount {
            amount = valid_money(raw_amount, "amount")?;
            if amount <= 0 {
                return Err(UnifiedError::bad_request(
                    "amount must be greater than zero",
                ));
            }
        } else {
            return Err(UnifiedError::bad_request(
                "Provide either items or amount",
            ));
        }
        if amount > remaining {
            return Err(UnifiedError::unprocessable(format!(
                "Refund amount {} {} exceeds the refundable balance {} {}",
                minor_to_major(amount, &sale.currency),
                sale.currency,
                minor_to_major(remaining, &sale.currency),
                sale.currency
            )));
        }
        if input.items.is_none() {
            // Plain-amount refund: distribute it across the lines
            // (whole units only) so `refunded_quantity` advances
            // and a later line refund cannot return the same goods
            // to stock again. Money that does not cover a whole
            // unit buys no stock movement.
            refund_items = distribute_plain_refund(&sale, amount);
        }
        // Refund method: the requested method, else the method of
        // the sale's first payment.
        let method = match &input.method_id {
            Some(raw) => state
                .payment_methods
                .get(raw)
                .map(|entry| entry.value().clone())
                .ok_or_else(|| {
                    UnifiedError::not_found(format!("Payment method not found: {raw}"))
                })?,
            None => {
                let first = state
                    .payments
                    .iter()
                    .map(|entry| entry.value().clone())
                    .find(|payment| payment.sale_id == sale.id)
                    .ok_or_else(|| {
                        UnifiedError::unprocessable("Sale has no payment to refund against")
                    })?;
                state
                    .payment_methods
                    .get(&first.method_id)
                    .map(|entry| entry.value().clone())
                    .ok_or_else(|| {
                        UnifiedError::not_found("Original payment method no longer exists")
                    })?
            }
        };
        if !method.active {
            return Err(UnifiedError::unprocessable(format!(
                "Payment method is disabled: {}",
                method.label
            )));
        }
        for refund_item in &refund_items {
            if let Some(item) = sale
                .items
                .iter_mut()
                .find(|item| item.id == refund_item.item_id)
            {
                item.refunded_quantity += refund_item.quantity;
            }
        }
        sale.refunded_amount += amount;
        sale.status = if sale.total == sale.refunded_amount {
            "refunded".to_string()
        } else {
            "partially_refunded".to_string()
        };
        sale.updated_at = now();
        let refund = PosRefund {
            id: new_id(),
            sale_id: sale.id.clone(),
            shift_id: sale.shift_id.clone(),
            amount,
            method_id: method.id.clone(),
            method_code: method.code.clone(),
            items: refund_items.clone(),
            reason: reason.clone(),
            refunded_by: caller.actor.clone(),
            created_at: now(),
        };
        state.refunds.insert(refund.id.clone(), refund.clone());
        state.sales.insert(sale.id.clone(), sale.clone());
        state.emit(
            "pos.sale.refunded",
            json!({
                "sale_id": sale.id,
                "number": sale.number,
                "refund_id": refund.id,
                "amount": minor_to_major(amount, &sale.currency),
                "currency": sale.currency,
                "method": method.code,
                "status": sale.status,
                "reason": reason,
                "refunded_by": caller.actor,
            }),
        );
        state.emit(
            "payment.refunded",
            json!({
                "order_id": sale.id,
                "sale_id": sale.id,
                "amount": minor_to_major(amount, &sale.currency),
                "currency": sale.currency,
                "method": method.code,
                "reason": reason,
            }),
        );
        // Returned goods go back to stock through the inventory
        // side, never by writing stock here.
        for refund_item in &refund_items {
            let item = sale
                .items
                .iter()
                .find(|item| item.id == refund_item.item_id);
            state.emit(
                EVENT_STOCK_INCREMENT_REQUESTED,
                json!({
                    "asset_id": refund_item.product_id,
                    "product_id": refund_item.product_id,
                    "sku": item.and_then(|item| item.sku.clone()),
                    "barcode": item.and_then(|item| item.barcode.clone()),
                    "quantity": refund_item.quantity,
                    "reason": format!("POS refund of sale {}", sale.number),
                    "sale_id": sale.id,
                    "refund_id": refund.id,
                }),
            );
        }
        Ok(json!({ "sale": sale, "refund": refund }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn hold_sale(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<PosState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PosSale> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = require_company_for_write(&_tenant, &req)?;

        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_POS_SELL)?;
        let id = path.into_inner();
        let mut sale = require_sale_in_scope(&state, &id, &_scope)?;
        if sale.status != "draft" {
            return Err(UnifiedError::unprocessable(format!(
                "Only a draft sale can be held (status: {})",
                sale.status
            )));
        }
        caller_may_act_on_sale(&ctx, &sale, &caller)?;
        sale.status = "held".to_string();
        sale.updated_at = now();
        state.sales.insert(sale.id.clone(), sale.clone());
        state.emit(
            "pos.sale.held",
            json!({
                "sale_id": sale.id,
                "number": sale.number,
                "held_by": caller.actor,
            }),
        );
        Ok(sale)
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn resume_sale(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<PosState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PosSale> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = require_company_for_write(&_tenant, &req)?;

        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_POS_SELL)?;
        let id = path.into_inner();
        let mut sale = require_sale_in_scope(&state, &id, &_scope)?;
        if sale.status != "held" {
            return Err(UnifiedError::unprocessable(format!(
                "Only a held sale can be resumed (status: {})",
                sale.status
            )));
        }
        caller_may_act_on_sale(&ctx, &sale, &caller)?;
        sale.status = "draft".to_string();
        sale.updated_at = now();
        state.sales.insert(sale.id.clone(), sale.clone());
        state.emit(
            "pos.sale.resumed",
            json!({
                "sale_id": sale.id,
                "number": sale.number,
                "resumed_by": caller.actor,
            }),
        );
        Ok(sale)
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

// ---------------------------------------------------------------------------
// Receipt handlers
// ---------------------------------------------------------------------------

pub(crate) fn load_receipt_by_sale(state: &PosState, sale_id: &str) -> UnifiedResult<PosReceipt> {
    state
        .receipts
        .iter()
        .map(|entry| entry.value().clone())
        .find(|receipt| receipt.sale_id == sale_id)
        .ok_or_else(|| UnifiedError::not_found(format!("Receipt not found for sale: {sale_id}")))
}

// ---------------------------------------------------------------------------
// Checkout helpers (pure, unit-tested below)
// ---------------------------------------------------------------------------

/// Change is cash only: a non-cash payment must tender exactly its
/// applied amount. `tendered` is documented as cash-only in
/// `types.rs`, so any excess on a card/bank/e-wallet payment would
/// otherwise surface as `change_due` the drawer never holds.
pub(crate) fn validate_non_cash_tendered(
    is_cash: bool,
    amount: i64,
    tendered: i64,
) -> UnifiedResult<()> {
    if !is_cash && tendered > amount {
        return Err(UnifiedError::unprocessable(
            "Non-cash payments must tender exactly the payment amount; change is cash only",
        ));
    }
    Ok(())
}

/// Combine repeated refund entries for the same sale line into one
/// quantity per line (first-seen order), so validation sees the
/// true total requested for the line.
pub(crate) fn aggregate_refund_quantities(raw_items: &[RefundItemRequest]) -> Vec<(String, u32)> {
    let mut aggregated: Vec<(String, u32)> = Vec::new();
    for raw in raw_items {
        if let Some(entry) = aggregated
            .iter_mut()
            .find(|(item_id, _)| item_id == &raw.item_id)
        {
            entry.1 = entry.1.saturating_add(raw.quantity);
        } else {
            aggregated.push((raw.item_id.clone(), raw.quantity));
        }
    }
    aggregated
}

/// Distribute a plain-amount refund across the sale lines, whole
/// units only, in line order. Each returned item carries the
/// pro-rata share of its line total, so applying it advances
/// `refunded_quantity` and the stock-increment events match the
/// goods actually returned. Money that does not cover a whole unit
/// buys no stock movement.
pub(crate) fn distribute_plain_refund(sale: &PosSale, amount: i64) -> Vec<PosRefundItem> {
    let mut items: Vec<PosRefundItem> = Vec::new();
    let mut left = amount;
    for item in &sale.items {
        if left <= 0 {
            break;
        }
        if item.quantity == 0 {
            continue;
        }
        let available = item.quantity - item.refunded_quantity;
        if available == 0 {
            continue;
        }
        let unit_total = div_rounded(item.line_total, item.quantity as i64);
        if unit_total <= 0 {
            continue;
        }
        let affordable = (left / unit_total) as u32;
        let take = available.min(affordable);
        if take == 0 {
            continue;
        }
        let line_amount = unit_total.saturating_mul(take as i64);
        left -= line_amount;
        items.push(PosRefundItem {
            item_id: item.id.clone(),
            product_id: item.product_id.clone(),
            quantity: take,
            amount: line_amount,
        });
    }
    items
}

/// The store-shaped `order.created` payload. `tax_inclusive` is
/// always present: the tax module defaults it to false and would
/// otherwise compute exclusive tax on the net amount of an
/// inclusive POS sale. `country_code` is included only when the
/// store country is configured.
pub(crate) fn order_created_payload(
    sale: &PosSale,
    order_items: Vec<Value>,
    country_code: Option<&str>,
) -> Value {
    let mut payload = json!({
        "id": sale.id,
        "user_id": sale.cashier_id,
        "company_id": sale.company_id,
        "items": order_items,
        "subtotal": minor_to_major(sale.subtotal, &sale.currency),
        "discount_total": minor_to_major(sale.discount_total, &sale.currency),
        "total": minor_to_major(sale.total, &sale.currency),
        "currency": sale.currency,
        "tax_inclusive": sale.tax_inclusive,
        "status": "paid",
        "provider": "pos",
        "pos_sale_number": sale.number,
        "pos_register_id": sale.register_id,
        "pos_shift_id": sale.shift_id,
    });
    if let Some(country) = country_code {
        if let Value::Object(map) = &mut payload {
            map.insert(
                "country_code".to_string(),
                Value::String(country.to_string()),
            );
        }
    }
    payload
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sale_item(id: &str, quantity: u32, refunded: u32, line_total: i64) -> PosSaleItem {
        PosSaleItem {
            id: id.to_string(),
            product_id: format!("prod-{id}"),
            name: format!("Item {id}"),
            barcode: None,
            sku: None,
            unit_price: if quantity == 0 {
                0
            } else {
                line_total / quantity as i64
            },
            quantity,
            refunded_quantity: refunded,
            discount_type: None,
            discount_value: 0.0,
            discount_amount: 0,
            line_subtotal: line_total,
            tax_rate: 0.0,
            tax_amount: 0,
            line_total,
        }
    }

    fn sale(items: Vec<PosSaleItem>, tax_inclusive: bool) -> PosSale {
        let total: i64 = items.iter().map(|item| item.line_total).sum();
        PosSale {
            id: "sale-1".to_string(),
            number: "POS-000001".to_string(),
            register_id: "reg-1".to_string(),
            shift_id: None,
            cashier_id: "cashier-1".to_string(),
            company_id: None,
            items,
            subtotal: total,
            discount_type: None,
            discount_value: 0.0,
            discount_total: 0,
            tax_total: 0,
            total,
            currency: "USD".to_string(),
            tax_inclusive,
            status: "completed".to_string(),
            amount_paid: total,
            change_due: 0,
            refunded_amount: 0,
            customer_name: None,
            notes: None,
            completed_at: Some(Utc::now()),
            voided_at: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }

    #[test]
    fn non_cash_overpayment_is_rejected_but_cash_change_is_allowed() {
        // Regression (O1): a card overpayment used to produce
        // change_due even though tendered is cash-only.
        assert!(validate_non_cash_tendered(false, 1000, 1000).is_ok());
        assert!(validate_non_cash_tendered(false, 1000, 1001).is_err());
        assert!(validate_non_cash_tendered(false, 1000, 1500).is_err());
        assert!(validate_non_cash_tendered(true, 1000, 1500).is_ok());
        assert!(validate_non_cash_tendered(true, 1000, 1000).is_ok());
    }

    #[test]
    fn duplicate_refund_lines_aggregate_to_one_quantity() {
        // Regression (O2): the same line twice, each within the
        // available quantity, used to over-refund it.
        let raw = vec![
            RefundItemRequest {
                item_id: "line-a".to_string(),
                quantity: 2,
            },
            RefundItemRequest {
                item_id: "line-b".to_string(),
                quantity: 1,
            },
            RefundItemRequest {
                item_id: "line-a".to_string(),
                quantity: 3,
            },
        ];
        assert_eq!(
            aggregate_refund_quantities(&raw),
            vec![("line-a".to_string(), 5), ("line-b".to_string(), 1),]
        );
        assert!(aggregate_refund_quantities(&[]).is_empty());
    }

    #[test]
    fn plain_amount_refund_distributes_whole_units_across_lines() {
        // Regression (O3): a plain-amount refund never updated
        // refunded_quantity, so a later line refund could return
        // the same goods to stock again.
        let sale = sale(
            vec![sale_item("a", 2, 0, 2000), sale_item("b", 1, 0, 500)],
            false,
        );
        let items = distribute_plain_refund(&sale, 1500);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].item_id, "a");
        assert_eq!(items[0].quantity, 1);
        assert_eq!(items[0].amount, 1000);
        // A whole line plus the next line, capped by availability.
        let items = distribute_plain_refund(&sale, 3000);
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].quantity, 2);
        assert_eq!(items[1].quantity, 1);
        // Money that covers no whole unit moves no stock.
        let cheap = sale(vec![sale_item("a", 1, 0, 1000)], false);
        assert!(distribute_plain_refund(&cheap, 500).is_empty());
        // Already-refunded units are not distributed again.
        let partial = sale(vec![sale_item("a", 2, 1, 2000)], false);
        let items = distribute_plain_refund(&partial, 1500);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].quantity, 1);
    }

    #[test]
    fn order_created_payload_carries_tax_inclusive_and_country() {
        // Regression (O5): the payload omitted tax_inclusive, so
        // the tax module defaulted to exclusive tax on inclusive
        // sales.
        let sale = sale(vec![sale_item("a", 1, 0, 11000)], true);
        let payload = order_created_payload(&sale, Vec::new(), Some("ID"));
        assert_eq!(payload.get("tax_inclusive"), Some(&Value::Bool(true)));
        assert_eq!(
            payload.get("country_code"),
            Some(&Value::String("ID".to_string()))
        );
        let exclusive = sale(vec![sale_item("a", 1, 0, 10000)], false);
        let payload = order_created_payload(&exclusive, Vec::new(), None);
        assert_eq!(payload.get("tax_inclusive"), Some(&Value::Bool(false)));
        assert!(payload.get("country_code").is_none());
    }
}
