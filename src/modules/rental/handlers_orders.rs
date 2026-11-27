//! HTTP handlers for rental orders: creation with pricing and
//! availability checks, status transitions, and deposit
//! handling.

use actix_web::{web, HttpRequest, HttpResponse};
use chrono::{DateTime, Utc};
use serde::Deserialize;
use serde_json::{json, Value};
use std::sync::Arc;
use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::pagination::PaginationParams;
use uuid::Uuid;

use super::helpers::{
    caller, caller_company, can_manage, check_availability, created, err, load_listing, load_order,
    load_rate, ok, order_view, paginate, require_permission, require_tenant, resolve_tenant,
};
use super::state::RentalState;
use super::types::{
    can_transition, compute_rental_amount, CreateOrderRequest, DepositStatus, RentalOrder,
    RentalOrderStatus, UpdateOrderRequest, PERM_RENTAL_MANAGE,
};

/// Path carrying an order id.
#[derive(Debug, Deserialize)]
struct OrderPath {
    order_id: String,
}

fn order_event(state: &RentalState, event: &str, tenant_id: &str, order: &RentalOrder) {
    state.emit(
        event,
        json!({
            "tenant_id": tenant_id,
            "order_id": order.id.to_string(),
            "listing_id": order.listing_id.to_string(),
            "customer_id": order.customer_id,
            "status": order.status.as_str(),
            "rental_amount_minor": order.rental_amount_minor,
            "total_minor": order.total_minor,
        }),
    );
}

/// An order is readable by its customer, by anyone holding the
/// manage capability, and by no one else (other tenants and
/// other customers read 404, never 403).
fn load_visible_order(
    state: &RentalState,
    ctx: &AppContext,
    tenant_id: &str,
    caller: &super::helpers::Caller,
    order_id: &str,
) -> UnifiedResult<RentalOrder> {
    let order = load_order(state, tenant_id, order_id)?;
    if order.customer_id != caller.actor && !can_manage(ctx, &caller.role) {
        return Err(UnifiedError::not_found(format!(
            "Order not found: {}",
            order.id
        )));
    }
    Ok(order)
}

// Order handlers (authenticated)

pub(crate) async fn list_orders(
    state: web::Data<RentalState>,
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let tenant_id = resolve_tenant(&req)?;
        let caller = caller(&req)?;
        let is_manager = can_manage(&ctx, &caller.role);
        let params = query.into_inner();
        let status_filter = params
            .filters
            .get("status")
            .and_then(|raw| RentalOrderStatus::from_str(raw));
        let listing_filter = params.filters.get("listing_id").cloned();
        let customer_filter = params.filters.get("customer_id").cloned();
        let from_filter = params
            .filters
            .get("starts_from")
            .and_then(|raw| DateTime::parse_from_rfc3339(raw).ok())
            .map(|value| value.with_timezone(&Utc));
        let to_filter = params
            .filters
            .get("starts_to")
            .and_then(|raw| DateTime::parse_from_rfc3339(raw).ok())
            .map(|value| value.with_timezone(&Utc));
        let mut items: Vec<RentalOrder> = state
            .orders
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|order| {
                if order.tenant_id != tenant_id {
                    return false;
                }
                if !is_manager && order.customer_id != caller.actor {
                    return false;
                }
                if let Some(status) = status_filter {
                    if order.status != status {
                        return false;
                    }
                }
                if let Some(listing_id) = &listing_filter {
                    if order.listing_id.to_string() != *listing_id {
                        return false;
                    }
                }
                if let Some(customer_id) = &customer_filter {
                    if order.customer_id != *customer_id {
                        return false;
                    }
                }
                if let Some(from) = from_filter {
                    if order.starts_at < from {
                        return false;
                    }
                }
                if let Some(to) = to_filter {
                    if order.starts_at > to {
                        return false;
                    }
                }
                true
            })
            .collect();
        items.sort_by_key(|order| order.starts_at);
        let views: Vec<Value> = items
            .iter()
            .map(|order| order_view(&state, order))
            .collect();
        Ok(paginate(views, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn create_order(
    state: web::Data<RentalState>,
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    body: web::Json<CreateOrderRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let tenant_id = resolve_tenant(&req)?;
        let caller = caller(&req)?;
        let input = body.into_inner();
        if input.ends_at <= input.starts_at {
            return Err(UnifiedError::bad_request("ends_at must be after starts_at"));
        }
        if input.quantity <= 0 {
            return Err(UnifiedError::bad_request("quantity must be positive"));
        }
        let settings = state.current_settings();
        let max_start = Utc::now() + chrono::Duration::days(settings.max_advance_booking_days);
        if input.starts_at > max_start {
            return Err(UnifiedError::bad_request(format!(
                "starts_at is more than {} days in advance",
                settings.max_advance_booking_days
            )));
        }
        let listing = load_listing(&state, &tenant_id, &input.listing_id.to_string())?;
        if !listing.active {
            return Err(UnifiedError::bad_request("Listing is not active"));
        }
        let rate = load_rate(&state, &tenant_id, &input.rate_id.to_string())?;
        if rate.listing_id != listing.id {
            return Err(UnifiedError::bad_request(
                "rate_id does not belong to listing_id",
            ));
        }
        if input.quantity > listing.quantity {
            return Err(UnifiedError::bad_request(
                "quantity exceeds the listing quantity",
            ));
        }
        check_availability(
            &state,
            &tenant_id,
            &listing,
            input.starts_at,
            input.ends_at,
            input.quantity,
        )?;
        let rental_amount =
            compute_rental_amount(&rate, input.starts_at, input.ends_at, input.quantity)
                .ok_or_else(|| {
                    UnifiedError::bad_request("rate period cannot price the requested range")
                })?;
        let deposit_amount = listing.deposit_amount_minor * input.quantity;
        let customer_id = match &input.customer_id {
            Some(raw) if !raw.trim().is_empty() => raw.trim().to_string(),
            _ => caller.actor.clone(),
        };
        // A manager may book for another customer; a plain
        // caller may only book for themselves.
        if customer_id != caller.actor && !can_manage(&ctx, &caller.role) {
            return Err(UnifiedError::forbidden(
                "Only a manager may create an order for another customer",
            ));
        }
        let now = Utc::now();
        let order = RentalOrder {
            id: Uuid::now_v7(),
            tenant_id: tenant_id.clone(),
            listing_id: listing.id,
            rate_id: rate.id,
            customer_id: customer_id.clone(),
            status: RentalOrderStatus::Draft,
            starts_at: input.starts_at,
            ends_at: input.ends_at,
            quantity: input.quantity,
            currency: listing.currency.clone(),
            rental_amount_minor: rental_amount,
            deposit_amount_minor: deposit_amount,
            deposit_status: DepositStatus::Pending,
            deposit_deducted_minor: 0,
            late_fee_minor: 0,
            damage_total_minor: 0,
            total_minor: rental_amount,
            contract_id: input.contract_id,
            agreement_signed: false,
            shipment_id: None,
            created_at: now,
            updated_at: now,
        };
        state.orders.insert(order.id, order.clone());
        order_event(&state, "rental.order.created", &tenant_id, &order);
        state.notify(
            "rental.order.created",
            &customer_id,
            "Rental order created",
            &format!(
                "Your rental of {} starts {}.",
                listing.title,
                order.starts_at.to_rfc3339()
            ),
            Some(format!("/rental/orders/{}", order.id)),
        );
        Ok(order_view(&state, &order))
    })();
    match result {
        Ok(data) => created(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn get_order(
    state: web::Data<RentalState>,
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    path: web::Path<OrderPath>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let tenant_id = resolve_tenant(&req)?;
        let caller = caller(&req)?;
        let order = load_visible_order(&state, &ctx, &tenant_id, &caller, &path.order_id)?;
        Ok(order_view(&state, &order))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn update_order(
    state: web::Data<RentalState>,
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    path: web::Path<OrderPath>,
    body: web::Json<UpdateOrderRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let tenant_id = resolve_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_RENTAL_MANAGE)?;
        let mut order = load_order(&state, &tenant_id, &path.order_id)?;
        let input = body.into_inner();
        if let Some(contract_id) = &input.contract_id {
            order.contract_id = contract_id.clone();
        }
        order.updated_at = Utc::now();
        state.orders.insert(order.id, order.clone());
        order_event(&state, "rental.order.updated", &tenant_id, &order);
        Ok(order_view(&state, &order))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn delete_order(
    state: web::Data<RentalState>,
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    path: web::Path<OrderPath>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let tenant_id = resolve_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_RENTAL_MANAGE)?;
        let order = load_order(&state, &tenant_id, &path.order_id)?;
        if order.status != RentalOrderStatus::Draft {
            return Err(UnifiedError::bad_request(
                "Only a draft order can be deleted; cancel a reserved order instead",
            ));
        }
        state.orders.remove(&order.id);
        order_event(&state, "rental.order.deleted", &tenant_id, &order);
        Ok(json!({ "deleted": true, "order_id": order.id.to_string() }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

// Status transitions

/// Shared transition runner: loads the order (customer or
/// manager), checks the status machine, applies the transition,
/// and emits the matching event.
fn transition(
    state: &RentalState,
    ctx: &AppContext,
    tenant_id: &str,
    caller: &super::helpers::Caller,
    order_id: &str,
    to: RentalOrderStatus,
    event: &str,
    manager_only: bool,
) -> UnifiedResult<RentalOrder> {
    let mut order = load_visible_order(state, ctx, tenant_id, caller, order_id)?;
    if manager_only && !can_manage(ctx, &caller.role) {
        return Err(UnifiedError::forbidden(
            "Only a manager may perform this transition",
        ));
    }
    if !can_transition(order.status, to) {
        return Err(UnifiedError::bad_request(format!(
            "Order cannot move from {} to {}",
            order.status.as_str(),
            to.as_str()
        )));
    }
    // Cancelling close to the start is blocked by the cutoff,
    // unless the caller is a manager.
    if to == RentalOrderStatus::Cancelled && !can_manage(ctx, &caller.role) {
        let cutoff =
            Utc::now() + chrono::Duration::hours(state.current_settings().cancel_cutoff_hours);
        if order.starts_at < cutoff {
            return Err(UnifiedError::bad_request(
                "Order can no longer be cancelled this close to its start",
            ));
        }
    }
    order.status = to;
    order.updated_at = Utc::now();
    state.orders.insert(order.id, order.clone());
    order_event(state, event, tenant_id, &order);
    Ok(order)
}

pub(crate) async fn reserve_order(
    state: web::Data<RentalState>,
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    path: web::Path<OrderPath>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let tenant_id = resolve_tenant(&req)?;
        let caller = caller(&req)?;
        // Reserving re-checks availability: another order may
        // have taken the units while this one was a draft.
        let draft = load_visible_order(&state, &ctx, &tenant_id, &caller, &path.order_id)?;
        let listing = load_listing(&state, &tenant_id, &draft.listing_id.to_string())?;
        check_availability(
            &state,
            &tenant_id,
            &listing,
            draft.starts_at,
            draft.ends_at,
            draft.quantity,
        )?;
        let order = transition(
            &state,
            &ctx,
            &tenant_id,
            &caller,
            &path.order_id,
            RentalOrderStatus::Reserved,
            "rental.order.reserved",
            false,
        )?;
        Ok(order_view(&state, &order))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn cancel_order(
    state: web::Data<RentalState>,
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    path: web::Path<OrderPath>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let tenant_id = resolve_tenant(&req)?;
        let caller = caller(&req)?;
        let order = transition(
            &state,
            &ctx,
            &tenant_id,
            &caller,
            &path.order_id,
            RentalOrderStatus::Cancelled,
            "rental.order.cancelled",
            false,
        )?;
        Ok(order_view(&state, &order))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn complete_order(
    state: web::Data<RentalState>,
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    path: web::Path<OrderPath>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let tenant_id = resolve_tenant(&req)?;
        let caller = caller(&req)?;
        let order = transition(
            &state,
            &ctx,
            &tenant_id,
            &caller,
            &path.order_id,
            RentalOrderStatus::Completed,
            "rental.order.completed",
            true,
        )?;
        Ok(order_view(&state, &order))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

// Deposit handling (manager)

pub(crate) async fn handle_deposit(
    state: web::Data<RentalState>,
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    path: web::Path<OrderPath>,
    body: web::Json<super::types::DepositRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let tenant_id = resolve_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_RENTAL_MANAGE)?;
        let mut order = load_order(&state, &tenant_id, &path.order_id)?;
        let input = body.into_inner();
        match input.action.as_str() {
            "refund" => {
                if order.deposit_status != DepositStatus::Held
                    && order.deposit_status != DepositStatus::PartiallyRefunded
                {
                    return Err(UnifiedError::bad_request(
                        "Deposit is not held, so there is nothing to refund",
                    ));
                }
                order.deposit_deducted_minor = 0;
                order.deposit_status = DepositStatus::Refunded;
            }
            "deduct" => {
                let amount = input.amount_minor.ok_or_else(|| {
                    UnifiedError::bad_request("amount_minor is required for a deduct action")
                })?;
                if amount < 0 || amount > order.deposit_amount_minor {
                    return Err(UnifiedError::bad_request(
                        "amount_minor must be between 0 and the deposit amount",
                    ));
                }
                if order.deposit_status != DepositStatus::Held {
                    return Err(UnifiedError::bad_request(
                        "Deposit is not held, so there is nothing to deduct from",
                    ));
                }
                order.deposit_deducted_minor = amount;
                order.deposit_status = if amount >= order.deposit_amount_minor {
                    DepositStatus::Deducted
                } else {
                    DepositStatus::PartiallyRefunded
                };
            }
            other => {
                return Err(UnifiedError::bad_request(format!(
                    "action must be refund or deduct, got \"{other}\""
                )));
            }
        }
        order.updated_at = Utc::now();
        state.orders.insert(order.id, order.clone());
        state.emit(
            "rental.deposit.updated",
            json!({
                "tenant_id": tenant_id,
                "order_id": order.id.to_string(),
                "deposit_status": order.deposit_status.as_str(),
                "deposit_deducted_minor": order.deposit_deducted_minor,
            }),
        );
        Ok(order_view(&state, &order))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}
