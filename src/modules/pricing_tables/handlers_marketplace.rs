//! pricing tables - handlers marketplace.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::json;
use unified_core::pagination::PaginationParams;
use actix_web::{web, HttpRequest, HttpResponse};
use serde_json::{json, Value};
use std::sync::Arc;
use super::types::*;
use super::state::*;
use super::validation::*;
use super::auth::*;

pub(crate) fn valid_rule_scope(scope: &str, scope_value: &str) -> UnifiedResult<()> {
    match scope {
        "global" => {
            if !scope_value.is_empty() {
                return Err(UnifiedError::bad_request(
                    "A global commission rule must not carry a scope_value",
                ));
            }
        }
        "vendor" | "category" => {
            if scope_value.trim().is_empty() {
                return Err(UnifiedError::bad_request(format!(
                    "A {scope} commission rule requires a scope_value"
                )));
            }
        }
        _ => {
            return Err(UnifiedError::bad_request(
                "scope must be one of: vendor, category, global",
            ));
        }
    }
    Ok(())
}

pub(crate) async fn list_commission_rules(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    state: web::Data<Arc<PricingTablesState>>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;

        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_MARKETPLACE_READ)?;
        let params = query.into_inner();
        let mut items: Vec<CommissionRule> = state
            .commission_rules
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|rule| {
                params
                    .filters
                    .get("scope")
                    .is_none_or(|scope| rule.scope == *scope)
                    && params
                        .filters
                        .get("scope_value")
                        .is_none_or(|value| rule.scope_value == *value)
            })
            .collect();
        items.sort_by(|a, b| a.scope.cmp(&b.scope).then(a.scope_value.cmp(&b.scope_value)));
        Ok(paginate(items, &params).to_json())
    })();
    json::respond(result, debug)
}

pub(crate) async fn create_commission_rule(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    state: web::Data<Arc<PricingTablesState>>,
    body: web::Json<CreateCommissionRule>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<CommissionRule> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = require_company_for_write(&_tenant, &req)?;

        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_MARKETPLACE_MANAGE)?;
        let input = body.into_inner();
        let scope_value = input.scope_value.unwrap_or_default().trim().to_string();
        valid_rule_scope(&input.scope, &scope_value)?;
        valid_percent(input.rate_percent, "rate_percent")?;
        if input.scope == "vendor" && !state.vendors.contains_key(scope_value.as_str()) {
            return Err(UnifiedError::not_found(format!(
                "Vendor {scope_value} not found"
            )));
        }
        if input.scope == "vendor" {
            require_vendor_in_scope(&state, scope_value.as_str(), &_scope)?;
        }
        if state.commission_rules.iter().any(|entry| {
            entry.value().scope == input.scope && entry.value().scope_value == scope_value
        }) {
            return Err(UnifiedError::conflict(format!(
                "A commission rule for scope \"{}\" and value \"{scope_value}\" already exists",
                input.scope
            )));
        }
        let rule = CommissionRule {
            id: new_id(),
            scope: input.scope,
            scope_value,
            rate_percent: input.rate_percent,
            created_at: now(),
            updated_at: now(),
        };
        state
            .commission_rules
            .insert(rule.id.clone(), rule.clone());
        let value = serde_json::to_value(&rule).unwrap_or(Value::Null);
        state.emit("marketplace.commission_rule_created", value);
        Ok(rule)
    })();
    respond_created(result, debug)
}

pub(crate) async fn get_commission_rule(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    state: web::Data<Arc<PricingTablesState>>,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<CommissionRule> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;

        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_MARKETPLACE_READ)?;
        state
            .commission_rules
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Commission rule not found"))
    })();
    json::respond(result, debug)
}

pub(crate) async fn update_commission_rule(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    state: web::Data<Arc<PricingTablesState>>,
    path: web::Path<String>,
    body: web::Json<UpdateCommissionRule>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<CommissionRule> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = require_company_for_write(&_tenant, &req)?;

        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_MARKETPLACE_MANAGE)?;
        let input = body.into_inner();
        let existing = state
            .commission_rules
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Commission rule not found"))?;
        let scope = input.scope.clone().unwrap_or(existing.scope);
        let scope_value = input
            .scope_value
            .clone()
            .map(|value| value.trim().to_string())
            .unwrap_or(existing.scope_value);
        valid_rule_scope(&scope, &scope_value)?;
        if let Some(rate) = input.rate_percent {
            valid_percent(rate, "rate_percent")?;
        }
        if scope == "vendor" && !state.vendors.contains_key(scope_value.as_str()) {
            return Err(UnifiedError::not_found(format!(
                "Vendor {scope_value} not found"
            )));
        }
        if scope == "vendor" {
            require_vendor_in_scope(&state, scope_value.as_str(), &_scope)?;
        }
        if state.commission_rules.iter().any(|entry| {
            entry.key() != path.as_str()
                && entry.value().scope == scope
                && entry.value().scope_value == scope_value
        }) {
            return Err(UnifiedError::conflict(format!(
                "A commission rule for scope \"{scope}\" and value \"{scope_value}\" already exists"
            )));
        }
        let mut rule = state
            .commission_rules
            .get_mut(path.as_str())
            .ok_or_else(|| UnifiedError::not_found("Commission rule not found"))?;
        rule.scope = scope;
        rule.scope_value = scope_value;
        if let Some(rate) = input.rate_percent {
            rule.rate_percent = rate;
        }
        rule.updated_at = now();
        let updated = rule.clone();
        drop(rule);
        let value = serde_json::to_value(&updated).unwrap_or(Value::Null);
        state.emit("marketplace.commission_rule_updated", value);
        Ok(updated)
    })();
    json::respond(result, debug)
}

pub(crate) async fn delete_commission_rule(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    state: web::Data<Arc<PricingTablesState>>,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = require_company_for_write(&_tenant, &req)?;

        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_MARKETPLACE_MANAGE)?;
        let rule = state
            .commission_rules
            .remove(path.as_str())
            .map(|(_, rule)| rule)
            .ok_or_else(|| UnifiedError::not_found("Commission rule not found"))?;
        let value = serde_json::to_value(&rule).unwrap_or(Value::Null);
        state.emit("marketplace.commission_rule_deleted", value);
        Ok(json!({ "deleted": true, "id": path.as_str() }))
    })();
    json::respond(result, debug)
}

pub(crate) async fn list_commissions(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    state: web::Data<Arc<PricingTablesState>>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;

        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_MARKETPLACE_READ)?;
        let params = query.into_inner();
        let mut items: Vec<Commission> = state
            .commissions
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|commission| {
                vendor_in_scope(&state, &commission.vendor_id, &_scope)
                    && params
                    .filters
                    .get("vendor_id")
                    .is_none_or(|vendor_id| commission.vendor_id == *vendor_id)
                    && params
                        .filters
                        .get("status")
                        .is_none_or(|status| commission.status == *status)
                    && params
                        .filters
                        .get("order_id")
                        .is_none_or(|order_id| commission.order_id == *order_id)
                    && params
                        .filters
                        .get("product_id")
                        .is_none_or(|product_id| commission.product_id == *product_id)
            })
            .collect();
        items.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        Ok(paginate(items, &params).to_json())
    })();
    json::respond(result, debug)
}

pub(crate) async fn get_commission(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    state: web::Data<Arc<PricingTablesState>>,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Commission> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;

        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_MARKETPLACE_READ)?;
        require_commission_in_scope(&state, path.as_str(), &_scope)
    })();
    json::respond(result, debug)
}

/// A vendor submits one of their products for marketplace approval.
/// The Store module stays the source of truth for the product; this
/// endpoint records only the approval state and the vendor link. The
/// product must not already have an approval record.
pub(crate) async fn submit_product(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    state: web::Data<Arc<PricingTablesState>>,
    path: web::Path<String>,
    body: web::Json<SubmitProduct>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<ProductApproval> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = require_company_for_write(&_tenant, &req)?;

        let caller = caller(&req)?;
        let input = body.into_inner();
        let product_id = path.as_str().to_string();
        if product_id.trim().is_empty() {
            return Err(UnifiedError::bad_request("product_id must not be empty"));
        }
        let vendor = state
            .vendors
            .get(input.vendor_id.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Vendor not found"))?;
        require_record_in_company_scope(&vendor.company_id, &_scope)?;
        require_vendor_self_or_admin(&ctx, &caller, &vendor)?;
        if vendor.status != "approved" {
            return Err(UnifiedError::conflict(format!(
                "Vendor status is \"{}\"; only an approved vendor may submit products",
                vendor.status
            )));
        }
        if state
            .product_approvals
            .iter()
            .any(|entry| entry.value().product_id == product_id)
        {
            return Err(UnifiedError::conflict(format!(
                "Product {product_id} already has an approval record"
            )));
        }
        let category = input
            .category
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty());
        let approval = ProductApproval {
            id: new_id(),
            product_id: product_id.clone(),
            vendor_id: vendor.id.clone(),
            category,
            status: "pending".to_string(),
            reason: String::new(),
            reviewed_by: None,
            reviewed_at: None,
            created_at: now(),
            updated_at: now(),
        };
        state
            .product_approvals
            .insert(approval.id.clone(), approval.clone());
        state.emit(
            "marketplace.product_approval_requested",
            json!({
                "approval_id": approval.id,
                "product_id": approval.product_id,
                "vendor_id": approval.vendor_id,
                "category": approval.category,
            }),
        );
        Ok(approval)
    })();
    respond_created(result, debug)
}

pub(crate) async fn list_product_approvals(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    state: web::Data<Arc<PricingTablesState>>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;

        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_MARKETPLACE_READ)?;
        let params = query.into_inner();
        let mut items: Vec<ProductApproval> = state
            .product_approvals
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|approval| {
                vendor_in_scope(&state, &approval.vendor_id, &_scope)
                    && params
                    .filters
                    .get("status")
                    .is_none_or(|status| approval.status == *status)
                    && params
                        .filters
                        .get("vendor_id")
                        .is_none_or(|vendor_id| approval.vendor_id == *vendor_id)
                    && params
                        .filters
                        .get("product_id")
                        .is_none_or(|product_id| approval.product_id == *product_id)
            })
            .collect();
        items.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        Ok(paginate(items, &params).to_json())
    })();
    json::respond(result, debug)
}

pub(crate) async fn get_product_approval(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    state: web::Data<Arc<PricingTablesState>>,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<ProductApproval> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;

        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_MARKETPLACE_READ)?;
        require_product_approval_in_scope(&state, path.as_str(), &_scope)
    })();
    json::respond(result, debug)
}

pub(crate) fn transition_product_approval(
    state: &PricingTablesState,
    approval_id: &str,
    to: &str,
    event: &str,
    reviewer: &str,
    reason: &str,
) -> UnifiedResult<ProductApproval> {
    let mut approval = state
        .product_approvals
        .get_mut(approval_id)
        .ok_or_else(|| UnifiedError::not_found("Product approval not found"))?;
    if approval.status != "pending" {
        return Err(UnifiedError::conflict(format!(
            "Product approval status is \"{}\"; only a pending approval can be reviewed",
            approval.status
        )));
    }
    approval.status = to.to_string();
    approval.reason = reason.trim().to_string();
    approval.reviewed_by = Some(reviewer.to_string());
    approval.reviewed_at = Some(now());
    approval.updated_at = now();
    let updated = approval.clone();
    drop(approval);
    state.emit(
        event,
        json!({
            "approval_id": updated.id,
            "product_id": updated.product_id,
            "vendor_id": updated.vendor_id,
            "status": updated.status,
            "reason": updated.reason,
        }),
    );
    Ok(updated)
}

pub(crate) async fn approve_product(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    state: web::Data<Arc<PricingTablesState>>,
    path: web::Path<String>,
    body: web::Json<ReviewInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<ProductApproval> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = require_company_for_write(&_tenant, &req)?;

        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_MARKETPLACE_MANAGE)?;
        require_product_approval_in_scope(&state, path.as_str(), &_scope)?;
        let reason = body.into_inner().reason.unwrap_or_default();
        transition_product_approval(
            &state,
            path.as_str(),
            "approved",
            "marketplace.product_approved",
            &caller.actor,
            &reason,
        )
    })();
    json::respond(result, debug)
}

pub(crate) async fn reject_product(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    state: web::Data<Arc<PricingTablesState>>,
    path: web::Path<String>,
    body: web::Json<ReviewInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<ProductApproval> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = require_company_for_write(&_tenant, &req)?;

        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_MARKETPLACE_MANAGE)?;
        require_product_approval_in_scope(&state, path.as_str(), &_scope)?;
        let reason = body.into_inner().reason.unwrap_or_default();
        if reason.trim().is_empty() {
            return Err(UnifiedError::bad_request(
                "A rejection reason is required",
            ));
        }
        transition_product_approval(
            &state,
            path.as_str(),
            "rejected",
            "marketplace.product_rejected",
            &caller.actor,
            &reason,
        )
    })();
    json::respond(result, debug)
}

/// Result of settling pending commissions against one payout.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct CommissionSettlement {
    /// Amount of the payout actually applied to commissions, in
    /// minor units.
    pub settled_amount: i64,
    /// Commissions fully settled by this payout.
    pub paid_ids: Vec<String>,
    /// Commissions partially settled (still pending for the remainder).
    pub partially_paid_ids: Vec<String>,
}

/// Settle a vendor's pending commissions oldest-first against
/// `amount` in `currency`. A commission larger than the remaining
/// amount is partially settled instead of stopping the loop: the
/// settled portion is recorded on `paid_amount` and the commission
/// stays pending for its remainder, so money already paid out is
/// never counted as pending again and a later payout cannot pay it
/// twice. Later, smaller commissions are still settled after an
/// oversized one. Commissions in another currency are never
/// touched: a payout settles only commissions in its own currency.
pub(crate) fn settle_commissions(
    state: &PricingTablesState,
    vendor_id: &str,
    amount: i64,
    currency: &str,
) -> CommissionSettlement {
    let mut remaining = amount;
    let mut paid_ids: Vec<String> = Vec::new();
    let mut partially_paid_ids: Vec<String> = Vec::new();
    let mut pending: Vec<Commission> = state
        .commissions
        .iter()
        .filter(|entry| {
            entry.value().vendor_id == vendor_id
                && entry.value().status == "pending"
                && entry.value().currency == currency
        })
        .map(|entry| entry.value().clone())
        .collect();
    pending.sort_by(|a, b| a.created_at.cmp(&b.created_at));
    for commission in pending {
        if remaining <= 0 {
            break;
        }
        let settled_so_far = commission
            .paid_amount
            .clamp(0, commission.commission_amount);
        let outstanding = commission.commission_amount - settled_so_far;
        if outstanding <= 0 {
            continue;
        }
        if outstanding <= remaining {
            remaining -= outstanding;
            if let Some(mut entry) = state.commissions.get_mut(commission.id.as_str()) {
                entry.value_mut().paid_amount = commission.commission_amount;
                entry.value_mut().status = "paid".to_string();
                entry.value_mut().updated_at = now();
            }
            paid_ids.push(commission.id);
        } else {
            if let Some(mut entry) = state.commissions.get_mut(commission.id.as_str()) {
                entry.value_mut().paid_amount = settled_so_far + remaining;
                entry.value_mut().updated_at = now();
            }
            partially_paid_ids.push(commission.id);
            remaining = 0;
        }
    }
    CommissionSettlement {
        settled_amount: (amount - remaining).max(0),
        paid_ids,
        partially_paid_ids,
    }
}

/// Distinct currencies of a vendor's pending commissions, sorted.
/// Empty when the vendor has no pending commission at all.
pub(crate) fn commission_currencies(
    state: &PricingTablesState,
    vendor_id: &str,
) -> Vec<String> {
    let mut currencies: Vec<String> = state
        .commissions
        .iter()
        .filter(|entry| {
            entry.value().vendor_id == vendor_id && entry.value().status == "pending"
        })
        .map(|entry| entry.value().currency.clone())
        .collect();
    currencies.sort();
    currencies.dedup();
    currencies
}

/// A vendor's spendable payout balance in one currency: the unpaid
/// remainder of pending commissions in that currency minus payouts
/// already requested or approved in the same currency. Amounts in
/// other currencies never fund a payout in this one.
pub(crate) fn available_payout_balance(
    state: &PricingTablesState,
    vendor_id: &str,
    currency: &str,
) -> i64 {
    let pending: i64 = state
        .commissions
        .iter()
        .filter(|entry| {
            entry.value().vendor_id == vendor_id
                && entry.value().status == "pending"
                && entry.value().currency == currency
        })
        .map(|entry| {
            entry.value().commission_amount
                - entry
                    .value()
                    .paid_amount
                    .clamp(0, entry.value().commission_amount)
        })
        .sum();
    let outstanding: i64 = state
        .payouts
        .iter()
        .filter(|entry| {
            entry.value().vendor_id == vendor_id
                && matches!(entry.value().status.as_str(), "requested" | "approved")
                && entry.value().currency == currency
        })
        .map(|entry| entry.value().amount)
        .sum();
    (pending - outstanding).max(0)
}

/// Reject a payout request whose currency matches none of the
/// vendor's pending commissions. A payout in EUR against USD
/// commissions would otherwise be approved against money that
/// exists only in another currency. Vendors with no pending
/// commission pass this check; the balance check rejects them.
pub(crate) fn validate_payout_currency(
    state: &PricingTablesState,
    vendor_id: &str,
    currency: &str,
) -> UnifiedResult<()> {
    let currencies = commission_currencies(state, vendor_id);
    if !currencies.is_empty() && !currencies.contains(&currency.to_string()) {
        return Err(UnifiedError::unprocessable(format!(
            "Payout currency \"{currency}\" does not match the currency of the vendor's pending commissions: {}",
            currencies.join(", ")
        )));
    }
    Ok(())
}

/// A vendor requests a payout of their pending commission balance.
/// The amount must be positive and must not exceed the vendor's
/// pending commission minus payouts already requested or approved.
pub(crate) async fn request_payout(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    state: web::Data<Arc<PricingTablesState>>,
    path: web::Path<String>,
    body: web::Json<CreatePayout>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Payout> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = require_company_for_write(&_tenant, &req)?;

        let caller = caller(&req)?;
        let vendor = state
            .vendors
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Vendor not found"))?;
        require_record_in_company_scope(&vendor.company_id, &_scope)?;
        require_vendor_self_or_admin(&ctx, &caller, &vendor)?;
        if vendor.status != "approved" {
            return Err(UnifiedError::conflict(format!(
                "Vendor status is \"{}\"; only an approved vendor may request payouts",
                vendor.status
            )));
        }
        let input = body.into_inner();
        valid_price(input.amount, "amount")?;
        if input.amount <= 0 {
            return Err(UnifiedError::bad_request("amount must be greater than zero"));
        }
        let currency = input.currency.unwrap_or_else(|| "USD".to_string());
        if !valid_currency(&currency) {
            return Err(UnifiedError::bad_request(
                "currency must be a 3-letter uppercase ISO 4217 code",
            ));
        }
        validate_payout_currency(&state, &vendor.id, &currency)?;
        let available = available_payout_balance(&state, &vendor.id, &currency);
        if input.amount > available {
            let available_major = minor_to_major(available, &currency);
            return Err(UnifiedError::unprocessable(format!(
                "Requested amount exceeds the available pending commission balance of {available_major} {currency}"
            ))
            .with_metadata("available", json!(available_major)));
        }
        let payout = Payout {
            id: new_id(),
            vendor_id: vendor.id.clone(),
            amount: input.amount,
            currency: currency.clone(),
            status: "requested".to_string(),
            invoice_id: None,
            reference: String::new(),
            requested_by: caller.actor.clone(),
            reviewed_by: None,
            review_reason: String::new(),
            created_at: now(),
            updated_at: now(),
        };
        state.payouts.insert(payout.id.clone(), payout.clone());
        // Billing pays the payout out; the marketplace only records
        // the request and reconciles the invoice later.
        state.emit(
            "marketplace.payout_requested",
            json!({
                "payout_id": payout.id,
                "vendor_id": payout.vendor_id,
                "amount": minor_to_major(payout.amount, &payout.currency),
                "currency": payout.currency,
                "requested_by": payout.requested_by,
            }),
        );
        Ok(payout)
    })();
    respond_created(result, debug)
}

pub(crate) async fn list_payouts(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    state: web::Data<Arc<PricingTablesState>>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;

        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_MARKETPLACE_READ)?;
        let params = query.into_inner();
        let mut items: Vec<Payout> = state
            .payouts
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|payout| {
                vendor_in_scope(&state, &payout.vendor_id, &_scope)
                    && params
                    .filters
                    .get("vendor_id")
                    .is_none_or(|vendor_id| payout.vendor_id == *vendor_id)
                    && params
                        .filters
                        .get("status")
                        .is_none_or(|status| payout.status == *status)
            })
            .collect();
        items.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        Ok(paginate(items, &params).to_json())
    })();
    json::respond(result, debug)
}

pub(crate) async fn get_payout(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    state: web::Data<Arc<PricingTablesState>>,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Payout> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;

        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_MARKETPLACE_READ)?;
        require_payout_in_scope(&state, path.as_str(), &_scope)
    })();
    json::respond(result, debug)
}

pub(crate) fn transition_payout(
    state: &PricingTablesState,
    payout_id: &str,
    from: &[&str],
    to: &str,
    event: &str,
    reviewer: &str,
    reason: &str,
) -> UnifiedResult<Payout> {
    let mut payout = state
        .payouts
        .get_mut(payout_id)
        .ok_or_else(|| UnifiedError::not_found("Payout not found"))?;
    if !from.contains(&payout.status.as_str()) {
        return Err(UnifiedError::conflict(format!(
            "Payout status is \"{}\"; expected one of: {}",
            payout.status,
            from.join(", ")
        )));
    }
    payout.status = to.to_string();
    payout.reviewed_by = Some(reviewer.to_string());
    payout.review_reason = reason.trim().to_string();
    payout.updated_at = now();
    let updated = payout.clone();
    drop(payout);
    let value = serde_json::to_value(&updated).unwrap_or(Value::Null);
    state.emit(event, value);
    Ok(updated)
}

pub(crate) async fn approve_payout(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    state: web::Data<Arc<PricingTablesState>>,
    path: web::Path<String>,
    body: web::Json<ReviewInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Payout> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = require_company_for_write(&_tenant, &req)?;

        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_MARKETPLACE_MANAGE)?;
        require_payout_in_scope(&state, path.as_str(), &_scope)?;
        let reason = body.into_inner().reason.unwrap_or_default();
        transition_payout(
            &state,
            path.as_str(),
            &["requested"],
            "approved",
            "marketplace.payout_approved",
            &caller.actor,
            &reason,
        )
    })();
    json::respond(result, debug)
}

pub(crate) async fn reject_payout(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    state: web::Data<Arc<PricingTablesState>>,
    path: web::Path<String>,
    body: web::Json<ReviewInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Payout> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = require_company_for_write(&_tenant, &req)?;

        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_MARKETPLACE_MANAGE)?;
        require_payout_in_scope(&state, path.as_str(), &_scope)?;
        let reason = body.into_inner().reason.unwrap_or_default();
        if reason.trim().is_empty() {
            return Err(UnifiedError::bad_request(
                "A rejection reason is required",
            ));
        }
        transition_payout(
            &state,
            path.as_str(),
            &["requested"],
            "rejected",
            "marketplace.payout_rejected",
            &caller.actor,
            &reason,
        )
    })();
    json::respond(result, debug)
}

/// Mark an approved payout as paid. The reference (bank transfer
/// id, billing payment id) is stored on the record. Paying a payout
/// also settles the vendor's oldest pending commissions in the
/// payout's currency, up to the payout amount, so the dashboard
/// pending balance falls as money goes out. A commission larger
/// than the remaining amount is partially settled and stays pending
/// for its remainder; commissions are settled in creation order.
pub(crate) async fn mark_payout_paid(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    state: web::Data<Arc<PricingTablesState>>,
    path: web::Path<String>,
    body: web::Json<MarkPaidInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Payout> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = require_company_for_write(&_tenant, &req)?;

        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_MARKETPLACE_MANAGE)?;
        require_payout_in_scope(&state, path.as_str(), &_scope)?;
        let reference = body
            .into_inner()
            .reference
            .unwrap_or_default()
            .trim()
            .to_string();
        let payout = transition_payout(
            &state,
            path.as_str(),
            &["approved"],
            "paid",
            "marketplace.payout_paid",
            &caller.actor,
            "",
        )?;
        if !reference.is_empty() {
            if let Some(mut entry) = state.payouts.get_mut(payout.id.as_str()) {
                entry.value_mut().reference = reference;
                entry.value_mut().updated_at = now();
            }
        }
        // Settle pending commissions oldest-first up to the payout,
        // in the payout's own currency. Oversized commissions are
        // partially settled, never skipped: see settle_commissions.
        let _settlement =
            settle_commissions(&state, &payout.vendor_id, payout.amount, &payout.currency);
        let updated = state
            .payouts
            .get(payout.id.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Payout not found"))?;
        Ok(updated)
    })();
    json::respond(result, debug)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration;

    fn test_vendor() -> Vendor {
        Vendor {
            id: "v-1".to_string(),
            user_id: "u-1".to_string(),
            company_id: None,
            company_name: "Acme".to_string(),
            slug: "acme".to_string(),
            email: "a@b.co".to_string(),
            status: "approved".to_string(),
            application_notes: String::new(),
            review_reason: String::new(),
            reviewed_by: None,
            reviewed_at: None,
            created_at: now(),
            updated_at: now(),
        }
    }

    fn test_commission(
        id: &str,
        amount: i64,
        currency: &str,
        created_seconds: i64,
    ) -> Commission {
        Commission {
            id: id.to_string(),
            order_id: format!("ord-{id}"),
            vendor_id: "v-1".to_string(),
            product_id: "p-1".to_string(),
            order_total: amount * 10,
            rate_percent: 10.0,
            commission_amount: amount,
            paid_amount: 0,
            currency: currency.to_string(),
            status: "pending".to_string(),
            created_at: now() + Duration::seconds(created_seconds),
            updated_at: now(),
        }
    }

    fn test_payout(id: &str, amount: i64, currency: &str, status: &str) -> Payout {
        Payout {
            id: id.to_string(),
            vendor_id: "v-1".to_string(),
            amount,
            currency: currency.to_string(),
            status: status.to_string(),
            invoice_id: None,
            reference: String::new(),
            requested_by: "u-1".to_string(),
            reviewed_by: None,
            review_reason: String::new(),
            created_at: now(),
            updated_at: now(),
        }
    }

    fn pending_commission(state: &PricingTablesState, id: &str) -> Commission {
        state
            .commissions
            .get(id)
            .map(|entry| entry.value().clone())
            .expect("commission still present")
    }

    #[test]
    fn settle_pays_oldest_first_and_partially_settles_the_remainder() {
        let state = PricingTablesState::new();
        state
            .commissions
            .insert("c-100".to_string(), test_commission("c-100", 10000, "USD", 0));
        state
            .commissions
            .insert("c-50".to_string(), test_commission("c-50", 5000, "USD", 1));
        state
            .commissions
            .insert("c-30".to_string(), test_commission("c-30", 3000, "USD", 2));
        let settlement = settle_commissions(&state, "v-1", 12000, "USD");
        assert_eq!(settlement.settled_amount, 12000);
        assert_eq!(settlement.paid_ids, vec!["c-100".to_string()]);
        assert_eq!(settlement.partially_paid_ids, vec!["c-50".to_string()]);
        assert_eq!(pending_commission(&state, "c-100").status, "paid");
        let partial = pending_commission(&state, "c-50");
        assert_eq!(partial.status, "pending");
        assert_eq!(partial.paid_amount, 2000);
        assert_eq!(pending_commission(&state, "c-30").paid_amount, 0);
        // The dashboard now counts only the unpaid remainders (30 + 30).
        let dashboard = state.dashboard_for(&test_vendor());
        assert_eq!(dashboard.pending_commission, 6000);
        assert_eq!(dashboard.paid_commission, 12000);
    }

    #[test]
    fn settle_partially_settles_a_single_oversized_commission() {
        let state = PricingTablesState::new();
        state
            .commissions
            .insert("c-200".to_string(), test_commission("c-200", 20000, "USD", 0));
        let settlement = settle_commissions(&state, "v-1", 5000, "USD");
        assert_eq!(settlement.settled_amount, 5000);
        assert!(settlement.paid_ids.is_empty());
        assert_eq!(settlement.partially_paid_ids, vec!["c-200".to_string()]);
        let commission = pending_commission(&state, "c-200");
        assert_eq!(commission.status, "pending");
        assert_eq!(commission.paid_amount, 5000);
        let dashboard = state.dashboard_for(&test_vendor());
        assert_eq!(dashboard.pending_commission, 15000);
        assert_eq!(dashboard.paid_commission, 5000);
    }

    #[test]
    fn settle_prefers_the_oldest_commission_over_a_smaller_later_one() {
        let state = PricingTablesState::new();
        state
            .commissions
            .insert("c-200".to_string(), test_commission("c-200", 20000, "USD", 0));
        state
            .commissions
            .insert("c-30".to_string(), test_commission("c-30", 3000, "USD", 1));
        let settlement = settle_commissions(&state, "v-1", 12000, "USD");
        assert_eq!(settlement.settled_amount, 12000);
        assert_eq!(settlement.partially_paid_ids, vec!["c-200".to_string()]);
        assert_eq!(pending_commission(&state, "c-200").paid_amount, 12000);
        assert_eq!(pending_commission(&state, "c-30").paid_amount, 0);
    }

    #[test]
    fn settle_ignores_commissions_in_other_currencies() {
        let state = PricingTablesState::new();
        state
            .commissions
            .insert("c-usd".to_string(), test_commission("c-usd", 10000, "USD", 0));
        state
            .commissions
            .insert("c-eur".to_string(), test_commission("c-eur", 10000, "EUR", 1));
        let settlement = settle_commissions(&state, "v-1", 10000, "USD");
        assert_eq!(settlement.paid_ids, vec!["c-usd".to_string()]);
        assert_eq!(pending_commission(&state, "c-eur").status, "pending");
        assert_eq!(pending_commission(&state, "c-eur").paid_amount, 0);
        let eur = settle_commissions(&state, "v-1", 4000, "EUR");
        assert_eq!(eur.settled_amount, 4000);
        assert_eq!(pending_commission(&state, "c-eur").paid_amount, 4000);
    }

    #[test]
    fn settle_with_zero_amount_settles_nothing() {
        let state = PricingTablesState::new();
        state
            .commissions
            .insert("c-100".to_string(), test_commission("c-100", 10000, "USD", 0));
        let settlement = settle_commissions(&state, "v-1", 0, "USD");
        assert_eq!(settlement.settled_amount, 0);
        assert!(settlement.paid_ids.is_empty());
        assert!(settlement.partially_paid_ids.is_empty());
        assert_eq!(pending_commission(&state, "c-100").paid_amount, 0);
    }

    #[test]
    fn available_balance_is_per_currency_and_net_of_outstanding_payouts() {
        let state = PricingTablesState::new();
        let mut usd = test_commission("c-usd", 10000, "USD", 0);
        usd.paid_amount = 4000;
        state.commissions.insert("c-usd".to_string(), usd);
        state
            .commissions
            .insert("c-eur".to_string(), test_commission("c-eur", 8000, "EUR", 1));
        state.payouts.insert(
            "pay-req".to_string(),
            test_payout("pay-req", 2500, "USD", "requested"),
        );
        state.payouts.insert(
            "pay-rej".to_string(),
            test_payout("pay-rej", 99900, "USD", "rejected"),
        );
        // USD: 60 remainder - 25 requested = 35. EUR is untouched.
        assert_eq!(available_payout_balance(&state, "v-1", "USD"), 3500);
        assert_eq!(available_payout_balance(&state, "v-1", "EUR"), 8000);
    }

    #[test]
    fn payout_currency_must_match_a_pending_commission_currency() {
        let state = PricingTablesState::new();
        state
            .commissions
            .insert("c-usd".to_string(), test_commission("c-usd", 10000, "USD", 0));
        assert!(validate_payout_currency(&state, "v-1", "USD").is_ok());
        let error = validate_payout_currency(&state, "v-1", "EUR").unwrap_err();
        assert!(error.to_string().contains("EUR"));
        assert!(error.to_string().contains("USD"));
    }

    #[test]
    fn payout_currency_check_passes_with_no_pending_commissions() {
        let state = PricingTablesState::new();
        // No pending commission means no currency to mismatch; the
        // balance check (available = 0) rejects the request instead.
        assert!(validate_payout_currency(&state, "v-1", "EUR").is_ok());
        assert_eq!(available_payout_balance(&state, "v-1", "EUR"), 0);
    }

    #[test]
    fn commission_currencies_lists_distinct_pending_currencies() {
        let state = PricingTablesState::new();
        state
            .commissions
            .insert("c-usd".to_string(), test_commission("c-usd", 10000, "USD", 0));
        state
            .commissions
            .insert("c-eur".to_string(), test_commission("c-eur", 5000, "EUR", 1));
        let mut paid = test_commission("c-paid", 1000, "GBP", 2);
        paid.status = "paid".to_string();
        state.commissions.insert("c-paid".to_string(), paid);
        assert_eq!(
            commission_currencies(&state, "v-1"),
            vec!["EUR".to_string(), "USD".to_string()]
        );
    }
}
