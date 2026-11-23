// pricing — handlers coupons.

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
use super::handlers_prices::*;
use super::handlers_bundles::*;
use super::handlers_reconciliation::*;
use super::module::*;

// ---------------------------------------------------------------------------
// HTTP handlers: coupons and discount evaluation.
// ---------------------------------------------------------------------------

pub(crate) async fn list_coupons(
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
    let mut items: Vec<PricingCoupon> = state
        .coupons
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|item| {
            item.tenant_id == tenant_id
                && matches_search(&format!("{} {}", item.code, item.kind), &params)
                && params
                    .filters
                    .get("kind")
                    .is_none_or(|value| item.kind == *value)
                && params
                    .filters
                    .get("active")
                    .is_none_or(|value| item.active == (*value == "true" || *value == "1"))
        })
        .collect();
    items.sort_by(|a, b| b.created_at.cmp(&a.created_at));
    if params.normalized().order == "asc" {
        items.reverse();
    }
    json::success_response(paginate(items, &params), 200)
}

pub(crate) async fn create_coupon(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<PricingState>>,
    req: HttpRequest,
    body: web::Json<CreatePricingCoupon>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PricingCoupon> = (|| {
        let call = caller(&req)?;
        require_permission(&ctx, &call.role, PERM_PRICING_MANAGE)?;
        let input = body.into_inner();
        let code = normalize_code(&input.code)?;
        if state
            .coupons
            .iter()
            .any(|entry| entry.value().code == code)
        {
            return Err(UnifiedError::conflict(format!(
                "Coupon code {code} already exists"
            )));
        }
        if !["percent", "fixed", "buy_x_get_y"].contains(&input.kind.as_str()) {
            return Err(UnifiedError::bad_request(
                "Coupon kind must be \"percent\", \"fixed\", or \"buy_x_get_y\"",
            ));
        }
        let currency = input
            .currency
            .unwrap_or_else(|| state.settings_for(&call.tenant_id).default_currency)
            .to_uppercase();
        require_currency_known(&state, &currency)?;
        if input.kind == "percent" && !(0..=BASIS_POINTS).contains(&input.percent_bp) {
            return Err(UnifiedError::bad_request(
                "Coupon percent must be between 0 and 10000 basis points",
            ));
        }
        if input.kind == "buy_x_get_y" && (input.buy_quantity == 0 || input.get_quantity == 0) {
            return Err(UnifiedError::bad_request(
                "A buy-X-get-Y coupon needs a buy quantity and a get quantity above zero",
            ));
        }
        let amount_scaled = match &input.amount {
            Some(amount) => {
                let money = resolve_money(amount, Some(&currency))?;
                if money.currency != currency {
                    return Err(UnifiedError::bad_request(
                        "Coupon amount currency must match the coupon currency",
                    ));
                }
                round_to_minor(money.amount_scaled, &currency)
            }
            None => 0,
        };
        if input.kind == "fixed" && amount_scaled <= 0 {
            return Err(UnifiedError::bad_request(
                "A fixed coupon needs an amount above zero",
            ));
        }
        if let (Some(from), Some(to)) = (input.valid_from, input.valid_to) {
            if to <= from {
                return Err(UnifiedError::bad_request(
                    "valid_to must be after valid_from",
                ));
            }
        }
        let now = now();
        let coupon = PricingCoupon {
            id: new_id(),
            tenant_id: call.tenant_id.clone(),
            code,
            kind: input.kind,
            percent_bp: input.percent_bp,
            amount_scaled,
            currency,
            buy_quantity: input.buy_quantity,
            get_quantity: input.get_quantity,
            product_ids: input.product_ids,
            valid_from: input.valid_from,
            valid_to: input.valid_to,
            usage_limit: input.usage_limit,
            per_customer_limit: input.per_customer_limit,
            used_count: 0,
            stackable: input.stackable.unwrap_or(false),
            exclusive: input.exclusive.unwrap_or(false),
            active: input.active.unwrap_or(true),
            created_by: call.actor,
            created_at: now,
            updated_at: now,
        };
        state.coupons.insert(coupon.id.clone(), coupon.clone());
        Ok(coupon)
    })();
    match result {
        Ok(item) => json::success_response(item, 201),
        Err(error) => json::error_response(&error, debug),
    }
}

pub(crate) async fn get_coupon(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<PricingState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<PricingCoupon> = (|| {
        let call = caller(&req)?;
        require_permission(&ctx, &call.role, PERM_PRICING_READ)?;
        state
            .coupons
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Coupon not found"))
            .and_then(|item| require_same_tenant(&item.tenant_id, &call.tenant_id).map(|_| item))
    })();
    json::respond(result, ctx.is_debug())
}

pub(crate) async fn update_coupon(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<PricingState>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdatePricingCoupon>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PricingCoupon> = (|| {
        let call = caller(&req)?;
        require_permission(&ctx, &call.role, PERM_PRICING_MANAGE)?;
        let mut coupon = state
            .coupons
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Coupon not found"))?;
        require_same_tenant(&coupon.tenant_id, &call.tenant_id)?;
        let input = body.into_inner();
        if let Some(kind) = input.kind {
            if !["percent", "fixed", "buy_x_get_y"].contains(&kind.as_str()) {
                return Err(UnifiedError::bad_request(
                    "Coupon kind must be \"percent\", \"fixed\", or \"buy_x_get_y\"",
                ));
            }
            coupon.kind = kind;
        }
        if let Some(percent_bp) = input.percent_bp {
            if !(0..=BASIS_POINTS).contains(&percent_bp) {
                return Err(UnifiedError::bad_request(
                    "Coupon percent must be between 0 and 10000 basis points",
                ));
            }
            coupon.percent_bp = percent_bp;
        }
        if let Some(amount) = input.amount {
            let money = resolve_money(&amount, Some(&coupon.currency))?;
            coupon.amount_scaled = round_to_minor(money.amount_scaled, &coupon.currency);
        }
        if let Some(value) = input.buy_quantity {
            coupon.buy_quantity = value;
        }
        if let Some(value) = input.get_quantity {
            coupon.get_quantity = value;
        }
        if let Some(ids) = input.product_ids {
            coupon.product_ids = ids;
        }
        if let Some(from) = input.valid_from {
            coupon.valid_from = Some(from);
        }
        if let Some(to) = input.valid_to {
            coupon.valid_to = Some(to);
        }
        if let (Some(from), Some(to)) = (coupon.valid_from, coupon.valid_to) {
            if to <= from {
                return Err(UnifiedError::bad_request(
                    "valid_to must be after valid_from",
                ));
            }
        }
        if let Some(limit) = input.usage_limit {
            coupon.usage_limit = Some(limit);
        }
        if let Some(limit) = input.per_customer_limit {
            coupon.per_customer_limit = Some(limit);
        }
        if let Some(stackable) = input.stackable {
            coupon.stackable = stackable;
        }
        if let Some(exclusive) = input.exclusive {
            coupon.exclusive = exclusive;
        }
        if let Some(active) = input.active {
            coupon.active = active;
        }
        coupon.updated_at = now();
        state.coupons.insert(coupon.id.clone(), coupon.clone());
        Ok(coupon)
    })();
    match result {
        Ok(item) => json::success_response(item, 200),
        Err(error) => json::error_response(&error, debug),
    }
}

pub(crate) async fn delete_coupon(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<PricingState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<Value> = (|| {
        let call = caller(&req)?;
        require_permission(&ctx, &call.role, PERM_PRICING_MANAGE)?;
        let existing = state
            .coupons
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Coupon not found"))?;
        require_same_tenant(&existing.tenant_id, &call.tenant_id)?;
        state
            .coupons
            .remove(path.as_str())
            .map(|_| json!({ "deleted": true, "id": path.as_str() }))
            .ok_or_else(|| UnifiedError::not_found("Coupon not found"))
    })();
    json::respond(result, ctx.is_debug())
}

/// Record one coupon redemption: the store calls this after an
/// order that used the coupon is confirmed. The coupon entry guard
/// is held across the limit check, the increment, and the
/// per-customer redemption update, so two concurrent redemptions
/// for the same coupon are serialized: the second one re-checks
/// the limits after the first one incremented, and is rejected
/// when the limit is already reached. Without the guard this
/// would be check-then-insert and concurrent redemptions could
/// overspend the global or per-customer limit.
pub(crate) fn redeem_coupon_record(
    state: &PricingState,
    coupon_id: &str,
    tenant_id: &str,
    customer_id: &str,
    now: DateTime<Utc>,
) -> UnifiedResult<PricingCoupon> {
    let mut entry = state
        .coupons
        .get_mut(coupon_id)
        .ok_or_else(|| UnifiedError::not_found("Coupon not found"))?;
    require_same_tenant(&entry.tenant_id, tenant_id)?;
    if let Some(reason) = coupon_usability(state, &entry, Some(customer_id), now) {
        return Err(UnifiedError::unprocessable(reason));
    }
    entry.used_count += 1;
    entry.updated_at = now;
    let coupon = (*entry).clone();
    let key = format!("{}:{}:{}", coupon.tenant_id, coupon.id, customer_id);
    let mut redemption = state
        .redemptions
        .get(&key)
        .map(|entry| entry.value().clone())
        .unwrap_or(CouponRedemption {
            coupon_id: coupon.id.clone(),
            tenant_id: coupon.tenant_id.clone(),
            customer_id: customer_id.to_string(),
            count: 0,
            last_redeemed_at: now,
        });
    redemption.count += 1;
    redemption.last_redeemed_at = now;
    state.redemptions.insert(key, redemption);
    drop(entry);
    Ok(coupon)
}

pub(crate) async fn redeem_coupon(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<PricingState>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<Value>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PricingCoupon> = (|| {
        let call = caller(&req)?;
        require_permission(&ctx, &call.role, PERM_PRICING_MANAGE)?;
        let customer_id = body
            .get("customer_id")
            .and_then(|value| value.as_str())
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string)
            .ok_or_else(|| UnifiedError::bad_request("customer_id is required"))?;
        let coupon = redeem_coupon_record(&state, path.as_str(), &call.tenant_id, &customer_id, now())?;
        // The record call already incremented the coupon and the
        // per-customer redemption count under the entry guard; the
        // event below only reports the redemption.
        state.emit(
            "pricing.coupon.redeemed",
            json!({
                "tenant_id": coupon.tenant_id,
                "coupon_id": coupon.id,
                "code": coupon.code,
                "customer_id": customer_id,
                "used_count": coupon.used_count,
            }),
        );
        Ok(coupon)
    })();
    match result {
        Ok(item) => json::success_response(item, 200),
        Err(error) => json::error_response(&error, debug),
    }
}

pub(crate) async fn list_coupon_redemptions(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<PricingState>>,
    req: HttpRequest,
    path: web::Path<String>,
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
    match state.coupons.get(path.as_str()) {
        Some(entry) if entry.tenant_id == tenant_id => {}
        _ => {
            return json::error_response(&UnifiedError::not_found("Coupon not found"), debug);
        }
    }
    let mut items: Vec<CouponRedemption> = state
        .redemptions
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|item| item.coupon_id == path.as_str() && item.tenant_id == tenant_id)
        .filter(|item| {
            matches_search(&item.customer_id, &params)
                && params
                    .filters
                    .get("customer_id")
                    .is_none_or(|value| item.customer_id == *value)
        })
        .collect();
    items.sort_by(|a, b| b.last_redeemed_at.cmp(&a.last_redeemed_at));
    json::success_response(paginate(items, &params), 200)
}

pub(crate) async fn evaluate_discounts_endpoint(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<PricingState>>,
    req: HttpRequest,
    body: web::Json<DiscountEvaluateRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let call = caller(&req)?;
        require_permission(&ctx, &call.role, PERM_PRICING_MANAGE)?;
        let input = body.into_inner();
        let currency = input
            .currency
            .clone()
            .unwrap_or_else(|| state.settings_for(&call.tenant_id).default_currency)
            .to_uppercase();
        require_currency_known(&state, &currency)?;
        let lines = discount_lines_from_request(&input, &currency)?;
        let (evaluation, coupon_problem) = evaluate_for_caller(
            &state,
            &call.tenant_id,
            &lines,
            input.coupon_code.as_deref(),
            input.customer_id.as_deref(),
            &currency,
            &input.rule_ids,
        )?;
        state.emit(
            "pricing.discount.evaluated",
            json!({
                "tenant_id": call.tenant_id,
                "subtotal_scaled": evaluation.subtotal_scaled,
                "total_discount_scaled": evaluation.total_discount_scaled,
                "total_scaled": evaluation.total_scaled,
                "currency": evaluation.currency,
                "coupon_code": input.coupon_code,
                "customer_id": input.customer_id,
            }),
        );
        let mut value = serialize_response(&evaluation)?;
        if let Value::Object(map) = &mut value {
            if let Some(problem) = coupon_problem {
                map.insert("coupon_problem".to_string(), Value::String(problem));
            }
        }
        Ok(value)
    })();
    match result {
        Ok(value) => json::success_response(value, 200),
        Err(error) => json::error_response(&error, debug),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_coupon(id: &str, tenant_id: &str, usage_limit: Option<u32>) -> PricingCoupon {
        let now = Utc::now();
        PricingCoupon {
            id: id.to_string(),
            tenant_id: tenant_id.to_string(),
            code: "SAVE10".to_string(),
            kind: "percent".to_string(),
            percent_bp: 1_000,
            amount_scaled: 0,
            currency: "USD".to_string(),
            buy_quantity: 0,
            get_quantity: 0,
            product_ids: vec![],
            valid_from: None,
            valid_to: None,
            usage_limit,
            per_customer_limit: None,
            used_count: 0,
            stackable: false,
            exclusive: false,
            active: true,
            created_by: "tester".to_string(),
            created_at: now,
            updated_at: now,
        }
    }

    #[test]
    fn concurrent_redemptions_cannot_overspend_the_usage_limit() {
        let state = Arc::new(PricingState::new());
        let tenant_id = "019a1234-5678-7abc-8def-0123456789ab";
        state
            .coupons
            .insert("coupon-1".to_string(), test_coupon("coupon-1", tenant_id, Some(1)));
        // Eight concurrent redemptions against a limit of 1: the
        // entry guard serializes the check-and-increment, so
        // exactly one succeeds.
        let outcomes: Vec<bool> = std::thread::scope(|scope| {
            let mut handles = Vec::new();
            for index in 0..8 {
                let state = Arc::clone(&state);
                handles.push(scope.spawn(move || {
                    redeem_coupon_record(
                        &state,
                        "coupon-1",
                        tenant_id,
                        &format!("customer-{index}"),
                        Utc::now(),
                    )
                    .is_ok()
                }));
            }
            handles
                .into_iter()
                .map(|handle| handle.join().expect("redemption thread panicked"))
                .collect()
        });
        assert_eq!(outcomes.iter().filter(|ok| **ok).count(), 1);
        assert_eq!(
            state.coupons.get("coupon-1").unwrap().used_count,
            1
        );
    }
}
