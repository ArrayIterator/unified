// pricing - handlers recommendations.

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
use super::handlers_rules::*;
use super::handlers_coupons::*;
use super::handlers_prices::*;
use super::handlers_bundles::*;
use super::handlers_reconciliation::*;
use super::module::*;

pub(crate) async fn create_cost_plus_recommendation(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<PricingState>>,
    req: HttpRequest,
    body: web::Json<CreateCostPlusRecommendation>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PricingRecommendation> = (|| {
        let call = caller(&req)?;
        require_permission(&ctx, &call.role, PERM_PRICING_MANAGE)?;
        let input = body.into_inner();
        let product_id = require_text(&input.product_id, "product_id", 128)?;
        let computed = cost_plus_recommendation(&input.costs, input.margin_bp)?;
        let inputs = serde_json::to_value(&input).unwrap_or(Value::Null);
        Ok(store_recommendation(
            &state,
            &call.tenant_id,
            &product_id,
            "cost_plus",
            computed.suggested_price_scaled,
            &computed.currency,
            computed.lines,
            inputs,
            &call.actor,
            &input.costs,
        ))
    })();
    match result {
        Ok(item) => json::success_response(item, 201),
        Err(error) => json::error_response(&error, debug),
    }
}

pub(crate) async fn create_tiered_recommendation(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<PricingState>>,
    req: HttpRequest,
    body: web::Json<CreateTieredRecommendation>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PricingRecommendation> = (|| {
        let call = caller(&req)?;
        require_permission(&ctx, &call.role, PERM_PRICING_MANAGE)?;
        let input = body.into_inner();
        let product_id = require_text(&input.product_id, "product_id", 128)?;
        let computed = tiered_price(&input.tiers, input.quantity)?;
        let inputs = serde_json::to_value(&input).unwrap_or(Value::Null);
        Ok(store_recommendation(
            &state,
            &call.tenant_id,
            &product_id,
            "tiered",
            computed.total_scaled,
            &computed.currency,
            computed.lines,
            inputs,
            &call.actor,
            &[],
        ))
    })();
    match result {
        Ok(item) => json::success_response(item, 201),
        Err(error) => json::error_response(&error, debug),
    }
}

pub(crate) async fn create_bundle_recommendation(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<PricingState>>,
    req: HttpRequest,
    body: web::Json<CreateBundleRecommendation>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PricingRecommendation> = (|| {
        let call = caller(&req)?;
        require_permission(&ctx, &call.role, PERM_PRICING_MANAGE)?;
        let input = body.into_inner();
        let product_id = require_text(&input.product_id, "product_id", 128)?;
        let computed = bundle_price(&input.components, &input.discount)?;
        let inputs = serde_json::to_value(&input).unwrap_or(Value::Null);
        Ok(store_recommendation(
            &state,
            &call.tenant_id,
            &product_id,
            "bundle",
            computed.total_scaled,
            &computed.currency,
            computed.lines,
            inputs,
            &call.actor,
            &[],
        ))
    })();
    match result {
        Ok(item) => json::success_response(item, 201),
        Err(error) => json::error_response(&error, debug),
    }
}

pub(crate) async fn list_recommendations(
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
    let mut items: Vec<PricingRecommendation> = state
        .recommendations
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|item| {
            item.tenant_id == tenant_id
                && matches_search(&format!("{} {} {}", item.product_id, item.kind, item.status), &params)
                && params
                    .filters
                    .get("product_id")
                    .is_none_or(|value| item.product_id == *value)
                && params
                    .filters
                    .get("kind")
                    .is_none_or(|value| item.kind == *value)
                && params
                    .filters
                    .get("status")
                    .is_none_or(|value| item.status == *value)
        })
        .collect();
    items.sort_by(|a, b| b.created_at.cmp(&a.created_at));
    if params.normalized().order == "asc" {
        items.reverse();
    }
    json::success_response(paginate(items, &params), 200)
}

pub(crate) async fn get_recommendation(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<PricingState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<PricingRecommendation> = (|| {
        let call = caller(&req)?;
        require_permission(&ctx, &call.role, PERM_PRICING_READ)?;
        state
            .recommendations
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Recommendation not found"))
            .and_then(|item| require_same_tenant(&item.tenant_id, &call.tenant_id).map(|_| item))
    })();
    json::respond(result, ctx.is_debug())
}

/// Confirm a recommendation: the admin accepts the suggestion.
/// Confirming creates a price version in `draft` (the approval
/// machine then decides whether a second approver is needed) and
/// marks the recommendation confirmed. The price is never applied
/// silently: activation is a separate, explicit step.
pub(crate) async fn confirm_recommendation(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<PricingState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PricingRecommendation> = (|| {
        let call = caller(&req)?;
        require_permission(&ctx, &call.role, PERM_PRICING_MANAGE)?;
        let mut recommendation = state
            .recommendations
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Recommendation not found"))?;
        require_same_tenant(&recommendation.tenant_id, &call.tenant_id)?;
        if recommendation.status != "suggested" {
            return Err(UnifiedError::unprocessable(
                "Only a suggested recommendation can be confirmed",
            ));
        }
        let settings = state.settings_for(&call.tenant_id);
        let previous = state
            .active_price_for(&call.tenant_id, &recommendation.product_id)
            .map(|version| version.price_scaled)
            .unwrap_or(0);
        let change_bp = price_change_bp(previous, recommendation.suggested_price_scaled);
        let above_threshold = needs_second_approver(
            previous,
            recommendation.suggested_price_scaled,
            settings.approval_threshold_bp,
        );
        let version = PricingPriceVersion {
            id: new_id(),
            tenant_id: call.tenant_id.clone(),
            product_id: recommendation.product_id.clone(),
            version: state.next_price_version(&call.tenant_id, &recommendation.product_id),
            price_scaled: recommendation.suggested_price_scaled,
            currency: recommendation.currency.clone(),
            price: recommendation.suggested_price.clone(),
            previous_price_scaled: previous,
            change_bp,
            above_threshold,
            status: "draft".to_string(),
            requested_by: call.actor.clone(),
            approved_by: None,
            reason: Some("Confirmed from a price recommendation".to_string()),
            recommendation_id: Some(recommendation.id.clone()),
            created_at: now(),
            updated_at: now(),
        };
        state
            .price_versions
            .insert(version.id.clone(), version.clone());
        recommendation.status = "confirmed".to_string();
        recommendation.confirmed_by = Some(call.actor.clone());
        recommendation.price_version_id = Some(version.id.clone());
        recommendation.updated_at = now();
        state
            .recommendations
            .insert(recommendation.id.clone(), recommendation.clone());
        state.emit(
            "pricing.recommendation.confirmed",
            json!({
                "tenant_id": call.tenant_id,
                "recommendation_id": recommendation.id,
                "product_id": recommendation.product_id,
                "price_version_id": version.id,
                "suggested_price_scaled": recommendation.suggested_price_scaled,
                "currency": recommendation.currency,
            }),
        );
        Ok(recommendation)
    })();
    match result {
        Ok(item) => json::success_response(item, 200),
        Err(error) => json::error_response(&error, debug),
    }
}

pub(crate) async fn get_recommendation_narrative(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<PricingState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<AiDraftResponse> = (|| {
        let call = caller(&req)?;
        require_permission(&ctx, &call.role, PERM_PRICING_READ)?;
        let recommendation = state
            .recommendations
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Recommendation not found"))?;
        require_same_tenant(&recommendation.tenant_id, &call.tenant_id)?;
        let draft = build_ai_narrative(&recommendation);
        state.emit(
            "pricing.ai.draft_created",
            json!({
                "tenant_id": call.tenant_id,
                "recommendation_id": recommendation.id,
                "currency": recommendation.currency,
            }),
        );
        Ok(AiDraftResponse {
            recommendation_id: recommendation.id.clone(),
            draft,
            numbers: json!({
                "suggested_price_scaled": recommendation.suggested_price_scaled,
                "suggested_price": recommendation.suggested_price,
                "currency": recommendation.currency,
                "lines": recommendation.lines,
            }),
        })
    })();
    json::respond(result, ctx.is_debug())
}
