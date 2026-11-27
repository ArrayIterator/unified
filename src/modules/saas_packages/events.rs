//! Saas Packages - Hook listeners for orders, subscriptions, and AI requests.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::{events, HookBus, HookContext};
use unified_core::json;
use unified_core::middleware::TenantContext;
use unified_core::modules::{Module, ModuleRegistry, RouteInfo, RouteVersionSupport};
use unified_core::tenancy as tenancy;
use unified_core::pagination::{PageResult, PaginationParams};
use crate::modules::ai::{register_ai_capabilities, run_ai_capability, AiCapability};
use crate::modules::licensing::check_module_entitlement;
use actix_web::{web, HttpRequest, HttpResponse};
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use std::collections::HashSet;
use std::sync::{Arc, RwLock};
use uuid::Uuid;
use super::{constants::*, types::*, state::*, seed::*, auth::*, helpers::*, provisioning::*, handlers_packages::*, handlers_purchases::*, handlers_ai::*, module::*};

// Hook handlers (event-driven; all read defensively -- a missing
// field or an unknown purchase is skipped, never an error).

pub(crate) fn opt_string(data: &Value, key: &str) -> Option<String> {
    data.get(key)
        .and_then(|value| value.as_str())
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

/// Find the purchase an order/subscription event names: a direct
/// `purchase_id`, an `order_id`/`subscription_id` matching a stored
/// purchase, or an `items` entry carrying a `purchase_id`.
pub(crate) fn purchase_for_event(state: &SaasPackagesState, data: &Value) -> Option<PackagePurchase> {
    if let Some(purchase_id) = opt_string(data, "purchase_id") {
        if let Ok(purchase) = load_purchase(state, &purchase_id) {
            return Some(purchase);
        }
    }
    if let Some(order_id) = opt_string(data, "order_id") {
        for entry in state.purchases.iter() {
            let purchase = entry.value();
            if purchase.order_id.as_deref() == Some(order_id.as_str()) {
                return Some(purchase.clone());
            }
        }
    }
    if let Some(order_id) = opt_string(data, "id") {
        for entry in state.purchases.iter() {
            let purchase = entry.value();
            if purchase.order_id.as_deref() == Some(order_id.as_str()) {
                return Some(purchase.clone());
            }
        }
    }
    if let Some(subscription_id) = opt_string(data, "subscription_id") {
        for entry in state.purchases.iter() {
            let purchase = entry.value();
            if purchase.subscription_id.as_deref() == Some(subscription_id.as_str()) {
                return Some(purchase.clone());
            }
        }
    }
    if let Some(items) = data.get("items").and_then(|value| value.as_array()) {
        for item in items {
            if let Some(purchase_id) = opt_string(item, "purchase_id") {
                if let Ok(purchase) = load_purchase(state, &purchase_id) {
                    return Some(purchase);
                }
            }
        }
    }
    None
}

/// One paid identifier inside a provisioning episode: the
/// purchase itself, or the order / subscription that paid for it.
pub(crate) fn paid_provision_claim_key(purchase_id: Uuid, kind: &str, id: &str) -> String {
    format!("{purchase_id}|{kind}:{id}")
}

/// Claim this paid trigger for the purchase. Returns true when this
/// is the first delivery of this paid episode, false when an
/// earlier `order.paid`, `store.order.completed`, or
/// `subscription.activated` for the same payment already started
/// provisioning.
///
/// Why a claim instead of a status check: the paid flow emits
/// several events for one payment (`order.paid`, then
/// `store.order.completed`, plus `subscription.activated`), and
/// each lands here. After the first run fails the purchase is
/// `failed`, and after it blocks it stays `provisioning`, so a
/// status check alone cannot tell a duplicate delivery from a
/// legitimate retry -- but a duplicate must not consume another
/// provisioning attempt. The claim records every identifier the
/// trigger and the stored purchase name (order id, subscription
/// id, always the purchase id), and a later event is a duplicate
/// when all of its identifiers are already inside one recorded
/// episode. That subset rule is what recognises the same payment
/// across different identifiers (the order event names the order
/// id, the subscription event names the subscription id) while
/// still letting a genuinely new payment through: a new order id
/// is not in any earlier episode, so it claims a fresh episode.
/// A refused duplicate never consumes an attempt; recovery from a
/// failed or blocked run is the manual HTTP retry endpoint, which
/// does not consult claims.
pub(crate) fn claim_paid_provision(
    state: &SaasPackagesState,
    purchase: &PackagePurchase,
    data: &Value,
) -> bool {
    let mut identifiers: HashSet<String> = HashSet::new();
    identifiers.insert(paid_provision_claim_key(
        purchase.id,
        "purchase",
        &purchase.id.to_string(),
    ));
    for (kind, id) in [
        ("order", opt_string(data, "order_id").or_else(|| opt_string(data, "id"))),
        ("subscription", opt_string(data, "subscription_id")),
        ("order", purchase.order_id.clone()),
        ("subscription", purchase.subscription_id.clone()),
    ] {
        if let Some(id) = id {
            identifiers.insert(paid_provision_claim_key(purchase.id, kind, &id));
        }
    }
    let mut episodes = state
        .paid_provision_episodes
        .entry(purchase.id)
        .or_default();
    if episodes
        .iter()
        .any(|episode| identifiers.iter().all(|id| episode.contains(id)))
    {
        return false;
    }
    episodes.push(identifiers.into_iter().collect());
    true
}

/// Shared paid-flow entry point for `on_order_paid` and
/// `on_subscription_activated`: status guards, paid-identifier
/// linking, duplicate-claim check, then a full provisioning run
/// on the hook path.
fn run_paid_provisioning(state: &SaasPackagesState, ctx: &mut HookContext, purchase: &PackagePurchase) {
    if purchase.status == PURCHASE_ACTIVE {
        ctx.add_result(json!({ "saas_packages": "skipped", "reason": "purchase is already provisioned and active", "purchase_id": purchase.id }));
        return;
    }
    if purchase.status != PURCHASE_PENDING_PAYMENT
        && purchase.status != PURCHASE_PROVISIONING
        && purchase.status != PURCHASE_FAILED
    {
        ctx.add_result(json!({ "saas_packages": "skipped", "reason": format!("purchase in status \"{}\" cannot be provisioned", purchase.status), "purchase_id": purchase.id }));
        return;
    }
    // Link whatever paid identifiers this event carries before
    // claiming, so a later event naming the same payment through
    // the other identifier is recognised as the same episode.
    let mut updated = purchase.clone();
    if updated.order_id.is_none() {
        updated.order_id = opt_string(&ctx.data, "order_id").or_else(|| opt_string(&ctx.data, "id"));
    }
    if updated.subscription_id.is_none() {
        updated.subscription_id = opt_string(&ctx.data, "subscription_id");
    }
    updated.updated_at = now();
    if !claim_paid_provision(state, &updated, &ctx.data) {
        ctx.add_result(json!({ "saas_packages": "skipped", "reason": "provisioning already started for this paid order or subscription; duplicate paid event ignored", "purchase_id": purchase.id }));
        return;
    }
    state.purchases.insert(updated.id, updated);
    match run_provisioning(state, purchase.id, "system", ProvisionMode::Full, None) {
        Ok(summary) => {
            ctx.add_result(json!({ "saas_packages": "provisioning_started", "purchase_id": purchase.id, "status": summary.status, "attempt": summary.attempt }));
        }
        Err(error) => {
            ctx.add_result(json!({ "saas_packages": "provisioning_error", "purchase_id": purchase.id, "reason": error.message }));
        }
    }
}

pub(crate) fn on_order_paid(state: SaasPackagesState, ctx: &mut HookContext) {
    let Some(purchase) = purchase_for_event(&state, &ctx.data.clone()) else {
        ctx.add_result(json!({ "saas_packages": "skipped", "reason": "order event names no SaaS package purchase" }));
        return;
    };
    run_paid_provisioning(&state, ctx, &purchase);
}

pub(crate) fn on_order_refunded_or_cancelled(state: SaasPackagesState, ctx: &mut HookContext) {
    let Some(purchase) = purchase_for_event(&state, &ctx.data.clone()) else {
        ctx.add_result(json!({ "saas_packages": "skipped", "reason": "order event names no SaaS package purchase" }));
        return;
    };
    if purchase.status != PURCHASE_ACTIVE && purchase.status != PURCHASE_GRACE_PERIOD {
        ctx.add_result(json!({ "saas_packages": "skipped", "reason": format!("purchase in status \"{}\" is not active", purchase.status), "purchase_id": purchase.id }));
        return;
    }
    let mut updated = purchase.clone();
    updated.status = PURCHASE_SUSPENDED.to_string();
    updated.updated_at = now();
    state.purchases.insert(updated.id, updated.clone());
    state.record_history(
        updated.id,
        "purchase.suspended",
        "system",
        json!({ "reason": "order refunded or cancelled" }),
    );
    ctx.add_result(json!({ "saas_packages": "suspended", "purchase_id": updated.id }));
}

pub(crate) fn on_subscription_activated(state: SaasPackagesState, ctx: &mut HookContext) {
    let Some(purchase) = purchase_for_event(&state, &ctx.data.clone()) else {
        ctx.add_result(json!({ "saas_packages": "skipped", "reason": "subscription event names no SaaS package purchase" }));
        return;
    };
    // Same paid flow as order.paid (see run_paid_provisioning):
    // the activation is a duplicate delivery of the payment that
    // already started provisioning unless it names a new payment.
    run_paid_provisioning(&state, ctx, &purchase);
}

pub(crate) fn on_subscription_cancelled(state: SaasPackagesState, ctx: &mut HookContext) {
    let Some(purchase) = purchase_for_event(&state, &ctx.data.clone()) else {
        ctx.add_result(json!({ "saas_packages": "skipped", "reason": "subscription event names no SaaS package purchase" }));
        return;
    };
    if purchase.status != PURCHASE_ACTIVE {
        ctx.add_result(json!({ "saas_packages": "skipped", "reason": format!("purchase in status \"{}\" is not active", purchase.status), "purchase_id": purchase.id }));
        return;
    }
    let settings = state.settings();
    let now = Utc::now();
    let mut updated = purchase.clone();
    updated.status = if settings.grace_period_days > 0 {
        PURCHASE_GRACE_PERIOD.to_string()
    } else {
        PURCHASE_CANCELLED.to_string()
    };
    updated.grace_ends_at = if settings.grace_period_days > 0 {
        Some(now + chrono::Duration::days(settings.grace_period_days))
    } else {
        None
    };
    updated.cancelled_at = Some(now);
    updated.cancel_reason = Some("Subscription cancelled".to_string());
    updated.updated_at = now;
    state.purchases.insert(updated.id, updated.clone());
    state.record_history(
        updated.id,
        "purchase.cancelled",
        "system",
        json!({ "reason": "subscription cancelled", "grace_ends_at": updated.grace_ends_at }),
    );
    emit_audit_state(
        &state,
        EVENT_CANCELLED,
        "system",
        "saas_packages.cancelled",
        "saas_package_purchase",
        &updated.id.to_string(),
        Some(updated.company_id),
        json!({ "reason": "subscription cancelled", "grace_ends_at": updated.grace_ends_at }),
    );
    ctx.add_result(json!({ "saas_packages": "grace_period", "purchase_id": updated.id, "grace_ends_at": updated.grace_ends_at }));
}

pub(crate) fn on_subscription_expired(state: SaasPackagesState, ctx: &mut HookContext) {
    let Some(purchase) = purchase_for_event(&state, &ctx.data.clone()) else {
        ctx.add_result(json!({ "saas_packages": "skipped", "reason": "subscription event names no SaaS package purchase" }));
        return;
    };
    if purchase.status != PURCHASE_ACTIVE && purchase.status != PURCHASE_GRACE_PERIOD {
        ctx.add_result(json!({ "saas_packages": "skipped", "reason": format!("purchase in status \"{}\" is not active", purchase.status), "purchase_id": purchase.id }));
        return;
    }
    let mut updated = purchase.clone();
    updated.status = PURCHASE_EXPIRED.to_string();
    updated.updated_at = now();
    state.purchases.insert(updated.id, updated.clone());
    state.record_history(
        updated.id,
        "purchase.expired",
        "system",
        json!({ "reason": "subscription expired" }),
    );
    ctx.add_result(json!({ "saas_packages": "expired", "purchase_id": updated.id }));
}

/// AI grounding for package recommendations: the published
/// packages with their module counts. Never buyer personal data.
pub(crate) fn on_ai_recommend_requested(state: SaasPackagesState, ctx: &mut HookContext) {
    let packages: Vec<Value> = state
        .published_packages()
        .iter()
        .map(|package| {
            json!({
                "package_id": package.id,
                "slug": package.slug,
                "name": package.name,
                "description": package.description,
                "module_count": package.modules.len(),
                "modules": package.modules.iter().map(|entry| entry.module_id.clone()).collect::<Vec<String>>(),
                "version": package.current_version,
            })
        })
        .collect();
    ctx.add_result(json!({
        "saas_packages": "recommend_grounding",
        "packages": packages,
    }));
}

/// AI grounding for describing one package: its module list and a
/// settings summary. Never buyer personal data.
pub(crate) fn on_ai_describe_requested(state: SaasPackagesState, ctx: &mut HookContext) {
    let Some(raw_id) = opt_string(&ctx.data, "package_id") else {
        ctx.add_result(json!({ "saas_packages": "skipped", "reason": "describe request carries no package_id" }));
        return;
    };
    let Ok(package) = load_package(&state, &raw_id) else {
        ctx.add_result(json!({ "saas_packages": "skipped", "reason": "no package matches the requested package_id" }));
        return;
    };
    let settings_summary = match &package.preset_settings {
        Value::Object(map) => json!(map.keys().cloned().collect::<Vec<String>>()),
        _ => json!([]),
    };
    ctx.add_result(json!({
        "saas_packages": "describe_grounding",
        "package_id": package.id,
        "slug": package.slug,
        "name": package.name,
        "description": package.description,
        "status": package.status,
        "version": package.current_version,
        "modules": package.modules.iter().map(|entry| json!({ "module_id": entry.module_id, "required": entry.required })).collect::<Vec<Value>>(),
        "wizard_steps": package.wizard_steps,
        "settings_keys": settings_summary,
    }));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state_with_registry() -> SaasPackagesState {
        let state = SaasPackagesState::new();
        *state
            .modules
            .write()
            .expect("modules lock poisoned") = Some(Arc::new(ModuleRegistry::default()));
        state
    }

    fn seed_package(state: &SaasPackagesState, modules: Vec<&str>) -> SaasPackage {
        let now = Utc::now();
        let package = SaasPackage {
            id: Uuid::now_v7(),
            slug: "test-package".to_string(),
            name: "Test Package".to_string(),
            description: String::new(),
            status: STATUS_PUBLISHED.to_string(),
            current_version: 1,
            product_id: None,
            preset_settings: json!({}),
            wizard_steps: Vec::new(),
            modules: modules
                .into_iter()
                .enumerate()
                .map(|(index, module_id)| PackageModuleEntry {
                    module_id: module_id.to_string(),
                    required: true,
                    sort_order: index as i32,
                })
                .collect(),
            created_by: "test".to_string(),
            vendor_id: None,
            created_at: now,
            updated_at: now,
        };
        state.store_version(&package);
        state.packages.insert(package.id, package.clone());
        package
    }

    fn seed_purchase(
        state: &SaasPackagesState,
        package: &SaasPackage,
        order_id: Option<&str>,
        subscription_id: Option<&str>,
    ) -> PackagePurchase {
        let now = Utc::now();
        let purchase = PackagePurchase {
            id: Uuid::now_v7(),
            package_id: package.id,
            package_version: package.current_version,
            buyer_id: "buyer-1".to_string(),
            company_id: Uuid::now_v7(),
            status: PURCHASE_PENDING_PAYMENT.to_string(),
            subdomain: None,
            subdomain_pending: false,
            order_id: order_id.map(str::to_string),
            subscription_id: subscription_id.map(str::to_string),
            product_id: None,
            enabled_modules: Vec::new(),
            grace_ends_at: None,
            cancelled_at: None,
            cancel_reason: None,
            provision_attempts: 0,
            created_at: now,
            updated_at: now,
        };
        state.purchases.insert(purchase.id, purchase.clone());
        purchase
    }

    fn stored_purchase(state: &SaasPackagesState, id: Uuid) -> PackagePurchase {
        state
            .purchases
            .get(&id)
            .map(|entry| entry.value().clone())
            .expect("purchase must still be stored")
    }

    fn result_kind(ctx: &HookContext) -> &str {
        ctx.results
            .first()
            .and_then(|value| value.get("saas_packages"))
            .and_then(Value::as_str)
            .unwrap_or("")
    }

    fn result_reason(ctx: &HookContext) -> String {
        ctx.results
            .first()
            .and_then(|value| value.get("reason"))
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string()
    }

    #[test]
    fn duplicate_paid_events_do_not_consume_extra_attempts() {
        // Defect 4: order.paid, store.order.completed, and
        // subscription.activated all fire for one payment. After
        // the first run fails, the duplicates used to re-run full
        // provisioning and consume another attempt each.
        let state = state_with_registry();
        let package = seed_package(&state, vec!["missing-module"]);
        let purchase = seed_purchase(&state, &package, Some("order-1"), Some("sub-1"));
        let paid = |order_id: &str| {
            json!({ "purchase_id": purchase.id.to_string(), "order_id": order_id })
        };

        let mut first = HookContext::new("order.paid", paid("order-1"));
        on_order_paid(state.clone(), &mut first);
        assert_eq!(result_kind(&first), "provisioning_started");
        assert_eq!(stored_purchase(&state, purchase.id).provision_attempts, 1);
        assert_eq!(stored_purchase(&state, purchase.id).status, PURCHASE_FAILED);

        let mut completed = HookContext::new("store.order.completed", paid("order-1"));
        on_order_paid(state.clone(), &mut completed);
        assert_eq!(result_kind(&completed), "skipped");
        assert!(result_reason(&completed).contains("duplicate"), "{}", result_reason(&completed));
        assert_eq!(stored_purchase(&state, purchase.id).provision_attempts, 1);

        let mut activated = HookContext::new(
            "subscription.activated",
            json!({ "purchase_id": purchase.id.to_string(), "subscription_id": "sub-1" }),
        );
        on_subscription_activated(state.clone(), &mut activated);
        assert_eq!(result_kind(&activated), "skipped");
        assert!(result_reason(&activated).contains("duplicate"), "{}", result_reason(&activated));
        assert_eq!(stored_purchase(&state, purchase.id).provision_attempts, 1);

        // A genuinely new payment still provisions: a new order id
        // is not in any recorded episode.
        let mut second_payment = HookContext::new("order.paid", paid("order-2"));
        on_order_paid(state.clone(), &mut second_payment);
        assert_eq!(result_kind(&second_payment), "provisioning_started");
        assert_eq!(stored_purchase(&state, purchase.id).provision_attempts, 2);

        // And the activation belonging to that second payment is a
        // duplicate of it, not a third attempt.
        let mut activated_again = HookContext::new(
            "subscription.activated",
            json!({ "purchase_id": purchase.id.to_string(), "subscription_id": "sub-1" }),
        );
        on_subscription_activated(state.clone(), &mut activated_again);
        assert_eq!(result_kind(&activated_again), "skipped");
        assert_eq!(stored_purchase(&state, purchase.id).provision_attempts, 2);
    }

    #[test]
    fn paid_event_without_identifiers_is_a_duplicate_after_the_first_trigger() {
        let state = state_with_registry();
        let package = seed_package(&state, vec!["missing-module"]);
        let purchase = seed_purchase(&state, &package, None, None);
        let payload = json!({ "purchase_id": purchase.id.to_string() });

        let mut first = HookContext::new("order.paid", payload.clone());
        on_order_paid(state.clone(), &mut first);
        assert_eq!(result_kind(&first), "provisioning_started");
        assert_eq!(stored_purchase(&state, purchase.id).provision_attempts, 1);

        let mut second = HookContext::new("store.order.completed", payload);
        on_order_paid(state.clone(), &mut second);
        assert_eq!(result_kind(&second), "skipped");
        assert!(result_reason(&second).contains("duplicate"), "{}", result_reason(&second));
        assert_eq!(stored_purchase(&state, purchase.id).provision_attempts, 1);
    }

    #[test]
    fn paid_events_after_a_successful_run_stay_skipped() {
        let state = state_with_registry();
        let package = seed_package(&state, vec![]);
        let purchase = seed_purchase(&state, &package, Some("order-1"), None);
        let payload = json!({ "purchase_id": purchase.id.to_string(), "order_id": "order-1" });

        let mut first = HookContext::new("order.paid", payload.clone());
        on_order_paid(state.clone(), &mut first);
        assert_eq!(result_kind(&first), "provisioning_started");
        assert_eq!(stored_purchase(&state, purchase.id).status, PURCHASE_ACTIVE);
        assert_eq!(stored_purchase(&state, purchase.id).provision_attempts, 1);

        let mut second = HookContext::new("store.order.completed", payload);
        on_order_paid(state.clone(), &mut second);
        assert_eq!(result_kind(&second), "skipped");
        assert_eq!(stored_purchase(&state, purchase.id).provision_attempts, 1);
    }
}
