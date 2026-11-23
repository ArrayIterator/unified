// pricing — handlers rules.

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
use super::handlers_coupons::*;
use super::handlers_prices::*;
use super::handlers_bundles::*;
use super::handlers_reconciliation::*;
use super::module::*;

// ---------------------------------------------------------------------------
// HTTP handlers: discount rules.
// ---------------------------------------------------------------------------

pub(crate) fn rule_from_create(
    state: &PricingState,
    input: CreatePricingRule,
    actor: &str,
    tenant_id: &str,
) -> UnifiedResult<PricingRule> {
    let name = require_text(&input.name, "name", 128)?;
    if !DISCOUNT_KINDS.contains(&input.kind.as_str()) {
        return Err(UnifiedError::bad_request(
            "Rule kind must be \"percent\", \"fixed\", \"buy_x_get_y\", or \"volume\"",
        ));
    }
    let currency = input
        .currency
        .unwrap_or_else(|| state.settings_for(tenant_id).default_currency)
        .to_uppercase();
    require_currency_known(state, &currency)?;
    if input.kind == "percent" || input.kind == "volume" {
        if !(0..=BASIS_POINTS).contains(&input.percent_bp) {
            return Err(UnifiedError::bad_request(
                "Discount percent must be between 0 and 10000 basis points",
            ));
        }
    }
    if input.kind == "volume" && input.min_quantity == 0 {
        return Err(UnifiedError::bad_request(
            "A volume rule needs a minimum quantity above zero",
        ));
    }
    if input.kind == "buy_x_get_y" && (input.buy_quantity == 0 || input.get_quantity == 0) {
        return Err(UnifiedError::bad_request(
            "A buy-X-get-Y rule needs a buy quantity and a get quantity above zero",
        ));
    }
    let amount_scaled = match &input.amount {
        Some(amount) => {
            let money = resolve_money(amount, Some(&currency))?;
            if money.currency != currency {
                return Err(UnifiedError::bad_request(
                    "Rule amount currency must match the rule currency",
                ));
            }
            round_to_minor(money.amount_scaled, &currency)
        }
        None => 0,
    };
    if input.kind == "fixed" && amount_scaled <= 0 {
        return Err(UnifiedError::bad_request(
            "A fixed rule needs an amount above zero",
        ));
    }
    let min_subtotal_scaled = match &input.min_subtotal {
        Some(amount) => {
            let money = resolve_money(amount, Some(&currency))?;
            if money.currency != currency {
                return Err(UnifiedError::bad_request(
                    "Minimum subtotal currency must match the rule currency",
                ));
            }
            round_to_minor(money.amount_scaled, &currency)
        }
        None => 0,
    };
    if let (Some(from), Some(to)) = (input.valid_from, input.valid_to) {
        if to <= from {
            return Err(UnifiedError::bad_request(
                "valid_to must be after valid_from",
            ));
        }
    }
    let status = input.status.unwrap_or_else(|| "active".to_string());
    if !["active", "draft", "archived"].contains(&status.as_str()) {
        return Err(UnifiedError::bad_request(
            "Rule status must be \"active\", \"draft\", or \"archived\"",
        ));
    }
    let now = now();
    Ok(PricingRule {
        id: new_id(),
        tenant_id: tenant_id.to_string(),
        name,
        kind: input.kind,
        percent_bp: input.percent_bp,
        amount_scaled,
        currency,
        buy_quantity: input.buy_quantity,
        get_quantity: input.get_quantity,
        min_quantity: input.min_quantity,
        min_subtotal_scaled,
        product_ids: input.product_ids,
        evaluation_order: input.evaluation_order,
        stackable: input.stackable.unwrap_or(true),
        exclusive: input.exclusive.unwrap_or(false),
        status,
        valid_from: input.valid_from,
        valid_to: input.valid_to,
        created_by: actor.to_string(),
        created_at: now,
        updated_at: now,
    })
}

pub(crate) async fn list_rules(
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
    let mut items: Vec<PricingRule> = state
        .rules
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|item| {
            item.tenant_id == tenant_id
                && matches_search(&format!("{} {} {}", item.name, item.kind, item.status), &params)
                && params
                    .filters
                    .get("kind")
                    .is_none_or(|value| item.kind == *value)
                && params
                    .filters
                    .get("status")
                    .is_none_or(|value| item.status == *value)
                && params
                    .filters
                    .get("product_id")
                    .is_none_or(|value| item.product_ids.iter().any(|id| id == value))
        })
        .collect();
    items.sort_by(|a, b| {
        a.evaluation_order
            .cmp(&b.evaluation_order)
            .then(a.created_at.cmp(&b.created_at))
    });
    if params.normalized().order == "desc" {
        items.reverse();
    }
    json::success_response(paginate(items, &params), 200)
}

pub(crate) async fn create_rule(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<PricingState>>,
    req: HttpRequest,
    body: web::Json<CreatePricingRule>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PricingRule> = (|| {
        let call = caller(&req)?;
        require_permission(&ctx, &call.role, PERM_PRICING_MANAGE)?;
        let rule = rule_from_create(&state, body.into_inner(), &call.actor, &call.tenant_id)?;
        state.rules.insert(rule.id.clone(), rule.clone());
        Ok(rule)
    })();
    match result {
        Ok(item) => json::success_response(item, 201),
        Err(error) => json::error_response(&error, debug),
    }
}

pub(crate) async fn get_rule(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<PricingState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<PricingRule> = (|| {
        let call = caller(&req)?;
        require_permission(&ctx, &call.role, PERM_PRICING_READ)?;
        state
            .rules
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Pricing rule not found"))
            .and_then(|item| require_same_tenant(&item.tenant_id, &call.tenant_id).map(|_| item))
    })();
    json::respond(result, ctx.is_debug())
}

pub(crate) async fn update_rule(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<PricingState>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdatePricingRule>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PricingRule> = (|| {
        let call = caller(&req)?;
        require_permission(&ctx, &call.role, PERM_PRICING_MANAGE)?;
        let mut rule = state
            .rules
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Pricing rule not found"))?;
        require_same_tenant(&rule.tenant_id, &call.tenant_id)?;
        let input = body.into_inner();
        if let Some(name) = input.name {
            rule.name = require_text(&name, "name", 128)?;
        }
        if let Some(percent_bp) = input.percent_bp {
            if !(0..=BASIS_POINTS).contains(&percent_bp) {
                return Err(UnifiedError::bad_request(
                    "Discount percent must be between 0 and 10000 basis points",
                ));
            }
            rule.percent_bp = percent_bp;
        }
        if let Some(amount) = input.amount {
            let money = resolve_money(&amount, Some(&rule.currency))?;
            if money.currency != rule.currency {
                return Err(UnifiedError::bad_request(
                    "Rule amount currency must match the rule currency",
                ));
            }
            rule.amount_scaled = round_to_minor(money.amount_scaled, &rule.currency);
        }
        if let Some(value) = input.buy_quantity {
            rule.buy_quantity = value;
        }
        if let Some(value) = input.get_quantity {
            rule.get_quantity = value;
        }
        if let Some(value) = input.min_quantity {
            rule.min_quantity = value;
        }
        if let Some(amount) = input.min_subtotal {
            let money = resolve_money(&amount, Some(&rule.currency))?;
            rule.min_subtotal_scaled = round_to_minor(money.amount_scaled, &rule.currency);
        }
        if let Some(ids) = input.product_ids {
            rule.product_ids = ids;
        }
        if let Some(order) = input.evaluation_order {
            rule.evaluation_order = order;
        }
        if let Some(stackable) = input.stackable {
            rule.stackable = stackable;
        }
        if let Some(exclusive) = input.exclusive {
            rule.exclusive = exclusive;
        }
        if let Some(status) = input.status {
            if !["active", "draft", "archived"].contains(&status.as_str()) {
                return Err(UnifiedError::bad_request(
                    "Rule status must be \"active\", \"draft\", or \"archived\"",
                ));
            }
            rule.status = status;
        }
        if let Some(from) = input.valid_from {
            rule.valid_from = Some(from);
        }
        if let Some(to) = input.valid_to {
            rule.valid_to = Some(to);
        }
        if let (Some(from), Some(to)) = (rule.valid_from, rule.valid_to) {
            if to <= from {
                return Err(UnifiedError::bad_request(
                    "valid_to must be after valid_from",
                ));
            }
        }
        rule.updated_at = now();
        state.rules.insert(rule.id.clone(), rule.clone());
        Ok(rule)
    })();
    match result {
        Ok(item) => json::success_response(item, 200),
        Err(error) => json::error_response(&error, debug),
    }
}

pub(crate) async fn delete_rule(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<PricingState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<Value> = (|| {
        let call = caller(&req)?;
        require_permission(&ctx, &call.role, PERM_PRICING_MANAGE)?;
        let existing = state
            .rules
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Pricing rule not found"))?;
        require_same_tenant(&existing.tenant_id, &call.tenant_id)?;
        state
            .rules
            .remove(path.as_str())
            .map(|_| json!({ "deleted": true, "id": path.as_str() }))
            .ok_or_else(|| UnifiedError::not_found("Pricing rule not found"))
    })();
    json::respond(result, ctx.is_debug())
}
