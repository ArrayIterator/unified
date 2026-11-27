// pricing - handlers bundles.

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
use super::store::*;
use super::common::*;
use super::service::*;
use super::handlers_recommendations::*;
use super::handlers_rules::*;
use super::handlers_coupons::*;
use super::handlers_prices::*;
use super::handlers_reconciliation::*;
use super::module::*;

pub(crate) async fn list_bundles(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<PricingState>>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let params = query.into_inner();
    let tenant_id = match caller(&req).and_then(|call| {
        require_permission(&ctx, &call.role, PERM_PRICING_READ).map(|_| call.tenant_id)
    }) {
        Ok(tenant_id) => tenant_id,
        Err(error) => return json::error_response(&error, debug),
    };
    let mut items: Vec<PricingBundle> = state
        .bundles
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|item| {
            item.tenant_id == tenant_id
                && matches_search(&format!("{} {}", item.name, item.product_id), &params)
                && params
                    .filters
                    .get("product_id")
                    .is_none_or(|value| item.product_id == *value)
                && params
                    .filters
                    .get("active")
                    .is_none_or(|value| item.active == (*value == "true" || *value == "1"))
        })
        .collect();
    items.sort_by(|a, b| b.created_at.cmp(&a.created_at));
    if params.normalized().order == "asc" {
        items.reverse();
    }
    json::success_response(paginate(items, &params), 200)
}

pub(crate) async fn create_bundle(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<PricingState>>,
    req: HttpRequest,
    body: web::Json<CreatePricingBundle>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PricingBundle> = (|| {
        let call = caller(&req)?;
        require_permission(&ctx, &call.role, PERM_PRICING_MANAGE)?;
        let input = body.into_inner();
        let name = require_text(&input.name, "name", 128)?;
        let product_id = require_text(&input.product_id, "product_id", 128)?;
        if !["percent", "fixed"].contains(&input.discount_kind.as_str()) {
            return Err(UnifiedError::bad_request(
                "Bundle discount kind must be \"percent\" or \"fixed\"",
            ));
        }
        if input.discount_kind == "percent"
            && !(0..=BASIS_POINTS).contains(&input.discount_percent_bp)
        {
            return Err(UnifiedError::bad_request(
                "Bundle discount percent must be between 0 and 10000 basis points",
            ));
        }
        let mut currency: Option<String> = None;
        let mut components: Vec<PricingBundleComponent> = Vec::new();
        let bundle_id = new_id();
        for component in &input.components {
            let money = resolve_money(&component.price, currency.as_deref())?;
            match &currency {
                None => currency = Some(money.currency.clone()),
                Some(expected) if *expected != money.currency => {
                    return Err(UnifiedError::bad_request(
                        "All bundle components must use the same currency",
                    ));
                }
                _ => {}
            }
            components.push(PricingBundleComponent {
                id: new_id(),
                tenant_id: call.tenant_id.clone(),
                bundle_id: bundle_id.clone(),
                product_id: require_text(&component.product_id, "component product_id", 128)?,
                label: component.label.trim().to_string(),
                price_scaled: round_to_minor(money.amount_scaled, &money.currency),
                currency: money.currency,
            });
        }
        let currency = currency.unwrap_or_else(|| state.settings_for(&call.tenant_id).default_currency);
        require_currency_known(&state, &currency)?;
        let discount_amount_scaled = match &input.discount_amount {
            Some(amount) => {
                let money = resolve_money(amount, Some(&currency))?;
                if money.currency != currency {
                    return Err(UnifiedError::bad_request(
                        "Bundle discount currency must match the component currency",
                    ));
                }
                round_to_minor(money.amount_scaled, &currency)
            }
            None => 0,
        };
        if input.discount_kind == "fixed" && discount_amount_scaled <= 0 {
            return Err(UnifiedError::bad_request(
                "A fixed bundle discount needs an amount above zero",
            ));
        }
        let now = now();
        let bundle = PricingBundle {
            id: bundle_id,
            tenant_id: call.tenant_id.clone(),
            name,
            product_id,
            discount_kind: input.discount_kind,
            discount_percent_bp: input.discount_percent_bp,
            discount_amount_scaled,
            currency,
            active: input.active.unwrap_or(true),
            created_by: call.actor,
            created_at: now,
            updated_at: now,
        };
        for component in components {
            state
                .bundle_components
                .insert(component.id.clone(), component);
        }
        state.bundles.insert(bundle.id.clone(), bundle.clone());
        Ok(bundle)
    })();
    match result {
        Ok(item) => json::success_response(item, 201),
        Err(error) => json::error_response(&error, debug),
    }
}

pub(crate) async fn get_bundle(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<PricingState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<Value> = (|| {
        let call = caller(&req)?;
        require_permission(&ctx, &call.role, PERM_PRICING_READ)?;
        let bundle = state
            .bundles
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Bundle not found"))?;
        require_same_tenant(&bundle.tenant_id, &call.tenant_id)?;
        let components: Vec<PricingBundleComponent> = state
            .bundle_components
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|component| {
                component.tenant_id == call.tenant_id && component.bundle_id == bundle.id
            })
            .collect();
        Ok(json!({ "bundle": bundle, "components": components }))
    })();
    json::respond(result, ctx.is_debug())
}

/// Price a stored bundle with the pure bundle engine.
pub(crate) async fn price_bundle(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<PricingState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<BundlePriceResult> = (|| {
        let call = caller(&req)?;
        require_permission(&ctx, &call.role, PERM_PRICING_READ)?;
        let bundle = state
            .bundles
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Bundle not found"))?;
        require_same_tenant(&bundle.tenant_id, &call.tenant_id)?;
        let components: Vec<BundleComponentInput> = state
            .bundle_components
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|component| {
                component.tenant_id == call.tenant_id && component.bundle_id == bundle.id
            })
            .map(|component| BundleComponentInput {
                product_id: component.product_id,
                label: component.label,
                price: MoneyInput {
                    amount: format_scaled(component.price_scaled, &component.currency),
                    currency: Some(component.currency),
                },
            })
            .collect();
        let discount_kind = bundle.discount_kind.clone();
        let discount = BundleDiscountInput {
            kind: discount_kind.clone(),
            percent_bp: bundle.discount_percent_bp,
            amount: if discount_kind == "fixed" {
                Some(MoneyInput {
                    amount: format_scaled(bundle.discount_amount_scaled, &bundle.currency),
                    currency: Some(bundle.currency.clone()),
                })
            } else {
                None
            },
        };
        bundle_price(&components, &discount)
    })();
    match result {
        Ok(value) => json::success_response(value, 200),
        Err(error) => json::error_response(&error, debug),
    }
}

// HTTP handlers: proration.

pub(crate) async fn compute_proration(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<PricingState>>,
    req: HttpRequest,
    body: web::Json<ComputeProrationRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PricingProration> = (|| {
        let call = caller(&req)?;
        require_permission(&ctx, &call.role, PERM_PRICING_MANAGE)?;
        let input = body.into_inner();
        let settings = state.settings_for(&call.tenant_id);
        let old_price = resolve_money(&input.old_price, None)?;
        let new_price = resolve_money(&input.new_price, Some(&old_price.currency))?;
        if old_price.currency != new_price.currency {
            return Err(UnifiedError::bad_request(
                "Old and new prices must use the same currency",
            ));
        }
        require_currency_known(&state, &old_price.currency)?;
        let basis = input
            .basis
            .unwrap_or_else(|| settings.proration_basis.clone());
        let computed = prorate(&ProrationInput {
            kind: input.kind,
            old_price_scaled: round_to_minor(old_price.amount_scaled, &old_price.currency),
            new_price_scaled: round_to_minor(new_price.amount_scaled, &new_price.currency),
            period_start: input.period_start,
            period_end: input.period_end,
            change_at: input.change_at,
            basis,
            days_per_month: settings.days_per_month,
            currency: old_price.currency.clone(),
        })?;
        let record = PricingProration {
            id: new_id(),
            tenant_id: call.tenant_id.clone(),
            subscription_id: input.subscription_id.map(|id| id.trim().to_string()).filter(|id| !id.is_empty()),
            customer_id: input.customer_id.map(|id| id.trim().to_string()).filter(|id| !id.is_empty()),
            kind: computed.kind,
            basis: computed.basis,
            period_start: input.period_start,
            period_end: input.period_end,
            change_at: input.change_at,
            period_days: computed.period_days,
            days_used: computed.days_used,
            days_remaining: computed.days_remaining,
            old_daily_rate_scaled: computed.old_daily_rate_scaled,
            new_daily_rate_scaled: computed.new_daily_rate_scaled,
            unused_credit_scaled: computed.unused_credit_scaled,
            new_charge_scaled: computed.new_charge_scaled,
            net_due_scaled: computed.net_due_scaled,
            currency: computed.currency,
            lines: computed.lines,
            created_by: call.actor,
            created_at: now(),
        };
        state.prorations.insert(record.id.clone(), record.clone());
        state.emit(
            "pricing.proration.computed",
            json!({
                "tenant_id": record.tenant_id,
                "proration_id": record.id,
                "subscription_id": record.subscription_id,
                "kind": record.kind,
                "net_due_scaled": record.net_due_scaled,
                "currency": record.currency,
            }),
        );
        Ok(record)
    })();
    match result {
        Ok(item) => json::success_response(item, 201),
        Err(error) => json::error_response(&error, debug),
    }
}

pub(crate) async fn list_prorations(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<PricingState>>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let params = query.into_inner();
    let tenant_id = match caller(&req).and_then(|call| {
        require_permission(&ctx, &call.role, PERM_PRICING_READ).map(|_| call.tenant_id)
    }) {
        Ok(tenant_id) => tenant_id,
        Err(error) => return json::error_response(&error, debug),
    };
    let mut items: Vec<PricingProration> = state
        .prorations
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|item| {
            item.tenant_id == tenant_id
                && params
                    .filters
                    .get("subscription_id")
                    .is_none_or(|value| item.subscription_id.as_deref() == Some(value.as_str()))
                && params
                    .filters
                    .get("customer_id")
                    .is_none_or(|value| item.customer_id.as_deref() == Some(value.as_str()))
                && params
                    .filters
                    .get("kind")
                    .is_none_or(|value| item.kind == *value)
        })
        .collect();
    items.sort_by(|a, b| b.created_at.cmp(&a.created_at));
    if params.normalized().order == "asc" {
        items.reverse();
    }
    json::success_response(paginate(items, &params), 200)
}

pub(crate) async fn get_proration(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<PricingState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<PricingProration> = (|| {
        let call = caller(&req)?;
        require_permission(&ctx, &call.role, PERM_PRICING_READ)?;
        state
            .prorations
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Proration not found"))
            .and_then(|item| require_same_tenant(&item.tenant_id, &call.tenant_id).map(|_| item))
    })();
    json::respond(result, ctx.is_debug())
}
