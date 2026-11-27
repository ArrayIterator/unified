// Store module - hook bus event handlers.
//
// Split from `src/modules/store.rs`; the module root re-exports the public API of this module.

use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::HookContext;
use unified_core::json;
use semver::Version;
use serde_json::{json, Value};
use std::sync::Arc;

use super::digital::{grant_digital_purchases_for_paid_order, reverse_digital_royalties_for_order};
use super::helpers::major_to_minor;
use super::pricing::{allowed_transition, subscription_grants_access, valid_currency};
use super::reference::{new_order_reference};
use super::signing::{suspend_order_licenses};
use super::state::{StoreState, new_id, now};
use super::subscriptions::{activate_entitlements_for_paid_order, activate_subscription_for_paid_order, cancel_subscriptions_for_order};
use super::types::{Order, OrderItem, Product, default_dunning_max_retries, default_dunning_retry_interval_days, default_proration_policy};

/// Refresh the store catalog entry for a published software release
/// (`software.release.published`). The payload carries the software
/// slug, version, changelog, and requirements (added by the software
/// module when it publishes). The entry's latest version only moves
/// forward: publishing an older version refreshes the changelog but
/// never downgrades `latest_version`. When no catalog entry exists
/// for the slug this is a no-op, not an error: not every software
/// record is sold through the store.
pub(crate) fn on_software_release_published(state: Arc<StoreState>, ctx: &mut HookContext) {
    let slug = ctx
        .get("software_slug")
        .and_then(|value| value.as_str())
        .map(str::to_string)
        .unwrap_or_default();
    if slug.trim().is_empty() {
        ctx.add_result(json!({
            "updated": false,
            "reason": "software_slug is missing from the published event",
        }));
        return;
    }
    let Some(version) = ctx
        .get("version")
        .and_then(|value| value.as_str())
        .map(str::to_string)
    else {
        ctx.add_result(json!({
            "updated": false,
            "reason": "version is missing from the published event",
            "slug": slug,
        }));
        return;
    };
    let changelog = ctx
        .get("changelog")
        .and_then(|value| value.as_str())
        .map(str::to_string);
    let requirements = ctx.get("requirements").cloned().unwrap_or(Value::Null);
    let requires_wp = requirements
        .get("wordpress")
        .and_then(|value| value.as_str())
        .map(str::to_string);
    let requires_php = requirements
        .get("php")
        .and_then(|value| value.as_str())
        .map(str::to_string);
    let release_id = ctx
        .get("id")
        .and_then(|value| value.as_str())
        .map(str::to_string);
    let mut updated = false;
    for mut entry in state.catalog.iter_mut() {
        if entry.value().slug != slug {
            continue;
        }
        let current = entry.value().latest_version.clone();
        let newer = match (Version::parse(&version), Version::parse(&current)) {
            (Ok(new), Ok(old)) => new > old,
            // Unparseable versions are treated as authoritative: the
            // publish event wins over the stored value.
            _ => true,
        };
        if newer {
            entry.value().latest_version = version.clone();
        }
        if let Some(changelog) = &changelog {
            entry.value().changelog = changelog.clone();
        }
        if let Some(requires_wp) = &requires_wp {
            entry.value().requires_wp = Some(requires_wp.clone());
        }
        if let Some(requires_php) = &requires_php {
            entry.value().requires_php = Some(requires_php.clone());
        }
        if let Some(release_id) = &release_id {
            entry.value().release_id = Some(release_id.clone());
        }
        entry.value().updated_at = now();
        updated = true;
    }
    ctx.add_result(json!({
        "updated": updated,
        "slug": slug,
        "version": version,
    }));
}

/// Apply a validated order status transition. Refunds suspend the
/// order's entitlements and issued licenses as a side effect.
pub(crate) fn transition_order(
    state: &StoreState,
    order_id: &str,
    target: &str,
) -> UnifiedResult<Order> {
    let mut order = state
        .orders
        .get_mut(order_id)
        .ok_or_else(|| UnifiedError::not_found("Order not found"))?;
    if !allowed_transition(&order.status, target) {
        return Err(UnifiedError::unprocessable(format!(
            "Cannot move order from {} to {}",
            order.status, target
        )));
    }
    order.status = target.to_string();
    order.updated_at = now();
    let updated = order.clone();
    drop(order);
    if target == "paid" {
        activate_entitlements_for_paid_order(state, order_id);
    }
    if target == "refunded" {
        suspend_order_licenses(state, order_id);
        reverse_digital_royalties_for_order(state, order_id);
    }
    Ok(updated)
}

// Payment hook listeners

/// Event names this module listens to (the payments module owns the
/// canonical constants; the literals are repeated here so the store
/// stays decoupled from the payments module).
pub(crate) const PAYMENT_CAPTURED: &str = "payment.captured";
pub(crate) const PAYMENT_REFUNDED: &str = "payment.refunded";
pub(crate) const BANK_TRANSFER_VERIFICATION_CREATED: &str = "bank_transfer.verification.created";
pub(crate) const BANK_TRANSFER_REJECTED: &str = "bank_transfer.rejected";

/// An approved payment (`payment.captured`, gateway or bank transfer)
/// moves the matching order to `paid`. Only `pending`,
/// `payment_pending`, and `pending_verification` may move to paid -
/// never `refunded`, `cancelled`, or `completed` - and an order that
/// is already paid is left alone, so redelivered captures are safe.
/// The payment reference (provider transaction id) is recorded on the
/// order, and the same downstream events the manual pay path emits
/// Serialize an order for the `order.paid` / `store.order.completed`
/// events, carrying the e-learning course id and the exams exam id
/// the activation listeners need. The contract is a top-level
/// `course_id` plus `module` = `"elearning"`, or an `items` entry
/// carrying them (and the same shape for exams: top-level `exam_id`
/// plus `module` = `"exams"`, or an `items` entry carrying them).
/// Order items already serialize their `course_id` and `exam_id`
/// (set at checkout); items also serialize `module_id`, never
/// `module`, so each item that carries a course id gains an explicit
/// `module: "elearning"` here, and each item that carries an exam id
/// gains `module: "exams"`. A single distinct course id is hoisted
/// to the top level, and a single distinct exam id is hoisted too
/// when no course id was hoisted (the top level carries one
/// `module` marker, so when both are present only the per-item
/// entries carry them). With several distinct course ids the top
/// level stays ambiguous and only the per-item entries carry them;
/// the same holds for exams.
pub(crate) fn order_paid_value(order: &Order) -> Value {
    let mut value = serde_json::to_value(order).unwrap_or(Value::Null);
    let mut course_ids: Vec<String> = Vec::new();
    let mut exam_ids: Vec<String> = Vec::new();
    if let Some(items) = value
        .get_mut("items")
        .and_then(|items| items.as_array_mut())
    {
        for item in items.iter_mut() {
            let course_id = item
                .get("course_id")
                .and_then(|id| id.as_str())
                .map(str::to_string);
            if let Some(course_id) = course_id {
                if let Value::Object(map) = item {
                    map.insert(
                        "module".to_string(),
                        Value::String("elearning".to_string()),
                    );
                }
                if !course_ids.contains(&course_id) {
                    course_ids.push(course_id);
                }
            }
            let exam_id = item
                .get("exam_id")
                .and_then(|id| id.as_str())
                .map(str::to_string);
            if let Some(exam_id) = exam_id {
                if let Value::Object(map) = item {
                    map.insert(
                        "module".to_string(),
                        Value::String("exams".to_string()),
                    );
                }
                if !exam_ids.contains(&exam_id) {
                    exam_ids.push(exam_id);
                }
            }
        }
    }
    if course_ids.len() == 1 {
        if let Value::Object(map) = &mut value {
            map.insert(
                "course_id".to_string(),
                Value::String(course_ids[0].clone()),
            );
            map.insert(
                "module".to_string(),
                Value::String("elearning".to_string()),
            );
        }
    } else if exam_ids.len() == 1 {
        if let Value::Object(map) = &mut value {
            map.insert(
                "exam_id".to_string(),
                Value::String(exam_ids[0].clone()),
            );
            map.insert(
                "module".to_string(),
                Value::String("exams".to_string()),
            );
        }
    }
    value
}

/// (`payment.completed`, `order.paid`) are emitted, so licensing,
/// hosting, and billing react exactly as they do for a manual payment.
///
/// Amount check: for gateway payments the captured amount must cover
/// the order total, so a partial capture (or one split part) never
/// marks the whole order paid. Bank-transfer captures skip the check:
/// the administrator already verified the amount when approving the
/// transfer, including approved mismatches. A currency mismatch is
/// never applied.
pub(crate) fn on_payment_captured(state: Arc<StoreState>, ctx: &mut HookContext) {
    let order_id = ctx
        .get("order_id")
        .and_then(|value| value.as_str())
        .map(str::to_string)
        .or_else(|| {
            ctx.get("order")
                .and_then(|order| order.get("id"))
                .and_then(|id| id.as_str())
                .map(str::to_string)
        });
    let Some(order_id) = order_id else {
        ctx.add_result(json!({
            "store": "skipped",
            "reason": "payment.captured carries no order id",
        }));
        return;
    };
    // An explicit non-captured status is never treated as a capture;
    // a missing status means the event name itself is the claim.
    if let Some(status) = ctx.get("status").and_then(|value| value.as_str()) {
        if status != "captured" {
            ctx.add_result(json!({
                "store": "skipped",
                "order_id": order_id,
                "reason": format!("payment.captured carries status \"{status}\", not \"captured\""),
            }));
            return;
        }
    }
    let provider = ctx
        .get("provider")
        .and_then(|value| value.as_str())
        .map(str::to_string);
    let reference = ctx
        .get("provider_transaction_id")
        .and_then(|value| value.as_str())
        .map(str::to_string)
        .or_else(|| {
            ctx.get("transaction_id")
                .and_then(|value| value.as_str())
                .map(str::to_string)
        });
    let amount_minor = ctx.get("amount_minor").and_then(|value| value.as_i64());
    let currency = ctx
        .get("currency")
        .and_then(|value| value.as_str())
        .map(str::to_string);
    let Some(order) = state
        .orders
        .get(&order_id)
        .map(|entry| entry.value().clone())
    else {
        ctx.add_result(json!({
            "store": "skipped",
            "order_id": order_id,
            "reason": "order not found",
        }));
        return;
    };
    match order.status.as_str() {
        "paid" | "completed" => {
            ctx.add_result(json!({
                "store": "already_paid",
                "order_id": order_id,
                "status": order.status,
            }));
            return;
        }
        "refunded" | "cancelled" => {
            ctx.add_result(json!({
                "store": "skipped",
                "order_id": order_id,
                "reason": format!("order is {} and can never move to paid", order.status),
            }));
            return;
        }
        "pending" | "payment_pending" | "pending_verification" => {}
        other => {
            ctx.add_result(json!({
                "store": "skipped",
                "order_id": order_id,
                "reason": format!("order in status {other} cannot move to paid"),
            }));
            return;
        }
    }
    if provider.as_deref() != Some("bank_transfer") {
        // Fail closed: a capture without an amount or a currency
        // proves nothing about what was paid, so it never marks
        // the order paid. A missing amount used to skip this whole
        // check and mark the order paid on the event name alone.
        let (Some(amount), Some(event_currency)) = (amount_minor, &currency) else {
            ctx.add_result(json!({
                "store": "skipped",
                "order_id": order_id,
                "reason": "payment.captured carries no captured amount or currency; refusing to mark the order paid",
            }));
            return;
        };
        if !event_currency.eq_ignore_ascii_case(&order.currency) {
            ctx.add_result(json!({
                "store": "skipped",
                "order_id": order_id,
                "reason": format!(
                    "captured currency {event_currency} does not match order currency {}",
                    order.currency
                ),
            }));
            return;
        }
        if amount < order.total {
            ctx.add_result(json!({
                "store": "skipped",
                "order_id": order_id,
                "reason": "captured amount does not cover the order total (partial capture or split part)",
            }));
            return;
        }
    }
    if let Some(reference) = &reference {
        if let Some(mut stored) = state.orders.get_mut(&order_id) {
            stored.payment_reference = Some(reference.clone());
            if let Some(provider) = &provider {
                stored.provider = Some(provider.clone());
            }
            stored.updated_at = now();
        }
    }
    match transition_order(&state, &order_id, "paid") {
        Ok(updated) => {
            let order_value = serde_json::to_value(&updated).unwrap_or(Value::Null);
            state.emit("payment.completed", order_value.clone());
            state.emit("order.paid", order_paid_value(&updated));
            let subscription = activate_subscription_for_paid_order(&state, &updated);
            grant_digital_purchases_for_paid_order(&state, &updated);
            ctx.add_result(json!({
                "store": "paid",
                "order_id": order_id,
                "payment_reference": reference,
                "subscription_id": subscription.map(|subscription| subscription.id),
            }));
        }
        Err(error) => {
            ctx.add_result(json!({
                "store": "skipped",
                "order_id": order_id,
                "reason": error.message,
            }));
        }
    }
}

/// A completed refund (`payment.refunded` with a `refunded` status)
/// moves the matching order to `refunded`, suspends its licenses
/// through the transition, and emits `store.order.refunded` so
/// licensing and billing react. Pending refunds are never applied
/// here: while the refund is still pending the order stays in
/// `refund_pending` and only the refund-completion path finishes it.
/// Partial refunds leave the order paid. The refund identifiers are
/// forwarded on the emitted event so billing can deduplicate the
/// credit it records.
pub(crate) fn on_payment_refunded(state: Arc<StoreState>, ctx: &mut HookContext) {
    let order_id = ctx
        .get("order_id")
        .and_then(|value| value.as_str())
        .map(str::to_string)
        .or_else(|| {
            ctx.get("order")
                .and_then(|order| order.get("id"))
                .and_then(|id| id.as_str())
                .map(str::to_string)
        });
    let Some(order_id) = order_id else {
        ctx.add_result(json!({
            "store": "skipped",
            "reason": "payment.refunded carries no order id",
        }));
        return;
    };
    // Fail closed: without an explicit completed status the refund
    // must not move the order. Pending and failed refunds are
    // recorded, never applied.
    let status = ctx
        .get("status")
        .and_then(|value| value.as_str())
        .unwrap_or("");
    match status {
        "refunded" => {}
        "partially_refunded" => {
            ctx.add_result(json!({
                "store": "partial_refund",
                "order_id": order_id,
                "reason": "partial refund recorded; order stays paid",
            }));
            return;
        }
        other => {
            ctx.add_result(json!({
                "store": "skipped",
                "order_id": order_id,
                "reason": format!(
                    "refund status \"{other}\" is not a completed refund",
                ),
            }));
            return;
        }
    }
    let Some(order) = state
        .orders
        .get(&order_id)
        .map(|entry| entry.value().clone())
    else {
        ctx.add_result(json!({
            "store": "skipped",
            "order_id": order_id,
            "reason": "order not found",
        }));
        return;
    };
    if order.status == "refunded" {
        ctx.add_result(json!({
            "store": "already_refunded",
            "order_id": order_id,
        }));
        return;
    }
    if !matches!(
        order.status.as_str(),
        "paid" | "completed" | "refund_pending"
    ) {
        ctx.add_result(json!({
            "store": "skipped",
            "order_id": order_id,
            "reason": format!("order in status {} cannot be refunded", order.status),
        }));
        return;
    }
    match transition_order(&state, &order_id, "refunded") {
        Ok(updated) => {
            let license_ids: Vec<String> = state
                .issued_licenses
                .iter()
                .filter(|entry| entry.value().order_id.as_deref() == Some(order_id.as_str()))
                .map(|entry| entry.value().license_id.clone())
                .collect();
            let cancelled_subscriptions =
                cancel_subscriptions_for_order(&state, &order_id, "order_refunded");
            state.emit(
                "store.order.refunded",
                json!({
                    "order": serde_json::to_value(&updated).unwrap_or(Value::Null),
                    "order_id": updated.id,
                    "license_ids": license_ids,
                    "cancelled_subscriptions": cancelled_subscriptions,
                    "reason": ctx.get("reason").cloned().unwrap_or(Value::Null),
                    "provider_refund_id": ctx.get("provider_refund_id").cloned().unwrap_or(Value::Null),
                    "transaction_id": ctx.get("transaction_id").cloned().unwrap_or(Value::Null),
                    "amount_minor": ctx.get("amount_minor").cloned().unwrap_or(Value::Null),
                    "currency": ctx.get("currency").cloned().unwrap_or(Value::Null),
                }),
            );
            ctx.add_result(json!({
                "store": "refunded",
                "order_id": order_id,
            }));
        }
        Err(error) => {
            ctx.add_result(json!({
                "store": "skipped",
                "order_id": order_id,
                "reason": error.message,
            }));
        }
    }
}

/// A bank-transfer verification opened for an order moves it from
/// `pending` / `payment_pending` to `pending_verification`, so the
/// order visibly waits for manual verification.
pub(crate) fn on_bank_transfer_verification_created(state: Arc<StoreState>, ctx: &mut HookContext) {
    let Some(order_id) = ctx
        .get("order_id")
        .and_then(|value| value.as_str())
        .map(str::to_string)
    else {
        ctx.add_result(json!({
            "store": "skipped",
            "reason": "bank_transfer.verification.created carries no order id",
        }));
        return;
    };
    let Some(order) = state
        .orders
        .get(&order_id)
        .map(|entry| entry.value().clone())
    else {
        ctx.add_result(json!({
            "store": "skipped",
            "order_id": order_id,
            "reason": "order not found",
        }));
        return;
    };
    if order.status == "pending_verification" {
        ctx.add_result(json!({
            "store": "already_pending_verification",
            "order_id": order_id,
        }));
        return;
    }
    match transition_order(&state, &order_id, "pending_verification") {
        Ok(_) => {
            ctx.add_result(json!({
                "store": "pending_verification",
                "order_id": order_id,
            }));
        }
        Err(error) => {
            ctx.add_result(json!({
                "store": "skipped",
                "order_id": order_id,
                "reason": error.message,
            }));
        }
    }
}

/// A rejected bank transfer cancels the order. The bank-transfer
/// module's own note suggested returning the order to `pending`
/// instead; the prescribed fallback for that contradiction is
/// cancellation on rejection, so rejection cancels here, while
/// `pending_verification` to `pending` stays allowed in the status
/// machine for an administrator who wants to put the order back to
/// awaiting payment for a retry.
pub(crate) fn on_bank_transfer_rejected(state: Arc<StoreState>, ctx: &mut HookContext) {
    let Some(order_id) = ctx
        .get("order_id")
        .and_then(|value| value.as_str())
        .map(str::to_string)
    else {
        ctx.add_result(json!({
            "store": "skipped",
            "reason": "bank_transfer.rejected carries no order id",
        }));
        return;
    };
    let Some(order) = state
        .orders
        .get(&order_id)
        .map(|entry| entry.value().clone())
    else {
        ctx.add_result(json!({
            "store": "skipped",
            "order_id": order_id,
            "reason": "order not found",
        }));
        return;
    };
    if order.status == "cancelled" {
        ctx.add_result(json!({
            "store": "already_cancelled",
            "order_id": order_id,
        }));
        return;
    }
    match transition_order(&state, &order_id, "cancelled") {
        Ok(_) => {
            let order_value = state
                .orders
                .get(&order_id)
                .map(|entry| serde_json::to_value(entry.value().clone()).unwrap_or(Value::Null))
                .unwrap_or(Value::Null);
            state.emit("store.order.cancelled", order_value);
            ctx.add_result(json!({
                "store": "cancelled",
                "order_id": order_id,
            }));
        }
        Err(error) => {
            ctx.add_result(json!({
                "store": "skipped",
                "order_id": order_id,
                "reason": error.message,
            }));
        }
    }
}

pub(crate) const PRODUCT_LISTED: &str = "product.listed";
pub(crate) const PRODUCT_DELISTED: &str = "product.delisted";
pub(crate) const STORE_ENTITLEMENT_CHECK: &str = "store.entitlement.check";

/// Mirror a catalog listing into the store (`product.listed`).
///
/// The products module emits a `ProductDetail` with the product
/// flattened at top level: `id`, `slug`, `name`, `description`,
/// `kind`, `price: {amount_minor, currency}`, `pricing_model`,
/// `billing_period`, `renewal_price`, `trial_days`,
/// `vendor_company_id`, `software_id`, and
/// `license_template.requires_license`. The store listing is keyed
/// by the products product id, so orders, entitlements, and the
/// `store.entitlement.check` answer all line up on the same id.
/// A listing that already exists is refreshed in place (price,
/// name, and flags); its store-only data (tiers, file, slug
/// overrides) is kept.
///
/// Assumptions, recorded because the payload cannot supply them:
/// tier data is NOT in the payload (only `tier_count`), so tiers
/// are never mirrored here - a new listing starts with none and
/// sells at its base price. The store kind is kept verbatim from
/// the catalog: the store's own create/update validation only
/// knows `theme`/`plugin`/`software`, but checkout does not
/// depend on kind, and the catalog vocabulary (`digital`,
/// `meeting`, `hosting`, ...) is wider - the listing stays
/// truthful to its source. `pricing_model` values with no store
/// equivalent (`usage`, `per_seat`) fall back to `one_time`.
pub(crate) fn on_product_listed(state: Arc<StoreState>, ctx: &mut HookContext) {
    let product_id = ctx
        .get("id")
        .or_else(|| ctx.get("product_id"))
        .and_then(|value| value.as_str())
        .map(str::to_string);
    let Some(product_id) = product_id.filter(|id| !id.is_empty()) else {
        ctx.add_result(json!({
            "store": "product_listed",
            "skipped": true,
            "reason": "missing_product_id",
        }));
        return;
    };
    let name = ctx
        .get("name")
        .and_then(|value| value.as_str())
        .unwrap_or("")
        .trim()
        .to_string();
    if name.is_empty() {
        ctx.add_result(json!({
            "store": "product_listed",
            "product_id": product_id,
            "skipped": true,
            "reason": "missing_product_name",
        }));
        return;
    }
    let currency = ctx
        .get("price")
        .and_then(|value| value.get("currency"))
        .and_then(|value| value.as_str())
        .unwrap_or("USD")
        .to_uppercase();
    if !valid_currency(&currency) {
        ctx.add_result(json!({
            "store": "product_listed",
            "product_id": product_id,
            "skipped": true,
            "reason": "invalid_currency",
        }));
        return;
    }
    let amount_minor = ctx
        .get("price")
        .and_then(|value| value.get("amount_minor"))
        .and_then(|value| value.as_i64())
        .unwrap_or(0);
    if amount_minor < 0 {
        ctx.add_result(json!({
            "store": "product_listed",
            "product_id": product_id,
            "skipped": true,
            "reason": "negative_price",
        }));
        return;
    }
    let kind = ctx
        .get("kind")
        .and_then(|value| value.as_str())
        .unwrap_or("software")
        .to_string();
    let pricing_model = match ctx
        .get("pricing_model")
        .and_then(|value| value.as_str())
        .unwrap_or("one_time")
    {
        "subscription" => "subscription".to_string(),
        _ => "one_time".to_string(),
    };
    let billing_period = ctx
        .get("billing_period")
        .and_then(|value| value.as_str())
        .map(str::to_string);
    let renewal_price = ctx
        .get("renewal_price")
        .and_then(|value| value.get("amount_minor"))
        .and_then(|value| value.as_i64());
    let trial_days = ctx
        .get("trial_days")
        .and_then(|value| value.as_u64())
        .unwrap_or(0) as u32;
    let proration_policy = ctx
        .get("proration_policy")
        .and_then(|value| value.as_str())
        .filter(|policy| matches!(*policy, "prorate" | "no_prorate" | "credit_only"))
        .unwrap_or("prorate")
        .to_string();
    let grace_period_days = ctx
        .get("grace_period_days")
        .and_then(|value| value.as_u64())
        .unwrap_or(0) as u32;
    let dunning_max_retries = ctx
        .get("dunning_max_retries")
        .and_then(|value| value.as_u64())
        .unwrap_or(3) as u32;
    let dunning_retry_interval_days = ctx
        .get("dunning_retry_interval_days")
        .and_then(|value| value.as_u64())
        .unwrap_or(3) as u32;
    let software_id = ctx
        .get("software_id")
        .and_then(|value| value.as_str())
        .map(str::to_string);
    let requires_license = ctx
        .get("license_template")
        .and_then(|value| value.get("requires_license"))
        .and_then(|value| value.as_bool())
        .unwrap_or(false);
    let description = ctx
        .get("description")
        .and_then(|value| value.as_str())
        .unwrap_or("")
        .to_string();
    let slug = ctx
        .get("slug")
        .and_then(|value| value.as_str())
        .map(str::to_string);
    let company_id = ctx
        .get("vendor_company_id")
        .and_then(|value| value.as_str())
        .map(str::to_string);
    let license_product = requires_license || software_id.is_some();
    let created = !state.products.contains_key(&product_id);
    if let Some(mut existing) = state.products.get_mut(&product_id) {
        existing.name = name;
        existing.description = description;
        existing.price = amount_minor;
        existing.currency = currency;
        existing.kind = kind;
        if let Some(slug) = slug {
            existing.slug = Some(slug);
        }
        existing.software_id = software_id;
        existing.pricing_model = pricing_model;
        existing.billing_period = billing_period;
        existing.renewal_price = renewal_price;
        existing.trial_days = trial_days;
        existing.proration_policy = proration_policy;
        existing.grace_period_days = grace_period_days;
        existing.dunning_max_retries = dunning_max_retries;
        existing.dunning_retry_interval_days = dunning_retry_interval_days;
        existing.license_product = license_product;
        existing.company_id = company_id;
        existing.status = "published".to_string();
        existing.updated_at = now();
    } else {
        state.products.insert(
            product_id.clone(),
            Product {
                id: product_id.clone(),
                name,
                description,
                price: amount_minor,
                currency,
                kind,
                license_product,
                file_id: None,
                slug,
                software_id,
                software_slug: None,
                status: "published".to_string(),
                company_id,
                pricing_model,
                billing_period,
                renewal_price,
                trial_days,
                proration_policy,
                grace_period_days,
                dunning_max_retries,
                dunning_retry_interval_days,
                module_id: None,
                updates_years: None,
                created_at: now(),
                updated_at: now(),
            },
        );
    }
    ctx.add_result(json!({
        "store": "product_listed",
        "product_id": product_id,
        "created": created,
        "status": "published",
    }));
}

/// Retire a catalog listing from the store (`product.delisted`).
///
/// The listing moves to `draft` so it can no longer be bought,
/// while the record - and its order history - is kept. A later
/// `product.listed` republishes it. Nothing is deleted: deleting
/// would orphan the orders and entitlements that reference the
/// product id.
pub(crate) fn on_product_delisted(state: Arc<StoreState>, ctx: &mut HookContext) {
    let product_id = ctx
        .get("id")
        .or_else(|| ctx.get("product_id"))
        .and_then(|value| value.as_str())
        .map(str::to_string);
    let Some(product_id) = product_id.filter(|id| !id.is_empty()) else {
        ctx.add_result(json!({
            "store": "product_delisted",
            "skipped": true,
            "reason": "missing_product_id",
        }));
        return;
    };
    let Some(mut product) = state.products.get_mut(&product_id) else {
        ctx.add_result(json!({
            "store": "product_delisted",
            "product_id": product_id,
            "skipped": true,
            "reason": "not_listed",
        }));
        return;
    };
    if product.status == "draft" {
        ctx.add_result(json!({
            "store": "product_delisted",
            "product_id": product_id,
            "already_draft": true,
        }));
        return;
    }
    product.status = "draft".to_string();
    product.updated_at = now();
    ctx.add_result(json!({
        "store": "product_delisted",
        "product_id": product_id,
        "status": "draft",
    }));
}

/// Create a store order for a properties block purchase
/// (`properties.block.purchase_requested`
/// `{purchase_id, property_id, plan_id, block_id, buyer_id, price,
/// currency}`): one line item for the block at the payload's price,
/// buyer as the order owner, status `pending` - the block is NOT
/// marked sold here; the properties module owns that transition on
/// `payment.captured`. Emits `properties.order.created`
/// `{purchase_id, order_id, block_id, property_id, buyer_id,
/// total, currency}` so the properties module can link the order
/// id to the purchase record. A repeat request for the same
/// `purchase_id` returns the existing order instead of creating a
/// second one (the properties module emits the request once per
/// purchase, but a redelivery must never double-sell the block).
/// The block is not a store product, so the line carries the block
/// id as its `product_id` with a descriptive name; no catalogue
/// lookup is performed.
pub(crate) fn on_properties_purchase_requested(state: Arc<StoreState>, ctx: &mut HookContext) {
    let data = ctx.data.clone();
    let required = |key: &str| {
        data.get(key)
            .and_then(|value| value.as_str())
            .map(str::to_string)
            .filter(|value| !value.trim().is_empty())
    };
    let Some(purchase_id) = required("purchase_id") else {
        ctx.add_result(json!({
            "store": "skipped",
            "reason": "properties.block.purchase_requested carries no purchase_id",
        }));
        return;
    };
    let Some(block_id) = required("block_id") else {
        ctx.add_result(json!({
            "store": "skipped",
            "purchase_id": purchase_id,
            "reason": "properties.block.purchase_requested carries no block_id",
        }));
        return;
    };
    let Some(property_id) = required("property_id") else {
        ctx.add_result(json!({
            "store": "skipped",
            "purchase_id": purchase_id,
            "reason": "properties.block.purchase_requested carries no property_id",
        }));
        return;
    };
    let Some(buyer_id) = required("buyer_id") else {
        ctx.add_result(json!({
            "store": "skipped",
            "purchase_id": purchase_id,
            "reason": "properties.block.purchase_requested carries no buyer_id",
        }));
        return;
    };
    let price = data.get("price").and_then(|value| value.as_f64());
    let Some(price) = price.filter(|value| value.is_finite() && *value >= 0.0) else {
        ctx.add_result(json!({
            "store": "skipped",
            "purchase_id": purchase_id,
            "reason": "properties.block.purchase_requested carries no valid price",
        }));
        return;
    };
    let currency = data
        .get("currency")
        .and_then(|value| value.as_str())
        .unwrap_or("USD")
        .to_string();
    if currency.len() != 3 || !currency.chars().all(|c| c.is_ascii_alphabetic()) {
        ctx.add_result(json!({
            "store": "skipped",
            "purchase_id": purchase_id,
            "reason": "properties.block.purchase_requested currency must be a 3-letter code",
        }));
        return;
    }
    // Idempotency: orders carry no purchase-link field, so a
    // repeat request is detected by the block line - a pending
    // order owned by this buyer with this block as its only line.
    // That is the same purchase by construction (one block sells
    // once), so the existing order is returned instead of a
    // second one.
    if let Some(existing) = state
        .orders
        .iter()
        .find(|entry| {
            let order = entry.value();
            order.user_id == buyer_id
                && order.status == "pending"
                && order.items.len() == 1
                && order.items[0].product_id == block_id
        })
        .map(|entry| entry.value().clone())
    {
        ctx.add_result(json!({
            "store": "order_already_created",
            "purchase_id": purchase_id,
            "order_id": existing.id,
            "block_id": block_id,
        }));
        return;
    }
    let Some(total) = major_to_minor(price, &currency) else {
        ctx.add_result(json!({
            "store": "skipped",
            "purchase_id": purchase_id,
            "reason": "properties.block.purchase_requested carries an out-of-range price",
        }));
        return;
    };
    let item = OrderItem {
        product_id: block_id.clone(),
        name: format!("Property block {block_id}"),
        unit_price: total,
        quantity: 1,
        line_total: total,
        license_product: false,
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
        download_limit: 0,
        course_id: None,
        exam_id: None,
        trial_days: 0,
        proration_policy: default_proration_policy(),
        grace_period_days: 0,
        dunning_max_retries: default_dunning_max_retries(),
        dunning_retry_interval_days: default_dunning_retry_interval_days(),
        module_id: Some("properties".to_string()),
    };
    let items = vec![item];
    let order = Order {
        id: new_id(),
        reference: new_order_reference(&state, &items),
        user_id: buyer_id.clone(),
        items,
        subtotal: total,
        discount_total: 0,
        total,
        currency: currency.to_uppercase(),
        coupon_code: None,
        discount_evaluation: None,
        company_id: None,
        trial_until: None,
        status: "pending".to_string(),
        provider: None,
        payment_reference: None,
        created_at: now(),
        updated_at: now(),
    };
    state.orders.insert(order.id.clone(), order.clone());
    state.emit(
        "properties.order.created",
        json!({
            "purchase_id": purchase_id,
            "order_id": order.id,
            "block_id": block_id,
            "property_id": property_id,
            "buyer_id": buyer_id,
            "total": total,
            "currency": currency.to_uppercase(),
        }),
    );
    ctx.add_result(json!({
        "store": "order_created",
        "purchase_id": purchase_id,
        "order_id": order.id,
        "block_id": block_id,
        "property_id": property_id,
        "total": total,
        "currency": currency.to_uppercase(),
    }));
}

/// Answer `store.entitlement.check` with payload
/// `{"product_id", "user_id"}`.
///
/// Entitled when the user holds an active entitlement for the
/// product - the same `DownloadEntitlement` records the store
/// entitlements list endpoint reads (status `active` and not past
/// `expires_at`) - or an active issued license for it. Issued
/// licenses carry the product NAME, not its id, so the license
/// fallback resolves the name through the store product. The
/// answer carries both `"entitled": bool` and an `entitlements`
/// array with the matching active records, because the products
/// module accepts either shape. This closes the loop for
/// `product.entitlement.check` (products) and the meetings join
/// check, which previously failed closed with
/// `no_entitlement_source`.
pub(crate) fn on_store_entitlement_check(state: Arc<StoreState>, ctx: &mut HookContext) {
    let product_id = ctx
        .get("product_id")
        .and_then(|value| value.as_str())
        .unwrap_or("")
        .to_string();
    let user_id = ctx
        .get("user_id")
        .and_then(|value| value.as_str())
        .unwrap_or("")
        .to_string();
    if product_id.is_empty() || user_id.is_empty() {
        ctx.add_result(json!({
            "entitled": false,
            "product_id": product_id,
            "user_id": user_id,
            "entitlements": [],
            "reason": "missing_product_id_or_user_id",
        }));
        return;
    }
    let now = now();
    let mut matches: Vec<Value> = state
        .entitlements
        .iter()
        .filter(|entry| {
            let entitlement = entry.value();
            entitlement.product_id == product_id
                && entitlement.user_id == user_id
                && entitlement.status == "active"
                && entitlement.expires_at.is_none_or(|expiry| expiry > now)
        })
        .map(|entry| {
            let entitlement = entry.value();
            json!({
                "id": entitlement.id,
                "source": "entitlement",
                "status": "active",
                "expires_at": entitlement.expires_at,
            })
        })
        .collect();
    if let Some(product) = state.products.get(&product_id) {
        let product_name = product.name.clone();
        drop(product);
        for entry in state.issued_licenses.iter() {
            let license = entry.value();
            if license.user_id.as_deref() == Some(user_id.as_str())
                && license.status == "active"
                && license.product == product_name
            {
                matches.push(json!({
                    "id": license.license_id,
                    "source": "license",
                    "status": "active",
                }));
            }
        }
    }
    // An active subscription to the product also entitles the user,
    // for as long as it grants access (trialing/active inside the
    // period, past-due inside grace, cancelled at period end inside
    // the period).
    for entry in state.subscriptions.iter() {
        let subscription = entry.value();
        if subscription.product_id == product_id
            && subscription.user_id == user_id
            && subscription_grants_access(subscription, now)
        {
            matches.push(json!({
                "id": subscription.id,
                "source": "subscription",
                "status": subscription.status,
                "expires_at": subscription.current_period_end,
            }));
        }
    }
    let entitled = !matches.is_empty();
    ctx.add_result(json!({
        "entitled": entitled,
        "product_id": product_id,
        "user_id": user_id,
        "entitlements": matches,
        "reason": if entitled { "entitled" } else { "no_active_entitlement" },
    }));
}

#[cfg(test)]
mod tests {
    use super::*;
    use unified_core::hooks::HookContext;

    fn order_with_total(total: i64) -> Order {
        let timestamp = now();
        Order {
            id: "ord-1".to_string(),
            reference: "ORD-1".to_string(),
            user_id: "user-1".to_string(),
            items: Vec::new(),
            subtotal: total,
            discount_total: 0,
            total,
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

    fn capture_ctx(payload: Value) -> HookContext {
        HookContext::new("payment.captured", payload)
    }

    fn store_result(ctx: &HookContext) -> Option<&str> {
        ctx.results
            .first()
            .and_then(|value| value.get("store"))
            .and_then(|value| value.as_str())
    }

    #[test]
    fn payment_captured_without_amount_or_currency_is_refused() {
        for payload in [
            json!({ "order_id": "ord-1", "provider": "stripe", "status": "captured" }),
            json!({ "order_id": "ord-1", "provider": "stripe", "status": "captured", "amount_minor": 1000 }),
            json!({ "order_id": "ord-1", "provider": "stripe", "status": "captured", "currency": "USD" }),
        ] {
            let state = Arc::new(StoreState::new());
            state.orders.insert("ord-1".to_string(), order_with_total(1000));
            let mut ctx = capture_ctx(payload);
            on_payment_captured(state.clone(), &mut ctx);
            assert_eq!(store_result(&ctx), Some("skipped"));
            assert_eq!(
                state.orders.get("ord-1").map(|entry| entry.status.clone()),
                Some("pending".to_string()),
                "an unverified capture must never mark the order paid"
            );
        }
    }

    #[test]
    fn payment_captured_with_covering_amount_marks_paid() {
        let state = Arc::new(StoreState::new());
        state.orders.insert("ord-1".to_string(), order_with_total(1000));
        let mut ctx = capture_ctx(json!({
            "order_id": "ord-1",
            "provider": "stripe",
            "status": "captured",
            "amount_minor": 1000,
            "currency": "USD",
            "provider_transaction_id": "txn-1",
        }));
        on_payment_captured(state.clone(), &mut ctx);
        assert_eq!(store_result(&ctx), Some("paid"));
        assert_eq!(
            state.orders.get("ord-1").map(|entry| entry.status.clone()),
            Some("paid".to_string())
        );
    }

    #[test]
    fn payment_captured_partial_amount_is_refused() {
        let state = Arc::new(StoreState::new());
        state.orders.insert("ord-1".to_string(), order_with_total(1000));
        let mut ctx = capture_ctx(json!({
            "order_id": "ord-1",
            "provider": "stripe",
            "status": "captured",
            "amount_minor": 999,
            "currency": "USD",
        }));
        on_payment_captured(state.clone(), &mut ctx);
        assert_eq!(store_result(&ctx), Some("skipped"));
        assert_eq!(
            state.orders.get("ord-1").map(|entry| entry.status.clone()),
            Some("pending".to_string())
        );
    }

    #[test]
    fn bank_transfer_capture_skips_the_amount_check_by_design() {
        let state = Arc::new(StoreState::new());
        state.orders.insert("ord-1".to_string(), order_with_total(1000));
        let mut ctx = capture_ctx(json!({
            "order_id": "ord-1",
            "provider": "bank_transfer",
            "status": "captured",
        }));
        on_payment_captured(state.clone(), &mut ctx);
        assert_eq!(store_result(&ctx), Some("paid"));
    }

    #[test]
    fn order_paid_value_hoists_a_single_exam_id() {
        let timestamp = now();
        let item = OrderItem {
            product_id: "prod-1".to_string(),
            name: "Certification Exam".to_string(),
            unit_price: 9900,
            quantity: 1,
            line_total: 9900,
            license_product: false,
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
            download_limit: 0,
            course_id: None,
            exam_id: Some("exam-1".to_string()),
            trial_days: 0,
            proration_policy: default_proration_policy(),
            grace_period_days: 0,
            dunning_max_retries: default_dunning_max_retries(),
            dunning_retry_interval_days: default_dunning_retry_interval_days(),
            module_id: None,
        };
        let mut order = order_with_total(9900);
        order.items = vec![item];
        let value = order_paid_value(&order);
        assert_eq!(
            value.get("exam_id").and_then(|id| id.as_str()),
            Some("exam-1")
        );
        assert_eq!(
            value.get("module").and_then(|module| module.as_str()),
            Some("exams")
        );
        let items = value.get("items").and_then(|items| items.as_array()).unwrap();
        assert_eq!(
            items[0].get("module").and_then(|module| module.as_str()),
            Some("exams")
        );
        assert_eq!(
            items[0].get("exam_id").and_then(|id| id.as_str()),
            Some("exam-1")
        );
    }
}
