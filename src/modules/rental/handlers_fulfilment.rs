//! HTTP handlers for hand-out / return deliveries and damage
//! reports.

use actix_web::{web, HttpRequest, HttpResponse};
use chrono::Utc;
use serde::Deserialize;
use serde_json::{json, Value};
use std::sync::Arc;
use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::pagination::PaginationParams;
use uuid::Uuid;

use super::handlers_listings::validate_photo_ids;
use super::helpers::{
    caller, caller_company, can_manage, created, err, load_listing, load_order, ok, order_view,
    paginate, require_permission, require_tenant, require_text, resolve_tenant, validate_notes,
};
use super::state::RentalState;
use super::types::{
    compute_late_fee, CreateDamageReportRequest, HandoverRequest, RentalDamageReport,
    RentalDelivery, RentalOrderStatus, ResolveDamageReportRequest, ReturnRequest,
    MAX_DESCRIPTION_LEN, PERM_RENTAL_MANAGE,
};

/// Path carrying an order id.
#[derive(Debug, Deserialize)]
struct OrderPath {
    order_id: String,
}

/// Path carrying an order id and a delivery id.
#[derive(Debug, Deserialize)]
struct DeliveryPath {
    order_id: String,
    delivery_id: String,
}

/// Path carrying an order id and a damage report id.
#[derive(Debug, Deserialize)]
struct DamagePath {
    order_id: String,
    report_id: String,
}

fn record_delivery(
    state: &RentalState,
    tenant_id: &str,
    order_id: Uuid,
    kind: &str,
    condition_notes: Option<String>,
    photo_ids: Vec<String>,
    recorded_by: &str,
) -> RentalDelivery {
    let delivery = RentalDelivery {
        id: Uuid::now_v7(),
        tenant_id: tenant_id.to_string(),
        order_id,
        kind: kind.to_string(),
        condition_notes,
        photo_ids,
        recorded_by: recorded_by.to_string(),
        created_at: Utc::now(),
    };
    state.deliveries.insert(delivery.id, delivery.clone());
    delivery
}

// ---------------------------------------------------------------------------
// Hand-out (manager): reserved -> out
// ---------------------------------------------------------------------------

pub(crate) async fn handover_order(
    state: web::Data<RentalState>,
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    path: web::Path<OrderPath>,
    body: web::Json<HandoverRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let tenant_id = resolve_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_RENTAL_MANAGE)?;
        let mut order = load_order(&state, &tenant_id, &path.order_id)?;
        if order.status != RentalOrderStatus::Reserved {
            return Err(UnifiedError::bad_request(format!(
                "Only a reserved order can be handed out, got {}",
                order.status.as_str()
            )));
        }
        let input = body.into_inner();
        let condition_notes = validate_notes(&input.condition_notes)?;
        let photo_ids = validate_photo_ids(&input.photo_ids)?;
        let delivery = record_delivery(
            &state,
            &tenant_id,
            order.id,
            "handover",
            condition_notes,
            photo_ids,
            &caller.actor,
        );
        order.status = RentalOrderStatus::Out;
        order.updated_at = Utc::now();
        state.orders.insert(order.id, order.clone());
        state.emit(
            "rental.order.out",
            json!({
                "tenant_id": tenant_id,
                "order_id": order.id.to_string(),
                "listing_id": order.listing_id.to_string(),
                "customer_id": order.customer_id,
                "delivery_id": delivery.id.to_string(),
            }),
        );
        state.notify(
            "rental.order.out",
            &order.customer_id,
            "Your rental is out",
            "Your rented unit has been handed out.",
            Some(format!("/rental/orders/{}", order.id)),
        );
        Ok(order_view(&state, &order))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

// ---------------------------------------------------------------------------
// Return (manager): out -> returned, with late-fee computation
// ---------------------------------------------------------------------------

pub(crate) async fn return_order(
    state: web::Data<RentalState>,
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    path: web::Path<OrderPath>,
    body: web::Json<ReturnRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let tenant_id = resolve_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_RENTAL_MANAGE)?;
        let mut order = load_order(&state, &tenant_id, &path.order_id)?;
        if order.status != RentalOrderStatus::Out {
            return Err(UnifiedError::bad_request(format!(
                "Only an out order can be returned, got {}",
                order.status.as_str()
            )));
        }
        let input = body.into_inner();
        let condition_notes = validate_notes(&input.condition_notes)?;
        let photo_ids = validate_photo_ids(&input.photo_ids)?;
        let returned_at = input.returned_at.unwrap_or_else(Utc::now);
        let listing = load_listing(&state, &tenant_id, &order.listing_id.to_string())?;
        let late_fee = compute_late_fee(&listing.late_fee_rule, order.ends_at, returned_at);
        let delivery = record_delivery(
            &state,
            &tenant_id,
            order.id,
            "return",
            condition_notes,
            photo_ids,
            &caller.actor,
        );
        order.late_fee_minor = late_fee;
        order.total_minor = order.rental_amount_minor + late_fee + order.damage_total_minor;
        order.status = RentalOrderStatus::Returned;
        order.updated_at = Utc::now();
        state.orders.insert(order.id, order.clone());
        state.emit(
            "rental.order.returned",
            json!({
                "tenant_id": tenant_id,
                "order_id": order.id.to_string(),
                "listing_id": order.listing_id.to_string(),
                "customer_id": order.customer_id,
                "delivery_id": delivery.id.to_string(),
                "late_fee_minor": late_fee,
                "returned_at": returned_at.to_rfc3339(),
            }),
        );
        if late_fee > 0 {
            state.emit(
                "rental.order.late",
                json!({
                    "tenant_id": tenant_id,
                    "order_id": order.id.to_string(),
                    "listing_id": order.listing_id.to_string(),
                    "customer_id": order.customer_id,
                    "late_fee_minor": late_fee,
                    "due_at": order.ends_at.to_rfc3339(),
                    "returned_at": returned_at.to_rfc3339(),
                }),
            );
        }
        state.notify(
            "rental.order.returned",
            &order.customer_id,
            "Your rental was returned",
            &format!("Late fee charged: {late_fee} minor units."),
            Some(format!("/rental/orders/{}", order.id)),
        );
        Ok(order_view(&state, &order))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

// ---------------------------------------------------------------------------
// Deliveries (authenticated)
// ---------------------------------------------------------------------------

pub(crate) async fn list_deliveries(
    state: web::Data<RentalState>,
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    path: web::Path<OrderPath>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let tenant_id = resolve_tenant(&req)?;
        let caller = caller(&req)?;
        let order = load_order(&state, &tenant_id, &path.order_id)?;
        if order.customer_id != caller.actor && !can_manage(&ctx, &caller.role) {
            return Err(UnifiedError::not_found(format!(
                "Order not found: {}",
                order.id
            )));
        }
        let params = query.into_inner();
        let kind_filter = params.filters.get("kind").cloned();
        let mut items: Vec<RentalDelivery> = state
            .deliveries
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|delivery| {
                if delivery.tenant_id != tenant_id || delivery.order_id != order.id {
                    return false;
                }
                if let Some(kind) = &kind_filter {
                    if delivery.kind != *kind {
                        return false;
                    }
                }
                true
            })
            .collect();
        items.sort_by_key(|delivery| delivery.created_at);
        Ok(paginate(items, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn get_delivery(
    state: web::Data<RentalState>,
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    path: web::Path<DeliveryPath>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<RentalDelivery> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let tenant_id = resolve_tenant(&req)?;
        let caller = caller(&req)?;
        let order = load_order(&state, &tenant_id, &path.order_id)?;
        if order.customer_id != caller.actor && !can_manage(&ctx, &caller.role) {
            return Err(UnifiedError::not_found(format!(
                "Order not found: {}",
                order.id
            )));
        }
        let delivery = state
            .deliveries
            .get(&Uuid::parse_str(&path.delivery_id).map_err(|_| {
                UnifiedError::bad_request(format!("Invalid delivery id: {}", path.delivery_id))
            })?)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| {
                UnifiedError::not_found(format!("Delivery not found: {}", path.delivery_id))
            })?;
        if delivery.tenant_id != tenant_id || delivery.order_id != order.id {
            return Err(UnifiedError::not_found(format!(
                "Delivery not found: {}",
                delivery.id
            )));
        }
        Ok(delivery)
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

// ---------------------------------------------------------------------------
// Damage reports (authenticated)
// ---------------------------------------------------------------------------

pub(crate) async fn list_damage_reports(
    state: web::Data<RentalState>,
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    path: web::Path<OrderPath>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let tenant_id = resolve_tenant(&req)?;
        let caller = caller(&req)?;
        let order = load_order(&state, &tenant_id, &path.order_id)?;
        if order.customer_id != caller.actor && !can_manage(&ctx, &caller.role) {
            return Err(UnifiedError::not_found(format!(
                "Order not found: {}",
                order.id
            )));
        }
        let params = query.into_inner();
        let status_filter = params.filters.get("status").cloned();
        let mut items: Vec<RentalDamageReport> = state
            .damage_reports
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|report| {
                if report.tenant_id != tenant_id || report.order_id != order.id {
                    return false;
                }
                if let Some(status) = &status_filter {
                    if report.status != *status {
                        return false;
                    }
                }
                true
            })
            .collect();
        items.sort_by_key(|report| report.created_at);
        Ok(paginate(items, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn create_damage_report(
    state: web::Data<RentalState>,
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    path: web::Path<OrderPath>,
    body: web::Json<CreateDamageReportRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<RentalDamageReport> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let tenant_id = resolve_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_RENTAL_MANAGE)?;
        let order = load_order(&state, &tenant_id, &path.order_id)?;
        if order.status != RentalOrderStatus::Out && order.status != RentalOrderStatus::Returned {
            return Err(UnifiedError::bad_request(
                "Damage can only be reported while an order is out or returned",
            ));
        }
        let input = body.into_inner();
        let description = require_text(&input.description, "description", MAX_DESCRIPTION_LEN)?;
        if input.estimated_cost_minor < 0 {
            return Err(UnifiedError::bad_request(
                "estimated_cost_minor must not be negative",
            ));
        }
        if let Some(delivery_id) = input.delivery_id {
            let delivery = state
                .deliveries
                .get(&delivery_id)
                .map(|entry| entry.value().clone())
                .ok_or_else(|| {
                    UnifiedError::not_found(format!("Delivery not found: {delivery_id}"))
                })?;
            if delivery.tenant_id != tenant_id || delivery.order_id != order.id {
                return Err(UnifiedError::not_found(format!(
                    "Delivery not found: {delivery_id}"
                )));
            }
        }
        let now = Utc::now();
        let report = RentalDamageReport {
            id: Uuid::now_v7(),
            tenant_id: tenant_id.clone(),
            order_id: order.id,
            delivery_id: input.delivery_id,
            description,
            estimated_cost_minor: input.estimated_cost_minor,
            status: "open".to_string(),
            reported_by: caller.actor.clone(),
            resolved_by: None,
            resolved_at: None,
            created_at: now,
            updated_at: now,
        };
        state.damage_reports.insert(report.id, report.clone());
        state.emit(
            "rental.damage.reported",
            json!({
                "tenant_id": tenant_id,
                "order_id": order.id.to_string(),
                "report_id": report.id.to_string(),
                "estimated_cost_minor": report.estimated_cost_minor,
                "reported_by": report.reported_by,
            }),
        );
        Ok(report)
    })();
    match result {
        Ok(data) => created(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn get_damage_report(
    state: web::Data<RentalState>,
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    path: web::Path<DamagePath>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<RentalDamageReport> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let tenant_id = resolve_tenant(&req)?;
        let caller = caller(&req)?;
        let order = load_order(&state, &tenant_id, &path.order_id)?;
        if order.customer_id != caller.actor && !can_manage(&ctx, &caller.role) {
            return Err(UnifiedError::not_found(format!(
                "Order not found: {}",
                order.id
            )));
        }
        let report = load_damage_report(&state, &tenant_id, &order.id, &path.report_id)?;
        Ok(report)
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

fn load_damage_report(
    state: &RentalState,
    tenant_id: &str,
    order_id: &Uuid,
    raw_id: &str,
) -> UnifiedResult<RentalDamageReport> {
    let id = Uuid::parse_str(raw_id)
        .map_err(|_| UnifiedError::bad_request(format!("Invalid report id: {raw_id}")))?;
    let report = state
        .damage_reports
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Damage report not found: {id}")))?;
    if report.tenant_id != tenant_id || report.order_id != *order_id {
        return Err(UnifiedError::not_found(format!(
            "Damage report not found: {id}"
        )));
    }
    Ok(report)
}

pub(crate) async fn resolve_damage_report(
    state: web::Data<RentalState>,
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    path: web::Path<DamagePath>,
    body: web::Json<ResolveDamageReportRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<RentalDamageReport> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let tenant_id = resolve_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_RENTAL_MANAGE)?;
        let order = load_order(&state, &tenant_id, &path.order_id)?;
        let mut report = load_damage_report(&state, &tenant_id, &order.id, &path.report_id)?;
        if report.status != "open" {
            return Err(UnifiedError::bad_request(format!(
                "Only an open report can be resolved, got {}",
                report.status
            )));
        }
        let input = body.into_inner();
        let now = Utc::now();
        match input.status.as_str() {
            "resolved" => {
                report.status = "resolved".to_string();
                report.resolved_by = Some(caller.actor.clone());
                report.resolved_at = Some(now);
                report.updated_at = now;
                state.damage_reports.insert(report.id, report.clone());
                // Charge the estimated cost to the order exactly
                // once: the report leaves `open`, so a second
                // resolve is rejected above and cannot double
                // the total.
                if let Some(mut order_row) = state.orders.get_mut(&order.id) {
                    order_row.damage_total_minor += report.estimated_cost_minor;
                    order_row.total_minor = order_row.rental_amount_minor
                        + order_row.late_fee_minor
                        + order_row.damage_total_minor;
                    order_row.updated_at = now;
                }
                state.emit(
                    "rental.damage.resolved",
                    json!({
                        "tenant_id": tenant_id,
                        "order_id": order.id.to_string(),
                        "report_id": report.id.to_string(),
                        "estimated_cost_minor": report.estimated_cost_minor,
                    }),
                );
            }
            "dismissed" => {
                report.status = "dismissed".to_string();
                report.resolved_by = Some(caller.actor.clone());
                report.resolved_at = Some(now);
                report.updated_at = now;
                state.damage_reports.insert(report.id, report.clone());
                state.emit(
                    "rental.damage.dismissed",
                    json!({
                        "tenant_id": tenant_id,
                        "order_id": order.id.to_string(),
                        "report_id": report.id.to_string(),
                    }),
                );
            }
            other => {
                return Err(UnifiedError::bad_request(format!(
                    "status must be resolved or dismissed, got \"{other}\""
                )));
            }
        }
        Ok(report)
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}
