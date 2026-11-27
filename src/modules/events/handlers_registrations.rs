// Events Manager - Registration, attendee, and waitlist handlers.

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
use super::handlers_tickets::*;
use super::handlers_reminders::*;
use super::handlers_public::*;
use super::hooks::*;
use super::module::*;

// Registration handlers

pub(crate) async fn register_for_event(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<EventsState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<RegisterRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = require_company_for_write(&_tenant, &req)?;
        let caller = caller(&req)?;
        let event = load_event(&state, &path.into_inner())?;
        require_record_in_company_scope(&event.company_id, &_scope)?;
        let input = body.into_inner();
        // A user registers themselves. Registering another user
        // requires the manage capability.
        let target_user = match &input.user_id {
            Some(user_id) if user_id != &caller.actor => {
                require_permission(&ctx, &caller.role, PERM_EVENTS_MANAGE)?;
                require_text(user_id, "user_id", 128)?
            }
            Some(user_id) => require_text(user_id, "user_id", 128)?,
            None => caller.actor.clone(),
        };
        let now = Utc::now();
        if !matches!(
            effective_status(&event, now),
            EventStatus::Published | EventStatus::Ongoing
        ) {
            return Err(UnifiedError::conflict(format!(
                "Registrations are only open for published events (status is \"{}\")",
                effective_status(&event, now).as_str()
            )));
        }
        let ticket = load_ticket_type(&state, event.id, &input.ticket_type_id.to_string())?;
        if input.quantity < 1 || input.quantity > ticket.per_order_limit {
            return Err(UnifiedError::bad_request(format!(
                "quantity must be between 1 and {} for this ticket type",
                ticket.per_order_limit
            )));
        }
        let occurrence_id = match input.occurrence_id {
            Some(id) => {
                let occurrence = load_occurrence(&state, event.id, &id.to_string())?;
                if occurrence.status == "cancelled" {
                    return Err(UnifiedError::conflict("This occurrence is cancelled"));
                }
                Some(id)
            }
            None => None,
        };
        for attendee in &input.attendees {
            require_text(&attendee.name, "attendee.name", MAX_NAME_LEN)?;
            if !valid_email(&attendee.email) {
                return Err(UnifiedError::bad_request(format!(
                    "Invalid attendee email: {}",
                    attendee.email
                )));
            }
            if !attendee.custom_answers.is_null() && !attendee.custom_answers.is_object() {
                return Err(UnifiedError::bad_request(
                    "attendee.custom_answers must be a JSON object",
                ));
            }
        }
        if !input.custom_answers.is_null() && !input.custom_answers.is_object() {
            return Err(UnifiedError::bad_request("custom_answers must be a JSON object"));
        }
        let order_id = match &input.order_id {
            Some(raw) => Some(require_text(raw, "order_id", 128)?),
            None => None,
        };
        // Sold out: join the waitlist instead of failing.
        let remaining = ticket.quantity - ticket.sold;
        if remaining < input.quantity {
            let position = state
                .waitlist
                .iter()
                .filter(|entry| {
                    entry.value().event_id == event.id
                        && entry.value().ticket_type_id == ticket.id
                        && entry.value().status == "waiting"
                })
                .count() as i64
                + 1;
            let entry = WaitlistEntry {
                id: Uuid::now_v7(),
                event_id: event.id,
                ticket_type_id: ticket.id,
                user_id: target_user.clone(),
                quantity: input.quantity,
                position,
                status: "waiting".to_string(),
                created_at: now,
                updated_at: now,
            };
            state.waitlist.insert(entry.id, entry.clone());
            state.emit(
                "events.waitlist.added",
                json!({
                    "event_id": event.id.to_string(),
                    "ticket_type_id": ticket.id.to_string(),
                    "waitlist_id": entry.id.to_string(),
                    "user_id": target_user,
                    "quantity": input.quantity,
                    "position": position,
                }),
            );
            return Ok(json!({
                "waitlisted": true,
                "waitlist": entry,
                "message": "The event is sold out for this ticket type; you have been added to the waitlist and will be promoted automatically when a seat opens up.",
            }));
        }
        ticket_available(&state, &ticket, input.quantity, now)?;
        let total = ticket.price.saturating_mul(input.quantity);
        let status = if total == 0 {
            RegistrationStatus::Confirmed
        } else {
            RegistrationStatus::PendingPayment
        };
        let registration = Registration {
            id: Uuid::now_v7(),
            event_id: event.id,
            occurrence_id,
            ticket_type_id: ticket.id,
            user_id: target_user.clone(),
            quantity: input.quantity,
            total_amount: total,
            currency: ticket.currency.clone(),
            status,
            order_id: order_id.clone(),
            payment_reference: None,
            payment_provider: None,
            check_in_code: Uuid::new_v4().to_string(),
            checked_in_at: None,
            cancelled_at: None,
            refund_reason: None,
            created_at: now,
            updated_at: now,
        };
        state.registrations.insert(registration.id, registration.clone());
        if status == RegistrationStatus::Confirmed {
            if let Some(mut stored) = state.ticket_types.get_mut(&ticket.id) {
                stored.sold += input.quantity;
                stored.updated_at = now;
            }
        }
        // Attendee rows: one per supplied guest, plus the
        // registrant's own row when none was supplied.
        let mut attendee_rows: Vec<Attendee> = Vec::new();
        if input.attendees.is_empty() {
            attendee_rows.push(Attendee {
                id: Uuid::now_v7(),
                event_id: event.id,
                registration_id: registration.id,
                name: target_user.clone(),
                email: String::new(),
                custom_answers: input.custom_answers.clone(),
                checked_in_at: None,
                created_at: now,
                updated_at: now,
            });
        } else {
            for attendee in &input.attendees {
                attendee_rows.push(Attendee {
                    id: Uuid::now_v7(),
                    event_id: event.id,
                    registration_id: registration.id,
                    name: attendee.name.trim().to_string(),
                    email: attendee.email.trim().to_string(),
                    custom_answers: attendee.custom_answers.clone(),
                    checked_in_at: None,
                    created_at: now,
                    updated_at: now,
                });
            }
        }
        for attendee in &attendee_rows {
            state.attendees.insert(attendee.id, attendee.clone());
        }
        state.emit(
            "events.registration.created",
            json!({
                "event_id": event.id.to_string(),
                "registration_id": registration.id.to_string(),
                "user_id": target_user,
                "ticket_type_id": ticket.id.to_string(),
                "quantity": input.quantity,
                "total_amount": minor_to_major(total, &ticket.currency),
                "currency": ticket.currency,
                "status": status.as_str(),
                "order_id": order_id,
            }),
        );
        if status == RegistrationStatus::Confirmed {
            state.emit(
                "events.registration.confirmed",
                json!({
                    "event_id": event.id.to_string(),
                    "registration_id": registration.id.to_string(),
                    "user_id": target_user,
                }),
            );
            state.notify(
                "event.registration.confirmed",
                &target_user,
                &format!("Registration confirmed: {}", event.title),
                &format!(
                    "Your registration for \"{}\" is confirmed. Check-in code: {}.",
                    event.title, registration.check_in_code
                ),
                Some(format!("/api/v1/events/public/{}", event.slug)),
            );
        }
        Ok(json!({
            "registration": registration,
            "attendees": attendee_rows,
            "status": status.as_str(),
            "payment_required": status == RegistrationStatus::PendingPayment,
            "message": if status == RegistrationStatus::Confirmed {
                "Registration confirmed."
            } else {
                "Registration created. It will be confirmed when payment.captured arrives for this registration (or its order_id)."
            },
        }))
    })();
    match result {
        Ok(data) => created(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn list_registrations(
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
        let status_filter = params.filters.get("status").cloned();
        let ticket_filter = params.filters.get("ticket_type_id").cloned();
        let user_filter = params.filters.get("user_id").cloned();
        let mut items: Vec<Registration> = state
            .registrations
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|registration| {
                if registration.event_id != event.id {
                    return false;
                }
                if let Some(status) = &status_filter {
                    if registration.status.as_str() != status.as_str() {
                        return false;
                    }
                }
                if let Some(ticket_id) = &ticket_filter {
                    if registration.ticket_type_id.to_string() != *ticket_id {
                        return false;
                    }
                }
                if let Some(user_id) = &user_filter {
                    if &registration.user_id != user_id {
                        return false;
                    }
                }
                true
            })
            .collect();
        items.sort_by_key(|registration| registration.created_at);
        Ok(paginate(items, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn get_registration(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<EventsState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        let registration = load_registration(&state, &path.into_inner())?;
        require_event_by_id_in_scope(&state, registration.event_id, &_scope)?;
        if registration.user_id != caller.actor {
            require_permission(&ctx, &caller.role, PERM_EVENTS_MANAGE)?;
        }
        let attendees: Vec<Attendee> = state
            .attendees
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|attendee| attendee.registration_id == registration.id)
            .collect();
        Ok(json!({ "registration": registration, "attendees": attendees }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn cancel_registration(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<EventsState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<CancelRegistrationRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Registration> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = require_company_for_write(&_tenant, &req)?;
        let caller = caller(&req)?;
        let registration = load_registration(&state, &path.into_inner())?;
        require_event_by_id_in_scope(&state, registration.event_id, &_scope)?;
        if registration.user_id != caller.actor {
            require_permission(&ctx, &caller.role, PERM_EVENTS_MANAGE)?;
        }
        if registration.status == RegistrationStatus::Cancelled {
            return Err(UnifiedError::conflict("This registration is already cancelled"));
        }
        let reason = body
            .into_inner()
            .reason
            .map(|raw| raw.trim().to_string())
            .filter(|raw| !raw.is_empty());
        Ok(cancel_registration_inner(&state, &registration, reason.as_deref()))
    })();
    match result {
        Ok(registration) => ok(registration),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn request_refund(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<EventsState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<RefundRequestInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = require_company_for_write(&_tenant, &req)?;
        let caller = caller(&req)?;
        let registration = load_registration(&state, &path.into_inner())?;
        require_event_by_id_in_scope(&state, registration.event_id, &_scope)?;
        if registration.user_id != caller.actor {
            require_permission(&ctx, &caller.role, PERM_EVENTS_MANAGE)?;
        }
        if !matches!(
            registration.status,
            RegistrationStatus::Confirmed
                | RegistrationStatus::PendingPayment
                | RegistrationStatus::PendingVerification
        ) {
            return Err(UnifiedError::conflict(format!(
                "A refund cannot be requested for a {} registration",
                registration.status.as_str()
            )));
        }
        let reason = body
            .into_inner()
            .reason
            .map(|raw| raw.trim().to_string())
            .filter(|raw| !raw.is_empty());
        if let Some(mut stored) = state.registrations.get_mut(&registration.id) {
            stored.refund_reason = reason.clone();
            stored.updated_at = Utc::now();
        }
        // Refunds are NOT processed here: the billing / store side
        // owns them. This only asks for one.
        state.emit(
            "events.registration.refund_requested",
            json!({
                "event_id": registration.event_id.to_string(),
                "registration_id": registration.id.to_string(),
                "user_id": registration.user_id,
                "order_id": registration.order_id,
                "total_amount": minor_to_major(registration.total_amount, &registration.currency),
                "currency": registration.currency,
                "payment_reference": registration.payment_reference,
                "reason": reason,
            }),
        );
        Ok(json!({
            "refund_requested": true,
            "registration_id": registration.id.to_string(),
            "message": "Refund requested. The billing/store side will process it; this module does not move money.",
        }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn check_in_registration(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<EventsState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<CheckInRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = require_company_for_write(&_tenant, &req)?;
        let caller = caller(&req)?;
        let raw_id = path.into_inner();
        let input = body.into_inner();
        let now = Utc::now();
        // By code: the attendee presents the code from their ticket
        // (the QR payload is this code value). By id: an organizer
        // checks the registration in directly.
        let registration = match &input.code {
            Some(code) => {
                let code = code.trim();
                if code.is_empty() || code.len() > MAX_CHECKIN_CODE_LEN {
                    return Err(UnifiedError::bad_request("Invalid check-in code"));
                }
                state
                    .registrations
                    .iter()
                    .map(|entry| entry.value().clone())
                    .find(|registration| registration.check_in_code == code)
                    .ok_or_else(|| UnifiedError::not_found("No registration matches this check-in code"))?
            }
            None => {
                require_permission(&ctx, &caller.role, PERM_EVENTS_CHECKIN)?;
                load_registration(&state, &raw_id)?
            }
        };
        require_event_by_id_in_scope(&state, registration.event_id, &_scope)?;
        if registration.status != RegistrationStatus::Confirmed {
            return Err(UnifiedError::conflict(format!(
                "Only a confirmed registration can be checked in (status is \"{}\")",
                registration.status.as_str()
            )));
        }
        if registration.checked_in_at.is_some() {
            return Err(UnifiedError::conflict(format!(
                "Already checked in at {}",
                registration
                    .checked_in_at
                    .map(|value| value.to_rfc3339())
                    .unwrap_or_default()
            )));
        }
        if let Some(mut stored) = state.registrations.get_mut(&registration.id) {
            stored.checked_in_at = Some(now);
            stored.updated_at = now;
        }
        let mut checked_in_attendees = 0usize;
        for mut entry in state.attendees.iter_mut() {
            if entry.value().registration_id == registration.id
                && entry.value().checked_in_at.is_none()
            {
                entry.value_mut().checked_in_at = Some(now);
                entry.value_mut().updated_at = now;
                checked_in_attendees += 1;
            }
        }
        state.emit(
            "events.registration.checked_in",
            json!({
                "event_id": registration.event_id.to_string(),
                "registration_id": registration.id.to_string(),
                "user_id": registration.user_id,
                "checked_in_at": now.to_rfc3339(),
                "attendees_checked_in": checked_in_attendees,
            }),
        );
        Ok(json!({
            "checked_in": true,
            "registration_id": registration.id.to_string(),
            "user_id": registration.user_id,
            "checked_in_at": now.to_rfc3339(),
            "attendees_checked_in": checked_in_attendees,
        }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

// Attendee / waitlist / reminder handlers

pub(crate) async fn list_attendees(
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
        let checked_in_filter = params.filters.get("checked_in").cloned();
        let registration_filter = params.filters.get("registration_id").cloned();
        let search = params.search.clone().unwrap_or_default().to_lowercase();
        let mut items: Vec<Attendee> = state
            .attendees
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|attendee| {
                if attendee.event_id != event.id {
                    return false;
                }
                if let Some(checked_in) = &checked_in_filter {
                    let expected = match checked_in.as_str() {
                        "true" => true,
                        "false" => false,
                        _ => return false,
                    };
                    if attendee.checked_in_at.is_some() != expected {
                        return false;
                    }
                }
                if let Some(registration_id) = &registration_filter {
                    if attendee.registration_id.to_string() != *registration_id {
                        return false;
                    }
                }
                if !search.is_empty() {
                    let haystack = format!("{} {}", attendee.name, attendee.email).to_lowercase();
                    if !haystack.contains(&search) {
                        return false;
                    }
                }
                true
            })
            .collect();
        items.sort_by_key(|attendee| attendee.created_at);
        Ok(paginate(items, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn export_attendees(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<EventsState>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<ExportQuery>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let _tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return err(error, debug),
    };
    let _scope = match caller_company(&_tenant, &req) {
        Ok(scope) => scope,
        Err(error) => return err(error, debug),
    };
    let caller_result = caller(&req);
    let caller = match caller_result {
        Ok(caller) => caller,
        Err(error) => return err(error, debug),
    };
    if let Err(error) = require_permission(&ctx, &caller.role, PERM_EVENTS_VIEW) {
        return err(error, debug);
    }
    let event = match load_event(&state, &path.into_inner()) {
        Ok(event) => event,
        Err(error) => return err(error, debug),
    };
    if let Err(error) = require_record_in_company_scope(&event.company_id, &_scope) {
        return err(error, debug);
    }
    let mut items: Vec<Attendee> = state
        .attendees
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|attendee| attendee.event_id == event.id)
        .collect();
    items.sort_by_key(|attendee| attendee.created_at);
    match query.into_inner().format.to_lowercase().as_str() {
        "csv" => HttpResponse::Ok()
            .content_type("text/csv; charset=utf-8")
            .insert_header((
                "Content-Disposition",
                format!("attachment; filename=\"event-{}-attendees.csv\"", event.slug),
            ))
            .body(attendees_csv(&items)),
        "json" => ok(json!({ "event_id": event.id.to_string(), "attendees": items })),
        other => err(
            UnifiedError::bad_request(format!("format must be \"csv\" or \"json\", got \"{other}\"")),
            debug,
        ),
    }
}

pub(crate) async fn list_waitlist(
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
        let status_filter = params.filters.get("status").cloned();
        let mut items: Vec<WaitlistEntry> = state
            .waitlist
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|entry| {
                if entry.event_id != event.id {
                    return false;
                }
                if let Some(status) = &status_filter {
                    if &entry.status != status {
                        return false;
                    }
                }
                true
            })
            .collect();
        items.sort_by_key(|entry| entry.position);
        Ok(paginate(items, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}
