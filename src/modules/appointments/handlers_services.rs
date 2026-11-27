// appointments - handlers services.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::{HookBus, HookContext};
use unified_core::json;
use unified_core::modules::{Module, ModuleRegistry, RouteInfo, RouteVersionSupport};
use unified_core::pagination::{PageResult, PaginationParams};
use actix_web::{web, HttpRequest, HttpResponse};
use chrono::{DateTime, Datelike, Duration, NaiveDate, TimeZone, Utc};
use dashmap::DashMap;
use rand::Rng;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::sync::{Arc, RwLock};
use uuid::Uuid;
use super::*;
use super::types::*;
use super::store::*;
use super::common::*;
use super::scheduling::*;
use super::reminders::*;
use super::ics::*;
use super::validation::*;
use super::handlers_staff::*;
use super::handlers_appointments::*;
use super::handlers_reminders::*;
use super::module::*;

pub(crate) async fn list_services(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<AppointmentsState>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_APPOINTMENTS_VIEW)?;
        let params = query.into_inner();
        let mut items: Vec<AppointmentService> = state
            .services
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|service| {
                params
                    .filters
                    .get("is_active")
                    .is_none_or(|value| service.is_active == (*value == "true" || *value == "1"))
                    && params.filters.get("location_type").is_none_or(|value| {
                        service.location_type == *value
                    })
                    && params.filters.get("company_id").is_none_or(|value| {
                        service.company_id.as_deref() == Some(value.as_str())
                    })
                        && in_company_scope(&service.company_id, &_scope)
                    && params.search.as_ref().is_none_or(|needle| {
                        service
                            .name
                            .to_lowercase()
                            .contains(&needle.to_lowercase())
                            || service
                                .description
                                .as_deref()
                                .unwrap_or("")
                                .to_lowercase()
                                .contains(&needle.to_lowercase())
                    })
            })
            .collect();
        items.sort_by(|a, b| a.name.cmp(&b.name));
        let page = paginate(items, &params);
        Ok(serde_json::to_value(page).unwrap_or(Value::Null))
    })();
    match result {
        Ok(value) => ok(value),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn create_service(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<AppointmentsState>,
    req: HttpRequest,
    body: web::Json<CreateServiceRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<AppointmentService> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = require_company_for_write(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_APPOINTMENTS_MANAGE)?;
        let input = body.into_inner();
        let settings = state.settings_snapshot();
        let name = require_text(&input.name, "name", MAX_NAME_LEN)?;
        let description = optional_text(&input.description, "description", MAX_DESCRIPTION_LEN)?;
        let price = validate_service_fields(
            input.duration_minutes,
            input.price,
            input.capacity.unwrap_or(1),
            input.buffer_before_minutes,
            input.buffer_after_minutes,
            input.cancellation_notice_hours,
            input.cancellation_fee_percent,
        )?;
        let currency = valid_currency(input.currency.as_deref().unwrap_or("USD"))?;
        let location_type = input.location_type.unwrap_or_else(|| "in_person".to_string());
        if !settings.allowed_location_types.contains(&location_type) {
            return Err(UnifiedError::bad_request(format!(
                "location_type must be one of: {}",
                settings.allowed_location_types.join(", ")
            )));
        }
        let location = optional_text(&input.location, "location", MAX_LOCATION_LEN)?;
        let company_id = optional_text(
            &require_company_match(&_tenant, &req, &input.company_id)?,
            "company_id",
            MAX_USER_ID_LEN,
        )?;
        let now = Utc::now();
        let service = AppointmentService {
            id: Uuid::now_v7(),
            name,
            description,
            duration_minutes: input.duration_minutes,
            price,
            currency,
            capacity: input.capacity.unwrap_or(1),
            requires_staff: input.requires_staff.unwrap_or(true),
            location_type,
            location,
            buffer_before_minutes: input.buffer_before_minutes,
            buffer_after_minutes: input.buffer_after_minutes,
            cancellation_notice_hours: input.cancellation_notice_hours,
            cancellation_fee_percent: input.cancellation_fee_percent,
            is_active: input.is_active.unwrap_or(true),
            company_id,
            created_at: now,
            updated_at: now,
        };
        state.services.insert(service.id, service.clone());
        state.emit(
            "appointments.service.created",
            json!({ "service_id": service.id, "name": service.name }),
        );
        Ok(service)
    })();
    match result {
        Ok(service) => created(service),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn get_service(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<AppointmentsState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_APPOINTMENTS_VIEW)?;
        load_service(&state, &path.into_inner())
    })();
    match result {
        Ok(service) => ok(service),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn update_service(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<AppointmentsState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdateServiceRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<AppointmentService> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = require_company_for_write(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_APPOINTMENTS_MANAGE)?;
        let input = body.into_inner();
        let settings = state.settings_snapshot();
        let mut service = load_service(&state, &path.into_inner())?;
        require_record_writable_in_company_scope(&service.company_id, &_scope)?;
        if let Some(name) = &input.name {
            service.name = require_text(name, "name", MAX_NAME_LEN)?;
        }
        if let Some(description) = &input.description {
            service.description = optional_text(description, "description", MAX_DESCRIPTION_LEN)?;
        }
        if let Some(duration) = input.duration_minutes {
            service.duration_minutes = duration;
        }
        if let Some(price) = input.price {
            service.price = price;
        }
        if let Some(currency) = &input.currency {
            service.currency = valid_currency(currency)?;
        }
        if let Some(capacity) = input.capacity {
            service.capacity = capacity;
        }
        if let Some(requires_staff) = input.requires_staff {
            service.requires_staff = requires_staff;
        }
        if let Some(location_type) = &input.location_type {
            if !settings.allowed_location_types.contains(location_type) {
                return Err(UnifiedError::bad_request(format!(
                    "location_type must be one of: {}",
                    settings.allowed_location_types.join(", ")
                )));
            }
            service.location_type = location_type.clone();
        }
        if let Some(location) = &input.location {
            service.location = optional_text(location, "location", MAX_LOCATION_LEN)?;
        }
        if let Some(buffer) = input.buffer_before_minutes {
            service.buffer_before_minutes = buffer;
        }
        if let Some(buffer) = input.buffer_after_minutes {
            service.buffer_after_minutes = buffer;
        }
        if let Some(notice) = input.cancellation_notice_hours {
            service.cancellation_notice_hours = notice;
        }
        if let Some(fee) = input.cancellation_fee_percent {
            service.cancellation_fee_percent = fee;
        }
        if let Some(is_active) = input.is_active {
            service.is_active = is_active;
        }
        if let Some(company_id) = &input.company_id {
            if let Some(raw) = company_id {
                require_company_match_for(&_scope, &Some(raw.clone()))?;
            }
            service.company_id = optional_text(company_id, "company_id", MAX_USER_ID_LEN)?;
        }
        validate_service_fields(
            service.duration_minutes,
            service.price,
            service.capacity,
            service.buffer_before_minutes,
            service.buffer_after_minutes,
            service.cancellation_notice_hours,
            service.cancellation_fee_percent,
        )?;
        service.price = valid_price(service.price)?;
        service.updated_at = Utc::now();
        state.services.insert(service.id, service.clone());
        state.emit(
            "appointments.service.updated",
            json!({ "service_id": service.id, "name": service.name }),
        );
        Ok(service)
    })();
    match result {
        Ok(service) => ok(service),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn delete_service(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<AppointmentsState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = require_company_for_write(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_APPOINTMENTS_MANAGE)?;
        let service = load_service(&state, &path.into_inner())?;
        require_record_writable_in_company_scope(&service.company_id, &_scope)?;
        let blocking = state.appointments.iter().any(|entry| {
            let appointment = entry.value();
            appointment.service_id == service.id && is_blocking(appointment.status)
        });
        if blocking {
            return Err(UnifiedError::conflict(
                "Service has active bookings and cannot be deleted; deactivate it instead",
            ));
        }
        state.services.remove(&service.id);
        let stale: Vec<Uuid> = state
            .staff_services
            .iter()
            .filter(|entry| entry.value().service_id == service.id)
            .map(|entry| *entry.key())
            .collect();
        for id in stale {
            state.staff_services.remove(&id);
        }
        state.emit(
            "appointments.service.deleted",
            json!({ "service_id": service.id, "name": service.name }),
        );
        Ok(json!({ "deleted": true, "id": service.id }))
    })();
    match result {
        Ok(value) => ok(value),
        Err(error) => err(error, debug),
    }
}

// Handlers: slots (staff + public share the generator)

pub(crate) fn slots_for_service(
    state: &AppointmentsState,
    service_id: &str,
    query: &SlotsQuery,
) -> UnifiedResult<Vec<BookingSlot>> {
    let service = load_service(state, service_id)?;
    if !service.is_active {
        return Err(UnifiedError::not_found(format!("Service not found: {service_id}")));
    }
    let settings = state.settings_snapshot();
    let date = parse_date(&query.date, "date")?;
    // Past dates have no bookable slots, and dates beyond the
    // booking horizon cannot be booked either.
    let today = Utc::now().date_naive();
    if date < today {
        return Err(UnifiedError::bad_request(
            "Slots cannot be listed for a past date",
        ));
    }
    if date > today + Duration::days(settings.guest_booking_max_advance_days) {
        return Err(UnifiedError::bad_request(format!(
            "Slots cannot be listed more than {} day(s) in advance",
            settings.guest_booking_max_advance_days
        )));
    }
    let slot_minutes = query.slot_minutes.unwrap_or(settings.default_slot_minutes);
    if !(MIN_SLOT_MINUTES..=MAX_SLOT_MINUTES).contains(&slot_minutes) {
        return Err(UnifiedError::bad_request(format!(
            "slot_minutes must be between {MIN_SLOT_MINUTES} and {MAX_SLOT_MINUTES}"
        )));
    }
    if slot_minutes < service.duration_minutes {
        return Err(UnifiedError::bad_request(
            "slot_minutes must be at least the service duration",
        ));
    }
    let staff_ids: Vec<String> = match &query.staff_id {
        Some(staff) => {
            let staff = valid_user_id(staff, "staff_id")?;
            if !staff_assigned_to(state, &staff, service.id) {
                return Err(UnifiedError::not_found(
                    "Staff member is not assigned to this service",
                ));
            }
            vec![staff]
        }
        None => staff_for_service(state, service.id),
    };
    Ok(generate_slots(state, &service, &staff_ids, date, slot_minutes))
}

pub(crate) async fn list_service_slots(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<AppointmentsState>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<SlotsQuery>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_APPOINTMENTS_VIEW)?;
        let slots = slots_for_service(&state, &path.into_inner(), &query.into_inner())?;
        Ok(json!({ "slots": slots, "count": slots.len() }))
    })();
    match result {
        Ok(value) => ok(value),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn list_public_services(
    state: web::Data<AppointmentsState>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let params = query.into_inner();
    let mut items: Vec<AppointmentService> = state
        .services
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|service| {
            service.is_active
                && params.filters.get("location_type").is_none_or(|value| {
                    service.location_type == *value
                })
                && params.search.as_ref().is_none_or(|needle| {
                    service
                        .name
                        .to_lowercase()
                        .contains(&needle.to_lowercase())
                })
        })
        .collect();
    items.sort_by(|a, b| a.name.cmp(&b.name));
    let page = paginate(items, &params);
    ok(serde_json::to_value(page).unwrap_or(Value::Null))
}

pub(crate) async fn list_public_service_slots(
    state: web::Data<AppointmentsState>,
    path: web::Path<String>,
    query: web::Query<SlotsQuery>,
) -> HttpResponse {
    let result: UnifiedResult<Value> = (|| {
        let slots = slots_for_service(&state, &path.into_inner(), &query.into_inner())?;
        Ok(json!({ "slots": slots, "count": slots.len() }))
    })();
    match result {
        Ok(value) => ok(value),
        Err(error) => err(error, false),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn service() -> AppointmentService {
        let now = Utc::now();
        AppointmentService {
            id: Uuid::new_v4(),
            name: "Consultation".to_string(),
            description: None,
            duration_minutes: 60,
            price: 10000,
            currency: "USD".to_string(),
            capacity: 1,
            requires_staff: false,
            location_type: "in_person".to_string(),
            location: None,
            buffer_before_minutes: None,
            buffer_after_minutes: None,
            cancellation_notice_hours: None,
            cancellation_fee_percent: None,
            is_active: true,
            company_id: None,
            created_at: now,
            updated_at: now,
        }
    }

    fn query_for(date: NaiveDate) -> SlotsQuery {
        SlotsQuery {
            staff_id: None,
            date: date.format("%Y-%m-%d").to_string(),
            slot_minutes: None,
        }
    }

    #[test]
    fn slot_lookup_rejects_past_and_beyond_horizon_dates() {
        let state = AppointmentsState::default();
        let svc = service();
        state.services.insert(svc.id, svc.clone());
        let today = Utc::now().date_naive();
        let past = slots_for_service(&state, &svc.id.to_string(), &query_for(today - Duration::days(1)));
        assert!(past.is_err_and(|error| error.message.contains("past date")));
        let beyond = slots_for_service(
            &state,
            &svc.id.to_string(),
            &query_for(today + Duration::days(AppointmentSettings::default().guest_booking_max_advance_days + 1)),
        );
        assert!(beyond.is_err_and(|error| error.message.contains("in advance")));
        let tomorrow = slots_for_service(&state, &svc.id.to_string(), &query_for(today + Duration::days(1)));
        assert!(tomorrow.is_ok());
    }
}
