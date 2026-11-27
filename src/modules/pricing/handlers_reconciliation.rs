// pricing - handlers reconciliation.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::{HookBus, HookContext};
use unified_core::middleware::TenantContext;
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
use super::money::*;
use super::recommendations::*;
use super::discounts::*;
use super::proration::*;
use super::reconciliation::*;
use super::types::*;
use super::store::*;
use super::common::*;
use super::service::*;
use super::handlers_recommendations::*;
use super::handlers_rules::*;
use super::handlers_coupons::*;
use super::handlers_prices::*;
use super::handlers_bundles::*;
use super::module::*;

// HTTP handlers: invoice preview, reconciliation, credit notes.

/// Resolve an invoice preview/reconcile request into engine inputs.
pub(crate) fn reconcile_inputs_from_request(
    state: &PricingState,
    tenant_id: &str,
    request: &InvoicePreviewRequest,
) -> UnifiedResult<(Vec<ReconcileLineInput>, ReconcileClaim, String)> {
    let currency = request
        .currency
        .clone()
        .unwrap_or_else(|| state.settings_for(tenant_id).default_currency)
        .to_uppercase();
    require_currency_known(state, &currency)?;
    let mut lines = Vec::new();
    for line in &request.lines {
        let money = resolve_money(&line.unit_price, Some(&currency))?;
        if money.currency != currency {
            return Err(UnifiedError::bad_request(
                "All invoice lines must use the request currency",
            ));
        }
        lines.push(ReconcileLineInput {
            name: line.name.trim().to_string(),
            quantity: line.quantity,
            unit_price_scaled: round_to_minor(money.amount_scaled, &currency),
            tax_bp: line.tax_bp,
            tax_inclusive: line.tax_inclusive,
        });
    }
    let claim_money = |input: &MoneyInput| -> UnifiedResult<i64> {
        let money = resolve_money(input, Some(&currency))?;
        if money.currency != currency {
            return Err(UnifiedError::bad_request(
                "All claimed totals must use the request currency",
            ));
        }
        Ok(round_to_minor(money.amount_scaled, &currency))
    };
    let claim = ReconcileClaim {
        subtotal_scaled: claim_money(&request.claimed.subtotal)?,
        discounts_scaled: claim_money(&request.claimed.discounts)?,
        proration_scaled: claim_money(&request.claimed.proration)?,
        tax_total_scaled: claim_money(&request.claimed.tax_total)?,
        total_scaled: claim_money(&request.claimed.total)?,
    };
    Ok((lines, claim, currency))
}

/// Preview an invoice: recompute it and return the exact invoice
/// together with the reconciliation result. The admin sees the
/// exact figures before anything is issued; billing must call
/// `pricing.invoice.validate_requested` (or this endpoint) and
/// refuse to issue when `reconciliation.matched` is false.
pub(crate) async fn preview_invoice(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<PricingState>>,
    req: HttpRequest,
    body: web::Json<InvoicePreviewRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let call = caller(&req)?;
        require_permission(&ctx, &call.role, PERM_PRICING_READ)?;
        let input = body.into_inner();
        let (lines, claim, currency) =
            reconcile_inputs_from_request(&state, &call.tenant_id, &input)?;
        let reconciliation = reconcile_invoice(
            &lines,
            &input.discounts,
            &input.proration_lines,
            &claim,
            &currency,
        )?;
        let record = state.record_reconciliation(
            &call.tenant_id,
            input.invoice_id.clone(),
            &reconciliation,
            &call.actor,
        );
        state.emit(
            "pricing.invoice.previewed",
            json!({
                "tenant_id": call.tenant_id,
                "invoice_id": input.invoice_id,
                "reconciliation_id": record.id,
                "matched": reconciliation.matched,
                "currency": currency,
            }),
        );
        if reconciliation.matched {
            state.emit(
                "pricing.invoice.reconciled",
                json!({
                    "tenant_id": call.tenant_id,
                    "invoice_id": input.invoice_id,
                    "reconciliation_id": record.id,
                    "currency": currency,
                }),
            );
        } else {
            state.emit(
                "pricing.invoice.reconciliation_failed",
                json!({
                    "tenant_id": call.tenant_id,
                    "invoice_id": input.invoice_id,
                    "reconciliation_id": record.id,
                    "currency": currency,
                    "total_discrepancy_scaled": reconciliation.total_discrepancy_scaled,
                    "components": reconciliation.components,
                }),
            );
        }
        Ok(json!({
            "invoice_id": input.invoice_id,
            "currency": currency,
            "reconciliation_id": record.id,
            "reconciliation": reconciliation,
            "can_issue": reconciliation.matched,
        }))
    })();
    match result {
        Ok(value) => json::success_response(value, 200),
        Err(error) => json::error_response(&error, debug),
    }
}

pub(crate) async fn reconcile_invoice_endpoint(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<PricingState>>,
    req: HttpRequest,
    body: web::Json<InvoicePreviewRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let call = caller(&req)?;
        require_permission(&ctx, &call.role, PERM_PRICING_MANAGE)?;
        let input = body.into_inner();
        let (lines, claim, currency) =
            reconcile_inputs_from_request(&state, &call.tenant_id, &input)?;
        let reconciliation = reconcile_invoice(
            &lines,
            &input.discounts,
            &input.proration_lines,
            &claim,
            &currency,
        )?;
        let record = state.record_reconciliation(
            &call.tenant_id,
            input.invoice_id.clone(),
            &reconciliation,
            &call.actor,
        );
        if reconciliation.matched {
            state.emit(
                "pricing.invoice.reconciled",
                json!({
                    "tenant_id": call.tenant_id,
                    "invoice_id": input.invoice_id,
                    "reconciliation_id": record.id,
                    "currency": currency,
                }),
            );
        } else {
            state.emit(
                "pricing.invoice.reconciliation_failed",
                json!({
                    "tenant_id": call.tenant_id,
                    "invoice_id": input.invoice_id,
                    "reconciliation_id": record.id,
                    "currency": currency,
                    "total_discrepancy_scaled": reconciliation.total_discrepancy_scaled,
                    "components": reconciliation.components,
                }),
            );
        }
        let mut value = serde_json::to_value(&reconciliation).unwrap_or(Value::Null);
        if let Value::Object(map) = &mut value {
            map.insert("valid".to_string(), Value::Bool(reconciliation.matched));
            map.insert(
                "reconciliation_id".to_string(),
                Value::String(record.id.clone()),
            );
        }
        Ok(value)
    })();
    match result {
        Ok(value) => json::success_response(value, 200),
        Err(error) => json::error_response(&error, debug),
    }
}

pub(crate) async fn list_reconciliations(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<PricingState>>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let params = query.into_inner();
    let tenant_id = match caller(&req).and_then(|call| {
        require_permission(&ctx, &call.role, PERM_PRICING_READ).map(|_| call.tenant_id)
    }) {
        Ok(tenant_id) => tenant_id,
        Err(error) => return json::error_response(&error, debug),
    };
    let mut items: Vec<PricingReconciliation> = state
        .reconciliations
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|item| {
            item.tenant_id == tenant_id
                && params
                    .filters
                    .get("invoice_id")
                    .is_none_or(|value| item.invoice_id.as_deref() == Some(value.as_str()))
                && params
                    .filters
                    .get("matched")
                    .is_none_or(|value| item.matched == (*value == "true" || *value == "1"))
        })
        .collect();
    items.sort_by(|a, b| b.checked_at.cmp(&a.checked_at));
    if params.normalized().order == "asc" {
        items.reverse();
    }
    json::success_response(paginate(items, &params), 200)
}

pub(crate) async fn get_reconciliation(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<PricingState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<PricingReconciliation> = (|| {
        let call = caller(&req)?;
        require_permission(&ctx, &call.role, PERM_PRICING_READ)?;
        state
            .reconciliations
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Reconciliation not found"))
            .and_then(|item| require_same_tenant(&item.tenant_id, &call.tenant_id).map(|_| item))
    })();
    json::respond(result, ctx.is_debug())
}

pub(crate) async fn list_credit_notes(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<PricingState>>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let params = query.into_inner();
    let tenant_id = match caller(&req).and_then(|call| {
        require_permission(&ctx, &call.role, PERM_PRICING_READ).map(|_| call.tenant_id)
    }) {
        Ok(tenant_id) => tenant_id,
        Err(error) => return json::error_response(&error, debug),
    };
    let mut items: Vec<PricingCreditNote> = state
        .credit_notes
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|item| {
            item.tenant_id == tenant_id
                && matches_search(&format!("{} {} {}", item.number, item.invoice_id, item.status), &params)
                && params
                    .filters
                    .get("invoice_id")
                    .is_none_or(|value| item.invoice_id == *value)
                && params
                    .filters
                    .get("status")
                    .is_none_or(|value| item.status == *value)
        })
        .collect();
    items.sort_by(|a, b| b.created_at.cmp(&a.created_at));
    if params.normalized().order == "asc" {
        items.reverse();
    }
    json::success_response(paginate(items, &params), 200)
}

pub(crate) async fn create_credit_note(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<PricingState>>,
    req: HttpRequest,
    body: web::Json<CreateCreditNote>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PricingCreditNote> = (|| {
        let call = caller(&req)?;
        require_permission(&ctx, &call.role, PERM_PRICING_MANAGE)?;
        let input = body.into_inner();
        let invoice_id = require_text(&input.invoice_id, "invoice_id", 128)?;
        let reason = require_text(&input.reason, "reason", 512)?;
        let money = resolve_money(&input.amount, None)?;
        require_currency_known(&state, &money.currency)?;
        let amount_scaled = round_to_minor(money.amount_scaled, &money.currency);
        let lines: Vec<ReconcileAdjustmentInput> = input
            .lines
            .iter()
            .map(|line| ReconcileAdjustmentInput {
                label: line.label.trim().to_string(),
                amount_scaled: line.amount_scaled,
            })
            .collect();
        // A credit note is only created when its own lines add up
        // to the credit amount exactly.
        let check = reconcile_credit_note(&lines, amount_scaled, &money.currency)?;
        let issue = input.issue.unwrap_or(false);
        if issue && !check.matched {
            return Err(UnifiedError::unprocessable(format!(
                "Credit note lines do not add up to the credit amount (discrepancy {})",
                check.total_discrepancy
            )));
        }
        let now = now();
        let note = PricingCreditNote {
            id: new_id(),
            tenant_id: call.tenant_id.clone(),
            number: state.next_credit_number(),
            invoice_id,
            amount_scaled,
            currency: money.currency.clone(),
            amount: format_scaled(amount_scaled, &money.currency),
            lines,
            reason,
            status: if issue { "issued".to_string() } else { "draft".to_string() },
            reconciled: check.matched,
            created_by: call.actor,
            created_at: now,
            updated_at: now,
        };
        state
            .credit_notes
            .insert(note.id.clone(), note.clone());
        state.emit(
            if issue {
                "pricing.credit_note.issued"
            } else {
                "pricing.credit_note.created"
            },
            json!({
                "tenant_id": note.tenant_id,
                "credit_note_id": note.id,
                "number": note.number,
                "invoice_id": note.invoice_id,
                "amount_scaled": note.amount_scaled,
                "currency": note.currency,
                "status": note.status,
            }),
        );
        Ok(note)
    })();
    match result {
        Ok(item) => json::success_response(item, 201),
        Err(error) => json::error_response(&error, debug),
    }
}

pub(crate) async fn get_credit_note(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<PricingState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<PricingCreditNote> = (|| {
        let call = caller(&req)?;
        require_permission(&ctx, &call.role, PERM_PRICING_READ)?;
        state
            .credit_notes
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Credit note not found"))
            .and_then(|item| require_same_tenant(&item.tenant_id, &call.tenant_id).map(|_| item))
    })();
    json::respond(result, ctx.is_debug())
}

/// Reconcile an issued or draft credit note on demand: the lines
/// must add up to the credit amount exactly.
pub(crate) async fn reconcile_credit_note_endpoint(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<PricingState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<ReconciliationResult> = (|| {
        let call = caller(&req)?;
        require_permission(&ctx, &call.role, PERM_PRICING_READ)?;
        let note = state
            .credit_notes
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Credit note not found"))?;
        require_same_tenant(&note.tenant_id, &call.tenant_id)?;
        reconcile_credit_note(&note.lines, note.amount_scaled, &note.currency)
    })();
    match result {
        Ok(value) => json::success_response(value, 200),
        Err(error) => json::error_response(&error, debug),
    }
}

// HTTP handlers: settings, currencies, conversion.

pub(crate) async fn get_settings(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<PricingState>>,
    req: HttpRequest,
) -> HttpResponse {
    let result: UnifiedResult<PricingSettings> = (|| {
        let call = caller(&req)?;
        require_permission(&ctx, &call.role, PERM_PRICING_READ)?;
        Ok(state.settings_for(&call.tenant_id))
    })();
    json::respond(result, ctx.is_debug())
}

pub(crate) async fn update_settings(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<PricingState>>,
    req: HttpRequest,
    body: web::Json<UpdatePricingSettings>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PricingSettings> = (|| {
        let call = caller(&req)?;
        require_permission(&ctx, &call.role, PERM_PRICING_MANAGE)?;
        let input = body.into_inner();
        let mut settings = state.settings_for(&call.tenant_id);
        if let Some(mode) = input.rounding_mode {
            if !ROUNDING_MODES.contains(&mode.as_str()) {
                return Err(UnifiedError::bad_request(
                    "rounding_mode must be \"half_up\" (the only implemented rounding rule)",
                ));
            }
            settings.rounding_mode = mode;
        }
        if let Some(basis) = input.proration_basis {
            if !PRORATION_BASES.contains(&basis.as_str()) {
                return Err(UnifiedError::bad_request(
                    "proration_basis must be \"per_day\" or \"per_month\"",
                ));
            }
            settings.proration_basis = basis;
        }
        if let Some(days) = input.days_per_month {
            if days <= 0 || days > 366 {
                return Err(UnifiedError::bad_request(
                    "days_per_month must be between 1 and 366",
                ));
            }
            settings.days_per_month = days;
        }
        if let Some(threshold) = input.approval_threshold_bp {
            if !(0..=BASIS_POINTS).contains(&threshold) {
                return Err(UnifiedError::bad_request(
                    "approval_threshold_bp must be between 0 and 10000",
                ));
            }
            settings.approval_threshold_bp = threshold;
        }
        if let Some(require) = input.require_second_approver {
            settings.require_second_approver = require;
        }
        if let Some(currency) = input.default_currency {
            let upper = currency.to_uppercase();
            require_currency_known(&state, &upper)?;
            settings.default_currency = upper;
        }
        state
            .settings
            .insert(call.tenant_id.clone(), settings.clone());
        Ok(settings)
    })();
    match result {
        Ok(value) => json::success_response(value, 200),
        Err(error) => json::error_response(&error, debug),
    }
}

pub(crate) async fn list_currencies(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<PricingState>>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let params = query.into_inner();
    let auth = caller(&req)
        .and_then(|call| require_permission(&ctx, &call.role, PERM_PRICING_READ).map(|_| call));
    if let Err(error) = auth {
        return json::error_response(&error, debug);
    }
    let mut items: Vec<PricingCurrency> = state
        .currencies
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|item| {
            matches_search(&item.code, &params)
                && params
                    .filters
                    .get("active")
                    .is_none_or(|value| item.active == (*value == "true" || *value == "1"))
        })
        .collect();
    items.sort_by(|a, b| a.code.cmp(&b.code));
    json::success_response(paginate(items, &params), 200)
}

pub(crate) async fn create_currency(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<PricingState>>,
    req: HttpRequest,
    body: web::Json<CreatePricingCurrency>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PricingCurrency> = (|| {
        let call = caller(&req)?;
        require_permission(&ctx, &call.role, PERM_PRICING_MANAGE)?;
        let input = body.into_inner();
        let code = input.code.trim().to_uppercase();
        validate_currency(&code)?;
        if state.currencies.contains_key(&code) {
            return Err(UnifiedError::conflict(format!(
                "Currency {code} already exists"
            )));
        }
        let exponent = input.exponent.unwrap_or_else(|| {
            if is_zero_decimal_currency(&code) {
                0
            } else {
                2
            }
        });
        if exponent != 0 && exponent != 2 {
            return Err(UnifiedError::bad_request(
                "Currency exponent must be 0 or 2",
            ));
        }
        if let (Some(num), Some(den)) = (input.rate_numerator, input.rate_denominator) {
            if num <= 0 || den <= 0 {
                return Err(UnifiedError::bad_request(
                    "Exchange rate numerator and denominator must be positive",
                ));
            }
        }
        let now = now();
        let currency = PricingCurrency {
            code,
            exponent,
            active: input.active.unwrap_or(true),
            rate_numerator: input.rate_numerator,
            rate_denominator: input.rate_denominator,
            rate_fetched_at: None,
            created_at: now,
            updated_at: now,
        };
        state
            .currencies
            .insert(currency.code.clone(), currency.clone());
        Ok(currency)
    })();
    match result {
        Ok(item) => json::success_response(item, 201),
        Err(error) => json::error_response(&error, debug),
    }
}

pub(crate) async fn update_currency(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<PricingState>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdatePricingCurrency>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PricingCurrency> = (|| {
        let call = caller(&req)?;
        require_permission(&ctx, &call.role, PERM_PRICING_MANAGE)?;
        let code = path.as_str().to_uppercase();
        let mut currency = state
            .currencies
            .get(&code)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Currency not found"))?;
        let input = body.into_inner();
        if let Some(exponent) = input.exponent {
            if exponent != 0 && exponent != 2 {
                return Err(UnifiedError::bad_request(
                    "Currency exponent must be 0 or 2",
                ));
            }
            currency.exponent = exponent;
        }
        if let Some(active) = input.active {
            currency.active = active;
        }
        if let Some(num) = input.rate_numerator {
            if num <= 0 {
                return Err(UnifiedError::bad_request(
                    "Exchange rate numerator must be positive",
                ));
            }
            currency.rate_numerator = Some(num);
        }
        if let Some(den) = input.rate_denominator {
            if den <= 0 {
                return Err(UnifiedError::bad_request(
                    "Exchange rate denominator must be positive",
                ));
            }
            currency.rate_denominator = Some(den);
        }
        if input.rate_numerator.is_some() || input.rate_denominator.is_some() {
            currency.rate_fetched_at = Some(now());
        }
        currency.updated_at = now();
        state
            .currencies
            .insert(currency.code.clone(), currency.clone());
        Ok(currency)
    })();
    match result {
        Ok(item) => json::success_response(item, 200),
        Err(error) => json::error_response(&error, debug),
    }
}

/// Convert an amount with a stored integer rate ratio only. A
/// missing rate is an explicit error, never a guessed conversion.
pub(crate) async fn convert_currency_endpoint(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<PricingState>>,
    req: HttpRequest,
    body: web::Json<ConvertCurrencyRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let call = caller(&req)?;
        require_permission(&ctx, &call.role, PERM_PRICING_READ)?;
        let input = body.into_inner();
        let from = resolve_money(&input.amount, None)?;
        require_currency_known(&state, &from.currency)?;
        let target = input.target_currency.trim().to_uppercase();
        require_currency_known(&state, &target)?;
        if from.currency == target {
            return Ok(json!({
                "amount_scaled": from.amount_scaled,
                "amount": format_scaled(from.amount_scaled, &target),
                "currency": target,
                "rate_numerator": 1,
                "rate_denominator": 1,
                "stale": false,
            }));
        }
        let currency = state
            .currencies
            .get(&target)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Target currency not found"))?;
        let (num, den) = match (currency.rate_numerator, currency.rate_denominator) {
            (Some(num), Some(den)) => (num, den),
            _ => {
                return Err(UnifiedError::unprocessable(format!(
                    "No stored exchange rate for {target}: enter a rate before converting"
                )));
            }
        };
        let converted = convert_currency(from.amount_scaled, num, den, &target)?;
        Ok(json!({
            "amount_scaled": converted,
            "amount": format_scaled(converted, &target),
            "currency": target,
            "rate_numerator": num,
            "rate_denominator": den,
            "stale": false,
        }))
    })();
    match result {
        Ok(value) => json::success_response(value, 200),
        Err(error) => json::error_response(&error, debug),
    }
}
