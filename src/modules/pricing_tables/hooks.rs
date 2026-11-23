//! pricing tables - hooks.

use unified_core::hooks::HookContext;
use serde_json::{json, Value};
use std::sync::Arc;
use super::types::*;
use super::state::*;
use super::validation::*;

/// Compute commissions for one paid order. Shared by the `order.paid`
/// and `store.order.completed` handlers: the payload is the Store
/// module's Order value, so each line carries `product_id`,
/// `line_total` (falling back to `unit_price * quantity`), and
/// `company_id`. Lines for the same product (and company) in one
/// order are aggregated into a single commission on their summed
/// line total, so a second line for the same product is paid instead
/// of being dropped. Idempotent per (order, product, vendor): a
/// commission already recorded for the same order + product + vendor
/// is never computed twice, so an order that emits both events
/// yields one ledger entry per product per vendor.
pub(crate) fn on_order_paid(state: Arc<PricingTablesState>, ctx: &mut HookContext) {
    let data = ctx.data.clone();
    let Some(order_id) = data.get("id").and_then(|v| v.as_str()) else {
        return;
    };
    let order_id = order_id.to_string();
    let currency = data
        .get("currency")
        .and_then(|v| v.as_str())
        .unwrap_or("USD")
        .to_string();
    let Some(items) = data.get("items").and_then(|v| v.as_array()) else {
        return;
    };
    // Aggregate the order's lines by (product, company): one order
    // may carry two lines for the same product, and each line's
    // value must earn its commission. Line totals are minor units;
    // the store payload carries major units.
    let mut groups: Vec<(String, Option<String>, i64)> = Vec::new();
    for item in items {
        let Some(product_id) = item.get("product_id").and_then(|v| v.as_str()) else {
            continue;
        };
        let line_total = item
            .get("line_total")
            .and_then(|v| v.as_f64())
            .map(|major| major_to_minor(major, &currency))
            .or_else(|| {
                let unit = item.get("unit_price").and_then(|v| v.as_f64())?;
                let quantity = item.get("quantity").and_then(|v| v.as_u64())? as f64;
                Some(major_to_minor(unit * quantity, &currency))
            })
            .unwrap_or(0);
        if line_total < 0 {
            continue;
        };
        let company_id = item
            .get("company_id")
            .and_then(|v| v.as_str())
            .map(str::to_string);
        if let Some(group) = groups.iter_mut().find(|(id, company, _)| {
            id.as_str() == product_id && *company == company_id
        }) {
            group.2 += line_total;
        } else {
            groups.push((product_id.to_string(), company_id, line_total));
        }
    }
    for (product_id, company_id, line_total) in groups {
        let Some((vendor_id, category)) =
            state.vendor_for_order_line(&product_id, company_id.as_deref())
        else {
            continue;
        };
        if state.commissions.iter().any(|entry| {
            entry.value().order_id == order_id
                && entry.value().product_id == product_id
                && entry.value().vendor_id == vendor_id
        }) {
            continue;
        }
        let rate = state.commission_rate_for(&vendor_id, category.as_deref());
        let amount = apply_percent_to_minor(line_total, rate);
        let commission = Commission {
            id: new_id(),
            order_id: order_id.clone(),
            vendor_id: vendor_id.clone(),
            product_id: product_id.clone(),
            order_total: line_total,
            rate_percent: rate,
            commission_amount: amount,
            paid_amount: 0,
            currency: currency.clone(),
            status: "pending".to_string(),
            created_at: now(),
            updated_at: now(),
        };
        state
            .commissions
            .insert(commission.id.clone(), commission.clone());
        state.emit(
            "marketplace.commission_computed",
            json!({
                "commission_id": commission.id,
                "order_id": commission.order_id,
                "vendor_id": commission.vendor_id,
                "product_id": commission.product_id,
                "order_total": minor_to_major(commission.order_total, &commission.currency),
                "rate_percent": commission.rate_percent,
                "commission_amount": minor_to_major(commission.commission_amount, &commission.currency),
                "currency": commission.currency,
                "status": "pending",
            }),
        );
    }
}

/// What a `store.order.refunded` payload establishes about the
/// refund's scope. The Store module never emits this event for a
/// partial refund through the payment path (a partial refund leaves
/// the order paid), but the payload may still carry a partial
/// amount: the gateway path forwards `amount_minor`, while the
/// manual paths carry no amount at all and move the whole order to
/// `refunded`. Only a refund known to cover the whole order may
/// reverse commissions.
enum RefundScope {
    /// The refund covers the whole order.
    Full,
    /// A refunded amount is known and is smaller than the order total.
    /// Amounts are minor units; `currency` is the display currency
    /// for the review payload (major units on the wire).
    Partial {
        refunded_amount: i64,
        order_total: i64,
        currency: String,
    },
    /// Fullness cannot be established from the payload.
    Unknown { reason: &'static str },
}

/// Read the refund scope from a `store.order.refunded` payload. The
/// refunded amount is read from `amount_minor` (minor units, as the
/// gateway path forwards it) or from a major-units
/// `refunded_amount` / `refund_amount` / `amount` field. When an
/// amount and the order total are both known, the refund is full
/// only when the amount covers the total; otherwise the nested
/// order's `refunded` status is what marks a full refund (the
/// manual paths move the whole order to `refunded` and carry no
/// amount). Anything else is unknown: the event alone cannot prove
/// the order was refunded in full.
fn refund_scope(data: &Value) -> RefundScope {
    let order = data.get("order");
    let order_status = order
        .and_then(|value| value.get("status"))
        .and_then(|value| value.as_str());
    let order_total = order
        .and_then(|value| value.get("total"))
        .and_then(|value| value.as_f64());
    let order_currency = order
        .and_then(|value| value.get("currency"))
        .and_then(|value| value.as_str());
    let event_currency = data.get("currency").and_then(|value| value.as_str());
    let currency = event_currency
        .or(order_currency)
        .unwrap_or("USD")
        .to_string();
    // `amount_minor` is already minor units; the major-unit fields
    // convert currency-aware (the old code divided `amount_minor`
    // by 100 unconditionally, which was wrong for zero-decimal
    // currencies).
    let refunded_amount = data
        .get("amount_minor")
        .and_then(|value| value.as_i64())
        .or_else(|| {
            data.get("refunded_amount")
                .and_then(|value| value.as_f64())
                .map(|major| major_to_minor(major, &currency))
        })
        .or_else(|| {
            data.get("refund_amount")
                .and_then(|value| value.as_f64())
                .map(|major| major_to_minor(major, &currency))
        })
        .or_else(|| {
            data.get("amount")
                .and_then(|value| value.as_f64())
                .map(|major| major_to_minor(major, &currency))
        });
    // An amount is only comparable to the order total when both are
    // in the same currency (or the currency is unknown on either side).
    let comparable = event_currency.is_none()
        || order_currency.is_none()
        || event_currency == order_currency;
    if let (Some(amount), Some(total)) = (refunded_amount, order_total) {
        if comparable {
            let total_minor = major_to_minor(total, &currency);
            // Minor-unit comparison is exact; the old 0.000001
            // epsilon only papered over 4-decimal float rounding.
            if amount >= total_minor {
                return RefundScope::Full;
            }
            return RefundScope::Partial {
                refunded_amount: amount,
                order_total: total_minor,
                currency,
            };
        }
    }
    if order_status == Some("refunded") {
        return RefundScope::Full;
    }
    RefundScope::Unknown {
        reason: if order_status.is_some() {
            "order status is not refunded and no full-refund amount was carried"
        } else {
            "payload carries neither a refunded amount nor an order with refunded status"
        },
    }
}

/// On `store.order.refunded`, reverse the order's pending
/// commissions -- but only when the refund is known to cover the
/// whole order. A partial refund reverses nothing: the commissions
/// stay pending and a single `partial_refund_review` event carries
/// the refunded amount, so billing can reconcile the clawback
/// instead of this module silently reversing money the vendor may
/// still have earned. A refund whose scope cannot be established
/// is reported the same way (`refund_review`) and also reverses
/// nothing. Commissions already paid out are left untouched and
/// reported in a single `paid_reversal_review` event, so billing
/// can reconcile the clawback instead of this module silently
/// rewriting paid money. A partially settled commission reverses
/// only its unpaid remainder; the amount already paid out stays paid.
pub(crate) fn on_order_refunded(state: Arc<PricingTablesState>, ctx: &mut HookContext) {
    let data = ctx.data.clone();
    // The Store module emits this event with the order nested under
    // `order` (plus `order_id` on most paths); a flat `id` is only
    // ever present on payloads that inline the order itself.
    let Some(order_id) = data
        .get("id")
        .and_then(|v| v.as_str())
        .or_else(|| data.get("order_id").and_then(|v| v.as_str()))
        .or_else(|| {
            data.get("order")
                .and_then(|order| order.get("id"))
                .and_then(|v| v.as_str())
        })
        .map(str::to_string)
    else {
        return;
    };
    let pending_ids = |state: &PricingTablesState| -> Vec<String> {
        state
            .commissions
            .iter()
            .filter(|entry| {
                entry.value().order_id == order_id && entry.value().status == "pending"
            })
            .map(|entry| entry.key().clone())
            .collect()
    };
    match refund_scope(&data) {
        RefundScope::Partial {
            refunded_amount,
            order_total,
            currency,
        } => {
            let ids = pending_ids(&state);
            if !ids.is_empty() {
                state.emit(
                    "marketplace.commission_computed",
                    json!({
                        "order_id": order_id,
                        "status": "partial_refund_review",
                        "refunded_amount": minor_to_major(refunded_amount, &currency),
                        "order_total": minor_to_major(order_total, &currency),
                        "currency": currency,
                        "pending_commission_ids": ids,
                    }),
                );
            }
        }
        RefundScope::Unknown { reason } => {
            let ids = pending_ids(&state);
            if !ids.is_empty() {
                state.emit(
                    "marketplace.commission_computed",
                    json!({
                        "order_id": order_id,
                        "status": "refund_review",
                        "reason": reason,
                        "pending_commission_ids": ids,
                    }),
                );
            }
        }
        RefundScope::Full => {
            let mut paid_ids: Vec<String> = Vec::new();
            let mut reversed: Vec<Commission> = Vec::new();
            for mut entry in state.commissions.iter_mut() {
                if entry.value().order_id != order_id {
                    continue;
                }
                match entry.value().status.as_str() {
                    "pending" => {
                        entry.value_mut().status = "reversed".to_string();
                        entry.value_mut().updated_at = now();
                        reversed.push(entry.value().clone());
                    }
                    "paid" => paid_ids.push(entry.key().clone()),
                    _ => {}
                }
            }
            for commission in reversed {
                let settled = commission
                    .paid_amount
                    .clamp(0, commission.commission_amount);
                state.emit(
                    "marketplace.commission_computed",
                    json!({
                        "commission_id": commission.id,
                        "order_id": commission.order_id,
                        "vendor_id": commission.vendor_id,
                        "product_id": commission.product_id,
                        "order_total": minor_to_major(commission.order_total, &commission.currency),
                        "rate_percent": commission.rate_percent,
                        "commission_amount": minor_to_major(commission.commission_amount, &commission.currency),
                        "paid_amount": minor_to_major(settled, &commission.currency),
                        "reversed_amount": minor_to_major(commission.commission_amount - settled, &commission.currency),
                        "currency": commission.currency,
                        "status": "reversed",
                    }),
                );
            }
            if !paid_ids.is_empty() {
                state.emit(
                    "marketplace.commission_computed",
                    json!({
                        "order_id": order_id,
                        "status": "paid_reversal_review",
                        "paid_commission_ids": paid_ids,
                    }),
                );
            }
        }
    }
}

/// On `invoice.created` (Billing), attach the invoice id to the
/// payout the invoice was raised for, when the payload names one;
/// otherwise keep the invoice reference in the reconciliation log.
pub(crate) fn on_invoice_created(state: Arc<PricingTablesState>, ctx: &mut HookContext) {
    let data = ctx.data.clone();
    let invoice_id = data
        .get("id")
        .or_else(|| data.get("invoice_id"))
        .or_else(|| data.get("number"))
        .and_then(|v| v.as_str())
        .map(str::to_string);
    let payout_id = data
        .get("payout_id")
        .or_else(|| data.get("metadata").and_then(|m| m.get("payout_id")))
        .and_then(|v| v.as_str())
        .map(str::to_string);
    if let (Some(invoice_id), Some(payout_id)) = (invoice_id.clone(), payout_id) {
        if let Some(mut payout) = state.payouts.get_mut(payout_id.as_str()) {
            payout.value_mut().invoice_id = Some(invoice_id.clone());
            payout.value_mut().updated_at = now();
        }
    }
    if let Some(invoice_id) = invoice_id {
        state.invoice_refs.insert(invoice_id, data);
    }
}

/// Answer `pricing_table.render_requested` with the render data for
/// a published table. Draft tables do not answer: a page must not
/// render unpublished pricing.
pub(crate) fn on_render_requested(state: Arc<PricingTablesState>, ctx: &mut HookContext) {
    let Some(table_id) = ctx.get("table_id").and_then(|v| v.as_str()) else {
        return;
    };
    let Some(table) = state
        .tables
        .get(table_id)
        .map(|entry| entry.value().clone())
    else {
        return;
    };
    if table.status != "published" {
        return;
    }
    let render = state.render_data(&table);
    let value = serde_json::to_value(&render).unwrap_or(Value::Null);
    ctx.add_result(value);
}

/// Answer `marketplace.vendor_check` with the vendor id and status
/// for a user, when the user owns a vendor record.
pub(crate) fn on_vendor_check(state: Arc<PricingTablesState>, ctx: &mut HookContext) {
    let Some(user_id) = ctx.get("user_id").and_then(|v| v.as_str()) else {
        return;
    };
    if let Some(vendor) = state
        .vendors
        .iter()
        .find(|entry| entry.value().user_id == user_id)
        .map(|entry| entry.value().clone())
    {
        ctx.add_result(json!({
            "user_id": user_id,
            "vendor_id": vendor.id,
            "status": vendor.status,
            "company_name": vendor.company_name,
            "slug": vendor.slug,
        }));
    }
}

/// Answer `marketplace.commission_quote` with the applicable rate and
/// the computed commission for an amount, without writing anything.
/// Resolution is vendor rule, then category rule, then global rule,
/// then the built-in default -- the same order the paid-order
/// handler uses.
pub(crate) fn on_commission_quote(state: Arc<PricingTablesState>, ctx: &mut HookContext) {
    let Some(amount) = ctx.get("amount").and_then(|v| v.as_f64()) else {
        return;
    };
    if !amount.is_finite() || amount < 0.0 {
        return;
    }
    // The quote request carries major units; the answer echoes major
    // units, with the commission computed from minor units.
    let currency = ctx
        .get("currency")
        .and_then(|v| v.as_str())
        .unwrap_or("USD");
    let amount_minor = major_to_minor(amount, currency);
    let vendor_id = ctx.get("vendor_id").and_then(|v| v.as_str());
    let category = ctx.get("category").and_then(|v| v.as_str());
    let rate = match vendor_id {
        Some(vendor_id) => state.commission_rate_for(vendor_id, category),
        None => {
            if let Some(category) = category {
                state
                    .commission_rules
                    .iter()
                    .find(|entry| {
                        entry.value().scope == "category"
                            && entry.value().scope_value == category
                    })
                    .map(|entry| entry.value().rate_percent)
                    .or_else(|| {
                        state
                            .commission_rules
                            .iter()
                            .find(|entry| {
                                entry.value().scope == "global"
                                    && entry.value().scope_value.is_empty()
                            })
                            .map(|entry| entry.value().rate_percent)
                    })
                    .unwrap_or(DEFAULT_COMMISSION_RATE_PERCENT)
            } else {
                state.commission_rate_for("", None)
            }
        }
    };
    ctx.add_result(json!({
        "amount": minor_to_major(amount_minor, currency),
        "rate_percent": rate,
        "commission_amount": minor_to_major(apply_percent_to_minor(amount_minor, rate), currency),
    }));
}

/// Answer `marketplace.product_approval_status` with the approval
/// status for a product, when an approval record exists.
pub(crate) fn on_product_approval_status(state: Arc<PricingTablesState>, ctx: &mut HookContext) {
    let Some(product_id) = ctx.get("product_id").and_then(|v| v.as_str()) else {
        return;
    };
    if let Some(approval) = state
        .product_approvals
        .iter()
        .find(|entry| entry.value().product_id == product_id)
        .map(|entry| entry.value().clone())
    {
        ctx.add_result(json!({
            "product_id": approval.product_id,
            "vendor_id": approval.vendor_id,
            "approval_id": approval.id,
            "status": approval.status,
        }));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn test_vendor(id: &str, company_id: &str) -> Vendor {
        Vendor {
            id: id.to_string(),
            user_id: format!("u-{id}"),
            company_id: Some(company_id.to_string()),
            company_name: "Acme".to_string(),
            slug: format!("acme-{id}"),
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
        order_id: &str,
        amount: i64,
        paid_amount: i64,
        status: &str,
    ) -> Commission {
        Commission {
            id: id.to_string(),
            order_id: order_id.to_string(),
            vendor_id: "v-1".to_string(),
            product_id: "p-1".to_string(),
            order_total: amount * 10,
            rate_percent: 10.0,
            commission_amount: amount,
            paid_amount,
            currency: "USD".to_string(),
            status: status.to_string(),
            created_at: now(),
            updated_at: now(),
        }
    }

    fn paid_order_ctx(items: Value) -> HookContext {
        HookContext::new(
            "order.paid",
            json!({
                "id": "ord-1",
                "currency": "USD",
                "items": items,
            }),
        )
    }

    fn commissions_for(state: &PricingTablesState, order_id: &str) -> Vec<Commission> {
        state
            .commissions
            .iter()
            .filter(|entry| entry.value().order_id == order_id)
            .map(|entry| entry.value().clone())
            .collect()
    }

    #[test]
    fn duplicate_product_lines_aggregate_into_one_commission() {
        let state = Arc::new(PricingTablesState::new());
        state
            .vendors
            .insert("v-1".to_string(), test_vendor("v-1", "co-1"));
        let mut ctx = paid_order_ctx(json!([
            {"product_id": "p-1", "line_total": 100.0, "company_id": "co-1"},
            {"product_id": "p-1", "line_total": 50.0, "company_id": "co-1"},
        ]));
        on_order_paid(state.clone(), &mut ctx);
        let commissions = commissions_for(&state, "ord-1");
        assert_eq!(commissions.len(), 1);
        assert_eq!(commissions[0].order_total, 15000);
        assert_eq!(commissions[0].commission_amount, 1500);
    }

    #[test]
    fn redelivered_paid_order_computes_no_duplicate_commission() {
        let state = Arc::new(PricingTablesState::new());
        state
            .vendors
            .insert("v-1".to_string(), test_vendor("v-1", "co-1"));
        let items = json!([
            {"product_id": "p-1", "line_total": 100.0, "company_id": "co-1"},
            {"product_id": "p-1", "line_total": 50.0, "company_id": "co-1"},
        ]);
        let mut first = paid_order_ctx(items.clone());
        on_order_paid(state.clone(), &mut first);
        // Second delivery of the same order (the other paid event).
        let mut second = paid_order_ctx(items);
        on_order_paid(state.clone(), &mut second);
        assert_eq!(commissions_for(&state, "ord-1").len(), 1);
    }

    #[test]
    fn same_product_for_two_companies_earns_two_commissions() {
        let state = Arc::new(PricingTablesState::new());
        state
            .vendors
            .insert("v-1".to_string(), test_vendor("v-1", "co-1"));
        state
            .vendors
            .insert("v-2".to_string(), test_vendor("v-2", "co-2"));
        let mut ctx = paid_order_ctx(json!([
            {"product_id": "p-1", "line_total": 100.0, "company_id": "co-1"},
            {"product_id": "p-1", "line_total": 100.0, "company_id": "co-2"},
        ]));
        on_order_paid(state.clone(), &mut ctx);
        let commissions = commissions_for(&state, "ord-1");
        assert_eq!(commissions.len(), 2);
        assert!(commissions.iter().any(|c| c.vendor_id == "v-1"));
        assert!(commissions.iter().any(|c| c.vendor_id == "v-2"));
    }

    fn refund_ctx(payload: Value) -> HookContext {
        HookContext::new("store.order.refunded", payload)
    }

    fn refunded_order_payload() -> Value {
        // The shape the Store module's manual refund paths emit:
        // the order nested under `order`, no top-level id.
        json!({
            "order": {
                "id": "ord-1",
                "status": "refunded",
                "total": 100.0,
                "currency": "USD",
            },
            "license_ids": [],
        })
    }

    #[test]
    fn full_refund_reverses_pending_commissions() {
        let state = Arc::new(PricingTablesState::new());
        state.commissions.insert(
            "c-1".to_string(),
            test_commission("c-1", "ord-1", 1000, 0, "pending"),
        );
        let mut ctx = refund_ctx(refunded_order_payload());
        on_order_refunded(state.clone(), &mut ctx);
        assert_eq!(
            state.commissions.get("c-1").unwrap().status,
            "reversed"
        );
    }

    #[test]
    fn partial_refund_reverses_nothing() {
        let state = Arc::new(PricingTablesState::new());
        state.commissions.insert(
            "c-1".to_string(),
            test_commission("c-1", "ord-1", 1000, 0, "pending"),
        );
        // Gateway path: amount_minor 4000 = 40.00 against a 100.00 order.
        let mut payload = refunded_order_payload();
        payload["amount_minor"] = json!(4000);
        payload["currency"] = json!("USD");
        payload["order_id"] = json!("ord-1");
        let mut ctx = refund_ctx(payload);
        on_order_refunded(state.clone(), &mut ctx);
        assert_eq!(state.commissions.get("c-1").unwrap().status, "pending");
    }

    #[test]
    fn refund_covering_the_full_amount_reverses() {
        let state = Arc::new(PricingTablesState::new());
        state.commissions.insert(
            "c-1".to_string(),
            test_commission("c-1", "ord-1", 1000, 0, "pending"),
        );
        let mut payload = refunded_order_payload();
        payload["amount_minor"] = json!(10000);
        payload["currency"] = json!("USD");
        payload["order_id"] = json!("ord-1");
        let mut ctx = refund_ctx(payload);
        on_order_refunded(state.clone(), &mut ctx);
        assert_eq!(
            state.commissions.get("c-1").unwrap().status,
            "reversed"
        );
    }

    #[test]
    fn refund_without_amount_or_status_reverses_nothing() {
        let state = Arc::new(PricingTablesState::new());
        state.commissions.insert(
            "c-1".to_string(),
            test_commission("c-1", "ord-1", 1000, 0, "pending"),
        );
        let mut ctx = refund_ctx(json!({ "order_id": "ord-1" }));
        on_order_refunded(state.clone(), &mut ctx);
        assert_eq!(state.commissions.get("c-1").unwrap().status, "pending");
    }

    #[test]
    fn refund_for_unrefunded_order_status_reverses_nothing() {
        let state = Arc::new(PricingTablesState::new());
        state.commissions.insert(
            "c-1".to_string(),
            test_commission("c-1", "ord-1", 1000, 0, "pending"),
        );
        let mut payload = refunded_order_payload();
        payload["order"]["status"] = json!("paid");
        let mut ctx = refund_ctx(payload);
        on_order_refunded(state.clone(), &mut ctx);
        assert_eq!(state.commissions.get("c-1").unwrap().status, "pending");
    }

    #[test]
    fn full_refund_reverses_only_the_unpaid_remainder() {
        let state = Arc::new(PricingTablesState::new());
        state.commissions.insert(
            "c-paid".to_string(),
            test_commission("c-paid", "ord-1", 5000, 5000, "paid"),
        );
        state.commissions.insert(
            "c-partial".to_string(),
            test_commission("c-partial", "ord-1", 10000, 4000, "pending"),
        );
        let mut ctx = refund_ctx(refunded_order_payload());
        on_order_refunded(state.clone(), &mut ctx);
        let paid = state.commissions.get("c-paid").unwrap();
        assert_eq!(paid.status, "paid");
        let partial = state.commissions.get("c-partial").unwrap();
        assert_eq!(partial.status, "reversed");
        assert_eq!(partial.paid_amount, 4000);
    }
}
