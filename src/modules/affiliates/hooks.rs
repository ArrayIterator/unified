// affiliates module — Hook listeners.
//
// Split from the former single-file module `src/modules/affiliates.rs`.
// The module root (`affiliates.rs`) re-exports the public items, so the
// public API is unchanged.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::{HookBus, HookContext};
use unified_core::json;
use unified_core::modules::{Module, ModuleRegistry, RouteInfo, RouteVersionSupport};
use unified_core::pagination::{PageResult, PaginationParams};
use actix_web::{web, HttpRequest, HttpResponse};
use chrono::{DateTime, Duration, Utc};
use dashmap::DashMap;
use rand::{distr::Alphanumeric, Rng};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::sync::{Arc, RwLock};
use uuid::Uuid;

use super::types::{AffiliateStatus, AffiliatesState, CommissionStatus};
use super::helpers::{created, major_to_minor, record_conversion, resolve_referral};

// ---------------------------------------------------------------------------
// Hook listeners
// ---------------------------------------------------------------------------

/// `user.created` -> signup conversion. Only when the payload
/// carries a referral signal (see `resolve_referral`); a plain
/// signup without one is not a referral and is skipped. The
/// customer id is read from `id` / `user_id` / `user.id`.
pub(crate) fn on_user_created(state: Arc<AffiliatesState>, ctx: &mut HookContext) {
    let customer_id = ctx
        .get("id")
        .and_then(|value| value.as_str())
        .map(str::to_string)
        .or_else(|| {
            ctx.get("user_id")
                .and_then(|value| value.as_str())
                .map(str::to_string)
        })
        .or_else(|| {
            ctx.get("user")
                .and_then(|user| user.get("id"))
                .and_then(|value| value.as_str())
                .map(str::to_string)
        });
    let Some(customer_id) = customer_id else {
        ctx.add_result(json!({
            "affiliates": "skipped",
            "reason": "user.created carries no customer id",
        }));
        return;
    };
    let Some((affiliate, link_id, click_id)) = resolve_referral(&state, ctx) else {
        ctx.add_result(json!({
            "affiliates": "skipped",
            "reason": "user.created carries no referral code, click id, or affiliate id",
        }));
        return;
    };
    if affiliate.status != AffiliateStatus::Approved {
        ctx.add_result(json!({
            "affiliates": "skipped",
            "affiliate_id": affiliate.id,
            "reason": format!("affiliate is {} and cannot earn conversions", affiliate.status.as_str()),
        }));
        return;
    }
    let now = Utc::now();
    match record_conversion(
        &state,
        &affiliate,
        link_id,
        click_id,
        &customer_id,
        "signup",
        None,
        0,
        &affiliate.currency,
        None,
        now,
    ) {
        Ok((conversion, _)) => {
            ctx.add_result(json!({
                "affiliates": "conversion_tracked",
                "conversion_id": conversion.id,
                "kind": "signup",
                "flagged": conversion.flagged,
            }));
        }
        Err(error) => {
            ctx.add_result(json!({
                "affiliates": "skipped",
                "reason": error.message,
            }));
        }
    }
}

/// `store.order.completed` -> purchase conversion. The order
/// payload is the store `Order` JSON (`id`, `user_id`, `total`,
/// `currency`, `items[].product_id`); the referral signal must be
/// present on the payload (`referral_code`, `affiliate_code`,
/// `click_id`, or `affiliate_id`). `payment.captured` is not
/// consumed as well, so one purchase is never attributed twice.
pub(crate) fn on_store_order_completed(state: Arc<AffiliatesState>, ctx: &mut HookContext) {
    let order = ctx.get("order").cloned().unwrap_or_else(|| ctx.data.clone());
    let order_id = order
        .get("id")
        .and_then(|value| value.as_str())
        .map(str::to_string);
    let customer_id = order
        .get("user_id")
        .and_then(|value| value.as_str())
        .map(str::to_string);
    let (Some(order_id), Some(customer_id)) = (order_id, customer_id) else {
        ctx.add_result(json!({
            "affiliates": "skipped",
            "reason": "store.order.completed carries no order id or user id",
        }));
        return;
    };
    // A completed order that was already attributed (for example a
    // duplicate emit) must not earn a second commission.
    if state
        .conversions
        .iter()
        .any(|entry| entry.value().order_id.as_deref() == Some(order_id.as_str()) && entry.value().kind == "purchase")
    {
        ctx.add_result(json!({
            "affiliates": "already_tracked",
            "order_id": order_id,
        }));
        return;
    }
    let Some((affiliate, link_id, click_id)) = resolve_referral(&state, ctx) else {
        ctx.add_result(json!({
            "affiliates": "skipped",
            "order_id": order_id,
            "reason": "store.order.completed carries no referral signal",
        }));
        return;
    };
    if affiliate.status != AffiliateStatus::Approved {
        ctx.add_result(json!({
            "affiliates": "skipped",
            "affiliate_id": affiliate.id,
            "order_id": order_id,
            "reason": format!("affiliate is {} and cannot earn conversions", affiliate.status.as_str()),
        }));
        return;
    }
    let currency = order
        .get("currency")
        .and_then(|value| value.as_str())
        .map(str::to_string)
        .unwrap_or_else(|| affiliate.currency.clone());
    // The store order payload carries major units; convert to minor
    // units at this boundary.
    let total = major_to_minor(
        order.get("total").and_then(|value| value.as_f64()).unwrap_or(0.0),
        &currency,
    );
    let product_id = order
        .get("items")
        .and_then(|items| items.as_array())
        .and_then(|items| items.first())
        .and_then(|item| item.get("product_id"))
        .and_then(|value| value.as_str())
        .map(str::to_string);
    let now = Utc::now();
    match record_conversion(
        &state,
        &affiliate,
        link_id,
        click_id,
        &customer_id,
        "purchase",
        Some(order_id.clone()),
        total,
        &currency,
        product_id,
        now,
    ) {
        Ok((conversion, commission)) => {
            ctx.add_result(json!({
                "affiliates": "conversion_tracked",
                "conversion_id": conversion.id,
                "kind": "purchase",
                "order_id": order_id,
                "commission_id": commission.map(|record| record.id),
                "flagged": conversion.flagged,
            }));
        }
        Err(error) => {
            ctx.add_result(json!({
                "affiliates": "skipped",
                "order_id": order_id,
                "reason": error.message,
            }));
        }
    }
}

/// `store.order.refunded` -> void still-pending commissions of the
/// refunded order. This is a refund-driven void (a business fact),
/// not a fraud judgement: approved and paid commissions are never
/// touched, and flagged commissions are voided here only because
/// the underlying order no longer stands.
pub(crate) fn on_store_order_refunded(state: Arc<AffiliatesState>, ctx: &mut HookContext) {
    let order = ctx.get("order").cloned().unwrap_or_else(|| ctx.data.clone());
    let Some(order_id) = order
        .get("id")
        .and_then(|value| value.as_str())
        .map(str::to_string)
    else {
        ctx.add_result(json!({
            "affiliates": "skipped",
            "reason": "store.order.refunded carries no order id",
        }));
        return;
    };
    let now = Utc::now();
    let mut voided: Vec<Uuid> = Vec::new();
    for mut entry in state.commissions.iter_mut() {
        let record = entry.value_mut();
        if record.order_id.as_deref() == Some(order_id.as_str())
            && record.status == CommissionStatus::Pending
        {
            record.status = CommissionStatus::Void;
            record.review_reason = Some("order_refunded".to_string());
            record.updated_at = now;
            voided.push(record.id);
        }
    }
    for id in &voided {
        if let Some(record) = state.commissions.get(id).map(|entry| entry.value().clone()) {
            state.emit(
                "affiliates.commission.voided",
                json!({
                    "commission_id": record.id,
                    "affiliate_id": record.affiliate_id,
                    "order_id": order_id,
                    "reason": "order_refunded",
                }),
            );
        }
    }
    ctx.add_result(json!({
        "affiliates": "refund_processed",
        "order_id": order_id,
        "commissions_voided": voided.len(),
    }));
}
