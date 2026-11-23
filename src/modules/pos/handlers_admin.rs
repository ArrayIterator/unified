// Pos module — payment method, settings, and report handlers.
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

// ---------------------------------------------------------------------------
// Payment-method handlers (admin configuration)
// ---------------------------------------------------------------------------

pub(crate) async fn list_payment_methods(
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
        state.seed_payment_methods();
        let params = query.into_inner();
        let active_filter = params.filters.get("active").cloned();
        let mut items: Vec<PosPaymentMethod> = state
            .payment_methods
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|method| {
                if let Some(active) = &active_filter {
                    let wanted = active.trim().to_lowercase() == "true";
                    if method.active != wanted {
                        return false;
                    }
                }
                true
            })
            .collect();
        items.sort_by(|a, b| a.code.cmp(&b.code));
        Ok(paginate(items, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn create_payment_method(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<PosState>,
    req: HttpRequest,
    body: web::Json<CreatePaymentMethodRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PosPaymentMethod> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = require_company_for_write(&_tenant, &req)?;

        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_POS_MANAGE)?;
        state.seed_payment_methods();
        let input = body.into_inner();
        let code = require_text(&input.code, "code", MAX_CODE_LEN)?.to_lowercase();
        if !code
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
        {
            return Err(UnifiedError::bad_request(
                "code must contain only lowercase letters, digits, and underscores",
            ));
        }
        if state
            .payment_methods
            .iter()
            .any(|entry| entry.value().code == code)
        {
            return Err(UnifiedError::conflict(format!(
                "Payment method code already exists: {code}"
            )));
        }
        let label = require_text(&input.label, "label", MAX_LABEL_LEN)?;
        let kind = match &input.kind {
            Some(raw) => require_text(raw, "kind", MAX_CODE_LEN)?.to_lowercase(),
            None => "other".to_string(),
        };
        let method = PosPaymentMethod {
            id: new_id(),
            code,
            label,
            kind,
            is_cash: input.is_cash.unwrap_or(false),
            requires_reference: input.requires_reference.unwrap_or(false),
            active: input.active.unwrap_or(true),
            created_at: now(),
            updated_at: now(),
        };
        state
            .payment_methods
            .insert(method.id.clone(), method.clone());
        state.emit(
            "pos.payment_method.created",
            json!({
                "method_id": method.id,
                "code": method.code,
                "created_by": caller.actor,
            }),
        );
        Ok(method)
    })();
    match result {
        Ok(data) => created(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn update_payment_method(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<PosState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdatePaymentMethodRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PosPaymentMethod> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = require_company_for_write(&_tenant, &req)?;

        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_POS_MANAGE)?;
        let id = path.into_inner();
        let input = body.into_inner();
        let mut method = state
            .payment_methods
            .get(&id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found(format!("Payment method not found: {id}")))?;
        if let Some(label) = &input.label {
            method.label = require_text(label, "label", MAX_LABEL_LEN)?;
        }
        if let Some(kind) = &input.kind {
            method.kind = require_text(kind, "kind", MAX_CODE_LEN)?.to_lowercase();
        }
        if let Some(is_cash) = input.is_cash {
            method.is_cash = is_cash;
        }
        if let Some(requires_reference) = input.requires_reference {
            method.requires_reference = requires_reference;
        }
        if let Some(active) = input.active {
            if !active
                && state
                    .payments
                    .iter()
                    .any(|entry| entry.value().method_id == id)
            {
                return Err(UnifiedError::unprocessable(
                    "Cannot disable a payment method that completed sales used; delete is also blocked, keep it enabled or leave it unused",
                ));
            }
            method.active = active;
        }
        method.updated_at = now();
        state
            .payment_methods
            .insert(method.id.clone(), method.clone());
        state.emit(
            "pos.payment_method.updated",
            json!({
                "method_id": method.id,
                "code": method.code,
                "active": method.active,
                "updated_by": caller.actor,
            }),
        );
        Ok(method)
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn delete_payment_method(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<PosState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = require_company_for_write(&_tenant, &req)?;

        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_POS_MANAGE)?;
        let id = path.into_inner();
        let method = state
            .payment_methods
            .get(&id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found(format!("Payment method not found: {id}")))?;
        if state
            .payments
            .iter()
            .any(|entry| entry.value().method_id == id)
            || state
                .refunds
                .iter()
                .any(|entry| entry.value().method_id == id)
        {
            return Err(UnifiedError::unprocessable(
                "Cannot delete a payment method that sales or refunds reference; disable it instead",
            ));
        }
        state.payment_methods.remove(&id);
        state.emit(
            "pos.payment_method.deleted",
            json!({
                "method_id": id,
                "code": method.code,
                "deleted_by": caller.actor,
            }),
        );
        Ok(json!({ "deleted": true, "method_id": id }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

// ---------------------------------------------------------------------------
// Settings handlers
// ---------------------------------------------------------------------------

pub(crate) async fn get_settings(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<PosState>,
    req: HttpRequest,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;

        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_POS_VIEW)?;
        let settings = state.settings();
        Ok(json!({
            "settings": settings,
            "capabilities": {
                // Honest capability flag: there is no offline client
                // in this codebase, so offline queueing and sync are
                // not supported server-side here. A future client may
                // queue sales and replay them through the normal sale
                // endpoints; until then this stays "not_supported".
                "offline_queueing": "not_supported",
                "split_payment": settings.allow_split_payment,
                "receipt_print_lines": true,
            },
        }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn update_settings(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<PosState>,
    req: HttpRequest,
    body: web::Json<UpdateSettingsRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PosSettings> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;

        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_POS_MANAGE)?;
        let input = body.into_inner();
        let mut settings = state.settings();
        if let Some(store_name) = &input.store_name {
            settings.store_name = require_text(store_name, "store_name", MAX_NAME_LEN)?;
        }
        if let Some(store_address) = &input.store_address {
            settings.store_address = match store_address {
                Some(text) => Some(require_text(text, "store_address", MAX_NAME_LEN)?),
                None => None,
            };
        }
        if let Some(receipt_footer) = &input.receipt_footer {
            settings.receipt_footer = match receipt_footer {
                Some(text) => Some(require_text(text, "receipt_footer", MAX_NOTES_LEN)?),
                None => None,
            };
        }
        if let Some(width) = input.receipt_width_chars {
            if width < 16 || width > MAX_RECEIPT_WIDTH {
                return Err(UnifiedError::bad_request(format!(
                    "receipt_width_chars must be between 16 and {MAX_RECEIPT_WIDTH}"
                )));
            }
            settings.receipt_width_chars = width;
        }
        if let Some(currency) = &input.default_currency {
            settings.default_currency = valid_currency(currency)?;
        }
        if let Some(country_code) = &input.country_code {
            settings.country_code = match country_code {
                Some(raw) => {
                    let upper = raw.trim().to_uppercase();
                    if upper.len() != 2 || !upper.chars().all(|c| c.is_ascii_alphabetic()) {
                        return Err(UnifiedError::bad_request(
                            "country_code must be a 2-letter ISO code such as \"ID\"",
                        ));
                    }
                    Some(upper)
                }
                None => None,
            };
        }
        if let Some(default_tax_rate) = input.default_tax_rate {
            if !default_tax_rate.is_finite() || default_tax_rate < 0.0 || default_tax_rate > 10.0 {
                return Err(UnifiedError::bad_request(
                    "default_tax_rate must be a finite fraction between 0 and 10",
                ));
            }
            settings.default_tax_rate = default_tax_rate;
        }
        if let Some(tax_inclusive) = input.tax_inclusive {
            settings.tax_inclusive = tax_inclusive;
        }
        if let Some(max_discount_percent) = input.max_discount_percent {
            if !max_discount_percent.is_finite()
                || max_discount_percent < 0.0
                || max_discount_percent > MAX_DISCOUNT_PERCENT
            {
                return Err(UnifiedError::bad_request(
                    "max_discount_percent must be between 0 and 100",
                ));
            }
            settings.max_discount_percent = max_discount_percent;
        }
        if let Some(require_cashier_assignment) = input.require_cashier_assignment {
            settings.require_cashier_assignment = require_cashier_assignment;
        }
        if let Some(allow_split_payment) = input.allow_split_payment {
            settings.allow_split_payment = allow_split_payment;
        }
        *state
            .settings
            .write()
            .expect("pos settings lock poisoned") = settings.clone();
        state.emit(
            "pos.settings.updated",
            json!({
                "updated_by": caller.actor,
                "default_currency": settings.default_currency,
                "default_tax_rate": settings.default_tax_rate,
            }),
        );
        Ok(settings)
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

// ---------------------------------------------------------------------------
// End-of-day report
// ---------------------------------------------------------------------------

pub(crate) async fn end_of_day_report(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<PosState>,
    req: HttpRequest,
    query: web::Query<ReportQuery>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;

        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_POS_VIEW)?;
        let input = query.into_inner();
        let date = match &input.date {
            Some(raw) => NaiveDate::parse_from_str(raw.trim(), "%Y-%m-%d").map_err(|_| {
                UnifiedError::bad_request("date must be YYYY-MM-DD")
            })?,
            None => Utc::now().date_naive(),
        };
        let day_start = date.and_hms_opt(0, 0, 0).unwrap().and_utc();
        let day_end = (date + chrono::Duration::days(1))
            .and_hms_opt(0, 0, 0)
            .unwrap()
            .and_utc();
        let register_filter = input.register_id.clone();
        // Money sums are integer minor units. Sales may span
        // currencies; the totals sum minor units as before (major
        // units were summed the same way) and are emitted as major
        // units on a 2-decimal basis.
        let mut gross_sales = 0i64;
        let mut discount_total = 0i64;
        let mut tax_total = 0i64;
        let mut sales_count = 0u64;
        let mut voids_count = 0u64;
        let mut refunds_total = 0i64;
        let mut refunds_count = 0u64;
        let mut by_method: std::collections::HashMap<String, i64> =
            std::collections::HashMap::new();
        let mut by_register: std::collections::HashMap<String, (i64, u64)> =
            std::collections::HashMap::new();
        for entry in state.sales.iter() {
            let sale = entry.value();
            if !in_company_scope(&sale.company_id, &_scope) {
                continue;
            }
            if let Some(register_id) = &register_filter {
                if sale.register_id != *register_id {
                    continue;
                }
            }
            if sale.status == "voided" {
                if sale.voided_at.map(|at| at >= day_start && at < day_end).unwrap_or(false) {
                    voids_count += 1;
                }
                continue;
            }
            let Some(completed_at) = sale.completed_at else {
                continue;
            };
            if completed_at < day_start || completed_at >= day_end {
                continue;
            }
            if !matches!(
                sale.status.as_str(),
                "completed" | "partially_refunded" | "refunded"
            ) {
                continue;
            }
            sales_count += 1;
            gross_sales += sale.total;
            discount_total += sale.discount_total;
            tax_total += sale.tax_total;
            let register_entry = by_register
                .entry(sale.register_id.clone())
                .or_insert((0, 0));
            register_entry.0 += sale.total;
            register_entry.1 += 1;
            for payment in state.payments.iter() {
                let payment = payment.value();
                if payment.sale_id == sale.id {
                    *by_method.entry(payment.method_code.clone()).or_insert(0) += payment.amount;
                }
            }
        }
        for entry in state.refunds.iter() {
            let refund = entry.value();
            if refund.created_at < day_start || refund.created_at >= day_end {
                continue;
            }
            // Refunds carry no company of their own; they are
            // scoped through the parent sale.
            if let Some(sale) = state.sales.get(&refund.sale_id) {
                if !in_company_scope(&sale.company_id, &_scope) {
                    continue;
                }
            }
            if let Some(register_id) = &register_filter {
                let sale = state.sales.get(&refund.sale_id);
                if sale.map(|sale| sale.value().register_id != *register_id).unwrap_or(true) {
                    continue;
                }
            }
            refunds_total += refund.amount;
            refunds_count += 1;
        }
        let mut by_method_json: Vec<Value> = by_method
            .into_iter()
            .map(|(method, amount)| json!({ "method": method, "amount": minor_to_major(amount, "USD") }))
            .collect();
        by_method_json.sort_by(|a, b| {
            a.get("method")
                .and_then(Value::as_str)
                .cmp(&b.get("method").and_then(Value::as_str))
        });
        let mut by_register_json: Vec<Value> = by_register
            .into_iter()
            .map(|(register_id, (total, count))| {
                json!({
                    "register_id": register_id,
                    "gross_sales": minor_to_major(total, "USD"),
                    "sales_count": count,
                })
            })
            .collect();
        by_register_json.sort_by(|a, b| {
            a.get("register_id")
                .and_then(Value::as_str)
                .cmp(&b.get("register_id").and_then(Value::as_str))
        });
        // Shift cash variances for shifts closed on this date.
        let mut variances: Vec<Value> = Vec::new();
        for entry in state.shifts.iter() {
            let shift = entry.value();
            if let Some(register_id) = &register_filter {
                if shift.register_id != *register_id {
                    continue;
                }
            }
            if shift.status != "closed" {
                continue;
            }
            let Some(closed_at) = shift.closed_at else {
                continue;
            };
            if closed_at < day_start || closed_at >= day_end {
                continue;
            }
            variances.push(json!({
                "shift_id": shift.id,
                "register_id": shift.register_id,
                "cashier_id": shift.cashier_id,
                "opening_float": minor_to_major(shift.opening_float, &shift.currency),
                "expected_cash": shift
                    .expected_cash
                    .map(|amount| minor_to_major(amount, &shift.currency)),
                "counted_cash": shift
                    .counted_cash
                    .map(|amount| minor_to_major(amount, &shift.currency)),
                "variance": shift
                    .variance
                    .map(|amount| minor_to_major(amount, &shift.currency)),
                "currency": shift.currency,
            }));
        }
        variances.sort_by(|a, b| {
            a.get("register_id")
                .and_then(Value::as_str)
                .cmp(&b.get("register_id").and_then(Value::as_str))
        });
        Ok(json!({
            "date": date.to_string(),
            "register_id": register_filter,
            "sales_count": sales_count,
            "gross_sales": minor_to_major(gross_sales, "USD"),
            "discount_total": minor_to_major(discount_total, "USD"),
            "tax_total": minor_to_major(tax_total, "USD"),
            "refunds_total": minor_to_major(refunds_total, "USD"),
            "refunds_count": refunds_count,
            "voids_count": voids_count,
            "net_sales": minor_to_major(gross_sales - refunds_total, "USD"),
            "by_payment_method": by_method_json,
            "by_register": by_register_json,
            "shift_variances": variances,
        }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}
