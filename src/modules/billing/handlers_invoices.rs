//! billing - handlers invoices.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::json;
use unified_core::pagination::PaginationParams;
use actix_web::{web, HttpRequest, HttpResponse};
use chrono::Duration;
use serde_json::{json, Value};
use std::sync::Arc;
use super::types::*;
use super::state::*;
use super::reference::*;
use super::helpers::*;

pub(crate) async fn list_invoices(
    state: web::Data<Arc<BillingState>>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let _tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return json::error_response(&error, false),
    };
    let _scope = match caller_company(&_tenant, &req) {
        Ok(scope) => scope,
        Err(error) => return json::error_response(&error, false),
    };
    let params = query.into_inner();
    let sort = params.allowed_sort(&["created_at", "total", "due_date"], "created_at");
    let mut items: Vec<Invoice> = state
        .invoices
        .iter()
        .map(|entry| view_invoice(entry.value()))
        .filter(|invoice| {
            // A caller sees only their own invoices: the list is
            // scoped to the caller identities, so an explicit
            // `user_id` filter for another user simply matches
            // nothing instead of leaking their invoices.
            in_company_scope(&invoice.company_id, &_scope)
                && caller_owns(&req, &invoice.user_id)
                && matches_search(&format!("{} {} {}", invoice.number, invoice.id, invoice.user_id), &params)
                && params
                    .filters
                    .get("status")
                    .is_none_or(|status| invoice.status == *status)
                && params
                    .filters
                    .get("user_id")
                    .is_none_or(|user_id| invoice.user_id == *user_id)
                && params.filters.get("order_id").is_none_or(|order_id| {
                    invoice.order_id.as_deref() == Some(order_id.as_str())
                })
                && params.filters.get("company_id").is_none_or(|company_id| {
                    invoice.company_id.as_deref() == Some(company_id.as_str())
                })
        })
        .collect();
    match sort.as_str() {
        "total" => items.sort_by(|a, b| a.total.cmp(&b.total)),
        "due_date" => items.sort_by(|a, b| a.due_date.cmp(&b.due_date)),
        _ => items.sort_by(|a, b| a.created_at.cmp(&b.created_at)),
    }
    if params.normalized().order == "desc" {
        items.reverse();
    }
    let page = paginate(items, &params);
    json::success_response(page, 200)
}

pub(crate) async fn create_invoice(
    state: web::Data<Arc<BillingState>>,
    req: HttpRequest,
    body: web::Json<CreateInvoice>,
) -> HttpResponse {
    let result: UnifiedResult<Invoice> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = require_company_for_write(&_tenant, &req)?;
        let input = body.into_inner();
        if input.user_id.trim().is_empty() {
            return Err(UnifiedError::bad_request("user_id must not be empty"));
        }
        // A caller may only create invoices for themselves: without
        // this check any caller could create invoices (and debt)
        // under another user's id.
        if !caller_owns(&req, input.user_id.trim()) {
            return Err(UnifiedError::forbidden(
                "Cannot create an invoice for a different user",
            ));
        }
        let company_id = require_company_match(&_tenant, &req, &input.company_id)?;
        if input.items.is_empty() {
            return Err(UnifiedError::bad_request(
                "An invoice needs at least one item",
            ));
        }
        let settings = state.settings();
        let tax_inclusive = input.tax_inclusive.unwrap_or(settings.tax_inclusive);
        let payment_terms_days = input
            .payment_terms_days
            .unwrap_or(settings.payment_terms_days);
        let mut items: Vec<InvoiceItem> = Vec::new();
        let mut subtotal: i64 = 0;
        let mut tax_total: i64 = 0;
        for item in input.items {
            if item.name.trim().is_empty() {
                return Err(UnifiedError::bad_request("Item name must not be empty"));
            }
            if item.unit_price < 0 {
                return Err(UnifiedError::bad_request(
                    "Item unit price must be a non-negative number",
                ));
            }
            let quantity = item.quantity.unwrap_or(1);
            if quantity == 0 {
                return Err(UnifiedError::bad_request("Item quantity must be at least 1"));
            }
            let tax_rate = item.tax_rate.unwrap_or(settings.default_tax_rate);
            if !tax_rate.is_finite() || !(0.0..=1.0).contains(&tax_rate) {
                return Err(UnifiedError::bad_request(
                    "Item tax rate must be a fraction between 0 and 1",
                ));
            }
            let (net, tax) = line_tax(item.unit_price, quantity, tax_rate, tax_inclusive);
            let line_total = if tax_inclusive {
                net + tax
            } else {
                net
            };
            subtotal += net;
            tax_total += tax;
            items.push(InvoiceItem {
                name: item.name.trim().to_string(),
                quantity,
                unit_price: item.unit_price,
                line_total,
                tax_rate,
                tax_amount: tax,
            });
        }
        let computed_total = subtotal + tax_total;
        let currency = input.currency.unwrap_or_else(|| "USD".to_string());
        if currency.len() != 3 || !currency.chars().all(|c| c.is_ascii_alphabetic()) {
            return Err(UnifiedError::bad_request(
                "Currency must be a 3-letter code such as USD",
            ));
        }
        // A caller-supplied total must equal the computed one: a
        // silent override would store lines that do not sum to the
        // invoice total.
        let total = resolve_invoice_total(input.total, computed_total, &currency)?;
        let created = now();
        let invoice_reference =
            invoice_reference_for_create(&**state, input.order_id.as_deref());
        let invoice = Invoice {
            id: new_id(),
            number: state.next_invoice_number(),
            reference: invoice_reference,
            order_id: input.order_id,
            subscription_id: None,
            user_id: input.user_id.trim().to_string(),
            company_id,
            items,
            subtotal,
            tax_total,
            total,
            currency: currency.to_uppercase(),
            status: "draft".to_string(),
            overdue: false,
            tax_inclusive,
            amount_paid: 0,
            credit_total: 0,
            balance: total,
            issue_date: Some(created),
            due_date: Some(
                input
                    .due_date
                    .unwrap_or(created + Duration::days(i64::from(payment_terms_days))),
            ),
            payment_terms_days: Some(payment_terms_days),
            paid_at: None,
            notes: input.notes.filter(|notes| !notes.trim().is_empty()),
            reminders_sent: Vec::new(),
            created_at: created,
            updated_at: created,
        };
        state.invoices.insert(invoice.id.clone(), invoice.clone());
        state.emit(
            "invoice.created",
            serde_json::to_value(&invoice).unwrap_or(Value::Null),
        );
        Ok(view_invoice(&invoice))
    })();
    respond_created(result)
}

pub(crate) async fn from_order(
    state: web::Data<Arc<BillingState>>,
    req: HttpRequest,
    body: web::Json<FromOrderRequest>,
) -> HttpResponse {
    let result: UnifiedResult<Invoice> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = require_company_for_write(&_tenant, &req)?;
        let input = body.into_inner();
        // Reuse an existing invoice for this order when one already exists
        // (for example the draft created automatically by the hook).
        if let Some(existing) = state
            .invoices
            .iter()
            .find(|entry| entry.value().order_id.as_deref() == Some(input.order_id.as_str()))
            .map(|entry| view_invoice(entry.value()))
        {
            require_invoice_owner(&req, &existing)?;
            require_record_in_company_scope(&existing.company_id, &_scope)?;
            return Ok(existing);
        }
        let order = state
            .seen_orders
            .get(&input.order_id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| {
                UnifiedError::not_found(
                    "Order not found: billing has not seen this order yet. \
                    Place the order through the store first, or create the \
                    invoice directly with items.",
                )
            })?;
        // A caller-supplied user id must match the order's owner:
        // otherwise one user could re-issue another user's order
        // under their own name.
        if let Some(user_id) = input.user_id.as_deref().filter(|id| !id.trim().is_empty()) {
            let order_user = order
                .get("user_id")
                .and_then(|value| value.as_str())
                .unwrap_or("");
            if user_id.trim() != order_user {
                return Err(UnifiedError::unprocessable(
                    "user_id does not match the owner of this order",
                ));
            }
        }
        let mut invoice = state
            .invoice_from_order_value(&order)
            .ok_or_else(|| UnifiedError::unprocessable("Stored order data is incomplete"))?;
        if let Some(user_id) = input.user_id.filter(|id| !id.trim().is_empty()) {
            invoice.user_id = user_id;
        }
        require_invoice_owner(&req, &invoice)?;
        // The company comes from the order, not the body: a named
        // order company must match the caller scope, and an unnamed
        // one adopts it, so the invoice is never stored unscoped
        // under a scoped caller.
        invoice.company_id = require_company_match_for(&_scope, &invoice.company_id)?;
        if let Some(due_date) = input.due_date {
            invoice.due_date = Some(due_date);
        }
        state.invoices.insert(invoice.id.clone(), invoice.clone());
        state.emit(
            "invoice.created",
            serde_json::to_value(&invoice).unwrap_or(Value::Null),
        );
        Ok(view_invoice(&invoice))
    })();
    respond_created(result)
}

pub(crate) async fn get_invoice(
    state: web::Data<Arc<BillingState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<Invoice> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let invoice = state
            .invoices
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Invoice not found"))?;
        require_record_in_company_scope(&invoice.company_id, &_scope)?;
        require_invoice_owner(&req, &invoice)?;
        Ok(view_invoice(&invoice))
    })();
    json::respond(result, false)
}

pub(crate) async fn update_invoice(
    state: web::Data<Arc<BillingState>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdateInvoice>,
) -> HttpResponse {
    let result: UnifiedResult<Invoice> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = require_company_for_write(&_tenant, &req)?;
        let input = body.into_inner();
        let mut invoice = state
            .invoices
            .get_mut(path.as_str())
            .ok_or_else(|| UnifiedError::not_found("Invoice not found"))?;
        require_record_writable_in_company_scope(&invoice.company_id, &_scope)?;
        require_invoice_owner(&req, &invoice)?;
        if matches!(
            invoice.status.as_str(),
            "paid" | "void" | "partially_paid" | "credited" | "partially_refunded"
        ) {
            return Err(UnifiedError::unprocessable(format!(
                "A {} invoice can no longer be edited",
                invoice.status
            )));
        }
        if let Some(status) = input.status {
            if !matches!(status.as_str(), "draft" | "issued") {
                return Err(UnifiedError::unprocessable(
                    "Status can only be changed to draft or issued here; use pay or void for the rest",
                ));
            }
            // Issuing is the enforcement point for pricing
            // reconciliation: a draft becomes `issued` only when
            // the pricing module reconciles every component. The
            // tenant id comes from the guard-resolved tenant
            // context above, never from a client header; without
            // one, the guard already rejected the request.
            if status == "issued" && invoice.status != "issued" {
                state.validate_invoice_with_pricing(&_tenant.tenant_id.to_string(), &invoice)?;
            }
            invoice.status = status;
        }
        if let Some(due_date) = input.due_date {
            invoice.due_date = Some(due_date);
        }
        if let Some(notes) = input.notes {
            invoice.notes = if notes.trim().is_empty() {
                None
            } else {
                Some(notes)
            };
        }
        invoice.updated_at = now();
        Ok(view_invoice(&invoice))
    })();
    json::respond(result, false)
}

pub(crate) async fn delete_invoice(
    state: web::Data<Arc<BillingState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = require_company_for_write(&_tenant, &req)?;
        let invoice = state
            .invoices
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Invoice not found"))?;
        require_record_writable_in_company_scope(&invoice.company_id, &_scope)?;
        require_invoice_owner(&req, &invoice)?;
        if invoice.status != "draft" {
            return Err(UnifiedError::unprocessable(
                "Only draft invoices can be deleted; void the invoice instead",
            ));
        }
        state.invoices.remove(path.as_str());
        // A draft has no payments or credit notes in normal use, but
        // remove any that exist so no orphan records remain.
        let payment_ids: Vec<String> = state
            .payments
            .iter()
            .filter(|entry| entry.value().invoice_id == path.as_str())
            .map(|entry| entry.key().clone())
            .collect();
        for id in payment_ids {
            state.payments.remove(&id);
        }
        let credit_ids: Vec<String> = state
            .credit_notes
            .iter()
            .filter(|entry| entry.value().invoice_id == path.as_str())
            .map(|entry| entry.key().clone())
            .collect();
        for id in credit_ids {
            state.credit_notes.remove(&id);
        }
        Ok(json!({ "deleted": true, "id": path.as_str() }))
    })();
    json::respond(result, false)
}

pub(crate) async fn pay_invoice(
    state: web::Data<Arc<BillingState>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<PayInvoiceRequest>,
) -> HttpResponse {
    let result: UnifiedResult<Invoice> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = require_company_for_write(&_tenant, &req)?;
        let method = body
            .into_inner()
            .method
            .unwrap_or_else(|| "manual".to_string());
        if method.trim().is_empty() {
            return Err(UnifiedError::bad_request("Payment method must not be empty"));
        }
        let mut invoice = state
            .invoices
            .get_mut(path.as_str())
            .ok_or_else(|| UnifiedError::not_found("Invoice not found"))?;
        require_record_writable_in_company_scope(&invoice.company_id, &_scope)?;
        require_invoice_owner(&req, &invoice)?;
        // A draft must be issued first: paying it in full here
        // would jump draft straight to paid and bypass the pricing
        // reconciliation that issuing enforces.
        check_payment_allowed(&invoice)?;
        let remaining = invoice.balance;
        if remaining <= 0 {
            return Err(UnifiedError::conflict(
                "Invoice has no outstanding balance",
            ));
        }
        // Full payment: record it as a payment entry so the payment
        // history is complete, then close the invoice.
        let payment = InvoicePayment {
            id: new_id(),
            invoice_id: invoice.id.clone(),
            amount: remaining,
            method: method.clone(),
            reference: None,
            recorded_at: now(),
        };
        state.payments.insert(payment.id.clone(), payment.clone());
        invoice.amount_paid += remaining;
        recompute_balance(&mut invoice);
        invoice.status = "paid".to_string();
        invoice.paid_at = Some(now());
        invoice.updated_at = now();
        let invoice_value = serde_json::to_value(&*invoice).unwrap_or(Value::Null);
        let payment_value = serde_json::to_value(&payment).unwrap_or(Value::Null);
        drop(invoice);
        state.emit("invoice.payment.recorded", payment_value);
        state.emit(
            "invoice.paid",
            json!({ "invoice": invoice_value, "method": method }),
        );
        // The invoice was just held mutably above; a concurrent
        // delete can still remove it before this re-read, so a
        // missing invoice is an error, not a panic.
        let paid = state
            .invoices
            .get(path.as_str())
            .map(|entry| view_invoice(entry.value()))
            .ok_or_else(|| {
                UnifiedError::internal("Invoice was deleted while recording its payment")
            })?;
        // A manually paid renewal invoice advances the subscription
        // period exactly like a captured gateway payment.
        state.maybe_renew_subscription(&paid);
        Ok(paid)
    })();
    json::respond(result, false)
}

pub(crate) async fn void_invoice(
    state: web::Data<Arc<BillingState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<Invoice> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = require_company_for_write(&_tenant, &req)?;
        let mut invoice = state
            .invoices
            .get_mut(path.as_str())
            .ok_or_else(|| UnifiedError::not_found("Invoice not found"))?;
        require_record_writable_in_company_scope(&invoice.company_id, &_scope)?;
        require_invoice_owner(&req, &invoice)?;
        // Voiding an invoice with recorded payments would leave
        // those payments on the books with no reversal: the
        // payments must be refunded or credited first.
        check_void_allowed(&invoice)?;
        invoice.status = "void".to_string();
        invoice.updated_at = now();
        let invoice_value = serde_json::to_value(&*invoice).unwrap_or(Value::Null);
        drop(invoice);
        state.emit("invoice.voided", invoice_value);
        // The invoice was just held mutably above; a concurrent
        // delete can still remove it before this re-read, so a
        // missing invoice is an error, not a panic.
        state
            .invoices
            .get(path.as_str())
            .map(|entry| view_invoice(entry.value()))
            .ok_or_else(|| UnifiedError::internal("Invoice was deleted while voiding it"))
    })();
    json::respond(result, false)
}

pub(crate) async fn render_invoice(
    state: web::Data<Arc<BillingState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let invoice = state
            .invoices
            .get(path.as_str())
            .map(|entry| view_invoice(entry.value()))
            .ok_or_else(|| UnifiedError::not_found("Invoice not found"))?;
        require_record_in_company_scope(&invoice.company_id, &_scope)?;
        require_invoice_owner(&req, &invoice)?;
        let html = render_invoice_html(&invoice);
        Ok(json!({ "invoice": invoice, "html": html }))
    })();
    json::respond(result, false)
}

pub(crate) async fn list_overdue(
    state: web::Data<Arc<BillingState>>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let _tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return json::error_response(&error, false),
    };
    let _scope = match caller_company(&_tenant, &req) {
        Ok(scope) => scope,
        Err(error) => return json::error_response(&error, false),
    };
    let params = query.into_inner();
    let mut items: Vec<Invoice> = state
        .invoices
        .iter()
        .map(|entry| view_invoice(entry.value()))
        .filter(|invoice| {
            invoice.overdue
                // Scoped to the caller like `list_invoices`: an
                // overdue list must not expose other users' debts.
                && in_company_scope(&invoice.company_id, &_scope)
                && caller_owns(&req, &invoice.user_id)
                && matches_search(&format!("{} {} {}", invoice.number, invoice.id, invoice.user_id), &params)
                && params
                    .filters
                    .get("user_id")
                    .is_none_or(|user_id| invoice.user_id == *user_id)
        })
        .collect();
    items.sort_by(|a, b| a.due_date.cmp(&b.due_date));
    if params.normalized().order == "desc" {
        items.reverse();
    }
    let page = paginate(items, &params);
    json::success_response(page, 200)
}

pub(crate) async fn get_settings(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<BillingState>>,
    req: HttpRequest,
) -> HttpResponse {
    let _tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return json::error_response(&error, ctx.is_debug()),
    };
    let _scope = match caller_company(&_tenant, &req) {
        Ok(scope) => scope,
        Err(error) => return json::error_response(&error, ctx.is_debug()),
    };
    if let Err(error) = billing_authorize(&ctx, &req, PERM_BILLING_SETTINGS_VIEW) {
        return json::error_response(&error, ctx.is_debug());
    }
    let settings = state.settings();
    json::success_response(
        json!({
            "invoice_prefix": settings.invoice_prefix,
            "default_tax_rate": settings.default_tax_rate,
            "tax_inclusive": settings.tax_inclusive,
            "payment_terms_days": settings.payment_terms_days,
            "reminder_before_days": settings.reminder_before_days,
            "reminder_after_days": settings.reminder_after_days,
            "next_invoice_number": state.next_invoice_number_preview(),
        }),
        200,
    )
}

pub(crate) async fn update_settings(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<BillingState>>,
    req: HttpRequest,
    body: web::Json<UpdateBillingSettings>,
) -> HttpResponse {
    let result: UnifiedResult<BillingSettings> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        billing_authorize(&ctx, &req, PERM_BILLING_SETTINGS_MANAGE)?;
        let input = body.into_inner();
        if let Some(prefix) = &input.invoice_prefix {
            let prefix = prefix.trim();
            if prefix.is_empty()
                || prefix.len() > 16
                || !prefix
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
            {
                return Err(UnifiedError::bad_request(
                    "invoice_prefix must be 1-16 letters, digits, '-' or '_'",
                ));
            }
        }
        if let Some(rate) = input.default_tax_rate {
            if !rate.is_finite() || !(0.0..=1.0).contains(&rate) {
                return Err(UnifiedError::bad_request(
                    "default_tax_rate must be a fraction between 0 and 1",
                ));
            }
        }
        if let Some(days) = input.payment_terms_days {
            if days > 365 {
                return Err(UnifiedError::bad_request(
                    "payment_terms_days must be between 0 and 365",
                ));
            }
        }
        for (name, days) in [
            ("reminder_before_days", &input.reminder_before_days),
            ("reminder_after_days", &input.reminder_after_days),
        ] {
            if let Some(days) = days {
                if days.len() > 30 || days.iter().any(|day| *day < 0 || *day > 365) {
                    return Err(UnifiedError::bad_request(format!(
                        "{name} must hold at most 30 entries, each between 0 and 365"
                    )));
                }
            }
        }
        let mut settings = state
            .settings
            .write()
            .expect("billing settings lock poisoned");
        if let Some(prefix) = input.invoice_prefix {
            settings.invoice_prefix = prefix.trim().to_string();
        }
        if let Some(rate) = input.default_tax_rate {
            settings.default_tax_rate = rate;
        }
        if let Some(inclusive) = input.tax_inclusive {
            settings.tax_inclusive = inclusive;
        }
        if let Some(days) = input.payment_terms_days {
            settings.payment_terms_days = days;
        }
        if let Some(days) = input.reminder_before_days {
            settings.reminder_before_days = days;
        }
        if let Some(days) = input.reminder_after_days {
            settings.reminder_after_days = days;
        }
        Ok(settings.clone())
    })();
    json::respond(result, ctx.is_debug())
}

pub(crate) async fn record_payment(
    state: web::Data<Arc<BillingState>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<RecordPaymentRequest>,
) -> HttpResponse {
    let result: UnifiedResult<InvoicePayment> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = require_company_for_write(&_tenant, &req)?;
        let input = body.into_inner();
        if input.amount <= 0 {
            return Err(UnifiedError::bad_request(
                "Payment amount must be a positive number",
            ));
        }
        let method = input
            .method
            .unwrap_or_else(|| "manual".to_string());
        if method.trim().is_empty() {
            return Err(UnifiedError::bad_request("Payment method must not be empty"));
        }
        let reference = input
            .reference
            .filter(|reference| !reference.trim().is_empty())
            .map(|reference| {
                if reference.len() > 128 {
                    reference.chars().take(128).collect()
                } else {
                    reference
                }
            });
        let mut invoice = state
            .invoices
            .get_mut(path.as_str())
            .ok_or_else(|| UnifiedError::not_found("Invoice not found"))?;
        require_record_in_company_scope(&invoice.company_id, &_scope)?;
        require_invoice_owner(&req, &invoice)?;
        // A draft must be issued first: recording a payment here
        // would bypass the pricing reconciliation that issuing
        // enforces, the same as paying a draft in full.
        check_payment_allowed(&invoice)?;
        let amount = input.amount;
        if amount > invoice.balance {
            return Err(UnifiedError::unprocessable(format!(
                "Payment of {:.2} exceeds the outstanding balance of {:.2}",
                minor_to_major(amount, &invoice.currency),
                minor_to_major(invoice.balance, &invoice.currency)
            )));
        }
        let payment = InvoicePayment {
            id: new_id(),
            invoice_id: invoice.id.clone(),
            amount,
            method: method.clone(),
            reference,
            recorded_at: now(),
        };
        state.payments.insert(payment.id.clone(), payment.clone());
        invoice.amount_paid += amount;
        recompute_balance(&mut invoice);
        if invoice.balance == 0 {
            invoice.status = "paid".to_string();
            invoice.paid_at = Some(now());
        } else {
            invoice.status = "partially_paid".to_string();
        }
        invoice.updated_at = now();
        let fully_paid = invoice.status == "paid";
        let invoice_value = serde_json::to_value(&*invoice).unwrap_or(Value::Null);
        let payment_value = serde_json::to_value(&payment).unwrap_or(Value::Null);
        drop(invoice);
        state.emit("invoice.payment.recorded", payment_value);
        if fully_paid {
            state.emit(
                "invoice.paid",
                json!({ "invoice": invoice_value, "method": method }),
            );
        }
        Ok(payment)
    })();
    match result {
        Ok(payment) => json::success_response(payment, 201),
        Err(error) => json::error_response(&error, false),
    }
}

pub(crate) async fn list_payments(
    state: web::Data<Arc<BillingState>>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let _tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return json::error_response(&error, false),
    };
    let _scope = match caller_company(&_tenant, &req) {
        Ok(scope) => scope,
        Err(error) => return json::error_response(&error, false),
    };
    let params = query.into_inner();
    let invoice = match state.invoices.get(path.as_str()) {
        Some(entry) => entry.value().clone(),
        None => {
            return json::error_response(&UnifiedError::not_found("Invoice not found"), false);
        }
    };
    if let Err(error) = require_record_in_company_scope(&invoice.company_id, &_scope) {
        return json::error_response(&error, false);
    }
    if let Err(error) = require_invoice_owner(&req, &invoice) {
        return json::error_response(&error, false);
    }
    let mut items: Vec<InvoicePayment> = state
        .payments
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|payment| {
            payment.invoice_id == path.as_str()
                && matches_search(
                    &format!("{} {}", payment.method, payment.reference.clone().unwrap_or_default()),
                    &params,
                )
        })
        .collect();
    items.sort_by(|a, b| a.recorded_at.cmp(&b.recorded_at));
    if params.normalized().order == "desc" {
        items.reverse();
    }
    let page = paginate(items, &params);
    json::success_response(page, 200)
}

pub(crate) async fn create_credit_note(
    state: web::Data<Arc<BillingState>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<CreateCreditNoteRequest>,
) -> HttpResponse {
    let result: UnifiedResult<CreditNote> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = require_company_for_write(&_tenant, &req)?;
        let input = body.into_inner();
        if input.amount <= 0 {
            return Err(UnifiedError::bad_request(
                "Credit note amount must be a positive number",
            ));
        }
        if input.reason.trim().is_empty() {
            return Err(UnifiedError::bad_request("Credit note reason must not be empty"));
        }
        if input.reason.len() > 500 {
            return Err(UnifiedError::bad_request(
                "Credit note reason must be at most 500 characters",
            ));
        }
        let mut invoice = state
            .invoices
            .get_mut(path.as_str())
            .ok_or_else(|| UnifiedError::not_found("Invoice not found"))?;
        require_record_in_company_scope(&invoice.company_id, &_scope)?;
        require_invoice_owner(&req, &invoice)?;
        if invoice.status == "void" {
            return Err(UnifiedError::unprocessable(
                "A void invoice cannot receive a credit note",
            ));
        }
        let amount = input.amount;
        // The creditable amount depends on the invoice state: a
        // paid (or partially refunded) invoice has no outstanding
        // balance, so its credit is capped at what was paid --
        // mirroring the `payment.refunded` hook, which credits
        // against `amount_paid`. Capping at the balance would make
        // a fully-paid invoice uncreditable at all.
        let cap = credit_note_cap(&invoice);
        if amount > cap {
            return Err(UnifiedError::unprocessable(format!(
                "Credit note of {:.2} exceeds the creditable amount of {:.2}",
                minor_to_major(amount, &invoice.currency),
                minor_to_major(cap, &invoice.currency)
            )));
        }
        let credit_note = CreditNote {
            id: new_id(),
            invoice_id: invoice.id.clone(),
            number: state.next_credit_note_number(),
            amount,
            reason: input.reason.trim().to_string(),
            created_at: now(),
        };
        state
            .credit_notes
            .insert(credit_note.id.clone(), credit_note.clone());
        if matches!(invoice.status.as_str(), "paid" | "partially_refunded") {
            // A credit against paid money is a refund of it: the
            // paid amount falls, exactly as the refund hook does,
            // instead of pushing the balance below zero.
            invoice.amount_paid = (invoice.amount_paid - amount).max(0);
            invoice.credit_total += amount;
            recompute_balance(&mut invoice);
            invoice.status = if invoice.amount_paid == 0 {
                "credited".to_string()
            } else {
                "partially_refunded".to_string()
            };
        } else {
            invoice.credit_total += amount;
            recompute_balance(&mut invoice);
            if invoice.balance == 0 {
                invoice.status = "credited".to_string();
            }
        }
        invoice.updated_at = now();
        let credit_value = serde_json::to_value(&credit_note).unwrap_or(Value::Null);
        drop(invoice);
        state.emit("invoice.credit_note.created", credit_value);
        Ok(credit_note)
    })();
    match result {
        Ok(credit_note) => json::success_response(credit_note, 201),
        Err(error) => json::error_response(&error, false),
    }
}

pub(crate) async fn list_credit_notes(
    state: web::Data<Arc<BillingState>>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let _tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return json::error_response(&error, false),
    };
    let _scope = match caller_company(&_tenant, &req) {
        Ok(scope) => scope,
        Err(error) => return json::error_response(&error, false),
    };
    let params = query.into_inner();
    let invoice = match state.invoices.get(path.as_str()) {
        Some(entry) => entry.value().clone(),
        None => {
            return json::error_response(&UnifiedError::not_found("Invoice not found"), false);
        }
    };
    if let Err(error) = require_record_in_company_scope(&invoice.company_id, &_scope) {
        return json::error_response(&error, false);
    }
    if let Err(error) = require_invoice_owner(&req, &invoice) {
        return json::error_response(&error, false);
    }
    let mut items: Vec<CreditNote> = state
        .credit_notes
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|note| {
            note.invoice_id == path.as_str()
                && matches_search(&format!("{} {}", note.number, note.reason), &params)
        })
        .collect();
    items.sort_by(|a, b| a.created_at.cmp(&b.created_at));
    if params.normalized().order == "desc" {
        items.reverse();
    }
    let page = paginate(items, &params);
    json::success_response(page, 200)
}

pub(crate) async fn pdf_data(
    state: web::Data<Arc<BillingState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let invoice = state
            .invoices
            .get(path.as_str())
            .map(|entry| view_invoice(entry.value()))
            .ok_or_else(|| UnifiedError::not_found("Invoice not found"))?;
        require_record_in_company_scope(&invoice.company_id, &_scope)?;
        require_invoice_owner(&req, &invoice)?;
        let payments: Vec<InvoicePayment> = state
            .payments
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|payment| payment.invoice_id == invoice.id)
            .collect();
        let credit_notes: Vec<CreditNote> = state
            .credit_notes
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|note| note.invoice_id == invoice.id)
            .collect();
        // PDF rendering itself is a follow-on step: feed this payload
        // to a PDF renderer (for example a headless-browser or
        // print-CSS pipeline). This endpoint returns data only, never
        // a rendered file.
        Ok(json!({
            "invoice": invoice,
            "company": {
                "id": invoice.company_id,
                "name": Value::Null,
                "note": "Populate company details from the company module or settings before rendering.",
            },
            "customer": { "user_id": invoice.user_id },
            "lines": invoice.items,
            "totals": {
                "subtotal": invoice.subtotal,
                "tax_total": invoice.tax_total,
                "total": invoice.total,
                "amount_paid": invoice.amount_paid,
                "credit_total": invoice.credit_total,
                "balance": invoice.balance,
                "currency": invoice.currency,
                "tax_inclusive": invoice.tax_inclusive,
            },
            "dates": {
                "issue_date": invoice.issue_date,
                "due_date": invoice.due_date,
                "paid_at": invoice.paid_at,
            },
            "payments": payments,
            "credit_notes": credit_notes,
            "notes": invoice.notes,
            "renderer": "PDF rendering is a follow-on step; this payload is shaped for a PDF renderer.",
        }))
    })();
    json::respond(result, false)
}

/// Emit one overdue reminder for an invoice. The reminder itself is
/// delivered by the mail module through the `billing.invoice.reminder`
/// hook; billing only decides that a reminder is due and records that
/// it was sent.
pub(crate) fn send_one_reminder(
    state: &BillingState,
    invoice_id: &str,
    offset_days: i32,
    message: Option<String>,
    manual: bool,
) -> UnifiedResult<Value> {
    let mut invoice = state
        .invoices
        .get_mut(invoice_id)
        .ok_or_else(|| UnifiedError::not_found("Invoice not found"))?;
    if invoice.balance <= 0 {
        return Err(UnifiedError::unprocessable(
            "Invoice has no outstanding balance to remind about",
        ));
    }
    if matches!(invoice.status.as_str(), "paid" | "void" | "credited") {
        return Err(UnifiedError::unprocessable(format!(
            "A {} invoice cannot receive reminders",
            invoice.status
        )));
    }
    let due_date = invoice
        .due_date
        .ok_or_else(|| UnifiedError::unprocessable("Invoice has no due date"))?;
    if !manual && invoice.reminders_sent.contains(&offset_days) {
        return Err(UnifiedError::conflict(
            "This reminder was already sent for the invoice",
        ));
    }
    if !invoice.reminders_sent.contains(&offset_days) {
        invoice.reminders_sent.push(offset_days);
    }
    invoice.updated_at = now();
    let days_until_due = (due_date.date_naive() - now().date_naive()).num_days();
    let invoice_value = serde_json::to_value(view_invoice(&invoice)).unwrap_or(Value::Null);
    drop(invoice);
    state.emit(
        "billing.invoice.reminder",
        json!({
            "invoice": invoice_value,
            "invoice_id": invoice_id,
            "offset_days": offset_days,
            "days_until_due": days_until_due,
            "due_date": due_date,
            "manual": manual,
            "message": message,
            "note": "Deliver this reminder through the mail module; billing only emits the hook.",
        }),
    );
    Ok(json!({
        "invoice_id": invoice_id,
        "offset_days": offset_days,
        "days_until_due": days_until_due,
        "manual": manual,
        "sent": true,
    }))
}

pub(crate) async fn remind_invoice(
    state: web::Data<Arc<BillingState>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<RemindRequest>,
) -> HttpResponse {
    let input = body.into_inner();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let invoice = state
            .invoices
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Invoice not found"))?;
        require_record_in_company_scope(&invoice.company_id, &_scope)?;
        require_invoice_owner(&req, &invoice)?;
        let offset_days = match input.offset_days {
            Some(offset) => offset,
            None => {
                let due = invoice
                    .due_date
                    .ok_or_else(|| UnifiedError::unprocessable("Invoice has no due date"))?;
                let days = (due.date_naive() - now().date_naive()).num_days();
                i32::try_from(days).unwrap_or(0)
            }
        };
        send_one_reminder(&state, path.as_str(), offset_days, input.message, true)
    })();
    json::respond(result, false)
}

/// Scan every open invoice for reminders that fall due today and emit
/// one `billing.invoice.reminder` hook per reminder. Returns a summary
/// of what was sent.
pub(crate) fn run_reminder_scan(
    state: &BillingState,
    scope: &Option<String>,
) -> Value {
    let settings = state.settings();
    let at = now();
    let invoice_ids: Vec<String> = state
        .invoices
        .iter()
        .map(|entry| entry.key().clone())
        .collect();
    let mut sent = 0u64;
    let mut checked = 0u64;
    let mut details: Vec<Value> = Vec::new();
    for invoice_id in &invoice_ids {
        let Some(invoice) = state
            .invoices
            .get(invoice_id)
            .map(|entry| entry.value().clone())
        else {
            continue;
        };
        // A scoped trigger only reminds its own company's
        // invoices; the scheduler hook passes `&None` and keeps
        // the global scan.
        if !in_company_scope(&invoice.company_id, scope) {
            continue;
        }
        checked += 1;
        for offset in reminders_due(&invoice, &settings, at) {
            match send_one_reminder(state, invoice_id, offset, None, false) {
                Ok(detail) => {
                    sent += 1;
                    details.push(detail);
                }
                Err(_) => {
                    // A reminder that can no longer be sent (invoice
                    // paid or credited since the scan started) is
                    // skipped; the scan continues with the rest.
                }
            }
        }
    }
    json!({
        "reminders_sent": sent,
        "invoices_checked": checked,
        "details": details,
    })
}

pub(crate) async fn run_reminders(
    state: web::Data<Arc<BillingState>>,
    req: HttpRequest,
) -> HttpResponse {
    let _tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return json::error_response(&error, false),
    };
    let _scope = match caller_company(&_tenant, &req) {
        Ok(scope) => scope,
        Err(error) => return json::error_response(&error, false),
    };
    let summary = run_reminder_scan(&state, &_scope);
    state.emit("billing.invoice.reminders_completed", summary.clone());
    json::success_response(summary, 200)
}

#[cfg(test)]
mod tests {

    /// Build a request carrying the server-verified caller the auth
    /// middleware would attach for this user and role. The old
    /// self-asserted identity headers grant nothing,
    /// so tests attach the verified caller to the request
    /// extensions instead.
    fn verified_request(user_id: &str, role: &str) -> actix_web::HttpRequest {
        use actix_web::HttpMessage;
        let req = actix_web::test::TestRequest::default().to_http_request();
        // The tenant guard requires a resolved tenant context:
        // attach the middleware context the tenancy layer reads.
        req.extensions_mut()
            .insert(unified_core::middleware::TenantContext {
                tenant_id: uuid::Uuid::now_v7().to_string(),
                company_id: String::new(),
                site_id: String::new(),
                kind: unified_core::middleware::TenantKind::Subdomain,
                host: "tenant.example.test".to_string(),
                scope: None,
                product_id: None,
                package_id: None,
                product_subdomain_id: None,
            });
        req.extensions_mut().insert(
            unified_core::auth::Caller::verified(test_user_uuid(user_id), Vec::new())
                .with_roles(vec![role.to_string()]),
        );
        req
    }

    /// Build a request carrying a verified caller with no roles
    /// resolved: the caller resolves, but holds the anonymous role.
    /// This is the "no role" case in the new model (roles are
    /// resolved server-side, never claimed in a header).
    fn verified_request_without_roles(user_id: &str) -> actix_web::HttpRequest {
        use actix_web::HttpMessage;
        let req = actix_web::test::TestRequest::default().to_http_request();
        req.extensions_mut()
            .insert(unified_core::middleware::TenantContext {
                tenant_id: uuid::Uuid::now_v7().to_string(),
                company_id: String::new(),
                site_id: String::new(),
                kind: unified_core::middleware::TenantKind::Subdomain,
                host: "tenant.example.test".to_string(),
                scope: None,
                product_id: None,
                package_id: None,
                product_subdomain_id: None,
            });
        req.extensions_mut()
            .insert(unified_core::auth::Caller::verified(
                test_user_uuid(user_id),
                Vec::new(),
            ));
        req
    }

    /// Deterministic test-user identity: distinct names map to
    /// distinct UUIDs (FNV-1a folded into the low bits), so
    /// ownership tests can still tell two users apart. A value
    /// that already parses as a UUID is used as-is.
    fn test_user_uuid(name: &str) -> uuid::Uuid {
        if let Ok(id) = uuid::Uuid::parse_str(name) {
            return id;
        }
        let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
        for byte in name.bytes() {
            hash ^= u64::from(byte);
            hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        }
        uuid::Uuid::from_u128(u128::from(hash) | 0x0000_0000_0000_4000_8000_0000_0000_0000)
    }

    use super::*;
    use unified_core::config::AppConfig;
    use unified_core::hooks::HookBus;
    use unified_core::modules::ModuleRegistry;
    use actix_web::http::StatusCode;
    use actix_web::test::TestRequest;

    fn test_context() -> AppContext {
        AppContext::new(
            AppConfig::default(),
            Arc::new(HookBus::new()),
            Arc::new(ModuleRegistry::new()),
        )
    }

    /// A context whose `role.check` handler allows every permission.
    fn allowing_context() -> AppContext {
        let ctx = test_context();
        ctx.hooks.register(
            "role.check",
            "test",
            Arc::new(|context: &mut unified_core::hooks::HookContext| {
                context.set("allowed", serde_json::Value::Bool(true));
            }),
        );
        ctx
    }

    fn authed_request() -> HttpRequest {
        verified_request("user-1", "admin")
    }

    fn ctx_data(ctx: AppContext) -> web::Data<Arc<AppContext>> {
        web::Data::new(Arc::new(ctx))
    }

    fn state_data() -> web::Data<Arc<BillingState>> {
        web::Data::new(Arc::new(BillingState::new()))
    }

    fn settings_body() -> web::Json<UpdateBillingSettings> {
        web::Json(UpdateBillingSettings {
            invoice_prefix: None,
            default_tax_rate: None,
            tax_inclusive: None,
            payment_terms_days: None,
            reminder_before_days: None,
            reminder_after_days: None,
        })
    }

    #[tokio::test]
    async fn get_settings_rejects_anonymous_callers() {
        let response = get_settings(
            ctx_data(test_context()),
            state_data(),
            TestRequest::default().to_http_request(),
        )
        .await;
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn get_settings_rejects_callers_without_billing_settings_view() {
        // Headers present, but no `role.check` handler answers:
        // the guard fails closed.
        let response = get_settings(ctx_data(test_context()), state_data(), authed_request()).await;
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn get_settings_accepts_callers_with_billing_settings_view() {
        let response =
            get_settings(ctx_data(allowing_context()), state_data(), authed_request()).await;
        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn update_settings_rejects_anonymous_callers() {
        let response = update_settings(
            ctx_data(test_context()),
            state_data(),
            TestRequest::default().to_http_request(),
            settings_body(),
        )
        .await;
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn update_settings_rejects_callers_without_billing_settings_manage() {
        let response = update_settings(
            ctx_data(test_context()),
            state_data(),
            authed_request(),
            settings_body(),
        )
        .await;
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn update_settings_accepts_callers_with_billing_settings_manage() {
        let response = update_settings(
            ctx_data(allowing_context()),
            state_data(),
            authed_request(),
            settings_body(),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
    }
}
