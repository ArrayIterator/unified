// hosting module — Hook listeners.
//
// Split from the former single-file module `src/modules/hosting.rs`.
// The module root (`hosting.rs`) re-exports the public items, so the
// public API is unchanged.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::{events, HookBus, HookContext};
use unified_core::json;
use unified_core::modules::{Module, ModuleRegistry, RouteInfo};
use unified_core::pagination::{PageResult, PaginationParams};
use unified_core::db::models::Domain;
use crate::modules::company::company_exists;
use crate::modules::network::DnsVerifier;
use actix_web::{web, HttpResponse};
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use diesel::prelude::*;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::sync::Arc;
use uuid::Uuid;

use super::types::{DomainObservation, HostingState, OrderClaim};
use super::helpers::parse_expiry;

// ---------------------------------------------------------------------------
// Hook listeners (store / domains / SSL observations).
// ---------------------------------------------------------------------------

/// Record a store order as a provisioning claim when any of its items
/// buys a hosting plan (the item's store product id matches a plan's
/// linked store product id). Claims are keyed by (order id, plan id):
/// one order can pay for several plans, and each plan keeps its own
/// claim instead of overwriting the others.
pub(crate) fn record_order_claim(state: &Arc<HostingState>, data: &Value) {
    let Some(order_id) = data.get("id").and_then(Value::as_str) else {
        return;
    };
    let user_id = data
        .get("user_id")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let company_id = data
        .get("company_id")
        .and_then(Value::as_str)
        .and_then(|value| value.parse::<Uuid>().ok());
    let Some(items) = data.get("items").and_then(Value::as_array) else {
        return;
    };
    for item in items {
        let Some(product_id) = item.get("product_id").and_then(Value::as_str) else {
            continue;
        };
        let Some(plan) = state
            .plans
            .iter()
            .find(|entry| entry.value().store_product_id.as_deref() == Some(product_id))
            .map(|entry| entry.value().clone())
        else {
            continue;
        };
        state.order_claims.insert(
            (order_id.to_string(), plan.id),
            OrderClaim {
                order_id: order_id.to_string(),
                plan_id: plan.id,
                user_id: user_id.clone(),
                company_id: company_id.map(|id| id.to_string()),
            },
        );
    }
}

/// Record domain / SSL facts from a `domain.verify` report. Only a
/// report with a passing DNS check counts as a verified domain. A
/// failed report is recorded too — as `exists: false` with a failed
/// SSL state — so the requirement check can surface the failure
/// instead of treating the domain as unverified (or, worse, as
/// verified).
pub(crate) fn observe_domain_verify(state: &Arc<HostingState>, data: &Value) {
    let Some(domain) = data.get("domain").and_then(Value::as_str) else {
        return;
    };
    let dns_ok = data.get("dns_ok").and_then(Value::as_bool).unwrap_or(false);
    if !dns_ok {
        state.domain_observations.insert(
            domain.to_string(),
            DomainObservation {
                exists: false,
                ssl_state: "failed".to_string(),
                ssl_expires_at: None,
                observed_at: Utc::now(),
            },
        );
        return;
    }
    let ssl_status = data
        .get("ssl_status")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let ssl_ok = data.get("ssl_ok").and_then(Value::as_bool).unwrap_or(false);
    let ssl_state = if !ssl_status.is_empty() {
        ssl_status
    } else if ssl_ok {
        "active".to_string()
    } else {
        "none".to_string()
    };
    state.domain_observations.insert(
        domain.to_string(),
        DomainObservation {
            exists: true,
            ssl_state,
            ssl_expires_at: parse_expiry(data.get("ssl_expires_at")),
            observed_at: Utc::now(),
        },
    );
}

/// Record a completed SSL renewal / swap: only payloads carrying a
/// new expiry (or an explicit `completed` flag) count, so a renewal
/// request is never mistaken for a completed one.
pub(crate) fn observe_ssl_completion(state: &Arc<HostingState>, data: &Value) {
    let completed = data.get("completed").and_then(Value::as_bool).unwrap_or(false);
    let expires_at = parse_expiry(data.get("ssl_expires_at"));
    if expires_at.is_none() && !completed {
        return;
    }
    let Some(domain) = data.get("domain").and_then(Value::as_str) else {
        return;
    };
    state.domain_observations.insert(
        domain.to_string(),
        DomainObservation {
            exists: true,
            ssl_state: "active".to_string(),
            ssl_expires_at: expires_at,
            observed_at: Utc::now(),
        },
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::modules::hosting::HostingPlan;

    fn plan(id: Uuid, name: &str, product_id: &str) -> HostingPlan {
        HostingPlan {
            id,
            name: name.to_string(),
            description: "Test plan".to_string(),
            disk_gb: 10,
            cpu_cores: 2.0,
            ram_mb: 1024,
            bandwidth_gb: 100,
            price: 999,
            currency: "USD".to_string(),
            billing_cycle: "monthly".to_string(),
            store_product_id: Some(product_id.to_string()),
            is_active: true,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }

    #[test]
    fn domain_verify_records_a_passing_dns_check() {
        let state = Arc::new(HostingState::new());
        observe_domain_verify(
            &state,
            &json!({
                "domain": "example.com",
                "dns_ok": true,
                "ssl_ok": true,
                "ssl_expires_at": "2030-01-01T00:00:00Z",
            }),
        );
        let observation = state.domain_observations.get("example.com").unwrap();
        assert!(observation.exists);
        assert_eq!(observation.ssl_state, "active");
        assert!(observation.ssl_expires_at.is_some());
    }

    #[test]
    fn domain_verify_records_a_failed_dns_check_as_not_existing() {
        let state = Arc::new(HostingState::new());
        observe_domain_verify(
            &state,
            &json!({
                "domain": "example.com",
                "dns_ok": false,
                "ssl_ok": false,
            }),
        );
        let observation = state.domain_observations.get("example.com").unwrap();
        assert!(!observation.exists);
        assert_eq!(observation.ssl_state, "failed");
        assert!(observation.ssl_expires_at.is_none());
    }

    #[test]
    fn domain_verify_without_a_dns_result_is_not_trusted() {
        let state = Arc::new(HostingState::new());
        observe_domain_verify(
            &state,
            &json!({ "domain": "example.com", "ssl_ok": true }),
        );
        let observation = state.domain_observations.get("example.com").unwrap();
        assert!(!observation.exists);
    }

    #[test]
    fn order_claim_keeps_one_claim_per_plan_in_a_multi_plan_order() {
        let state = Arc::new(HostingState::new());
        let plan_a = plan(Uuid::new_v4(), "Basic", "prod-basic");
        let plan_b = plan(Uuid::new_v4(), "Pro", "prod-pro");
        state.plans.insert(plan_a.id, plan_a.clone());
        state.plans.insert(plan_b.id, plan_b.clone());
        record_order_claim(
            &state,
            &json!({
                "id": "order-1",
                "user_id": "user-1",
                "items": [
                    { "product_id": "prod-basic" },
                    { "product_id": "prod-pro" },
                ],
            }),
        );
        let claim_a = state
            .order_claims
            .get(&("order-1".to_string(), plan_a.id))
            .expect("claim for the Basic plan must exist");
        assert_eq!(claim_a.plan_id, plan_a.id);
        let claim_b = state
            .order_claims
            .get(&("order-1".to_string(), plan_b.id))
            .expect("claim for the Pro plan must exist");
        assert_eq!(claim_b.plan_id, plan_b.id);
        assert_eq!(state.order_claims.len(), 2);
    }
}
