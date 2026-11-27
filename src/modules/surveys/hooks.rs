// surveys module - Trigger listeners.
//
// Split from the former single-file module `src/modules/surveys.rs`.
// The module root (`surveys.rs`) re-exports the public items, so the
// public API is unchanged.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::{HookBus, HookContext};
use unified_core::json;
use unified_core::modules::{Module, ModuleRegistry, RouteInfo, RouteVersionSupport};
use unified_core::pagination::{PageResult, PaginationParams};
use actix_web::{web, HttpRequest, HttpResponse};
use base64::Engine;
use chrono::{DateTime, Datelike, Duration, Utc};
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use uuid::Uuid;

use super::types::SurveyDistribution;
use super::requests::DistributionRecipient;
use super::state::SurveysState;
use super::helpers::opt_string;
use super::service::{invite_recipient, send_distribution_invitations};

// Trigger listeners

/// Invite a recipient to every published survey version that has
/// an active trigger distribution matching the given trigger and
/// filters. Used by the ticket-closed and purchase listeners.
pub(crate) fn invite_for_trigger(
    state: &SurveysState,
    trigger: &str,
    recipient_id: &str,
    recipient_email: Option<String>,
    agent_id: Option<String>,
    department_id: Option<String>,
    product_id: Option<String>,
) {
    let distributions: Vec<SurveyDistribution> = state
        .distributions
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|distribution| {
            distribution.trigger == trigger
                && distribution.status != "cancelled"
                && distribution
                    .department_id
                    .as_ref()
                    .map(|department| department_id.as_deref() == Some(department.as_str()))
                    .unwrap_or(true)
                && distribution
                    .product_id
                    .as_ref()
                    .map(|product| product_id.as_deref() == Some(product.as_str()))
                    .unwrap_or(true)
        })
        .collect();
    for distribution in &distributions {
        let Some(survey) = state.surveys.get(&distribution.survey_id).map(|entry| entry.value().clone()) else {
            continue;
        };
        if survey.status != "published" {
            continue;
        }
        invite_recipient(
            state,
            &survey,
            Some(distribution),
            recipient_id,
            recipient_email.clone(),
            agent_id.clone().or_else(|| distribution.agent_id.clone()),
            department_id.clone().or_else(|| distribution.department_id.clone()),
            product_id.clone().or_else(|| distribution.product_id.clone()),
        );
    }
}

/// A ticket moving to `resolved` or `closed` invites its user to
/// the surveys with an active `ticket_closed` distribution. The
/// tickets module emits `ticket.updated` with the full ticket JSON
/// (id, user_id, department_id, status, assigned_to); there is no
/// separate `ticket.closed` event in this tree.
pub(crate) fn on_ticket_updated(state: SurveysState, ctx: &mut HookContext) {
    let data = ctx.data.clone();
    let status = opt_string(&data, "status").unwrap_or_default();
    if status != "resolved" && status != "closed" {
        return;
    }
    let Some(user_id) = opt_string(&data, "user_id") else {
        return;
    };
    invite_for_trigger(
        &state,
        "ticket_closed",
        &user_id,
        None,
        opt_string(&data, "assigned_to"),
        opt_string(&data, "department_id"),
        opt_string(&data, "product"),
    );
}

/// A completed order invites its buyer to the surveys with an
/// active `purchase` distribution. Both `store.order.completed`
/// and the legacy `order.paid` event are listened to; the payload
/// is read defensively (`user_id`, `customer_id`,
/// `items[].product_id`, `total`). An order matches a
/// distribution when any of its items matches the distribution's
/// product filter.
pub(crate) fn on_order_completed(state: SurveysState, ctx: &mut HookContext) {
    let data = ctx.data.clone();
    let Some(user_id) = opt_string(&data, "user_id").or_else(|| opt_string(&data, "customer_id")) else {
        return;
    };
    let email = opt_string(&data, "customer_email").or_else(|| opt_string(&data, "email"));
    let mut product_ids: Vec<String> = data
        .get("items")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|item| {
                    item.get("product_id")
                        .and_then(Value::as_str)
                        .map(str::to_string)
                })
                .collect()
        })
        .unwrap_or_default();
    product_ids.sort();
    product_ids.dedup();
    if product_ids.is_empty() {
        // No item data: only distributions without a product
        // filter can match.
        invite_for_trigger(&state, "purchase", &user_id, email, None, None, None);
        return;
    }
    // One pass per distinct product: a distribution whose filter
    // matches any item invites the buyer, and the per-recipient
    // dedup in `invite_recipient` keeps the buyer to one response
    // per survey version.
    for product_id in product_ids {
        invite_for_trigger(
            &state,
            "purchase",
            &user_id,
            email.clone(),
            None,
            None,
            Some(product_id),
        );
    }
}

/// Send the pending invitations of the named distribution when the
/// scheduler fires the `surveys.send` job: the recipients supplied
/// at creation are invited now.
pub(crate) fn on_send_requested(state: SurveysState, ctx: &mut HookContext) {
    let data = ctx.data.clone();
    let Some(raw_id) = opt_string(&data, "distribution_id") else {
        return;
    };
    let Ok(id) = Uuid::parse_str(&raw_id) else {
        return;
    };
    let Some(mut distribution) = state.distributions.get(&id).map(|entry| entry.value().clone()) else {
        return;
    };
    if distribution.status == "cancelled" {
        return;
    }
    let recipients: Vec<DistributionRecipient> = Vec::new();
    match send_distribution_invitations(&state, &mut distribution, recipients) {
        Ok(issued) => {
            ctx.add_result(json!({
                "distribution_id": distribution.id.to_string(),
                "sent": true,
                "issued": issued,
            }));
        }
        Err(error) => {
            // A failed scheduled send is recorded on the
            // distribution and reported in the hook result instead
            // of being discarded.
            distribution.scheduler_status = "failed".to_string();
            distribution.updated_at = Utc::now();
            state.distributions.insert(distribution.id, distribution.clone());
            ctx.add_result(json!({
                "distribution_id": distribution.id.to_string(),
                "sent": false,
                "error": error.to_string(),
            }));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::modules::surveys::{Survey, SurveyDistribution};

    fn survey() -> Survey {
        let now = Utc::now();
        let id = Uuid::new_v4();
        Survey {
            id,
            family_id: id,
            version: 1,
            supersedes: None,
            title: "Feedback".to_string(),
            description: None,
            status: "published".to_string(),
            created_by: "test".to_string(),
            created_at: now,
            updated_at: now,
        }
    }

    fn purchase_distribution(survey_id: Uuid, product_id: Option<&str>) -> SurveyDistribution {
        let now = Utc::now();
        SurveyDistribution {
            id: Uuid::new_v4(),
            survey_id,
            trigger: "purchase".to_string(),
            department_id: None,
            product_id: product_id.map(str::to_string),
            agent_id: None,
            responsible_user_id: None,
            scheduled_at: None,
            expires_in_days: 30,
            status: "pending".to_string(),
            scheduler_status: "not_scheduled".to_string(),
            scheduler_job_id: None,
            sent_count: 0,
            created_by: "test".to_string(),
            created_at: now,
            updated_at: now,
        }
    }

    #[test]
    fn on_order_completed_invites_when_any_item_matches_the_product_filter() {
        let state = SurveysState::default();
        let survey = survey();
        state.surveys.insert(survey.id, survey.clone());
        let distribution = purchase_distribution(survey.id, Some("prod-b"));
        state.distributions.insert(distribution.id, distribution.clone());
        let mut ctx = HookContext::new(
            "store.order.completed",
            json!({
                "user_id": "buyer-1",
                "product_id": "prod-a",
                "product_ids": ["prod-a"],
                "items": [
                    { "product_id": "prod-a" },
                    { "product_id": "prod-b" },
                ],
            }),
        );
        on_order_completed(state.clone(), &mut ctx);
        let invited: Vec<_> = state
            .responses
            .iter()
            .map(|entry| entry.value().clone())
            .collect();
        assert_eq!(invited.len(), 1, "the prod-b item must match the filter");
        assert_eq!(invited[0].recipient_id, "buyer-1");
        assert_eq!(invited[0].product_id.as_deref(), Some("prod-b"));
    }

    #[test]
    fn on_order_completed_does_not_invite_when_no_item_matches() {
        let state = SurveysState::default();
        let survey = survey();
        state.surveys.insert(survey.id, survey.clone());
        let distribution = purchase_distribution(survey.id, Some("prod-b"));
        state.distributions.insert(distribution.id, distribution.clone());
        let mut ctx = HookContext::new(
            "store.order.completed",
            json!({
                "user_id": "buyer-1",
                "items": [{ "product_id": "prod-a" }],
            }),
        );
        on_order_completed(state.clone(), &mut ctx);
        assert!(state.responses.is_empty());
    }

    #[test]
    fn on_send_requested_records_a_failed_send_on_the_distribution() {
        let state = SurveysState::default();
        // The distribution points at a survey that does not exist,
        // so the send fails.
        let distribution = purchase_distribution(Uuid::new_v4(), None);
        let distribution = SurveyDistribution {
            trigger: "scheduled".to_string(),
            ..distribution
        };
        state.distributions.insert(distribution.id, distribution.clone());
        let mut ctx = HookContext::new(
            "surveys.send_requested",
            json!({ "distribution_id": distribution.id.to_string() }),
        );
        on_send_requested(state.clone(), &mut ctx);
        let stored = state
            .distributions
            .get(&distribution.id)
            .expect("distribution still stored");
        assert_eq!(stored.value().scheduler_status, "failed");
        assert_eq!(ctx.results.len(), 1);
        assert_eq!(ctx.results[0]["sent"], json!(false));
        assert!(ctx.results[0]["error"].as_str().is_some());
    }
}
