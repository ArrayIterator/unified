// Store module - cart and checkout handlers.
//
// Split from `src/modules/store.rs`; the module root re-exports the public API of this module.

use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::json;
use actix_web::{web, HttpRequest, HttpResponse};
use chrono::{DateTime, Utc};
use serde_json::{json, Value};
use std::sync::Arc;
use uuid::Uuid;

use super::helpers::{
    caller_company, line_total_for, minor_to_major, owner_of, require_caller_company,
    require_tenant, respond_created,
};
use super::pricing::{claim_coupon_use, evaluate_coupon_with_pricing, license_expiry_for, resolve_tier, validate_coupon_usable};
use super::reference::{new_order_reference};
use super::state::{StoreState, new_id, now};
use super::subscriptions::{create_entitlements_from_hook};
use super::types::{AddCartItem, Cart, CartItem, CheckoutItem, CheckoutRequest, Coupon, Order, OrderItem, RemoveCartItem, default_download_limit};

/// Maximum units of one line at checkout. The cart accepts any
/// positive quantity, but an order line above this is rejected:
/// an unbounded `u32` quantity lets one checkout claim absurd
/// stock or overflow line totals downstream.
pub(crate) const MAX_CHECKOUT_QUANTITY: u32 = 1000;

/// Validate a checkout quantity: default 1, at least 1, at most
/// `MAX_CHECKOUT_QUANTITY`.
pub(crate) fn checkout_quantity(quantity: Option<u32>) -> UnifiedResult<u32> {
    let quantity = quantity.unwrap_or(1);
    if quantity == 0 {
        return Err(UnifiedError::bad_request("Quantity must be at least 1"));
    }
    if quantity > MAX_CHECKOUT_QUANTITY {
        return Err(UnifiedError::bad_request(format!(
            "Quantity must be at most {MAX_CHECKOUT_QUANTITY} per line",
        )));
    }
    Ok(quantity)
}

/// Parse an optional UUID-valued checkout field (course id, exam
/// id): blank or absent stays `None`, a present value must parse
/// as a UUID. The store never invents these ids.
pub(crate) fn optional_uuid_field(
    raw: &Option<String>,
    field: &str,
) -> UnifiedResult<Option<String>> {
    match raw {
        Some(raw) if !raw.trim().is_empty() => {
            Uuid::parse_str(raw.trim())
                .map_err(|_| UnifiedError::bad_request(format!("{field} must be a UUID")))?;
            Ok(Some(raw.trim().to_string()))
        }
        _ => Ok(None),
    }
}

// ---------------------------------------------------------------------------
// Cart handlers
// ---------------------------------------------------------------------------

pub(crate) fn empty_cart(owner: &str) -> Cart {
    Cart {
        owner: owner.to_string(),
        items: Vec::new(),
        currency: "USD".to_string(),
        total: 0,
        updated_at: now(),
    }
}

pub(crate) fn recalculate_cart(cart: &mut Cart) {
    cart.total = cart.items.iter().map(|item| item.line_total).sum();
    cart.updated_at = now();
}

pub(crate) async fn get_cart(state: web::Data<Arc<StoreState>>, req: HttpRequest) -> HttpResponse {
    let tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return json::error_response(&error, false),
    };
    if let Err(error) = caller_company(&tenant, &req) {
        return json::error_response(&error, false);
    }
    let owner = owner_of(&req);
    let cart = state
        .carts
        .get(&owner)
        .map(|entry| entry.value().clone())
        .unwrap_or_else(|| empty_cart(&owner));
    json::success_response(cart, 200)
}

pub(crate) async fn add_to_cart(
    state: web::Data<Arc<StoreState>>,
    req: HttpRequest,
    body: web::Json<AddCartItem>,
) -> HttpResponse {
    let result: UnifiedResult<Cart> = (|| {
        let tenant = require_tenant(&req)?;
        let _company = require_caller_company(&tenant, &req)?;
        let input = body.into_inner();
        let quantity = input.quantity.unwrap_or(1);
        if quantity == 0 {
            return Err(UnifiedError::bad_request("Quantity must be at least 1"));
        }
        let product = state
            .products
            .get(&input.product_id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Product not found"))?;
        if product.status != "published" {
            return Err(UnifiedError::unprocessable(
                "Only published products can be added to the cart",
            ));
        }
        let (unit_price, tier_name) = match &input.tier_id {
            Some(tier_id) => {
                let tier = resolve_tier(&state, &product.id, tier_id)?;
                (tier.price, Some(tier.label))
            }
            None => (product.price, None),
        };
        let owner = owner_of(&req);
        let mut cart = state
            .carts
            .get(&owner)
            .map(|entry| entry.value().clone())
            .unwrap_or_else(|| empty_cart(&owner));
        if !cart.items.is_empty() && cart.currency != product.currency {
            return Err(UnifiedError::unprocessable(
                "All cart items must use the same currency",
            ));
        }
        cart.currency = product.currency.clone();
        match cart.items.iter_mut().find(|item| {
            item.product_id == product.id
                && item.tier_id == input.tier_id
                && item.exam_id == input.exam_id
        }) {
            Some(item) => {
                item.quantity += quantity;
                item.unit_price = unit_price;
                item.line_total = line_total_for(unit_price, item.quantity);
            }
            None => cart.items.push(CartItem {
                product_id: product.id.clone(),
                name: product.name.clone(),
                unit_price,
                quantity,
                line_total: line_total_for(unit_price, quantity),
                tier_id: input.tier_id.clone(),
                tier_name,
                exam_id: input.exam_id.clone(),
            }),
        }
        recalculate_cart(&mut cart);
        state.carts.insert(owner, cart.clone());
        Ok(cart)
    })();
    json::respond(result, false)
}

pub(crate) async fn remove_from_cart(
    state: web::Data<Arc<StoreState>>,
    req: HttpRequest,
    body: Option<web::Json<RemoveCartItem>>,
) -> HttpResponse {
    let result: UnifiedResult<Cart> = (|| {
        let tenant = require_tenant(&req)?;
        let _company = require_caller_company(&tenant, &req)?;
        let owner = owner_of(&req);
        let mut cart = state
            .carts
            .get(&owner)
            .map(|entry| entry.value().clone())
            .unwrap_or_else(|| empty_cart(&owner));
        let (product_id, _tier_id) = match body {
            Some(payload) => {
                let inner = payload.into_inner();
                (inner.product_id, inner.tier_id)
            }
            None => (None, None),
        };
        match product_id {
            Some(product_id) => {
                let before = cart.items.len();
                cart.items.retain(|item| item.product_id != product_id);
                if cart.items.len() == before {
                    return Err(UnifiedError::not_found("Product is not in the cart"));
                }
            }
            None => cart.items.clear(),
        }
        if cart.items.is_empty() {
            cart.currency = "USD".to_string();
        }
        recalculate_cart(&mut cart);
        state.carts.insert(owner, cart.clone());
        Ok(cart)
    })();
    json::respond(result, false)
}

// ---------------------------------------------------------------------------
// Order handlers
// ---------------------------------------------------------------------------

pub(crate) async fn checkout(
    state: web::Data<Arc<StoreState>>,
    req: HttpRequest,
    body: web::Json<CheckoutRequest>,
) -> HttpResponse {
    // C1/C2 (2026-11-22): the buyer is the server-verified Caller,
    // never the `user_id` the request body used to carry — the body
    // value let any caller check out as any other user. The global
    // auth middleware attaches the verified Caller; without it
    // (public route, or a request that never passed the middleware)
    // there is no buyer and checkout is refused with 401.
    // `CheckoutRequest` carries no `user_id` field (removed
    // 2026-11-22): clients that still send one are unaffected,
    // serde ignores unknown fields.
    let tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return json::error_response(&error, false),
    };
    if let Err(error) = require_caller_company(&tenant, &req) {
        return json::error_response(&error, false);
    }
    let caller = unified_core::auth::caller_from(&req);
    let Some(user_id) = caller.user_id.filter(|_| caller.is_authenticated()) else {
        return json::error_response(
            &UnifiedError::unauthorized(
                "Authentication required: provide a Bearer token",
            ),
            false,
        );
    };
    let owner = user_id.to_string();
    let result: UnifiedResult<Order> = (|| {
        let input = body.into_inner();
        // Resolve the items: an explicit list wins, otherwise the stored cart.
        let requested: Vec<CheckoutItem> = match input.items {
            Some(items) => items,
            None => state
                .carts
                .get(&owner)
                .map(|entry| {
                    entry
                        .value()
                        .items
                        .iter()
                        .map(|item| CheckoutItem {
                            product_id: item.product_id.clone(),
                            quantity: Some(item.quantity),
                            tier_id: item.tier_id.clone(),
                            course_id: None,
                            exam_id: item.exam_id.clone(),
                        })
                        .collect()
                })
                .unwrap_or_default(),
        };
        if requested.is_empty() {
            return Err(UnifiedError::unprocessable(
                "Cannot check out an empty cart",
            ));
        }
        let mut items: Vec<OrderItem> = Vec::new();
        let mut currency: Option<String> = None;
        let mut subtotal: i64 = 0;
        let mut company_id: Option<String> = None;
        let mut trial_until: Option<DateTime<Utc>> = None;
        for requested_item in requested {
            let quantity = checkout_quantity(requested_item.quantity)?;
            let course_id = optional_uuid_field(&requested_item.course_id, "course_id")?;
            let exam_id = optional_uuid_field(&requested_item.exam_id, "exam_id")?;
            let product = state
                .products
                .get(&requested_item.product_id)
                .map(|entry| entry.value().clone())
                .ok_or_else(|| {
                    UnifiedError::not_found(format!(
                        "Product {} not found",
                        requested_item.product_id
                    ))
                })?;
            if product.status != "published" {
                return Err(UnifiedError::unprocessable(format!(
                    "Product {} is not published",
                    product.name
                )));
            }
            match &currency {
                Some(existing) if *existing != product.currency => {
                    return Err(UnifiedError::unprocessable(
                        "All order items must use the same currency",
                    ));
                }
                None => currency = Some(product.currency.clone()),
                _ => {}
            }
            let (unit_price, tier_id, tier_name, seats, trial_days, download_limit, billing_period) =
                match &requested_item.tier_id {
                    Some(tier_id) => {
                        let tier = resolve_tier(&state, &product.id, tier_id)?;
                        (
                            tier.price,
                            Some(tier.id.clone()),
                            Some(tier.label.clone()),
                            tier.seats,
                            tier.trial_days,
                            tier.download_limit,
                            tier.billing_period
                                .or_else(|| product.billing_period.clone()),
                        )
                    }
                    None => (
                        product.price,
                        None,
                        None,
                        1,
                        product.trial_days,
                        default_download_limit(),
                        product.billing_period.clone(),
                    ),
                };
            if trial_days > 0 {
                let ends = now() + chrono::Duration::days(trial_days as i64);
                trial_until = Some(match trial_until {
                    Some(existing) if existing > ends => existing,
                    _ => ends,
                });
            }
            if company_id.is_none() {
                company_id = product.company_id.clone();
            }
            let line_total = line_total_for(unit_price, quantity);
            subtotal += line_total;
            // A subscription line with an unknown billing period
            // has no expiry to compute: reject the checkout instead
            // of silently granting a 30-day license.
            let expires_at = license_expiry_for(
                &product.pricing_model,
                product.billing_period.as_deref(),
                product.updates_years,
            );
            if product.pricing_model == "subscription" && expires_at.is_none() {
                return Err(UnifiedError::bad_request(format!(
                    "Product {} has unknown billing period {:?}",
                    product.name, product.billing_period
                )));
            }
            items.push(OrderItem {
                product_id: product.id.clone(),
                name: product.name.clone(),
                unit_price,
                quantity,
                line_total,
                license_product: product.license_product,
                file_id: product.file_id.clone(),
                tier_id,
                tier_name,
                software_id: product.software_id.clone(),
                software_slug: product.software_slug.clone(),
                company_id: product.company_id.clone(),
                pricing_model: product.pricing_model.clone(),
                billing_period,
                seats,
                expires_at,
                download_limit,
                course_id,
                exam_id,
                trial_days,
                proration_policy: product.proration_policy.clone(),
                grace_period_days: product.grace_period_days,
                dunning_max_retries: product.dunning_max_retries,
                dunning_retry_interval_days: product.dunning_retry_interval_days,
                module_id: product.module_id.clone(),
            });
        }
        // A subscription order holds exactly one subscription
        // product: a subscription is one recurring purchase, and
        // mixing it with other items would leave the subscription
        // created at payment ambiguous about which item it covers.
        let subscription_items = items
            .iter()
            .filter(|item| item.pricing_model == "subscription")
            .count();
        if subscription_items > 0 {
            if items.len() != 1 || subscription_items != 1 {
                return Err(UnifiedError::unprocessable(
                    "A subscription order must reference exactly one subscription product",
                ));
            }
            if items[0].quantity != 1 {
                return Err(UnifiedError::bad_request(
                    "A subscription product is bought one at a time (quantity must be 1)",
                ));
            }
            if items[0].billing_period.is_none() {
                return Err(UnifiedError::unprocessable(
                    "A subscription product requires a billing period",
                ));
            }
        }
        // A coupon discounts only the line items it covers; a coupon
        // that covers nothing in the order is rejected below. The
        // discount is evaluated through the shared pricing
        // evaluator first, with the store-local float math as the
        // fallback when pricing cannot price this coupon — exactly
        // one evaluator's math is applied, and the trace is
        // recorded on the order.
        let mut discount_total: i64 = 0;
        let mut coupon_code: Option<String> = None;
        let mut applied_coupon: Option<Coupon> = None;
        let mut discount_evaluation: Option<Value> = None;
        if let Some(code) = input.coupon_code.filter(|code| !code.trim().is_empty()) {
            let coupon = validate_coupon_usable(&state, &code)?;
            let (order_discount, trace) = evaluate_coupon_with_pricing(
                &state,
                &req,
                &items,
                &coupon,
                &owner,
                currency.as_deref().unwrap_or("USD"),
            );
            if order_discount <= 0 {
                return Err(UnifiedError::unprocessable(
                    "Coupon does not apply to any item in this order",
                ));
            }
            // Claim the use atomically (check-and-increment under
            // the coupon entry lock) before the order is inserted,
            // so two concurrent checkouts cannot both spend the
            // last use. Nothing fallible runs between the claim and
            // the insert, so a claimed use is never stranded.
            let claimed = claim_coupon_use(&state, &code)?;
            discount_total = order_discount.min(subtotal);
            discount_evaluation = Some(trace);
            coupon_code = Some(code.trim().to_uppercase());
            applied_coupon = Some(claimed);
        }
        // Single payment reference for the order (batch 18): issued
        // once here, in the shared format, and never changed. The
        // reference travels with the order into `order.created` and
        // `payment.initiated`, so billing and the gateway see the
        // same value.
        let order_reference = new_order_reference(&state, &items);
        let order = Order {
            id: new_id(),
            reference: order_reference,
            user_id: owner.clone(),
            items,
            subtotal,
            discount_total,
            total: subtotal - discount_total,
            currency: currency.clone().unwrap_or_else(|| "USD".to_string()),
            coupon_code: coupon_code.clone(),
            discount_evaluation: discount_evaluation.clone(),
            company_id: company_id.clone(),
            trial_until,
            status: "pending".to_string(),
            provider: None,
            payment_reference: None,
            created_at: now(),
            updated_at: now(),
        };
        state.orders.insert(order.id.clone(), order.clone());
        // The cart is consumed by the checkout.
        state.carts.remove(&owner);
        // Emit the legacy event first (licensing and billing listen to
        // it), then the namespaced store event. The licensing results
        // carry the issued license ids, which become entitlements.
        let order_value = serde_json::to_value(&order).unwrap_or(Value::Null);
        let hook_context = state.emit_with_results("order.created", order_value.clone());
        state.emit("store.order.created", order_value);
        if let Some(coupon) = applied_coupon {
            // The use was already claimed atomically before the
            // order insert (see above); this only announces it.
            state.emit(
                "store.coupon.used",
                json!({
                    "coupon_id": coupon.id,
                    "code": coupon.code,
                    "order_id": order.id,
                    "discount_total": minor_to_major(
                        discount_total,
                        currency.as_deref().unwrap_or("USD")
                    ),
                }),
            );
        }
        create_entitlements_from_hook(&state, &order, &hook_context);
        Ok(order)
    })();
    respond_created(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checkout_quantity_defaults_and_caps() {
        assert_eq!(checkout_quantity(None).unwrap(), 1);
        assert_eq!(checkout_quantity(Some(1)).unwrap(), 1);
        assert_eq!(
            checkout_quantity(Some(MAX_CHECKOUT_QUANTITY)).unwrap(),
            MAX_CHECKOUT_QUANTITY
        );
        assert!(checkout_quantity(Some(0)).is_err());
        assert!(checkout_quantity(Some(MAX_CHECKOUT_QUANTITY + 1)).is_err());
        assert!(checkout_quantity(Some(u32::MAX)).is_err());
    }

    #[test]
    fn optional_uuid_field_accepts_blank_and_validates() {
        assert_eq!(optional_uuid_field(&None, "exam_id").unwrap(), None);
        assert_eq!(
            optional_uuid_field(&Some("   ".to_string()), "exam_id").unwrap(),
            None
        );
        assert_eq!(
            optional_uuid_field(
                &Some("  123e4567-e89b-12d3-a456-426614174000  ".to_string()),
                "exam_id"
            )
            .unwrap(),
            Some("123e4567-e89b-12d3-a456-426614174000".to_string())
        );
        assert!(optional_uuid_field(&Some("not-a-uuid".to_string()), "exam_id").is_err());
        assert!(optional_uuid_field(&Some("not-a-uuid".to_string()), "course_id").is_err());
    }
}
