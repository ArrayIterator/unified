// pricing - handlers prices.

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
use super::handlers_bundles::*;
use super::handlers_reconciliation::*;
use super::module::*;

// HTTP handlers: market reference prices.

pub(crate) async fn list_reference_prices(
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
    let mut items: Vec<PricingReferencePrice> = state
        .reference_prices
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|item| {
            item.tenant_id == tenant_id
                && matches_search(&format!("{} {} {}", item.product_id, item.source, item.note), &params)
                && params
                    .filters
                    .get("product_id")
                    .is_none_or(|value| item.product_id == *value)
                && params
                    .filters
                    .get("source")
                    .is_none_or(|value| item.source == *value)
        })
        .collect();
    items.sort_by(|a, b| b.observed_at.cmp(&a.observed_at));
    if params.normalized().order == "asc" {
        items.reverse();
    }
    json::success_response(paginate(items, &params), 200)
}

pub(crate) async fn create_reference_price(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<PricingState>>,
    req: HttpRequest,
    body: web::Json<CreateReferencePrice>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PricingReferencePrice> = (|| {
        let call = caller(&req)?;
        require_permission(&ctx, &call.role, PERM_PRICING_MANAGE)?;
        let input = body.into_inner();
        let product_id = require_text(&input.product_id, "product_id", 128)?;
        let source = require_text(&input.source, "source", 128)?;
        let money = resolve_money(&input.price, None)?;
        require_currency_known(&state, &money.currency)?;
        if let Some(url) = &input.source_url {
            let trimmed = url.trim();
            if !trimmed.is_empty()
                && !trimmed.starts_with("http://")
                && !trimmed.starts_with("https://")
            {
                return Err(UnifiedError::bad_request(
                    "source_url must start with http:// or https://",
                ));
            }
        }
        let now = now();
        let record = PricingReferencePrice {
            id: new_id(),
            tenant_id: call.tenant_id.clone(),
            product_id,
            source,
            source_url: input.source_url.map(|url| url.trim().to_string()).filter(|url| !url.is_empty()),
            price_scaled: round_to_minor(money.amount_scaled, &money.currency),
            currency: money.currency.clone(),
            price: format_scaled(round_to_minor(money.amount_scaled, &money.currency), &money.currency),
            observed_at: input.observed_at,
            note: input.note.unwrap_or_default().trim().to_string(),
            entered_by: call.actor,
            created_at: now,
        };
        state
            .reference_prices
            .insert(record.id.clone(), record.clone());
        Ok(record)
    })();
    match result {
        Ok(item) => json::success_response(item, 201),
        Err(error) => json::error_response(&error, debug),
    }
}

pub(crate) async fn get_reference_price(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<PricingState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<PricingReferencePrice> = (|| {
        let call = caller(&req)?;
        require_permission(&ctx, &call.role, PERM_PRICING_READ)?;
        state
            .reference_prices
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Reference price not found"))
            .and_then(|item| require_same_tenant(&item.tenant_id, &call.tenant_id).map(|_| item))
    })();
    json::respond(result, ctx.is_debug())
}

pub(crate) async fn delete_reference_price(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<PricingState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<Value> = (|| {
        let call = caller(&req)?;
        require_permission(&ctx, &call.role, PERM_PRICING_MANAGE)?;
        let existing = state
            .reference_prices
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Reference price not found"))?;
        require_same_tenant(&existing.tenant_id, &call.tenant_id)?;
        state
            .reference_prices
            .remove(path.as_str())
            .map(|_| json!({ "deleted": true, "id": path.as_str() }))
            .ok_or_else(|| UnifiedError::not_found("Reference price not found"))
    })();
    json::respond(result, ctx.is_debug())
}

/// Summarize the human-entered reference prices for one product:
/// count, lowest, highest, and median. References are data, never
/// scraped; a product with no references answers with a count of
/// zero rather than an invented figure.
pub(crate) async fn summarize_reference_prices(
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
    let product_id = params.filters.get("product_id").cloned().unwrap_or_default();
    if product_id.trim().is_empty() {
        return json::error_response(
            &UnifiedError::bad_request("Query parameter product_id is required"),
            debug,
        );
    }
    let mut by_currency: Vec<(String, Vec<i64>)> = Vec::new();
    for item in state
        .reference_prices
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|item| item.tenant_id == tenant_id && item.product_id == product_id)
    {
        match by_currency.iter_mut().find(|(currency, _)| *currency == item.currency) {
            Some((_, prices)) => prices.push(item.price_scaled),
            None => by_currency.push((item.currency, vec![item.price_scaled])),
        }
    }
    by_currency.sort_by(|a, b| a.0.cmp(&b.0));
    // One summary per currency: references in different currencies
    // are never averaged together.
    let mut summaries: Vec<Value> = Vec::new();
    for (currency, prices) in by_currency {
        if let Some((lowest, highest, median, count)) = summarize_references(&prices) {
            summaries.push(json!({
                "currency": currency,
                "count": count,
                "lowest_scaled": lowest,
                "lowest": format_scaled(lowest, &currency),
                "highest_scaled": highest,
                "highest": format_scaled(highest, &currency),
                "median_scaled": median,
                "median": format_scaled(median, &currency),
            }));
        }
    }
    json::success_response(
        json!({ "product_id": product_id, "summaries": summaries }),
        200,
    )
}

// HTTP handlers: price versions and approvals.

pub(crate) async fn list_price_versions(
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
    let mut items: Vec<PricingPriceVersion> = state
        .price_versions
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|item| {
            item.tenant_id == tenant_id
                && matches_search(&format!("{} {}", item.product_id, item.status), &params)
                && params
                    .filters
                    .get("product_id")
                    .is_none_or(|value| item.product_id == *value)
                && params
                    .filters
                    .get("status")
                    .is_none_or(|value| item.status == *value)
        })
        .collect();
    items.sort_by(|a, b| {
        b.product_id
            .cmp(&a.product_id)
            .then(b.version.cmp(&a.version))
    });
    if params.normalized().order == "asc" {
        items.reverse();
    }
    json::success_response(paginate(items, &params), 200)
}

pub(crate) async fn create_price_version(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<PricingState>>,
    req: HttpRequest,
    body: web::Json<CreatePriceVersion>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PricingPriceVersion> = (|| {
        let call = caller(&req)?;
        require_permission(&ctx, &call.role, PERM_PRICING_MANAGE)?;
        let input = body.into_inner();
        let product_id = require_text(&input.product_id, "product_id", 128)?;
        let money = resolve_money(&input.price, None)?;
        require_currency_known(&state, &money.currency)?;
        let price_scaled = round_to_minor(money.amount_scaled, &money.currency);
        if let Some(recommendation_id) = &input.recommendation_id {
            match state.recommendations.get(recommendation_id.as_str()) {
                Some(entry) if entry.tenant_id == call.tenant_id => {}
                _ => {
                    return Err(UnifiedError::not_found(
                        "Recommendation not found",
                    ));
                }
            }
        }
        let settings = state.settings_for(&call.tenant_id);
        let previous = state
            .active_price_for(&call.tenant_id, &product_id)
            .map(|version| version.price_scaled)
            .unwrap_or(0);
        let change_bp = price_change_bp(previous, price_scaled);
        let above_threshold =
            needs_second_approver(previous, price_scaled, settings.approval_threshold_bp);
        let now = now();
        let version = PricingPriceVersion {
            id: new_id(),
            tenant_id: call.tenant_id.clone(),
            product_id: product_id.clone(),
            version: state.next_price_version(&call.tenant_id, &product_id),
            price_scaled,
            currency: money.currency.clone(),
            price: format_scaled(price_scaled, &money.currency),
            previous_price_scaled: previous,
            change_bp,
            above_threshold,
            status: "draft".to_string(),
            requested_by: call.actor,
            approved_by: None,
            reason: input.reason.map(|reason| reason.trim().to_string()).filter(|reason| !reason.is_empty()),
            recommendation_id: input.recommendation_id,
            created_at: now,
            updated_at: now,
        };
        state
            .price_versions
            .insert(version.id.clone(), version.clone());
        Ok(version)
    })();
    match result {
        Ok(item) => json::success_response(item, 201),
        Err(error) => json::error_response(&error, debug),
    }
}

pub(crate) async fn get_price_version(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<PricingState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<PricingPriceVersion> = (|| {
        let call = caller(&req)?;
        require_permission(&ctx, &call.role, PERM_PRICING_READ)?;
        state
            .price_versions
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Price version not found"))
            .and_then(|item| require_same_tenant(&item.tenant_id, &call.tenant_id).map(|_| item))
    })();
    json::respond(result, ctx.is_debug())
}

pub(crate) fn fetch_price_version(
    state: &PricingState,
    id: &str,
    tenant_id: &str,
) -> UnifiedResult<PricingPriceVersion> {
    let version = state
        .price_versions
        .get(id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found("Price version not found"))?;
    require_same_tenant(&version.tenant_id, tenant_id)?;
    Ok(version)
}

pub(crate) fn save_price_version(
    state: &PricingState,
    mut version: PricingPriceVersion,
    event: &str,
) -> PricingPriceVersion {
    version.updated_at = now();
    state
        .price_versions
        .insert(version.id.clone(), version.clone());
    state.emit(
        event,
        json!({
            "tenant_id": version.tenant_id,
            "price_version_id": version.id,
            "product_id": version.product_id,
            "version": version.version,
            "status": version.status,
            "price_scaled": version.price_scaled,
            "currency": version.currency,
        }),
    );
    version
}

/// Only a version waiting in `pending_approval` may be approved
/// or rejected. A `draft` must go through submit first: submit is
/// the step that applies the threshold rule (below threshold it
/// approves directly, above threshold it waits for a second
/// approver) and records the submitted audit event. Approving or
/// rejecting a draft would skip that step entirely.
pub(crate) fn require_pending_approval(
    version: &PricingPriceVersion,
    action: &str,
) -> UnifiedResult<()> {
    if version.status == "pending_approval" {
        Ok(())
    } else {
        Err(UnifiedError::unprocessable(format!(
            "Only a pending_approval price version can be {action}"
        )))
    }
}

/// Submit a draft: below the threshold it moves straight to
/// `approved`; above the threshold it moves to
/// `pending_approval` and waits for a second approver.
pub(crate) async fn submit_price_version(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<PricingState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PricingPriceVersion> = (|| {
        let call = caller(&req)?;
        require_permission(&ctx, &call.role, PERM_PRICING_MANAGE)?;
        let mut version = fetch_price_version(&state, path.as_str(), &call.tenant_id)?;
        if version.status != "draft" {
            return Err(UnifiedError::unprocessable(
                "Only a draft price version can be submitted",
            ));
        }
        version.status = if version.above_threshold {
            "pending_approval".to_string()
        } else {
            "approved".to_string()
        };
        Ok(save_price_version(&state, version, "pricing.price_version.submitted"))
    })();
    match result {
        Ok(item) => json::success_response(item, 200),
        Err(error) => json::error_response(&error, debug),
    }
}

/// Approve a pending version. The approver must hold
/// `pricing:approve` and must not be the requester when the
/// settings require a second approver: the second pair of eyes
/// is the point of the threshold rule.
pub(crate) async fn approve_price_version(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<PricingState>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<PriceApprovalInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PricingPriceVersion> = (|| {
        let call = caller(&req)?;
        require_permission(&ctx, &call.role, PERM_PRICING_APPROVE)?;
        let mut version = fetch_price_version(&state, path.as_str(), &call.tenant_id)?;
        require_pending_approval(&version, "approved")?;
        let settings = state.settings_for(&call.tenant_id);
        if settings.require_second_approver && version.above_threshold && version.requested_by == call.actor {
            return Err(UnifiedError::forbidden(
                "The requester cannot approve their own price change above the threshold: a second approver is required",
            ));
        }
        version.status = "approved".to_string();
        version.approved_by = Some(call.actor.clone());
        let approval = PricingPriceApproval {
            id: new_id(),
            tenant_id: call.tenant_id.clone(),
            price_version_id: version.id.clone(),
            approver: call.actor.clone(),
            decision: "approved".to_string(),
            reason: body.reason.clone().map(|reason| reason.trim().to_string()).filter(|reason| !reason.is_empty()),
            decided_at: now(),
        };
        state
            .price_approvals
            .insert(approval.id.clone(), approval);
        Ok(save_price_version(&state, version, "pricing.price_version.approved"))
    })();
    match result {
        Ok(item) => json::success_response(item, 200),
        Err(error) => json::error_response(&error, debug),
    }
}

/// Reject a pending version: it returns to `draft` so the
/// requester can revise and resubmit it.
pub(crate) async fn reject_price_version(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<PricingState>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<PriceApprovalInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PricingPriceVersion> = (|| {
        let call = caller(&req)?;
        require_permission(&ctx, &call.role, PERM_PRICING_APPROVE)?;
        let mut version = fetch_price_version(&state, path.as_str(), &call.tenant_id)?;
        require_pending_approval(&version, "rejected")?;
        version.status = "draft".to_string();
        let approval = PricingPriceApproval {
            id: new_id(),
            tenant_id: call.tenant_id.clone(),
            price_version_id: version.id.clone(),
            approver: call.actor.clone(),
            decision: "rejected".to_string(),
            reason: body.reason.clone().map(|reason| reason.trim().to_string()).filter(|reason| !reason.is_empty()),
            decided_at: now(),
        };
        state
            .price_approvals
            .insert(approval.id.clone(), approval);
        Ok(save_price_version(&state, version, "pricing.price_version.rejected"))
    })();
    match result {
        Ok(item) => json::success_response(item, 200),
        Err(error) => json::error_response(&error, debug),
    }
}

/// Activate an approved version: it becomes the product's current
/// price, and any other active version for the same product in
/// the same tenant is superseded back to `approved`.
pub(crate) async fn activate_price_version(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<PricingState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PricingPriceVersion> = (|| {
        let call = caller(&req)?;
        require_permission(&ctx, &call.role, PERM_PRICING_MANAGE)?;
        let mut version = fetch_price_version(&state, path.as_str(), &call.tenant_id)?;
        if version.status != "approved" {
            return Err(UnifiedError::unprocessable(
                "Only an approved price version can be activated",
            ));
        }
        for mut entry in state.price_versions.iter_mut() {
            let other = entry.value_mut();
            if other.tenant_id == call.tenant_id
                && other.product_id == version.product_id
                && other.id != version.id
                && other.status == "active"
            {
                other.status = "approved".to_string();
                other.updated_at = now();
            }
        }
        version.status = "active".to_string();
        Ok(save_price_version(&state, version, "pricing.price_version.activated"))
    })();
    match result {
        Ok(item) => json::success_response(item, 200),
        Err(error) => json::error_response(&error, debug),
    }
}

pub(crate) async fn list_price_approvals(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<PricingState>>,
    req: HttpRequest,
    path: web::Path<String>,
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
    match fetch_price_version(&state, path.as_str(), &tenant_id) {
        Ok(_) => {}
        Err(error) => return json::error_response(&error, debug),
    }
    let mut items: Vec<PricingPriceApproval> = state
        .price_approvals
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|item| {
            item.tenant_id == tenant_id
                && item.price_version_id == path.as_str()
                && params
                    .filters
                    .get("decision")
                    .is_none_or(|value| item.decision == *value)
        })
        .collect();
    items.sort_by(|a, b| b.decided_at.cmp(&a.decided_at));
    json::success_response(paginate(items, &params), 200)
}

// HTTP handlers: tiers and bundles.

pub(crate) async fn list_tiers(
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
    let mut items: Vec<PricingTier> = state
        .tiers
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|item| {
            item.tenant_id == tenant_id
                && params
                    .filters
                    .get("product_id")
                    .is_none_or(|value| item.product_id == *value)
        })
        .collect();
    items.sort_by(|a, b| {
        a.product_id
            .cmp(&b.product_id)
            .then(a.min_quantity.cmp(&b.min_quantity))
    });
    if params.normalized().order == "desc" {
        items.reverse();
    }
    json::success_response(paginate(items, &params), 200)
}

pub(crate) async fn create_tier(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<PricingState>>,
    req: HttpRequest,
    body: web::Json<CreatePricingTier>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PricingTier> = (|| {
        let call = caller(&req)?;
        require_permission(&ctx, &call.role, PERM_PRICING_MANAGE)?;
        let input = body.into_inner();
        let product_id = require_text(&input.product_id, "product_id", 128)?;
        if input.min_quantity == 0 {
            return Err(UnifiedError::bad_request(
                "Tier minimum quantity must be at least 1",
            ));
        }
        if let Some(max) = input.max_quantity {
            if max < input.min_quantity {
                return Err(UnifiedError::bad_request(
                    "Tier maximum quantity must not be below its minimum",
                ));
            }
        }
        let money = resolve_money(&input.unit_price, None)?;
        require_currency_known(&state, &money.currency)?;
        // Overlap check against the tenant's tiers for this product.
        for entry in state.tiers.iter() {
            let other = entry.value();
            if other.tenant_id != call.tenant_id || other.product_id != product_id {
                continue;
            }
            let other_max = other.max_quantity.unwrap_or(u32::MAX);
            let new_max = input.max_quantity.unwrap_or(u32::MAX);
            if input.min_quantity <= other_max && other.min_quantity <= new_max {
                return Err(UnifiedError::conflict(
                    "Tier overlaps an existing tier for this product",
                ));
            }
        }
        let now = now();
        let tier = PricingTier {
            id: new_id(),
            tenant_id: call.tenant_id.clone(),
            product_id,
            min_quantity: input.min_quantity,
            max_quantity: input.max_quantity,
            unit_price_scaled: round_to_minor(money.amount_scaled, &money.currency),
            currency: money.currency,
            created_at: now,
            updated_at: now,
        };
        state.tiers.insert(tier.id.clone(), tier.clone());
        Ok(tier)
    })();
    match result {
        Ok(item) => json::success_response(item, 201),
        Err(error) => json::error_response(&error, debug),
    }
}

pub(crate) async fn delete_tier(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<PricingState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<Value> = (|| {
        let call = caller(&req)?;
        require_permission(&ctx, &call.role, PERM_PRICING_MANAGE)?;
        let existing = state
            .tiers
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Tier not found"))?;
        require_same_tenant(&existing.tenant_id, &call.tenant_id)?;
        state
            .tiers
            .remove(path.as_str())
            .map(|_| json!({ "deleted": true, "id": path.as_str() }))
            .ok_or_else(|| UnifiedError::not_found("Tier not found"))
    })();
    json::respond(result, ctx.is_debug())
}

/// Price a quantity against the tenant's stored tiers for a
/// product, using the same pure engine as the recommendation.
pub(crate) async fn price_with_stored_tiers(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<PricingState>>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let params = query.into_inner();
    let result: UnifiedResult<TieredPriceResult> = (|| {
        let call = caller(&req)?;
        require_permission(&ctx, &call.role, PERM_PRICING_READ)?;
        let product_id = params
            .filters
            .get("product_id")
            .cloned()
            .unwrap_or_default();
        if product_id.trim().is_empty() {
            return Err(UnifiedError::bad_request(
                "Query parameter product_id is required",
            ));
        }
        let quantity: u32 = params
            .filters
            .get("quantity")
            .and_then(|value| value.parse().ok())
            .unwrap_or(0);
        if quantity == 0 {
            return Err(UnifiedError::bad_request(
                "Query parameter quantity must be at least 1",
            ));
        }
        let mut tiers: Vec<TierInput> = state
            .tiers
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|tier| tier.tenant_id == call.tenant_id && tier.product_id == product_id)
            .map(|tier| TierInput {
                min_quantity: tier.min_quantity,
                max_quantity: tier.max_quantity,
                unit_price: MoneyInput {
                    amount: format_scaled(tier.unit_price_scaled, &tier.currency),
                    currency: Some(tier.currency.clone()),
                },
            })
            .collect();
        tiers.sort_by_key(|tier| tier.min_quantity);
        tiered_price(&tiers, quantity)
    })();
    match result {
        Ok(value) => json::success_response(value, 200),
        Err(error) => json::error_response(&error, debug),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_version(status: &str) -> PricingPriceVersion {
        let now = Utc::now();
        PricingPriceVersion {
            id: "version-1".to_string(),
            tenant_id: "019a1234-5678-7abc-8def-0123456789ab".to_string(),
            product_id: "product-1".to_string(),
            version: 1,
            price_scaled: 1_100_000,
            currency: "USD".to_string(),
            price: "110.00".to_string(),
            previous_price_scaled: 1_000_000,
            change_bp: Some(1_000),
            above_threshold: true,
            status: status.to_string(),
            requested_by: "requester".to_string(),
            approved_by: None,
            reason: None,
            recommendation_id: None,
            created_at: now,
            updated_at: now,
        }
    }

    #[test]
    fn approve_and_reject_require_pending_approval_not_draft() {
        // A draft must go through submit first; approving or
        // rejecting it directly would skip the submit step and its
        // audit event.
        assert!(require_pending_approval(&test_version("draft"), "approved").is_err());
        assert!(require_pending_approval(&test_version("draft"), "rejected").is_err());
        assert!(require_pending_approval(&test_version("pending_approval"), "approved").is_ok());
        assert!(require_pending_approval(&test_version("pending_approval"), "rejected").is_ok());
        assert!(require_pending_approval(&test_version("approved"), "approved").is_err());
        assert!(require_pending_approval(&test_version("active"), "rejected").is_err());
    }
}
