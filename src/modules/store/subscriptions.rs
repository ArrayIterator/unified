// Store module - subscription lifecycle handlers.
//
// Split from `src/modules/store.rs`; the module root re-exports the public API of this module.

use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::HookContext;
use unified_core::json;
use unified_core::pagination::PaginationParams;
use actix_web::{web, HttpRequest, HttpResponse};
use chrono::{DateTime, Utc};
use serde_json::{json, Value};
use std::sync::Arc;

use super::digital::{digital_can_manage, require_digital_permission};
use super::helpers::{
    caller_company, caller_owns, in_company_scope, matches_search, minor_to_major, paginate,
    record_visible_to_caller, require_caller_company, require_tenant,
};
use super::pricing::{period_days, period_end, proration_amounts, resolve_tier, subscription_grants_access};
use super::state::{StoreState, new_id, now};
use super::types::{CancelSubscriptionRequest, ChangePlanRequest, DownloadEntitlement, IssuedLicense, Order, PERM_STORE_MANAGE, Subscription};

/// Turn the licensing hook results of `order.created` into download
/// entitlements and a local mirror of the issued licenses.
pub(crate) fn create_entitlements_from_hook(state: &StoreState, order: &Order, hook_context: &HookContext) {
    for result in &hook_context.results {
        if result.get("issued").and_then(|value| value.as_bool()) != Some(true) {
            continue;
        }
        let license_id = result
            .get("license_id")
            .and_then(|value| value.as_str())
            .map(str::to_string);
        let license_key = result
            .get("license_key")
            .and_then(|value| value.as_str())
            .map(str::to_string);
        let product = result
            .get("product")
            .and_then(|value| value.as_str())
            .unwrap_or("")
            .to_string();
        let Some(item) = order.items.iter().find(|item| item.name == product) else {
            continue;
        };
        let release_id = result
            .get("release_id")
            .and_then(|value| value.as_str())
            .map(str::to_string)
            .or_else(|| {
                state
                    .catalog
                    .iter()
                    .find(|entry| entry.value().product_id.as_deref() == Some(item.product_id.as_str()))
                    .and_then(|entry| entry.value().release_id.clone())
            });
        let software_slug = result
            .get("software_slug")
            .and_then(|value| value.as_str())
            .map(str::to_string)
            .or_else(|| item.software_slug.clone());
        let entitlement = DownloadEntitlement {
            id: new_id(),
            license_id: license_id.clone(),
            license_key: license_key.clone(),
            order_id: order.id.clone(),
            user_id: order.user_id.clone(),
            product_id: item.product_id.clone(),
            tier_id: item.tier_id.clone(),
            software_id: item.software_id.clone(),
            software_slug,
            release_id,
            download_limit: item.download_limit,
            downloads_used: 0,
            // Pending until the order is paid: an unpaid order must
            // never download, and the entitlement check only counts
            // `active` records. `activate_entitlements_for_paid_order`
            // flips this to `active` when the order becomes paid.
            status: "pending".to_string(),
            expires_at: item.expires_at,
            created_at: now(),
            updated_at: now(),
        };
        state
            .entitlements
            .insert(entitlement.id.clone(), entitlement);
        if let (Some(license_id), Some(license_key)) = (license_id, license_key) {
            state.issued_licenses.insert(
                license_id.clone(),
                IssuedLicense {
                    license_id,
                    license_key,
                    product,
                    order_id: Some(order.id.clone()),
                    user_id: Some(order.user_id.clone()),
                    company_id: order.company_id.clone(),
                    // Same pending rule as the entitlement above:
                    // the license mirror only counts as active once
                    // the order is paid.
                    status: "pending".to_string(),
                    created_at: now(),
                },
            );
        }
    }
}

/// Activate every pending download entitlement and issued license
/// of an order that just became paid. Returns the number of
/// entitlements activated. Idempotent: already-active records are
/// left alone, so redelivered captures never double-activate.
pub(crate) fn activate_entitlements_for_paid_order(state: &StoreState, order_id: &str) -> usize {
    let mut activated = 0usize;
    for mut entry in state.entitlements.iter_mut() {
        if entry.value().order_id == order_id && entry.value().status == "pending" {
            entry.value().status = "active".to_string();
            entry.value().updated_at = now();
            activated += 1;
        }
    }
    for mut entry in state.issued_licenses.iter_mut() {
        if entry.value().order_id.as_deref() == Some(order_id)
            && entry.value().status == "pending"
        {
            entry.value().status = "active".to_string();
        }
    }
    activated
}

/// Emit `subscription.entitlement_changed` for a subscription whose
/// access changed, so the licensing module (and any listener) can
/// grant or revoke the module the subscription covers. Only
/// subscriptions bound to a sellable module (`module_id`) emit it.
pub(crate) fn emit_subscription_entitlement_changed(
    state: &StoreState,
    subscription: &Subscription,
    entitled: bool,
    reason: &str,
) {
    let Some(module_id) = &subscription.module_id else {
        return;
    };
    state.emit(
        "subscription.entitlement_changed",
        json!({
            "subscription_id": subscription.id,
            "user_id": subscription.user_id,
            "company_id": subscription.company_id,
            "product_id": subscription.product_id,
            "module_id": module_id,
            "order_id": subscription.order_id,
            "entitled": entitled,
            "reason": reason,
        }),
    );
}

/// Create the subscription for a paid subscription order, or return
/// the existing one when the order already has one (redelivered
/// `payment.captured` events and manual re-pays must never create a
/// second subscription). A trial starts as `trialing` with the
/// trial end recorded; otherwise the subscription starts `active`.
/// The first period runs from now to one billing period after the
/// trial ends (trialing) or from now (no trial). The recurring price
/// is the product renewal price when one is set, else the price paid.
/// Emits `subscription.created` and, for module products,
/// `subscription.entitlement_changed` with `entitled: true`. A
/// subscription born active (no trial) additionally emits
/// `subscription.activated` here; a trialing one emits it when
/// the trial ends (see the `end_trial` lifecycle action).
pub(crate) fn activate_subscription_for_paid_order(
    state: &StoreState,
    order: &Order,
) -> Option<Subscription> {
    let item = order
        .items
        .iter()
        .find(|item| item.pricing_model == "subscription")?;
    if let Some(existing) = state
        .subscriptions
        .iter()
        .find(|entry| entry.value().order_id == order.id)
        .map(|entry| entry.value().clone())
    {
        return Some(existing);
    }
    let billing_period = item.billing_period.clone()?;
    let price = state
        .products
        .get(&item.product_id)
        .and_then(|product| product.renewal_price)
        .unwrap_or(item.unit_price);
    let start = now();
    let trial_end = if item.trial_days > 0 {
        Some(start + chrono::Duration::days(i64::from(item.trial_days)))
    } else {
        None
    };
    // An unknown billing period ends subscription creation instead
    // of silently running a 30-day period: the order stays paid,
    // but no subscription is granted on a period nobody defined.
    let current_period_end = match trial_end {
        Some(trial_end) => period_end(trial_end, &billing_period)?,
        None => period_end(start, &billing_period)?,
    };
    let subscription = Subscription {
        id: new_id(),
        user_id: order.user_id.clone(),
        company_id: order.company_id.clone(),
        product_id: item.product_id.clone(),
        tier_id: item.tier_id.clone(),
        order_id: order.id.clone(),
        module_id: item.module_id.clone(),
        status: if trial_end.is_some() {
            "trialing".to_string()
        } else {
            "active".to_string()
        },
        billing_period,
        price,
        currency: order.currency.clone(),
        trial_days: item.trial_days,
        trial_end,
        current_period_start: start,
        current_period_end,
        proration_policy: item.proration_policy.clone(),
        grace_period_days: item.grace_period_days,
        dunning_max_retries: item.dunning_max_retries,
        dunning_retry_interval_days: item.dunning_retry_interval_days,
        cancel_at_period_end: false,
        cancelled_at: None,
        pending_product_id: None,
        pending_tier_id: None,
        pending_price: None,
        pending_billing_period: None,
        created_at: start,
        updated_at: start,
    };
    state
        .subscriptions
        .insert(subscription.id.clone(), subscription.clone());
    state.emit(
        "subscription.created",
        serde_json::to_value(&subscription).unwrap_or(Value::Null),
    );
    // A subscription born active is activated by this same payment:
    // emit `subscription.activated` so the SaaS provisioning
    // listener fires. (A trialing subscription activates when the
    // trial ends instead.)
    if subscription.status == "active" {
        state.emit(
            "subscription.activated",
            json!({
                "subscription": serde_json::to_value(&subscription).unwrap_or(Value::Null),
                "subscription_id": subscription.id,
                "order_id": subscription.order_id,
                "company_id": subscription.company_id,
            }),
        );
    }
    emit_subscription_entitlement_changed(&state, &subscription, true, "subscription_created");
    Some(subscription)
}

/// Cancel every live subscription of a refunded order. A refund is
/// final, so the cancellation is immediate (not at period end) and
/// revokes the module entitlement when the subscription covers one.
pub(crate) fn cancel_subscriptions_for_order(state: &StoreState, order_id: &str, reason: &str) -> usize {
    let ids: Vec<String> = state
        .subscriptions
        .iter()
        .filter(|entry| {
            entry.value().order_id == order_id && entry.value().status != "cancelled"
        })
        .map(|entry| entry.key().clone())
        .collect();
    let mut cancelled = 0usize;
    for id in ids {
        let Some(mut subscription) = state.subscriptions.get_mut(&id) else {
            continue;
        };
        subscription.status = "cancelled".to_string();
        subscription.cancel_at_period_end = false;
        subscription.cancelled_at = Some(now());
        subscription.updated_at = now();
        let updated = subscription.clone();
        drop(subscription);
        cancelled += 1;
        state.emit(
            "subscription.cancelled",
            json!({
                "subscription": serde_json::to_value(&updated).unwrap_or(Value::Null),
                "subscription_id": updated.id,
                "reason": reason,
            }),
        );
        emit_subscription_entitlement_changed(&state, &updated, false, reason);
    }
    cancelled
}

/// Apply a lifecycle decision from the billing module
/// (`subscription.lifecycle.apply`). Billing owns the lifecycle
/// engine (renewals, dunning, grace, reminders) but never writes
/// this store's records directly; it asks here, and this handler
/// applies the decision to the authoritative subscription and emits
/// the public `subscription.*` event billing mirrors from.
///
/// Actions: `renew` (advance the period to the supplied
/// `current_period_start` / `current_period_end`, applying any
/// pending plan change), `mark_past_due`, `end_trial`,
/// `cancel` (with `reason`). Unknown actions and transitions from
/// a terminal state are reported in the hook result, never applied.
pub(crate) fn on_subscription_lifecycle_apply(state: Arc<StoreState>, ctx: &mut HookContext) {
    let subscription_id = ctx
        .get("subscription_id")
        .and_then(|value| value.as_str())
        .unwrap_or("")
        .to_string();
    let action = ctx
        .get("action")
        .and_then(|value| value.as_str())
        .unwrap_or("")
        .to_string();
    let Some(mut subscription) = state.subscriptions.get_mut(&subscription_id) else {
        ctx.add_result(json!({
            "store": "subscription_not_found",
            "subscription_id": subscription_id,
            "action": action,
        }));
        return;
    };
    let parse_date = |value: Option<&Value>| {
        value
            .and_then(|value| value.as_str())
            .and_then(|text| text.parse::<DateTime<Utc>>().ok())
    };
    let updated: Option<Subscription> = match action.as_str() {
        "renew" => {
            if subscription.status == "cancelled" {
                ctx.add_result(json!({
                    "store": "skipped",
                    "subscription_id": subscription_id,
                    "action": action,
                    "reason": "subscription is cancelled",
                }));
                return;
            }
            if subscription.cancel_at_period_end {
                ctx.add_result(json!({
                    "store": "skipped",
                    "subscription_id": subscription_id,
                    "action": action,
                    "reason": "subscription is scheduled for cancellation at period end",
                }));
                return;
            }
            let Some(period_start) = parse_date(ctx.get("current_period_start")) else {
                ctx.add_result(json!({
                    "store": "skipped",
                    "subscription_id": subscription_id,
                    "action": action,
                    "reason": "current_period_start is required",
                }));
                return;
            };
            let Some(period_end_value) = parse_date(ctx.get("current_period_end")) else {
                ctx.add_result(json!({
                    "store": "skipped",
                    "subscription_id": subscription_id,
                    "action": action,
                    "reason": "current_period_end is required",
                }));
                return;
            };
            if period_end_value <= period_start {
                ctx.add_result(json!({
                    "store": "skipped",
                    "subscription_id": subscription_id,
                    "action": action,
                    "reason": "current_period_end must be after current_period_start",
                }));
                return;
            }
            // A plan change waiting for the next period takes effect
            // with the renewal.
            if let Some(pending_product_id) = subscription.pending_product_id.clone() {
                subscription.product_id = pending_product_id;
                if let Some(pending_price) = subscription.pending_price {
                    subscription.price = pending_price;
                }
                if let Some(pending_period) = subscription.pending_billing_period.clone() {
                    subscription.billing_period = pending_period;
                }
                subscription.tier_id = subscription.pending_tier_id.clone();
                subscription.pending_product_id = None;
                subscription.pending_tier_id = None;
                subscription.pending_price = None;
                subscription.pending_billing_period = None;
            }
            subscription.status = "active".to_string();
            subscription.current_period_start = period_start;
            subscription.current_period_end = period_end_value;
            subscription.updated_at = now();
            let updated = subscription.clone();
            drop(subscription);
            state.emit(
                "subscription.renewed",
                json!({
                    "subscription": serde_json::to_value(&updated).unwrap_or(Value::Null),
                    "subscription_id": updated.id,
                    "invoice_id": ctx.get("invoice_id").cloned().unwrap_or(Value::Null),
                }),
            );
            Some(updated)
        }
        "mark_past_due" => {
            if !matches!(subscription.status.as_str(), "active" | "trialing") {
                ctx.add_result(json!({
                    "store": "skipped",
                    "subscription_id": subscription_id,
                    "action": action,
                    "reason": format!("subscription is {}", subscription.status),
                }));
                return;
            }
            subscription.status = "past_due".to_string();
            subscription.updated_at = now();
            let updated = subscription.clone();
            drop(subscription);
            state.emit(
                "subscription.past_due",
                json!({
                    "subscription": serde_json::to_value(&updated).unwrap_or(Value::Null),
                    "subscription_id": updated.id,
                    "reason": ctx.get("reason").cloned().unwrap_or(Value::Null),
                }),
            );
            Some(updated)
        }
        "end_trial" => {
            if subscription.status != "trialing" {
                ctx.add_result(json!({
                    "store": "skipped",
                    "subscription_id": subscription_id,
                    "action": action,
                    "reason": format!("subscription is {}", subscription.status),
                }));
                return;
            }
            subscription.status = "active".to_string();
            subscription.updated_at = now();
            let updated = subscription.clone();
            drop(subscription);
            state.emit(
                "subscription.trial_ended",
                json!({
                    "subscription": serde_json::to_value(&updated).unwrap_or(Value::Null),
                    "subscription_id": updated.id,
                }),
            );
            // Trial over and now active: this is the activation
            // the SaaS provisioning listener waits for.
            state.emit(
                "subscription.activated",
                json!({
                    "subscription": serde_json::to_value(&updated).unwrap_or(Value::Null),
                    "subscription_id": updated.id,
                    "order_id": updated.order_id,
                    "company_id": updated.company_id,
                }),
            );
            Some(updated)
        }
        "cancel" => {
            if subscription.status == "cancelled" {
                ctx.add_result(json!({
                    "store": "already_cancelled",
                    "subscription_id": subscription_id,
                    "action": action,
                }));
                return;
            }
            let reason = ctx
                .get("reason")
                .and_then(|value| value.as_str())
                .unwrap_or("cancelled")
                .to_string();
            subscription.status = "cancelled".to_string();
            subscription.cancel_at_period_end = false;
            subscription.cancelled_at = Some(now());
            subscription.updated_at = now();
            let updated = subscription.clone();
            drop(subscription);
            state.emit(
                "subscription.cancelled",
                json!({
                    "subscription": serde_json::to_value(&updated).unwrap_or(Value::Null),
                    "subscription_id": updated.id,
                    "reason": reason,
                }),
            );
            emit_subscription_entitlement_changed(&state, &updated, false, &reason);
            Some(updated)
        }
        other => {
            ctx.add_result(json!({
                "store": "skipped",
                "subscription_id": subscription_id,
                "action": action,
                "reason": format!("unknown lifecycle action \"{other}\""),
            }));
            return;
        }
    };
    if let Some(updated) = updated {
        ctx.add_result(json!({
            "store": "applied",
            "subscription_id": updated.id,
            "action": action,
            "status": updated.status,
        }));
    }
}

/// Answer `subscription.module_entitlement.check` with payload
/// `{"company_id", "module_id"}`: entitled when the company holds a
/// subscription that covers the module and currently grants access
/// (trialing/active inside the period, past-due inside grace, or
/// cancelled at period end inside the period). This is the hook the
/// licensing module's `check_module_entitlement` must call (see the
/// integration notes) so an active subscription counts as a module
/// entitlement alongside a license.
pub(crate) fn on_subscription_module_entitlement_check(state: Arc<StoreState>, ctx: &mut HookContext) {
    let company_id = ctx
        .get("company_id")
        .and_then(|value| value.as_str())
        .unwrap_or("")
        .to_string();
    let module_id = ctx
        .get("module_id")
        .and_then(|value| value.as_str())
        .unwrap_or("")
        .to_string();
    let now = now();
    let found = state
        .subscriptions
        .iter()
        .find(|entry| {
            let subscription = entry.value();
            subscription.module_id.as_deref() == Some(module_id.as_str())
                && subscription.company_id.as_deref() == Some(company_id.as_str())
                && subscription_grants_access(subscription, now)
        })
        .map(|entry| entry.value().clone());
    match found {
        Some(subscription) => ctx.add_result(json!({
            "entitled": true,
            "module_id": module_id,
            "company_id": company_id,
            "subscription_id": subscription.id,
            "source": "subscription",
        })),
        None => ctx.add_result(json!({
            "entitled": false,
            "module_id": module_id,
            "company_id": company_id,
            "source": "subscription",
            "reason": "no_active_subscription",
        })),
    }
}

// Subscription handlers

pub(crate) fn require_subscription_owner(req: &HttpRequest, subscription: &Subscription) -> UnifiedResult<()> {
    if caller_owns(req, &subscription.user_id) {
        Ok(())
    } else {
        Err(UnifiedError::forbidden(
            "Subscription belongs to a different user",
        ))
    }
}

pub(crate) async fn list_subscriptions(
    state: web::Data<Arc<StoreState>>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return json::error_response(&error, false),
    };
    let scope = match caller_company(&tenant, &req) {
        Ok(scope) => scope,
        Err(error) => return json::error_response(&error, false),
    };
    let params = query.into_inner();
    let manage = digital_can_manage(&state, &req);
    let mut items: Vec<Subscription> = state
        .subscriptions
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|subscription| {
            record_visible_to_caller(&req, &subscription.user_id, manage)
                && (!manage || in_company_scope(&subscription.company_id, &scope))
                && matches_search(
                &format!(
                    "{} {} {}",
                    subscription.id, subscription.user_id, subscription.product_id
                ),
                &params,
            ) && params
                .filters
                .get("status")
                .is_none_or(|status| subscription.status == *status)
                && params
                    .filters
                    .get("user_id")
                    .is_none_or(|user_id| subscription.user_id == *user_id)
                && params
                    .filters
                    .get("product_id")
                    .is_none_or(|product_id| subscription.product_id == *product_id)
                && params.filters.get("module_id").is_none_or(|module_id| {
                    subscription.module_id.as_deref() == Some(module_id.as_str())
                })
                && params
                    .filters
                    .get("company_id")
                    .is_none_or(|company_id| {
                        subscription.company_id.as_deref() == Some(company_id.as_str())
                    })
        })
        .collect();
    items.sort_by(|a, b| a.created_at.cmp(&b.created_at));
    if params.normalized().order == "desc" {
        items.reverse();
    }
    let page = paginate(items, &params);
    json::success_response(page, 200)
}

pub(crate) async fn get_subscription(
    state: web::Data<Arc<StoreState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<Subscription> = (|| {
        let tenant = require_tenant(&req)?;
        let _company = caller_company(&tenant, &req)?;
        let subscription = state
            .subscriptions
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Subscription not found"))?;
        require_subscription_owner(&req, &subscription)?;
        Ok(subscription)
    })();
    json::respond(result, false)
}

/// Change the plan of a subscription mid-cycle, applying the
/// subscription's proration policy.
///
/// `prorate` applies the change immediately: the unused share of
/// the old plan is credited, the new plan is charged for the
/// remaining time, and the new period starts now. `no_prorate`
/// stores the new plan as pending and applies it at the next period
/// end with no mid-cycle charge or credit. `credit_only` credits
/// the unused time now and applies the new price from the next
/// period. The `subscription.upgraded` / `subscription.downgraded`
/// event carries the proration lines (`credit`, `charge`, `net`,
/// `currency`) so billing can invoice them.
pub(crate) async fn change_subscription_plan(
    state: web::Data<Arc<StoreState>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<ChangePlanRequest>,
) -> HttpResponse {
    let result: UnifiedResult<Subscription> = (|| {
        let tenant = require_tenant(&req)?;
        let _company = require_caller_company(&tenant, &req)?;
        let input = body.into_inner();
        let existing = state
            .subscriptions
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Subscription not found"))?;
        require_subscription_owner(&req, &existing)?;
        if !matches!(existing.status.as_str(), "active" | "trialing") {
            return Err(UnifiedError::unprocessable(format!(
                "A {} subscription cannot change plan",
                existing.status
            )));
        }
        let product = state
            .products
            .get(&input.product_id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| {
                UnifiedError::not_found(format!("Product {} not found", input.product_id))
            })?;
        if product.status != "published" {
            return Err(UnifiedError::unprocessable(format!(
                "Product {} is not published",
                product.name
            )));
        }
        if product.pricing_model != "subscription" {
            return Err(UnifiedError::unprocessable(
                "The new product is not a subscription product",
            ));
        }
        let Some(new_billing_period) = product.billing_period.clone() else {
            return Err(UnifiedError::unprocessable(
                "The new product has no billing period",
            ));
        };
        let (new_price, tier_id) = match &input.tier_id {
            Some(tier_id) => {
                let tier = resolve_tier(&state, &product.id, tier_id)?;
                (tier.price, Some(tier.id.clone()))
            }
            None => (
                product.renewal_price.unwrap_or(product.price),
                None,
            ),
        };
        if new_price < 0 {
            return Err(UnifiedError::bad_request(
                "The new plan price must be a non-negative number",
            ));
        }
        let now = now();
        let remaining_days = ((existing.current_period_end - now).num_seconds() as f64 / 86_400.0)
            .max(0.0);
        let Some(period_total_days) = period_days(&existing.billing_period) else {
            return Err(UnifiedError::unprocessable(format!(
                "Subscription has unknown billing period \"{}\"",
                existing.billing_period
            )));
        };
        let Some(new_period_total_days) = period_days(&new_billing_period) else {
            return Err(UnifiedError::unprocessable(format!(
                "New product has unknown billing period \"{new_billing_period}\"",
            )));
        };
        let Some(new_period_end) = period_end(now, &new_billing_period) else {
            return Err(UnifiedError::unprocessable(format!(
                "New product has unknown billing period \"{new_billing_period}\"",
            )));
        };
        let period_total_days = period_total_days as f64;
        let new_period_total_days = new_period_total_days as f64;
        let (credit, charge, net) = proration_amounts(
            &existing.proration_policy,
            existing.price,
            new_price,
            remaining_days,
            period_total_days,
            new_period_total_days,
        );
        let direction = if new_price >= existing.price {
            "upgraded"
        } else {
            "downgraded"
        };
        let event = format!("subscription.{direction}");
        let previous_product_id = existing.product_id.clone();
        let previous_price = existing.price;
        let mut subscription = state
            .subscriptions
            .get_mut(path.as_str())
            .ok_or_else(|| UnifiedError::not_found("Subscription not found"))?;
        let effective_at = match existing.proration_policy.as_str() {
            // Immediate change: the new period starts now.
            "prorate" => {
                subscription.product_id = product.id.clone();
                subscription.tier_id = tier_id;
                subscription.price = new_price;
                subscription.billing_period = new_billing_period.clone();
                subscription.current_period_start = now;
                subscription.current_period_end = new_period_end;
                subscription.pending_product_id = None;
                subscription.pending_tier_id = None;
                subscription.pending_price = None;
                subscription.pending_billing_period = None;
                subscription.current_period_end
            }
            // Deferred change: the new plan waits for the next
            // period end; the current period runs out unchanged.
            _ => {
                subscription.pending_product_id = Some(product.id.clone());
                subscription.pending_tier_id = tier_id;
                subscription.pending_price = Some(new_price);
                subscription.pending_billing_period = Some(new_billing_period);
                subscription.current_period_end
            }
        };
        subscription.module_id = product.module_id.clone();
        subscription.updated_at = now;
        let updated = subscription.clone();
        drop(subscription);
        state.emit(
            &event,
            json!({
                "subscription": serde_json::to_value(&updated).unwrap_or(Value::Null),
                "subscription_id": updated.id,
                "previous_product_id": previous_product_id,
                "previous_price": minor_to_major(previous_price, &updated.currency),
                "proration": {
                    "policy": updated.proration_policy,
                    "credit": minor_to_major(credit, &updated.currency),
                    "charge": minor_to_major(charge, &updated.currency),
                    "net": minor_to_major(net, &updated.currency),
                    "currency": updated.currency,
                },
                "effective_at": effective_at,
            }),
        );
        // An upgrade is also the store order upgrade the licensing
        // module listens for (`store.order.upgraded`): name the
        // order's licenses so an expired license can return to
        // active on the new product. The license ids ride along
        // when the store knows them; otherwise the handler resolves
        // them from the order through the store (see
        // `on_store_order_licenses`).
        if direction == "upgraded" {
            let license_ids: Vec<String> = state
                .issued_licenses
                .iter()
                .filter(|entry| {
                    entry.value().order_id.as_deref() == Some(existing.order_id.as_str())
                })
                .map(|entry| entry.value().license_id.clone())
                .collect();
            state.emit(
                "store.order.upgraded",
                json!({
                    "order_id": existing.order_id,
                    "license_ids": license_ids,
                    "subscription_id": updated.id,
                    "new_product": updated.product_id,
                    "previous_product_id": previous_product_id,
                }),
            );
        }
        emit_subscription_entitlement_changed(
            &state,
            &updated,
            true,
            &format!("subscription_{direction}"),
        );
        Ok(updated)
    })();
    json::respond(result, false)
}

/// Cancel a subscription. With `cancel_at_period_end` (default)
/// the subscription stays active until the end of the current
/// period and access continues until then; with `false` it ends
/// immediately. Emits `subscription.cancelled` and, for module
/// products, `subscription.entitlement_changed`.
pub(crate) async fn cancel_subscription(
    state: web::Data<Arc<StoreState>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<CancelSubscriptionRequest>,
) -> HttpResponse {
    let result: UnifiedResult<Subscription> = (|| {
        let tenant = require_tenant(&req)?;
        let _company = require_caller_company(&tenant, &req)?;
        let cancel_at_period_end = body.into_inner().cancel_at_period_end;
        let existing = state
            .subscriptions
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Subscription not found"))?;
        require_subscription_owner(&req, &existing)?;
        if existing.status == "cancelled" {
            return Err(UnifiedError::conflict("Subscription is already cancelled"));
        }
        if !matches!(existing.status.as_str(), "active" | "trialing" | "past_due") {
            return Err(UnifiedError::unprocessable(format!(
                "A {} subscription cannot be cancelled",
                existing.status
            )));
        }
        let mut subscription = state
            .subscriptions
            .get_mut(path.as_str())
            .ok_or_else(|| UnifiedError::not_found("Subscription not found"))?;
        let effective = if cancel_at_period_end {
            subscription.cancel_at_period_end = true;
            subscription.updated_at = now();
            "period_end"
        } else {
            subscription.status = "cancelled".to_string();
            subscription.cancel_at_period_end = false;
            subscription.cancelled_at = Some(now());
            subscription.updated_at = now();
            "immediate"
        };
        let updated = subscription.clone();
        drop(subscription);
        state.emit(
            "subscription.cancelled",
            json!({
                "subscription": serde_json::to_value(&updated).unwrap_or(Value::Null),
                "subscription_id": updated.id,
                "effective": effective,
                "reason": "cancelled_by_customer",
            }),
        );
        if effective == "immediate" {
            emit_subscription_entitlement_changed(
                &state,
                &updated,
                false,
                "cancelled_by_customer",
            );
        }
        Ok(updated)
    })();
    json::respond(result, false)
}

/// Expire subscriptions whose access window has closed. A live
/// subscription (`active`, `trialing`, or `past_due` inside grace)
/// whose period end - plus the grace days for `past_due` - is past
/// no longer grants access (see `subscription_grants_access`), so
/// the sweep moves it to the terminal `expired` status and emits
/// `subscription.expired` (plus `subscription.entitlement_changed`
/// with `entitled: false`) so SaaS provisioning and entitlements
/// lapse it. A subscription cancelled at period end instead becomes
/// `cancelled` at the same boundary, with `subscription.cancelled`
/// and reason `period_end`. Manager-only (`store.manage`); run on a
/// schedule or on demand - call this endpoint from a scheduled job
/// until the scheduler learns a kind for it.
pub(crate) async fn expire_due_subscriptions(
    state: web::Data<Arc<StoreState>>,
    req: HttpRequest,
) -> HttpResponse {
    let result: UnifiedResult<Value> = (|| {
        let tenant = require_tenant(&req)?;
        let _company = require_caller_company(&tenant, &req)?;
        require_digital_permission(&state, &req, PERM_STORE_MANAGE)?;
        let now = now();
        let due: Vec<Subscription> = state
            .subscriptions
            .iter()
            .filter(|entry| {
                let subscription = entry.value();
                matches!(
                    subscription.status.as_str(),
                    "active" | "trialing" | "past_due"
                ) && !subscription_grants_access(subscription, now)
            })
            .map(|entry| entry.value().clone())
            .collect();
        let mut expired: Vec<String> = Vec::new();
        let mut cancelled: Vec<String> = Vec::new();
        for subscription in due {
            let Some(mut stored) = state.subscriptions.get_mut(&subscription.id) else {
                continue;
            };
            // Re-check under the lock: a renewal or another sweep
            // may have moved the subscription since the snapshot.
            if !matches!(stored.status.as_str(), "active" | "trialing" | "past_due")
                || subscription_grants_access(&stored, now)
            {
                continue;
            }
            let was_cancel_at_period_end = stored.cancel_at_period_end;
            if was_cancel_at_period_end {
                stored.status = "cancelled".to_string();
                stored.cancel_at_period_end = false;
                stored.cancelled_at = Some(now);
            } else {
                stored.status = "expired".to_string();
            }
            stored.updated_at = now;
            let updated = stored.clone();
            drop(stored);
            if was_cancel_at_period_end {
                cancelled.push(updated.id.clone());
                state.emit(
                    "subscription.cancelled",
                    json!({
                        "subscription": serde_json::to_value(&updated).unwrap_or(Value::Null),
                        "subscription_id": updated.id,
                        "reason": "period_end",
                    }),
                );
            } else {
                expired.push(updated.id.clone());
                state.emit(
                    "subscription.expired",
                    json!({
                        "subscription": serde_json::to_value(&updated).unwrap_or(Value::Null),
                        "subscription_id": updated.id,
                        "order_id": updated.order_id,
                        "company_id": updated.company_id,
                    }),
                );
            }
            emit_subscription_entitlement_changed(
                &state,
                &updated,
                false,
                if was_cancel_at_period_end {
                    "subscription_cancelled"
                } else {
                    "subscription_expired"
                },
            );
        }
        Ok(json!({
            "swept_at": now.to_rfc3339(),
            "expired": expired,
            "cancelled": cancelled,
        }))
    })();
    json::respond(result, false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use unified_core::hooks::HookContext;
    use super::types::{OrderItem, default_dunning_max_retries, default_dunning_retry_interval_days, default_proration_policy};

    fn order_item(name: &str) -> OrderItem {
        OrderItem {
            product_id: "prod-1".to_string(),
            name: name.to_string(),
            unit_price: 4900,
            quantity: 1,
            line_total: 4900,
            license_product: true,
            file_id: None,
            tier_id: None,
            tier_name: None,
            software_id: None,
            software_slug: None,
            company_id: None,
            pricing_model: "one_time".to_string(),
            billing_period: None,
            seats: 1,
            expires_at: None,
            download_limit: 5,
            course_id: None,
            exam_id: None,
            trial_days: 0,
            proration_policy: default_proration_policy(),
            grace_period_days: 0,
            dunning_max_retries: default_dunning_max_retries(),
            dunning_retry_interval_days: default_dunning_retry_interval_days(),
            module_id: None,
        }
    }

    fn pending_order() -> Order {
        let timestamp = now();
        Order {
            id: "ord-1".to_string(),
            reference: "ORD-1".to_string(),
            user_id: "user-1".to_string(),
            items: vec![order_item("Pro Plugin")],
            subtotal: 4900,
            discount_total: 0,
            total: 4900,
            currency: "USD".to_string(),
            coupon_code: None,
            discount_evaluation: None,
            company_id: None,
            trial_until: None,
            status: "pending".to_string(),
            provider: None,
            payment_reference: None,
            created_at: timestamp,
            updated_at: timestamp,
        }
    }

    fn issued_result() -> Value {
        json!({
            "issued": true,
            "license_id": "lic-1",
            "license_key": "KEY-1",
            "product": "Pro Plugin",
            "release_id": "rel-1",
        })
    }

    #[test]
    fn entitlements_start_pending_and_activate_on_payment() {
        let state = StoreState::new();
        let order = pending_order();
        let mut ctx = HookContext::new("order.created", json!({}));
        ctx.add_result(issued_result());
        create_entitlements_from_hook(&state, &order, &ctx);
        let entitlement = state
            .entitlements
            .iter()
            .next()
            .expect("one entitlement is created")
            .value()
            .clone();
        assert_eq!(entitlement.status, "pending");
        assert_eq!(entitlement.order_id, "ord-1");
        let license = state
            .issued_licenses
            .get("lic-1")
            .expect("issued license is mirrored")
            .value()
            .clone();
        assert_eq!(license.status, "pending");
        assert_eq!(activate_entitlements_for_paid_order(&state, "ord-1"), 1);
        assert_eq!(
            state
                .entitlements
                .get(&entitlement.id)
                .map(|entry| entry.status.clone()),
            Some("active".to_string())
        );
        assert_eq!(
            state
                .issued_licenses
                .get("lic-1")
                .map(|entry| entry.status.clone()),
            Some("active".to_string())
        );
        assert_eq!(
            activate_entitlements_for_paid_order(&state, "ord-1"),
            0,
            "activation is idempotent"
        );
    }

    #[test]
    fn entitlements_are_not_created_for_unissued_results() {
        let state = StoreState::new();
        let order = pending_order();
        let mut ctx = HookContext::new("order.created", json!({}));
        ctx.add_result(json!({ "issued": false, "product": "Pro Plugin" }));
        create_entitlements_from_hook(&state, &order, &ctx);
        assert_eq!(state.entitlements.len(), 0);
        assert_eq!(state.issued_licenses.len(), 0);
    }
}
