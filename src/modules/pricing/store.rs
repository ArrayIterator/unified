// pricing - store.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::{HookBus, HookContext};
use unified_core::middleware::TenantContext;
use unified_core::json;
use unified_core::modules::{Module, ModuleRegistry, RouteInfo};
use unified_core::pagination::{PageResult, PaginationParams};
use actix_web::{web, HttpRequest, HttpResponse};
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::sync::{Arc, RwLock};
use uuid::Uuid;
use super::*;
use super::money::*;
use super::recommendations::*;
use super::discounts::*;
use super::proration::*;
use super::reconciliation::*;
use super::types::*;
use super::common::*;
use super::service::*;
use super::handlers_recommendations::*;
use super::handlers_rules::*;
use super::handlers_coupons::*;
use super::handlers_prices::*;
use super::handlers_bundles::*;
use super::handlers_reconciliation::*;
use super::module::*;

impl PricingState {
    pub(crate) fn new() -> Self {
        let state = Self {
            hooks: RwLock::new(Arc::new(HookBus::new())),
            settings: DashMap::new(),
            currencies: DashMap::new(),
            rules: DashMap::new(),
            coupons: DashMap::new(),
            redemptions: DashMap::new(),
            recommendations: DashMap::new(),
            cost_lines: DashMap::new(),
            tiers: DashMap::new(),
            bundles: DashMap::new(),
            bundle_components: DashMap::new(),
            reference_prices: DashMap::new(),
            price_versions: DashMap::new(),
            price_approvals: DashMap::new(),
            prorations: DashMap::new(),
            credit_notes: DashMap::new(),
            reconciliations: DashMap::new(),
            credit_seq: RwLock::new(0),
        };
        for (code, exponent) in SEEDED_CURRENCIES {
            let now = Utc::now();
            state.currencies.insert(
                code.to_string(),
                PricingCurrency {
                    code: code.to_string(),
                    exponent,
                    active: true,
                    rate_numerator: if code == DEFAULT_CURRENCY { Some(1) } else { None },
                    rate_denominator: if code == DEFAULT_CURRENCY { Some(1) } else { None },
                    rate_fetched_at: None,
                    created_at: now,
                    updated_at: now,
                },
            );
        }
        state
    }

    pub(crate) fn bus(&self) -> Arc<HookBus> {
        self.hooks.read().expect("hooks lock poisoned").clone()
    }

    pub(crate) fn emit(&self, event: &str, data: Value) {
        self.bus().emit(event, data);
    }

    /// Settings for one tenant, lazily seeded with the
    /// configurable defaults on first use. There is no global
    /// settings row: a tenant without a context never reaches
    /// this function, because `caller()` fails closed first.
    pub(crate) fn settings_for(&self, tenant_id: &str) -> PricingSettings {
        self.settings
            .entry(tenant_id.to_string())
            .or_insert_with(PricingSettings::default)
            .clone()
    }

    pub(crate) fn next_credit_number(&self) -> String {
        let mut seq = self.credit_seq.write().expect("credit sequence lock poisoned");
        *seq += 1;
        format!("CN-{:06}", *seq)
    }

    /// Active discount rules for one tenant, in declared
    /// evaluation order. Rules of other tenants are never returned.
    pub(crate) fn active_rules(&self, tenant_id: &str) -> Vec<DiscountRuleInput> {
        let now = Utc::now();
        let mut rules: Vec<DiscountRuleInput> = self
            .rules
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|rule| rule.tenant_id == tenant_id)
            .filter(|rule| rule.status == "active")
            .filter(|rule| {
                rule.valid_from.is_none_or(|from| now >= from)
                    && rule.valid_to.is_none_or(|to| now <= to)
            })
            .map(|rule| rule_to_engine(&rule))
            .collect();
        rules.sort_by(|a, b| {
            a.evaluation_order
                .cmp(&b.evaluation_order)
                .then(a.rule_id.cmp(&b.rule_id))
        });
        rules
    }

    /// The next version number for a product's price in one tenant.
    pub(crate) fn next_price_version(&self, tenant_id: &str, product_id: &str) -> u32 {
        self.price_versions
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|version| version.tenant_id == tenant_id && version.product_id == product_id)
            .map(|version| version.version)
            .max()
            .unwrap_or(0)
            + 1
    }

    /// The currently active price for a product in one tenant, if any.
    pub(crate) fn active_price_for(&self, tenant_id: &str, product_id: &str) -> Option<PricingPriceVersion> {
        self.price_versions
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|version| {
                version.tenant_id == tenant_id
                    && version.product_id == product_id
                    && version.status == "active"
            })
            .max_by_key(|version| version.version)
    }

    pub(crate) fn record_reconciliation(
        &self,
        tenant_id: &str,
        invoice_id: Option<String>,
        result: &ReconciliationResult,
        actor: &str,
    ) -> PricingReconciliation {
        let record = PricingReconciliation {
            id: new_id(),
            tenant_id: tenant_id.to_string(),
            invoice_id,
            matched: result.matched,
            currency: result.currency.clone(),
            components: result.components.clone(),
            total_discrepancy_scaled: result.total_discrepancy_scaled,
            checked_by: actor.to_string(),
            checked_at: Utc::now(),
        };
        self.reconciliations
            .insert(record.id.clone(), record.clone());
        record
    }
}

/// Normalize a stored rule into the shape the pure engine takes.
pub(crate) fn rule_to_engine(rule: &PricingRule) -> DiscountRuleInput {
    DiscountRuleInput {
        rule_id: rule.id.clone(),
        name: rule.name.clone(),
        kind: rule.kind.clone(),
        percent_bp: rule.percent_bp,
        amount_scaled: rule.amount_scaled,
        buy_quantity: rule.buy_quantity,
        get_quantity: rule.get_quantity,
        min_quantity: rule.min_quantity,
        min_subtotal_scaled: rule.min_subtotal_scaled,
        product_ids: rule.product_ids.clone(),
        evaluation_order: rule.evaluation_order,
        stackable: rule.stackable,
        exclusive: rule.exclusive,
    }
}

/// Normalize a coupon into rule shape. Coupons always evaluate
/// after stored rules (evaluation order 1,000,000 plus the coupon
/// creation order is not tracked, so coupons share one order and
/// are tied by rule id).
pub(crate) fn coupon_to_engine(coupon: &PricingCoupon) -> DiscountRuleInput {
    DiscountRuleInput {
        rule_id: format!("coupon:{}", coupon.id),
        name: format!("Coupon {}", coupon.code),
        kind: coupon.kind.clone(),
        percent_bp: coupon.percent_bp,
        amount_scaled: coupon.amount_scaled,
        buy_quantity: coupon.buy_quantity,
        get_quantity: coupon.get_quantity,
        min_quantity: 0,
        min_subtotal_scaled: 0,
        product_ids: coupon.product_ids.clone(),
        evaluation_order: 1_000_000,
        stackable: coupon.stackable,
        exclusive: coupon.exclusive,
    }
}

/// Validate a coupon for one customer at one moment: active,
/// inside its validity window, under its global limit, and under
/// the customer's limit. Returns the reason when unusable.
pub(crate) fn coupon_usability(
    state: &PricingState,
    coupon: &PricingCoupon,
    customer_id: Option<&str>,
    now: DateTime<Utc>,
) -> Option<String> {
    if !coupon.active {
        return Some("Coupon is not active".to_string());
    }
    if let Some(from) = coupon.valid_from {
        if now < from {
            return Some("Coupon is not yet valid".to_string());
        }
    }
    if let Some(to) = coupon.valid_to {
        if now > to {
            return Some("Coupon has expired".to_string());
        }
    }
    if let Some(limit) = coupon.usage_limit {
        if coupon.used_count >= limit {
            return Some("Coupon usage limit has been reached".to_string());
        }
    }
    if let (Some(limit), Some(customer)) = (coupon.per_customer_limit, customer_id) {
        let key = format!("{}:{}:{}", coupon.tenant_id, coupon.id, customer);
        if let Some(entry) = state.redemptions.get(&key) {
            if entry.count >= limit {
                return Some("Per-customer coupon usage limit has been reached".to_string());
            }
        }
    }
    None
}

// Helpers.

pub(crate) fn new_id() -> String {
    Uuid::new_v4().to_string()
}

pub(crate) fn now() -> DateTime<Utc> {
    Utc::now()
}

pub(crate) fn paginate<T: Serialize + Clone>(items: Vec<T>, params: &PaginationParams) -> PageResult<T> {
    let pagination = params.normalized();
    let total = items.len() as u64;
    let start = ((pagination.page - 1) * pagination.per_page) as usize;
    let page_items: Vec<T> = items
        .into_iter()
        .skip(start)
        .take(pagination.per_page as usize)
        .collect();
    PageResult::new(page_items, total, &pagination)
}

pub(crate) fn matches_search(haystack: &str, params: &PaginationParams) -> bool {
    match &params.search {
        Some(needle) if !needle.trim().is_empty() => haystack
            .to_lowercase()
            .contains(&needle.trim().to_lowercase()),
        _ => true,
    }
}
