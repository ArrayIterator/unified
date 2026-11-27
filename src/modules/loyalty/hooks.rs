//! Loyalty - hook listeners for orders and signups.
//!
//! Part of the `loyalty` module split; see `src/modules/loyalty.rs`.

use unified_core::error::UnifiedResult;
use unified_core::hooks::HookContext;
use unified_core::json;
use chrono::DateTime;
use chrono::Utc;
use serde_json::Value;
use serde_json::json;
use std::sync::Arc;
use uuid::Uuid;

use super::constants::MAX_AWARD_POINTS;
use super::helpers::EarnOutcome;
use super::helpers::active_program_for;
use super::helpers::append_ledger;
use super::helpers::earn_under_rule_locked;
use super::helpers::emit_earn_outcome;
use super::helpers::fixed_or_rate_points;
use super::helpers::has_earn_for_reference;
use super::helpers::ledger_lock;
use super::helpers::points_from_total;
use super::helpers::round_money;
use super::types::EarnRuleKind;
use super::types::LedgerKind;
use super::types::LoyaltyEarnRule;
use super::types::LoyaltyProgram;
use super::types::LoyaltyState;

// Hook listeners

/// `store.order.completed` -> purchase earn. The order payload is
/// the store `Order` JSON (`id`, `user_id`, `total`, `currency`,
/// `company_id` when present). The active program of the order's
/// company is used, or the default program when the order carries
/// no company. An order that already earned points (duplicate
/// emit) earns nothing twice: the order id is matched against
/// purchase earn rows.
pub(crate) fn on_store_order_completed(state: Arc<LoyaltyState>, ctx: &mut HookContext) {
    let order = ctx
        .get("order")
        .cloned()
        .unwrap_or_else(|| ctx.data.clone());
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
            "loyalty": "skipped",
            "reason": "store.order.completed carries no order id or user id",
        }));
        return;
    };
    if state.ledger.iter().any(|entry| {
        let value = entry.value();
        value.kind == LedgerKind::Earn
            && value.earn_kind.as_deref() == Some("purchase")
            && value.reference_id.as_deref() == Some(order_id.as_str())
    }) {
        ctx.add_result(json!({
            "loyalty": "already_earned",
            "order_id": order_id,
        }));
        return;
    }
    let company_id = order
        .get("company_id")
        .and_then(|value| value.as_str())
        .map(str::to_string);
    let Some(program) = active_program_for(&state, company_id.as_deref()) else {
        ctx.add_result(json!({
            "loyalty": "skipped",
            "order_id": order_id,
            "reason": "no active loyalty program matches this order",
        }));
        return;
    };
    let Some(rule) = state
        .earn_rules
        .iter()
        .map(|entry| entry.value().clone())
        .find(|rule| {
            rule.program_id == program.id && rule.kind == EarnRuleKind::Purchase && rule.active
        })
    else {
        ctx.add_result(json!({
            "loyalty": "skipped",
            "order_id": order_id,
            "reason": "no active purchase earn rule on this program",
        }));
        return;
    };
    let total = order
        .get("total")
        .and_then(|value| value.as_f64())
        .unwrap_or(0.0);
    if !total.is_finite() {
        ctx.add_result(json!({
            "loyalty": "skipped",
            "order_id": order_id,
            "reason": "order total is not a finite number",
        }));
        return;
    }
    if let Some(minimum) = rule.min_order_amount {
        if total < minimum {
            ctx.add_result(json!({
                "loyalty": "skipped",
                "order_id": order_id,
                "reason": format!("order total {total} is below the minimum {minimum}"),
            }));
            return;
        }
    }
    // A non-finite total or an absurd total can never award
    // i64::MAX points: the award is rejected or clamped (see
    // `points_from_total`).
    let points = match rule.fixed_points {
        Some(fixed) => fixed.max(0).min(MAX_AWARD_POINTS),
        None => match points_from_total(total, rule.points_per_unit) {
            Ok(points) => points,
            Err(error) => {
                ctx.add_result(json!({
                    "loyalty": "skipped",
                    "order_id": order_id,
                    "reason": error.message,
                }));
                return;
            }
        },
    };
    let now = Utc::now();
    let reason = format!("purchase {order_id}");
    // Dedup check and award run atomically under the ledger lock:
    // two concurrent duplicate deliveries cannot both pass the
    // check before either inserts.
    let outcome = {
        let _guard = ledger_lock(&state);
        if state.ledger.iter().any(|entry| {
            let value = entry.value();
            value.kind == LedgerKind::Earn
                && value.earn_kind.as_deref() == Some("purchase")
                && value.reference_id.as_deref() == Some(order_id.as_str())
        }) {
            ctx.add_result(json!({
                "loyalty": "already_earned",
                "order_id": order_id,
            }));
            return;
        }
        earn_under_rule_locked(
            &state,
            &program,
            &rule,
            &customer_id,
            points,
            &reason,
            "system",
            Some(order_id.clone()),
            Some(round_money(total)),
            now,
        )
    };
    match outcome {
        Ok(outcome) => {
            emit_earn_outcome(&state, &program, &customer_id, &outcome, &reason);
            match outcome.entry {
                Some(entry) => {
                    ctx.add_result(json!({
                        "loyalty": "points_earned",
                        "order_id": order_id,
                        "points": entry.points,
                        "balance_after": entry.balance_after,
                    }));
                }
                None => {
                    ctx.add_result(json!({
                        "loyalty": "skipped",
                        "order_id": order_id,
                        "reason": "earn rule caps leave nothing to award",
                    }));
                }
            }
        }
        Err(error) => {
            ctx.add_result(json!({
                "loyalty": "skipped",
                "order_id": order_id,
                "reason": error.message,
            }));
        }
    }
}

/// Award once, idempotently, under the ledger lock: when a ledger
/// row already exists with this `earn_kind` and `reference_id`
/// for the program (a duplicate `user.created` delivery), nothing
/// is awarded and `Ok(None)` is returned. Otherwise the award
/// runs under the same lock, so the dedup check and the insert
/// cannot interleave with a concurrent duplicate delivery. The
/// caller emits the outcome with `emit_earn_outcome` after the
/// lock is released.
#[allow(clippy::too_many_arguments)]
fn earn_once(
    state: &LoyaltyState,
    program: &LoyaltyProgram,
    rule: &LoyaltyEarnRule,
    customer_id: &str,
    points: i64,
    reason: &str,
    reference_id: &str,
    now: DateTime<Utc>,
) -> UnifiedResult<Option<EarnOutcome>> {
    let _guard = ledger_lock(state);
    if has_earn_for_reference(state, program.id, rule.kind.as_str(), reference_id) {
        return Ok(None);
    }
    let outcome = earn_under_rule_locked(
        state,
        program,
        rule,
        customer_id,
        points,
        reason,
        "system",
        Some(reference_id.to_string()),
        None,
        now,
    )?;
    Ok(Some(outcome))
}

/// `user.created` -> signup earn for the new customer, plus a
/// referral earn for the referrer when the payload carries
/// `referred_by` (a customer user id). The customer id is read
/// from `id` / `user_id` / `user.id`; the company from
/// `company_id` when present. Both earns are idempotent: a
/// duplicate delivery awards nothing twice, mirroring the
/// purchase dedup in `on_store_order_completed`.
pub(crate) fn on_user_created(state: Arc<LoyaltyState>, ctx: &mut HookContext) {
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
            "loyalty": "skipped",
            "reason": "user.created carries no customer id",
        }));
        return;
    };
    let company_id = ctx
        .get("company_id")
        .and_then(|value| value.as_str())
        .map(str::to_string);
    let Some(program) = active_program_for(&state, company_id.as_deref()) else {
        ctx.add_result(json!({
            "loyalty": "skipped",
            "reason": "no active loyalty program matches this signup",
        }));
        return;
    };
    let now = Utc::now();
    let mut results: Vec<Value> = Vec::new();
    // Signup earn for the new customer.
    if let Some(rule) = state
        .earn_rules
        .iter()
        .map(|entry| entry.value().clone())
        .find(|rule| {
            rule.program_id == program.id && rule.kind == EarnRuleKind::Signup && rule.active
        })
    {
        match fixed_or_rate_points(&rule) {
            Err(error) => results.push(json!({
                "loyalty": "skipped",
                "earn_kind": "signup",
                "reason": error.message,
            })),
            Ok(points) => match earn_once(
                &state,
                &program,
                &rule,
                &customer_id,
                points,
                "signup",
                &customer_id,
                now,
            ) {
                Ok(Some(outcome)) => {
                    emit_earn_outcome(&state, &program, &customer_id, &outcome, "signup");
                    match outcome.entry {
                        Some(entry) => results.push(json!({
                            "loyalty": "points_earned",
                            "earn_kind": "signup",
                            "customer_id": customer_id,
                            "points": entry.points,
                        })),
                        None => results.push(json!({
                            "loyalty": "skipped",
                            "earn_kind": "signup",
                            "reason": "earn rule caps leave nothing to award",
                        })),
                    }
                }
                Ok(None) => results.push(json!({
                    "loyalty": "already_earned",
                    "earn_kind": "signup",
                    "customer_id": customer_id,
                })),
                Err(error) => results.push(json!({
                    "loyalty": "skipped",
                    "earn_kind": "signup",
                    "reason": error.message,
                })),
            },
        }
    }
    // Referral earn for the referrer.
    if let Some(referrer_id) = ctx
        .get("referred_by")
        .and_then(|value| value.as_str())
        .map(str::to_string)
    {
        if referrer_id != customer_id {
            if let Some(rule) = state
                .earn_rules
                .iter()
                .map(|entry| entry.value().clone())
                .find(|rule| {
                    rule.program_id == program.id
                        && rule.kind == EarnRuleKind::Referral
                        && rule.active
                })
            {
                match fixed_or_rate_points(&rule) {
                    Err(error) => results.push(json!({
                        "loyalty": "skipped",
                        "earn_kind": "referral",
                        "reason": error.message,
                    })),
                    Ok(points) => match earn_once(
                        &state,
                        &program,
                        &rule,
                        &referrer_id,
                        points,
                        &format!("referral signup {customer_id}"),
                        &customer_id,
                        now,
                    ) {
                        Ok(Some(outcome)) => {
                            let reason = format!("referral signup {customer_id}");
                            emit_earn_outcome(&state, &program, &referrer_id, &outcome, &reason);
                            match outcome.entry {
                                Some(entry) => results.push(json!({
                                    "loyalty": "points_earned",
                                    "earn_kind": "referral",
                                    "customer_id": referrer_id,
                                    "points": entry.points,
                                })),
                                None => results.push(json!({
                                    "loyalty": "skipped",
                                    "earn_kind": "referral",
                                    "reason": "earn rule caps leave nothing to award",
                                })),
                            }
                        }
                        Ok(None) => results.push(json!({
                            "loyalty": "already_earned",
                            "earn_kind": "referral",
                            "customer_id": referrer_id,
                        })),
                        Err(error) => results.push(json!({
                            "loyalty": "skipped",
                            "earn_kind": "referral",
                            "reason": error.message,
                        })),
                    },
                }
            }
        }
    }
    for result in results {
        ctx.add_result(result);
    }
    if ctx.results.is_empty() {
        ctx.add_result(json!({
            "loyalty": "skipped",
            "reason": "no active signup or referral earn rule on this program",
        }));
    }
}

// Discount confirmation (L4)

/// Find the pending discount redemption a store event refers to:
/// by `redemption_id` when present, else by `order_id` (the one
/// pending discount redemption for that order). `None` when the
/// payload names no pending discount redemption.
fn find_pending_discount_redemption(
    state: &LoyaltyState,
    ctx: &HookContext,
) -> Option<super::types::LoyaltyRedemption> {
    if let Some(raw) = ctx.get("redemption_id").and_then(|value| value.as_str()) {
        if let Ok(id) = Uuid::parse_str(raw) {
            if let Some(redemption) = state.redemptions.get(&id).map(|entry| entry.value().clone())
            {
                if redemption.kind == "discount" && redemption.status == "pending" {
                    return Some(redemption);
                }
            }
            return None;
        }
        return None;
    }
    let order_id = ctx.get("order_id").and_then(|value| value.as_str())?;
    state
        .redemptions
        .iter()
        .map(|entry| entry.value().clone())
        .find(|redemption| {
            redemption.kind == "discount"
                && redemption.status == "pending"
                && redemption.order_id.as_deref() == Some(order_id)
        })
}

/// `store.discount.applied` -> confirm a pending discount
/// redemption. A discount redemption starts `pending` (its points
/// are already spent); only this confirmation moves it to
/// `completed`, which is when `loyalty.redemption.completed` is
/// emitted. Until the store emits this event the redemption stays
/// visibly pending and can be cancelled (refunded) instead.
pub(crate) fn on_store_discount_applied(state: Arc<LoyaltyState>, ctx: &mut HookContext) {
    let Some(mut redemption) = find_pending_discount_redemption(&state, ctx) else {
        ctx.add_result(json!({
            "loyalty": "skipped",
            "reason": "no pending discount redemption matches this event",
        }));
        return;
    };
    redemption.status = "completed".to_string();
    redemption.updated_at = Utc::now();
    state
        .redemptions
        .insert(redemption.id, redemption.clone());
    state.emit(
        "loyalty.redemption.completed",
        json!({
            "redemption_id": redemption.id,
            "program_id": redemption.program_id,
            "customer_id": redemption.customer_id,
            "points_spent": redemption.points_spent,
            "kind": redemption.kind,
            "discount_amount": redemption.discount_amount,
            "currency": redemption.currency,
            "product_id": redemption.product_id,
            "order_id": redemption.order_id,
        }),
    );
    ctx.add_result(json!({
        "loyalty": "redemption_confirmed",
        "redemption_id": redemption.id,
    }));
}

/// `store.discount.failed` -> the store could not apply a pending
/// discount, so the redemption is cancelled and its points are
/// refunded through a new ledger row (the original spend row is
/// never rewritten).
pub(crate) fn on_store_discount_failed(state: Arc<LoyaltyState>, ctx: &mut HookContext) {
    let Some(mut redemption) = find_pending_discount_redemption(&state, ctx) else {
        ctx.add_result(json!({
            "loyalty": "skipped",
            "reason": "no pending discount redemption matches this event",
        }));
        return;
    };
    let Some(program) = state
        .programs
        .get(&redemption.program_id)
        .map(|entry| entry.value().clone())
    else {
        ctx.add_result(json!({
            "loyalty": "skipped",
            "reason": "program of this redemption no longer exists",
        }));
        return;
    };
    let reason = ctx
        .get("reason")
        .and_then(|value| value.as_str())
        .unwrap_or("store could not apply discount");
    let now = Utc::now();
    let entry = match append_ledger(
        &state,
        &program,
        &redemption.customer_id,
        LedgerKind::Adjust,
        redemption.points_spent,
        &format!("refund: {reason}"),
        "system",
        None,
        Some(redemption.id.to_string()),
        None,
        now,
    ) {
        Ok(entry) => entry,
        Err(error) => {
            ctx.add_result(json!({
                "loyalty": "skipped",
                "reason": error.message,
            }));
            return;
        }
    };
    redemption.status = "cancelled".to_string();
    redemption.updated_at = now;
    state
        .redemptions
        .insert(redemption.id, redemption.clone());
    state.emit(
        "loyalty.redemption.cancelled",
        json!({
            "redemption_id": redemption.id,
            "program_id": program.id,
            "customer_id": redemption.customer_id,
            "points_refunded": redemption.points_spent,
            "balance_after": entry.balance_after,
            "reason": reason,
        }),
    );
    ctx.add_result(json!({
        "loyalty": "redemption_cancelled",
        "redemption_id": redemption.id,
        "points_refunded": redemption.points_spent,
    }));
}

#[cfg(test)]
mod tests {
    use super::super::types::ExpiryMode;
    use super::super::types::LoyaltyProgram;
    use super::super::types::LoyaltyRedemption;
    use super::*;
    use unified_core::hooks::HookContext;

    fn test_program() -> LoyaltyProgram {
        let now = Utc::now();
        LoyaltyProgram {
            id: Uuid::new_v4(),
            company_id: None,
            name: "Test".to_string(),
            description: None,
            points_name: "points".to_string(),
            currency: "USD".to_string(),
            discount_per_point: 0.01,
            active: true,
            expiry_days: None,
            expiry_mode: ExpiryMode::Fixed,
            created_at: now,
            updated_at: now,
        }
    }

    fn test_rule(
        program_id: Uuid,
        kind: EarnRuleKind,
        fixed_points: Option<i64>,
    ) -> LoyaltyEarnRule {
        let now = Utc::now();
        LoyaltyEarnRule {
            id: Uuid::new_v4(),
            program_id,
            kind,
            points_per_unit: 1.0,
            fixed_points,
            cap_per_day: None,
            cap_total: None,
            min_order_amount: None,
            active: true,
            created_at: now,
            updated_at: now,
        }
    }

    fn seed_program_with_rules(state: &LoyaltyState) -> LoyaltyProgram {
        let program = test_program();
        state.programs.insert(program.id, program.clone());
        for (kind, fixed) in [
            (EarnRuleKind::Signup, Some(100)),
            (EarnRuleKind::Referral, Some(50)),
            (EarnRuleKind::Purchase, None),
        ] {
            let rule = test_rule(program.id, kind, fixed);
            state.earn_rules.insert(rule.id, rule);
        }
        program
    }

    fn result_loyalty(ctx: &HookContext, index: usize) -> String {
        ctx.results[index]
            .get("loyalty")
            .and_then(|value| value.as_str())
            .unwrap_or("")
            .to_string()
    }

    #[test]
    fn duplicate_user_created_awards_signup_once() {
        let state = Arc::new(LoyaltyState::default());
        seed_program_with_rules(&state);
        let payload = json!({ "id": "cust-1" });
        for _ in 0..2 {
            let mut ctx = HookContext::new("user.created", payload.clone());
            on_user_created(Arc::clone(&state), &mut ctx);
        }
        let mut ctx = HookContext::new("user.created", payload);
        on_user_created(Arc::clone(&state), &mut ctx);
        assert_eq!(result_loyalty(&ctx, 0), "already_earned");
        let earns: Vec<_> = state
            .ledger
            .iter()
            .filter(|entry| {
                entry.value().kind == LedgerKind::Earn
                    && entry.value().earn_kind.as_deref() == Some("signup")
            })
            .collect();
        assert_eq!(earns.len(), 1);
        assert_eq!(earns[0].value().points, 100);
    }

    #[test]
    fn duplicate_user_created_awards_referral_once() {
        let state = Arc::new(LoyaltyState::default());
        seed_program_with_rules(&state);
        let payload = json!({ "id": "cust-new", "referred_by": "cust-ref" });
        for _ in 0..2 {
            let mut ctx = HookContext::new("user.created", payload.clone());
            on_user_created(Arc::clone(&state), &mut ctx);
        }
        let mut ctx = HookContext::new("user.created", payload);
        on_user_created(Arc::clone(&state), &mut ctx);
        assert_eq!(result_loyalty(&ctx, 0), "already_earned");
        assert_eq!(result_loyalty(&ctx, 1), "already_earned");
        let referral_earns: Vec<_> = state
            .ledger
            .iter()
            .filter(|entry| {
                entry.value().kind == LedgerKind::Earn
                    && entry.value().earn_kind.as_deref() == Some("referral")
            })
            .collect();
        assert_eq!(referral_earns.len(), 1);
        assert_eq!(referral_earns[0].value().customer_id, "cust-ref");
        assert_eq!(referral_earns[0].value().points, 50);
    }

    #[test]
    fn duplicate_order_completed_awards_purchase_once() {
        let state = Arc::new(LoyaltyState::default());
        seed_program_with_rules(&state);
        let payload = json!({ "order": { "id": "ord-1", "user_id": "cust-1", "total": 40.0 } });
        for _ in 0..2 {
            let mut ctx = HookContext::new("store.order.completed", payload.clone());
            on_store_order_completed(Arc::clone(&state), &mut ctx);
        }
        let mut ctx = HookContext::new("store.order.completed", payload);
        on_store_order_completed(Arc::clone(&state), &mut ctx);
        assert_eq!(result_loyalty(&ctx, 0), "already_earned");
        let earns: Vec<_> = state
            .ledger
            .iter()
            .filter(|entry| {
                entry.value().kind == LedgerKind::Earn
                    && entry.value().earn_kind.as_deref() == Some("purchase")
            })
            .collect();
        assert_eq!(earns.len(), 1);
        assert_eq!(earns[0].value().points, 40);
    }

    #[test]
    fn absurd_order_total_is_clamped_not_maxed() {
        let state = Arc::new(LoyaltyState::default());
        let program = seed_program_with_rules(&state);
        // A 1e300 total at 1 point per unit used to saturate to i64::MAX.
        let payload =
            json!({ "order": { "id": "ord-huge", "user_id": "cust-1", "total": 1e300 } });
        let mut ctx = HookContext::new("store.order.completed", payload);
        on_store_order_completed(Arc::clone(&state), &mut ctx);
        assert_eq!(result_loyalty(&ctx, 0), "points_earned");
        let entry = state
            .ledger
            .iter()
            .find(|entry| {
                entry.value().earn_kind.as_deref() == Some("purchase")
                    && entry.value().reference_id.as_deref() == Some("ord-huge")
            })
            .expect("purchase earn row must exist");
        assert_eq!(entry.value().points, MAX_AWARD_POINTS);
        assert!(entry.value().points < i64::MAX);
        let _ = program;
    }

    fn pending_discount_redemption(state: &LoyaltyState, program: &LoyaltyProgram) -> LoyaltyRedemption {
        let now = Utc::now();
        let redemption = LoyaltyRedemption {
            id: Uuid::new_v4(),
            program_id: program.id,
            customer_id: "cust-1".to_string(),
            points_spent: 20,
            kind: "discount".to_string(),
            discount_amount: 0.2,
            currency: "USD".to_string(),
            product_id: None,
            order_id: Some("ord-9".to_string()),
            status: "pending".to_string(),
            created_at: now,
            updated_at: now,
        };
        state
            .redemptions
            .insert(redemption.id, redemption.clone());
        redemption
    }

    #[test]
    fn discount_applied_confirms_pending_redemption() {
        let state = Arc::new(LoyaltyState::default());
        let program = test_program();
        state.programs.insert(program.id, program.clone());
        let redemption = pending_discount_redemption(&state, &program);
        let mut ctx = HookContext::new(
            "store.discount.applied",
            json!({ "redemption_id": redemption.id.to_string() }),
        );
        on_store_discount_applied(Arc::clone(&state), &mut ctx);
        assert_eq!(result_loyalty(&ctx, 0), "redemption_confirmed");
        assert_eq!(
            state
                .redemptions
                .get(&redemption.id)
                .expect("redemption must still exist")
                .value()
                .status,
            "completed"
        );
        // A second confirmation is a no-op skip, not a double confirm.
        let mut ctx = HookContext::new(
            "store.discount.applied",
            json!({ "redemption_id": redemption.id.to_string() }),
        );
        on_store_discount_applied(Arc::clone(&state), &mut ctx);
        assert_eq!(result_loyalty(&ctx, 0), "skipped");
    }

    #[test]
    fn discount_failed_cancels_pending_redemption_and_refunds() {
        let state = Arc::new(LoyaltyState::default());
        let program = test_program();
        state.programs.insert(program.id, program.clone());
        // Seed the balance the redemption already spent.
        append_ledger(
            &state,
            &program,
            "cust-1",
            LedgerKind::Earn,
            100,
            "seed",
            "system",
            None,
            None,
            None,
            Utc::now(),
        )
        .unwrap();
        append_ledger(
            &state,
            &program,
            "cust-1",
            LedgerKind::Spend,
            -20,
            "redemption: discount",
            "cust-1",
            None,
            Some("ord-9".to_string()),
            None,
            Utc::now(),
        )
        .unwrap();
        let redemption = pending_discount_redemption(&state, &program);
        let mut ctx = HookContext::new(
            "store.discount.failed",
            json!({ "redemption_id": redemption.id.to_string() }),
        );
        on_store_discount_failed(Arc::clone(&state), &mut ctx);
        assert_eq!(result_loyalty(&ctx, 0), "redemption_cancelled");
        assert_eq!(
            state
                .redemptions
                .get(&redemption.id)
                .expect("redemption must still exist")
                .value()
                .status,
            "cancelled"
        );
        let balance: i64 = state
            .ledger
            .iter()
            .filter(|entry| {
                entry.value().program_id == program.id && entry.value().customer_id == "cust-1"
            })
            .map(|entry| entry.value().points)
            .sum();
        assert_eq!(balance, 100);
    }

    #[test]
    fn discount_events_skip_when_no_pending_redemption() {
        let state = Arc::new(LoyaltyState::default());
        let mut ctx = HookContext::new(
            "store.discount.applied",
            json!({ "order_id": "ord-missing" }),
        );
        on_store_discount_applied(Arc::clone(&state), &mut ctx);
        assert_eq!(result_loyalty(&ctx, 0), "skipped");
        let mut ctx = HookContext::new(
            "store.discount.failed",
            json!({ "order_id": "ord-missing" }),
        );
        on_store_discount_failed(Arc::clone(&state), &mut ctx);
        assert_eq!(result_loyalty(&ctx, 0), "skipped");
    }
}
