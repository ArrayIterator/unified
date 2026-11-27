// Events Manager - Ticket-type handlers.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::{HookBus, HookContext};
use unified_core::json;
use unified_core::modules::{Module, ModuleRegistry, RouteInfo, RouteVersionSupport};
use unified_core::pagination::{PageResult, PaginationParams};
use actix_web::{web, HttpRequest, HttpResponse};
use chrono::{DateTime, Datelike, Duration, NaiveDate, TimeZone, Utc};
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::sync::{Arc, RwLock};
use uuid::Uuid;

use super::constants::*;
use super::types::*;
use super::store::*;
use super::helpers::*;
use super::validation::*;
use super::service::*;
use super::handlers_events::*;
use super::handlers_registrations::*;
use super::handlers_reminders::*;
use super::handlers_public::*;
use super::hooks::*;
use super::module::*;
use super::constants::{DEFAULT_PER_ORDER};

// Ticket-type handlers

pub(crate) async fn list_ticket_types(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<EventsState>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_EVENTS_VIEW)?;
        let event = load_event(&state, &path.into_inner())?;
        require_record_in_company_scope(&event.company_id, &_scope)?;
        let params = query.into_inner();
        let mut items: Vec<TicketType> = state
            .ticket_types
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|ticket| ticket.event_id == event.id)
            .collect();
        items.sort_by_key(|ticket| ticket.created_at);
        Ok(paginate(items, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn create_ticket_type(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<EventsState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<CreateTicketTypeRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<TicketType> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = require_company_for_write(&_tenant, &req)?;
        let caller = caller(&req)?;
        let event = load_event(&state, &path.into_inner())?;
        require_record_in_company_scope(&event.company_id, &_scope)?;
        can_manage_event(&ctx, &caller.role, &event, &caller.actor)?;
        if !matches!(event.status, EventStatus::Draft | EventStatus::Published) {
            return Err(UnifiedError::conflict(format!(
                "Ticket types can only be added to draft or published events (status is \"{}\")",
                event.status.as_str()
            )));
        }
        let input = body.into_inner();
        let name = require_text(&input.name, "name", MAX_NAME_LEN)?;
        let description = match &input.description {
            Some(raw) => Some(require_text(raw, "description", MAX_DESCRIPTION_LEN)?),
            None => None,
        };
        let price = valid_price(input.price)?;
        let currency = valid_currency(input.currency.as_deref().unwrap_or("USD"))?;
        if input.quantity < 0 || input.quantity > MAX_TICKET_QUANTITY {
            return Err(UnifiedError::bad_request(format!(
                "quantity must be between 0 and {MAX_TICKET_QUANTITY}"
            )));
        }
        if let (Some(start), Some(end)) = (input.sales_start, input.sales_end) {
            if end <= start {
                return Err(UnifiedError::bad_request("sales_end must be after sales_start"));
            }
        }
        let per_order_limit = input.per_order_limit.unwrap_or(DEFAULT_PER_ORDER);
        if per_order_limit < 1 || per_order_limit > MAX_PER_ORDER {
            return Err(UnifiedError::bad_request(format!(
                "per_order_limit must be between 1 and {MAX_PER_ORDER}"
            )));
        }
        let now = Utc::now();
        let ticket = TicketType {
            id: Uuid::now_v7(),
            event_id: event.id,
            name,
            description,
            price,
            currency,
            quantity: input.quantity,
            sold: 0,
            sales_start: input.sales_start,
            sales_end: input.sales_end,
            per_order_limit,
            created_at: now,
            updated_at: now,
        };
        state.ticket_types.insert(ticket.id, ticket.clone());
        state.emit(
            "events.ticket_type.created",
            json!({
                "event_id": event.id.to_string(),
                "ticket_type_id": ticket.id.to_string(),
                "name": ticket.name,
                "price": minor_to_major(ticket.price, &ticket.currency),
                "currency": ticket.currency,
                "quantity": ticket.quantity,
            }),
        );
        Ok(ticket)
    })();
    match result {
        Ok(ticket) => created(ticket),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn get_ticket_type(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<EventsState>,
    req: HttpRequest,
    path: web::Path<(String, String)>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<TicketType> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_EVENTS_VIEW)?;
        let (event_id, ticket_type_id) = path.into_inner();
        let event = load_event(&state, &event_id)?;
        require_record_in_company_scope(&event.company_id, &_scope)?;
        load_ticket_type(&state, event.id, &ticket_type_id)
    })();
    match result {
        Ok(ticket) => ok(ticket),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn update_ticket_type(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<EventsState>,
    req: HttpRequest,
    path: web::Path<(String, String)>,
    body: web::Json<UpdateTicketTypeRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<TicketType> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = require_company_for_write(&_tenant, &req)?;
        let caller = caller(&req)?;
        let (event_id, ticket_type_id) = path.into_inner();
        let event = load_event(&state, &event_id)?;
        require_record_in_company_scope(&event.company_id, &_scope)?;
        can_manage_event(&ctx, &caller.role, &event, &caller.actor)?;
        let mut ticket = load_ticket_type(&state, event.id, &ticket_type_id)?;
        let input = body.into_inner();
        if let Some(name) = &input.name {
            ticket.name = require_text(name, "name", MAX_NAME_LEN)?;
        }
        if let Some(description) = &input.description {
            ticket.description = match description {
                Some(raw) => Some(require_text(raw, "description", MAX_DESCRIPTION_LEN)?),
                None => None,
            };
        }
        if let Some(price) = input.price {
            if ticket.sold > 0 {
                return Err(UnifiedError::conflict(
                    "price cannot change after sales started for this ticket type",
                ));
            }
            ticket.price = valid_price(price)?;
        }
        if let Some(currency) = &input.currency {
            if ticket.sold > 0 {
                return Err(UnifiedError::conflict(
                    "currency cannot change after sales started for this ticket type",
                ));
            }
            ticket.currency = valid_currency(currency)?;
        }
        if let Some(quantity) = input.quantity {
            if quantity < 0 || quantity > MAX_TICKET_QUANTITY {
                return Err(UnifiedError::bad_request(format!(
                    "quantity must be between 0 and {MAX_TICKET_QUANTITY}"
                )));
            }
            if quantity < ticket.sold {
                return Err(UnifiedError::conflict(format!(
                    "quantity cannot drop below the {} seat(s) already sold",
                    ticket.sold
                )));
            }
            ticket.quantity = quantity;
        }
        if let Some(sales_start) = &input.sales_start {
            ticket.sales_start = sales_start.clone();
        }
        if let Some(sales_end) = &input.sales_end {
            ticket.sales_end = sales_end.clone();
        }
        if let (Some(start), Some(end)) = (ticket.sales_start, ticket.sales_end) {
            if end <= start {
                return Err(UnifiedError::bad_request("sales_end must be after sales_start"));
            }
        }
        if let Some(limit) = input.per_order_limit {
            if limit < 1 || limit > MAX_PER_ORDER {
                return Err(UnifiedError::bad_request(format!(
                    "per_order_limit must be between 1 and {MAX_PER_ORDER}"
                )));
            }
            ticket.per_order_limit = limit;
        }
        ticket.updated_at = Utc::now();
        state.ticket_types.insert(ticket.id, ticket.clone());
        state.emit(
            "events.ticket_type.updated",
            json!({
                "event_id": event.id.to_string(),
                "ticket_type_id": ticket.id.to_string(),
            }),
        );
        Ok(ticket)
    })();
    match result {
        Ok(ticket) => ok(ticket),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn delete_ticket_type(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<EventsState>,
    req: HttpRequest,
    path: web::Path<(String, String)>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = require_company_for_write(&_tenant, &req)?;
        let caller = caller(&req)?;
        let (event_id, ticket_type_id) = path.into_inner();
        let event = load_event(&state, &event_id)?;
        require_record_in_company_scope(&event.company_id, &_scope)?;
        can_manage_event(&ctx, &caller.role, &event, &caller.actor)?;
        let ticket = load_ticket_type(&state, event.id, &ticket_type_id)?;
        if ticket.sold > 0 {
            return Err(UnifiedError::conflict(
                "A ticket type with sales cannot be deleted",
            ));
        }
        let has_registrations = state
            .registrations
            .iter()
            .any(|entry| entry.value().ticket_type_id == ticket.id);
        if has_registrations {
            return Err(UnifiedError::conflict(
                "A ticket type with registrations cannot be deleted",
            ));
        }
        state.ticket_types.remove(&ticket.id);
        state.emit(
            "events.ticket_type.deleted",
            json!({
                "event_id": event.id.to_string(),
                "ticket_type_id": ticket.id.to_string(),
            }),
        );
        Ok(json!({ "deleted": true, "ticket_type_id": ticket.id.to_string() }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}
