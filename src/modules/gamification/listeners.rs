// Gamification — Event listeners (defensive: missing ids are skipped, never errors).

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::{HookBus, HookContext};
use unified_core::json;
use unified_core::modules::{Module, ModuleRegistry, RouteInfo, RouteVersionSupport};
use unified_core::pagination::{PageResult, PaginationParams};
use actix_web::{web, HttpRequest, HttpResponse};
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::sync::{Arc, RwLock};
use uuid::Uuid;

use super::*;
use super::helpers::{has_source_award, has_source_award_today};

// ---------------------------------------------------------------------------
// Event listeners (defensive: missing ids are skipped, never errors)
// ---------------------------------------------------------------------------

pub(crate) fn on_ticket_updated(state: GamificationState, ctx: &mut HookContext) {
    let data = ctx.data.clone();
    let status = opt_string(&data, "status").unwrap_or_default();
    if status != "resolved" && status != "closed" {
        ctx.add_result(json!({ "gamification": "skipped", "reason": format!("ticket status is \"{status}\", not resolved/closed") }));
        return;
    }
    // A transition into resolved/closed is required. When the
    // payload carries the status the ticket had before this update
    // and it was already resolved/closed, this update is an edit
    // of an already-resolved ticket, not a fresh resolution, so
    // nothing is awarded. The tickets emitter does not send
    // `previous_status` today, so an absent field cannot prove a
    // transition; the per-day dedup below still stops re-awards
    // on edits in that case.
    if let Some(previous) = opt_string(&data, "previous_status") {
        if previous == "resolved" || previous == "closed" {
            ctx.add_result(json!({ "gamification": "skipped", "reason": format!("ticket was already \"{previous}\"; no transition into resolved/closed") }));
            return;
        }
    }
    let Some(user_id) = opt_string(&data, "assigned_to")
        .or_else(|| opt_string(&data, "assignee_id"))
        .or_else(|| opt_string(&data, "user_id"))
    else {
        ctx.add_result(json!({ "gamification": "skipped", "reason": "ticket.updated carries no assignee" }));
        return;
    };
    // One ticket awards at most once per rule per day: repeated
    // updates of the same resolved ticket do not re-award.
    let ticket_id = opt_string(&data, "id");
    if let Some(ticket_id) = &ticket_id {
        if has_source_award_today(&state, &user_id, "ticket_closed", ticket_id) {
            ctx.add_result(json!({ "gamification": "skipped", "reason": format!("ticket {ticket_id} already awarded today") }));
            return;
        }
    }
    let outcome = award_points_for_action(
        &state,
        &user_id,
        "ticket_closed",
        opt_string(&data, "company_id"),
        opt_string(&data, "department_id"),
        Some(format!("Ticket {} closed", ticket_id.clone().unwrap_or_default())),
        ticket_id,
    );
    ctx.add_result(json!({ "gamification": "ticket_closed", "user_id": user_id, "awarded": outcome.awarded, "points": outcome.points }));
}

pub(crate) fn on_order_completed(state: GamificationState, ctx: &mut HookContext) {
    let data = ctx.data.clone();
    let Some(user_id) = opt_string(&data, "user_id").or_else(|| opt_string(&data, "customer_id")) else {
        ctx.add_result(json!({ "gamification": "skipped", "reason": "order event carries no user_id or customer_id" }));
        return;
    };
    // Both `order.paid` and `store.order.completed` fire for one
    // order and both land here, so an order awards at most once:
    // the order id is recorded on the ledger entry and a repeat
    // event for the same order+action is skipped.
    let order_id = opt_string(&data, "order_id").or_else(|| opt_string(&data, "id"));
    if let Some(order_id) = &order_id {
        if has_source_award(&state, &user_id, "sale_completed", order_id) {
            ctx.add_result(json!({ "gamification": "skipped", "reason": format!("order {order_id} already awarded") }));
            return;
        }
    }
    let outcome = award_points_for_action(
        &state,
        &user_id,
        "sale_completed",
        opt_string(&data, "company_id"),
        None,
        Some(format!("Order {} completed", order_id.clone().unwrap_or_default())),
        order_id,
    );
    ctx.add_result(json!({ "gamification": "sale_completed", "user_id": user_id, "awarded": outcome.awarded, "points": outcome.points }));
}

pub(crate) fn on_survey_response_received(state: GamificationState, ctx: &mut HookContext) {
    let data = ctx.data.clone();
    let Some(user_id) = opt_string(&data, "recipient_id").or_else(|| opt_string(&data, "user_id")) else {
        ctx.add_result(json!({ "gamification": "skipped", "reason": "surveys.response.received carries no recipient_id" }));
        return;
    };
    let outcome = award_points_for_action(
        &state,
        &user_id,
        "review_received",
        opt_string(&data, "company_id"),
        opt_string(&data, "department_id"),
        Some("Survey response received".to_string()),
        None,
    );
    ctx.add_result(json!({ "gamification": "review_received", "user_id": user_id, "awarded": outcome.awarded, "points": outcome.points }));
}

pub(crate) fn on_community_post_replied(state: GamificationState, ctx: &mut HookContext) {
    let data = ctx.data.clone();
    let Some(user_id) = opt_string(&data, "author_id").or_else(|| opt_string(&data, "user_id")) else {
        ctx.add_result(json!({ "gamification": "skipped", "reason": "community.post.replied carries no author_id" }));
        return;
    };
    let outcome = award_points_for_action(
        &state,
        &user_id,
        "community_help",
        opt_string(&data, "company_id"),
        None,
        Some(format!("Reply to community topic {}", opt_string(&data, "topic_id").unwrap_or_default())),
        None,
    );
    ctx.add_result(json!({ "gamification": "community_help", "user_id": user_id, "awarded": outcome.awarded, "points": outcome.points }));
}

pub(crate) fn on_community_topic_created(state: GamificationState, ctx: &mut HookContext) {
    let data = ctx.data.clone();
    let Some(user_id) = opt_string(&data, "author_id").or_else(|| opt_string(&data, "user_id")) else {
        ctx.add_result(json!({ "gamification": "skipped", "reason": "community.topic.created carries no author_id" }));
        return;
    };
    let outcome = award_points_for_action(
        &state,
        &user_id,
        "topic_created",
        opt_string(&data, "company_id"),
        None,
        Some(format!("Topic {} created", opt_string(&data, "topic_id").unwrap_or_default())),
        None,
    );
    ctx.add_result(json!({ "gamification": "topic_created", "user_id": user_id, "awarded": outcome.awarded, "points": outcome.points }));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state_with_rule(action: &str, points: i64) -> GamificationState {
        let state = GamificationState::default();
        let now = Utc::now();
        let rule = GamificationRule {
            id: Uuid::now_v7(),
            name: format!("{action} rule"),
            action: action.to_string(),
            points,
            company_id: None,
            daily_cap: None,
            active: true,
            created_at: now,
            updated_at: now,
        };
        state.rules.insert(rule.id, rule);
        state
    }

    fn awarded(result: &Value) -> bool {
        result.get("awarded").and_then(Value::as_bool).unwrap_or(false)
    }

    #[test]
    fn same_order_awards_only_once_across_both_order_events() {
        let state = state_with_rule("sale_completed", 10);
        let payload = json!({ "user_id": "u1", "order_id": "order-42" });
        let mut first = HookContext::new("order.paid", payload.clone());
        on_order_completed(state.clone(), &mut first);
        assert!(awarded(&first.results[0]), "first order event must award");
        let mut second = HookContext::new("store.order.completed", payload);
        on_order_completed(state.clone(), &mut second);
        assert!(!awarded(&second.results[0]), "second order event must not award again");
        assert_eq!(state.ledger.len(), 1);
        assert_eq!(user_balance(&state, "u1", None, None), 10);
    }

    #[test]
    fn different_orders_award_independently() {
        let state = state_with_rule("sale_completed", 10);
        for order_id in ["order-1", "order-2"] {
            let mut ctx = HookContext::new(
                "order.paid",
                json!({ "user_id": "u1", "order_id": order_id }),
            );
            on_order_completed(state.clone(), &mut ctx);
            assert!(awarded(&ctx.results[0]));
        }
        assert_eq!(state.ledger.len(), 2);
        assert_eq!(user_balance(&state, "u1", None, None), 20);
    }

    #[test]
    fn ticket_edit_with_no_transition_does_not_award() {
        let state = state_with_rule("ticket_closed", 10);
        let mut ctx = HookContext::new(
            "ticket.updated",
            json!({ "id": "ticket-1", "status": "resolved", "previous_status": "resolved", "assigned_to": "u1" }),
        );
        on_ticket_updated(state.clone(), &mut ctx);
        assert!(!awarded(&ctx.results[0]));
        assert_eq!(state.ledger.len(), 0);
    }

    #[test]
    fn ticket_transition_from_open_awards_once_per_day() {
        let state = state_with_rule("ticket_closed", 10);
        let payload = json!({ "id": "ticket-1", "status": "resolved", "previous_status": "open", "assigned_to": "u1" });
        let mut first = HookContext::new("ticket.updated", payload.clone());
        on_ticket_updated(state.clone(), &mut first);
        assert!(awarded(&first.results[0]));
        // Same ticket updated again the same day: no second award.
        let mut second = HookContext::new("ticket.updated", payload);
        on_ticket_updated(state.clone(), &mut second);
        assert!(!awarded(&second.results[0]));
        assert_eq!(state.ledger.len(), 1);
    }

    #[test]
    fn ticket_without_previous_status_still_awards_once_per_day() {
        // The tickets emitter does not send previous_status today,
        // so an absent field must not disable awards entirely; the
        // per-day dedup is what stops re-awards on edits.
        let state = state_with_rule("ticket_closed", 10);
        let payload = json!({ "id": "ticket-9", "status": "closed", "assigned_to": "u1" });
        let mut first = HookContext::new("ticket.updated", payload.clone());
        on_ticket_updated(state.clone(), &mut first);
        assert!(awarded(&first.results[0]));
        let mut second = HookContext::new("ticket.updated", payload);
        on_ticket_updated(state.clone(), &mut second);
        assert!(!awarded(&second.results[0]));
        assert_eq!(state.ledger.len(), 1);
    }
}
