// appointments — handlers appointments.

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
use super::handlers_services::*;
use super::handlers_staff::*;
use super::handlers_reminders::*;
use super::module::*;

// ---------------------------------------------------------------------------
// Handlers: appointments
// ---------------------------------------------------------------------------

/// Shared booking creation for the staff and guest flows. The end
/// time is always computed from the service duration; the caller
/// never supplies it.
#[allow(clippy::too_many_arguments)]
pub(crate) fn build_appointment(
    state: &AppointmentsState,
    service: &AppointmentService,
    staff_user_id: Option<String>,
    customer_user_id: Option<String>,
    customer_name: String,
    customer_email: String,
    customer_phone: Option<String>,
    starts_at: DateTime<Utc>,
    notes: Option<String>,
    source: &str,
    company_id: Option<String>,
) -> UnifiedResult<Appointment> {
    let settings = state.settings_snapshot();
    let now = Utc::now();
    validate_start_time(&settings, starts_at, now)?;
    let ends_at = starts_at + Duration::minutes(service.duration_minutes);
    if service.requires_staff {
        let staff = staff_user_id.as_deref().ok_or_else(|| {
            UnifiedError::bad_request("This service requires a staff member (staff_user_id)")
        })?;
        if !staff_assigned_to(state, staff, service.id) {
            return Err(UnifiedError::bad_request(
                "Staff member is not assigned to this service",
            ));
        }
    }
    // The requested window must fit the staff member's working
    // hours (or, for a staff-less booking, at least one assigned
    // staff member's hours): no 3 AM bookings, no bookings on a
    // day off or during a break.
    validate_working_hours(
        state,
        service.id,
        staff_user_id.as_deref(),
        starts_at,
        ends_at,
    )?;
    if let Some(conflict) = find_conflict(
        state,
        service,
        staff_user_id.as_deref(),
        starts_at,
        ends_at,
        None,
    ) {
        return Err(UnifiedError::conflict(conflict));
    }
    let status = if settings.require_confirmation {
        AppointmentStatus::PendingConfirmation
    } else {
        AppointmentStatus::Confirmed
    };
    let appointment = Appointment {
        id: Uuid::now_v7(),
        service_id: service.id,
        staff_user_id,
        customer_user_id,
        customer_name,
        customer_email,
        customer_phone,
        starts_at,
        ends_at,
        status,
        source: source.to_string(),
        price: service.price,
        currency: service.currency.clone(),
        cancellation_fee_charged: None,
        cancelled_at: None,
        cancel_reason: None,
        notes,
        confirmation_code: confirmation_code(),
        company_id,
        created_at: now,
        updated_at: now,
    };
    state
        .appointments
        .insert(appointment.id, appointment.clone());
    state.emit(
        "appointments.created",
        json!({
            "appointment_id": appointment.id,
            "service_id": appointment.service_id,
            "staff_user_id": appointment.staff_user_id,
            "customer_user_id": appointment.customer_user_id,
            "starts_at": appointment.starts_at.to_rfc3339(),
            "status": appointment.status.as_str(),
            "source": appointment.source,
        }),
    );
    if status == AppointmentStatus::Confirmed {
        materialise_reminders(state, &appointment);
    }
    Ok(appointment)
}

/// Notify the customer and the staff member about a booking event.
/// `kind` is the notification kind, for example
/// `appointments.confirmed`.
pub(crate) fn notify_parties(state: &AppointmentsState, appointment: &Appointment, kind: &str, title: &str, body: &str) {
    let settings = state.settings_snapshot();
    let channels = settings.default_reminder_channels.clone();
    let (customer_id, customer_email) = AppointmentsState::customer_recipient(appointment);
    let link = format!(
        "/api/v1/appointments/public/bookings/{}",
        appointment.confirmation_code
    );
    state.notify(
        kind,
        &customer_id,
        Some(&customer_email),
        title,
        body,
        Some(link),
        &channels,
    );
    if let Some(staff) = &appointment.staff_user_id {
        state.notify(kind, staff, None, title, body, None, &channels);
    }
}

/// Ownership scoping for appointment reads: a caller without the
/// manage capability may only see appointments where they are the
/// customer or the assigned staff member. Managers see everything.
/// Guest bookings (no `customer_user_id`) are visible to nobody
/// except managers through this path; guests use the confirmation
/// code lookup instead.
pub(crate) fn appointment_visible_to(
    appointment: &Appointment,
    actor: &str,
    is_manager: bool,
) -> bool {
    is_manager
        || appointment.customer_user_id.as_deref() == Some(actor)
        || appointment.staff_user_id.as_deref() == Some(actor)
}

pub(crate) async fn list_appointments(
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
        let is_manager = check_capability(&ctx, &caller.role, PERM_APPOINTMENTS_MANAGE);
        let params = query.into_inner();
        let from = match params.filters.get("from") {
            Some(raw) => Some(parse_datetime(raw, "from")?),
            None => None,
        };
        let to = match params.filters.get("to") {
            Some(raw) => Some(parse_datetime(raw, "to")?),
            None => None,
        };
        let mut items: Vec<Appointment> = state
            .appointments
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|appointment| {
                appointment_visible_to(appointment, &caller.actor, is_manager)
                    && params.filters.get("service_id").is_none_or(|value| {
                    appointment.service_id.to_string() == *value
                }) && params.filters.get("staff_user_id").is_none_or(|value| {
                    appointment.staff_user_id.as_deref() == Some(value.as_str())
                }) && params.filters.get("customer_user_id").is_none_or(|value| {
                    appointment.customer_user_id.as_deref() == Some(value.as_str())
                }) && params.filters.get("status").is_none_or(|value| {
                    appointment.status.as_str() == value.as_str()
                }) && params.filters.get("source").is_none_or(|value| {
                    appointment.source == *value
                }) && params.filters.get("company_id").is_none_or(|value| {
                    appointment.company_id.as_deref() == Some(value.as_str())
                }) && from.is_none_or(|from| appointment.ends_at >= from)
                    && to.is_none_or(|to| appointment.starts_at <= to)
                    && in_company_scope(&appointment.company_id, &_scope)
                    && params.search.as_ref().is_none_or(|needle| {
                        let needle = needle.to_lowercase();
                        appointment.customer_name.to_lowercase().contains(&needle)
                            || appointment.customer_email.to_lowercase().contains(&needle)
                    })
            })
            .collect();
        items.sort_by_key(|appointment| appointment.starts_at);
        if params.normalized().order == "desc" {
            items.reverse();
        }
        let page = paginate(items, &params);
        Ok(serde_json::to_value(page).unwrap_or(Value::Null))
    })();
    match result {
        Ok(value) => ok(value),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn create_appointment(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<AppointmentsState>,
    req: HttpRequest,
    body: web::Json<CreateAppointmentRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Appointment> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_APPOINTMENTS_CREATE)?;
        let input = body.into_inner();
        let service = load_service_by_id(&state, input.service_id)?;
        require_record_in_company_scope(&service.company_id, &_scope)?;
        if !service.is_active {
            return Err(UnifiedError::not_found(format!("Service not found: {}", input.service_id)));
        }
        // Booking for another user needs the manage capability.
        let customer_user_id = match &input.customer_user_id {
            Some(other) if *other != caller.actor => {
                require_permission(&ctx, &caller.role, PERM_APPOINTMENTS_MANAGE)?;
                Some(valid_user_id(other, "customer_user_id")?)
            }
            Some(other) => Some(valid_user_id(other, "customer_user_id")?),
            None => Some(caller.actor.clone()),
        };
        let staff_user_id = match &input.staff_user_id {
            Some(staff) => Some(valid_user_id(staff, "staff_user_id")?),
            None => None,
        };
        let customer_name = require_text(&input.customer_name, "customer_name", MAX_NAME_LEN)?;
        let customer_email = valid_email(&input.customer_email)?;
        let customer_phone = optional_text(&input.customer_phone, "customer_phone", MAX_PHONE_LEN)?;
        let notes = optional_text(&input.notes, "notes", MAX_NOTES_LEN)?;
        let company_id = optional_text(
            &require_company_match(&_tenant, &req, &input.company_id)?,
            "company_id",
            MAX_USER_ID_LEN,
        )?;
        let appointment = build_appointment(
            &state,
            &service,
            staff_user_id,
            customer_user_id,
            customer_name,
            customer_email,
            customer_phone,
            input.starts_at,
            notes,
            "customer",
            company_id,
        )?;
        let title = format!("Booking received: {}", service.name);
        let body = format!(
            "Your appointment \"{}\" is {} for {}. Confirmation code: {}.",
            service.name,
            appointment.status.as_str(),
            appointment.starts_at.to_rfc3339(),
            appointment.confirmation_code
        );
        notify_parties(&state, &appointment, "appointments.created", &title, &body);
        Ok(appointment)
    })();
    match result {
        Ok(appointment) => created(appointment),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn get_appointment(
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
        let raw_id = path.into_inner();
        let appointment = load_appointment(&state, &raw_id)?;
        require_record_in_company_scope(&appointment.company_id, &_scope)?;
        // Without the manage capability the caller may only fetch
        // their own appointments (as customer or staff). A stranger
        // gets a 404, so the lookup never confirms that another
        // customer's booking exists.
        if !appointment_visible_to(
            &appointment,
            &caller.actor,
            check_capability(&ctx, &caller.role, PERM_APPOINTMENTS_MANAGE),
        ) {
            return Err(UnifiedError::not_found(format!(
                "Appointment not found: {raw_id}"
            )));
        }
        Ok(appointment)
    })();
    match result {
        Ok(appointment) => ok(appointment),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn update_appointment(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<AppointmentsState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdateAppointmentRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Appointment> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_APPOINTMENTS_MANAGE)?;
        let input = body.into_inner();
        let mut appointment = load_appointment(&state, &path.into_inner())?;
        require_record_in_company_scope(&appointment.company_id, &_scope)?;
        if matches!(
            appointment.status,
            AppointmentStatus::Cancelled | AppointmentStatus::Completed | AppointmentStatus::NoShow
        ) {
            return Err(UnifiedError::conflict(format!(
                "A {} appointment cannot be edited",
                appointment.status.as_str()
            )));
        }
        if let Some(name) = &input.customer_name {
            appointment.customer_name = require_text(name, "customer_name", MAX_NAME_LEN)?;
        }
        if let Some(email) = &input.customer_email {
            appointment.customer_email = valid_email(email)?;
        }
        if let Some(phone) = &input.customer_phone {
            appointment.customer_phone = optional_text(phone, "customer_phone", MAX_PHONE_LEN)?;
        }
        if let Some(notes) = &input.notes {
            appointment.notes = optional_text(notes, "notes", MAX_NOTES_LEN)?;
        }
        appointment.updated_at = Utc::now();
        state
            .appointments
            .insert(appointment.id, appointment.clone());
        Ok(appointment)
    })();
    match result {
        Ok(appointment) => ok(appointment),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn confirm_appointment(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<AppointmentsState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Appointment> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_APPOINTMENTS_MANAGE)?;
        let mut appointment = load_appointment(&state, &path.into_inner())?;
        require_record_in_company_scope(&appointment.company_id, &_scope)?;
        if appointment.status != AppointmentStatus::PendingConfirmation {
            return Err(UnifiedError::conflict(format!(
                "Only a pending_confirmation appointment can be confirmed (current status: {})",
                appointment.status.as_str()
            )));
        }
        appointment.status = AppointmentStatus::Confirmed;
        appointment.updated_at = Utc::now();
        state
            .appointments
            .insert(appointment.id, appointment.clone());
        materialise_reminders(&state, &appointment);
        let service = load_service_by_id(&state, appointment.service_id)?;
        require_record_in_company_scope(&service.company_id, &_scope)?;
        state.emit(
            "appointments.confirmed",
            json!({
                "appointment_id": appointment.id,
                "service_id": appointment.service_id,
                "starts_at": appointment.starts_at.to_rfc3339(),
            }),
        );
        let title = format!("Booking confirmed: {}", service.name);
        let body = format!(
            "Your appointment \"{}\" is confirmed for {}. Confirmation code: {}.",
            service.name,
            appointment.starts_at.to_rfc3339(),
            appointment.confirmation_code
        );
        notify_parties(&state, &appointment, "appointments.confirmed", &title, &body);
        Ok(appointment)
    })();
    match result {
        Ok(appointment) => ok(appointment),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn reschedule_appointment(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<AppointmentsState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<RescheduleRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Appointment> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_APPOINTMENTS_MANAGE)?;
        let input = body.into_inner();
        let mut appointment = load_appointment(&state, &path.into_inner())?;
        require_record_in_company_scope(&appointment.company_id, &_scope)?;
        if !matches!(
            appointment.status,
            AppointmentStatus::PendingConfirmation | AppointmentStatus::Confirmed
        ) {
            return Err(UnifiedError::conflict(format!(
                "A {} appointment cannot be rescheduled",
                appointment.status.as_str()
            )));
        }
        let service = load_service_by_id(&state, appointment.service_id)?;
        require_record_in_company_scope(&service.company_id, &_scope)?;
        if !service.is_active {
            return Err(UnifiedError::conflict("The service of this appointment is inactive"));
        }
        let settings = state.settings_snapshot();
        validate_start_time(&settings, input.starts_at, Utc::now())?;
        let ends_at = input.starts_at + Duration::minutes(service.duration_minutes);
        let staff_user_id = match &input.staff_user_id {
            Some(staff) => Some(valid_user_id(staff, "staff_user_id")?),
            None => appointment.staff_user_id.clone(),
        };
        if service.requires_staff {
            let staff = staff_user_id.as_deref().ok_or_else(|| {
                UnifiedError::bad_request("This service requires a staff member (staff_user_id)")
            })?;
            if !staff_assigned_to(&state, staff, service.id) {
                return Err(UnifiedError::bad_request(
                    "Staff member is not assigned to this service",
                ));
            }
        }
        // The new time must fit the (possibly new) staff member's
        // working hours, exactly like a fresh booking.
        validate_working_hours(
            &state,
            service.id,
            staff_user_id.as_deref(),
            input.starts_at,
            ends_at,
        )?;
        if let Some(conflict) = find_conflict(
            &state,
            &service,
            staff_user_id.as_deref(),
            input.starts_at,
            ends_at,
            Some(appointment.id),
        ) {
            return Err(UnifiedError::conflict(conflict));
        }
        let previous_start = appointment.starts_at;
        appointment.starts_at = input.starts_at;
        appointment.ends_at = ends_at;
        appointment.staff_user_id = staff_user_id;
        appointment.updated_at = Utc::now();
        state
            .appointments
            .insert(appointment.id, appointment.clone());
        // Reminders belong to the old time: cancel them and build
        // fresh ones for the new time.
        cancel_pending_reminders(&state, appointment.id);
        if appointment.status == AppointmentStatus::Confirmed {
            materialise_reminders(&state, &appointment);
        }
        state.emit(
            "appointments.rescheduled",
            json!({
                "appointment_id": appointment.id,
                "previous_starts_at": previous_start.to_rfc3339(),
                "starts_at": appointment.starts_at.to_rfc3339(),
            }),
        );
        let title = format!("Appointment rescheduled: {}", service.name);
        let body = format!(
            "Your appointment \"{}\" moved from {} to {}. Confirmation code: {}.",
            service.name,
            previous_start.to_rfc3339(),
            appointment.starts_at.to_rfc3339(),
            appointment.confirmation_code
        );
        notify_parties(&state, &appointment, "appointments.rescheduled", &title, &body);
        Ok(appointment)
    })();
    match result {
        Ok(appointment) => ok(appointment),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn cancel_appointment(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<AppointmentsState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<CancelRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Appointment> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_APPOINTMENTS_MANAGE)?;
        let input = body.into_inner();
        let mut appointment = load_appointment(&state, &path.into_inner())?;
        require_record_in_company_scope(&appointment.company_id, &_scope)?;
        if !matches!(
            appointment.status,
            AppointmentStatus::PendingConfirmation | AppointmentStatus::Confirmed
        ) {
            return Err(UnifiedError::conflict(format!(
                "A {} appointment cannot be cancelled",
                appointment.status.as_str()
            )));
        }
        let service = load_service_by_id(&state, appointment.service_id)?;
        require_record_in_company_scope(&service.company_id, &_scope)?;
        let settings = state.settings_snapshot();
        let now = Utc::now();
        let fee = cancellation_fee(&service, &settings, &appointment, now);
        let reason = optional_text(&input.reason, "reason", MAX_REASON_LEN)?;
        appointment.status = AppointmentStatus::Cancelled;
        appointment.cancellation_fee_charged = Some(fee);
        appointment.cancelled_at = Some(now);
        appointment.cancel_reason = reason;
        appointment.updated_at = now;
        state
            .appointments
            .insert(appointment.id, appointment.clone());
        cancel_pending_reminders(&state, appointment.id);
        state.emit(
            "appointments.cancelled",
            json!({
                "appointment_id": appointment.id,
                "service_id": appointment.service_id,
                "cancellation_fee_charged": minor_to_major(fee, &appointment.currency),
                "currency": appointment.currency,
            }),
        );
        let title = format!("Appointment cancelled: {}", service.name);
        let body = if fee > 0 {
            format!(
                "Your appointment \"{}\" on {} was cancelled. A cancellation fee of {} {} applies.",
                service.name,
                appointment.starts_at.to_rfc3339(),
                minor_to_major(fee, &appointment.currency),
                appointment.currency
            )
        } else {
            format!(
                "Your appointment \"{}\" on {} was cancelled. No fee applies.",
                service.name,
                appointment.starts_at.to_rfc3339()
            )
        };
        notify_parties(&state, &appointment, "appointments.cancelled", &title, &body);
        Ok(appointment)
    })();
    match result {
        Ok(appointment) => ok(appointment),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn complete_appointment(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<AppointmentsState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Appointment> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_APPOINTMENTS_MANAGE)?;
        let mut appointment = load_appointment(&state, &path.into_inner())?;
        require_record_in_company_scope(&appointment.company_id, &_scope)?;
        if appointment.status != AppointmentStatus::Confirmed {
            return Err(UnifiedError::conflict(format!(
                "Only a confirmed appointment can be completed (current status: {})",
                appointment.status.as_str()
            )));
        }
        appointment.status = AppointmentStatus::Completed;
        appointment.updated_at = Utc::now();
        state
            .appointments
            .insert(appointment.id, appointment.clone());
        cancel_pending_reminders(&state, appointment.id);
        let service = load_service_by_id(&state, appointment.service_id)?;
        require_record_in_company_scope(&service.company_id, &_scope)?;
        state.emit(
            "appointments.completed",
            json!({
                "appointment_id": appointment.id,
                "service_id": appointment.service_id,
                "staff_user_id": appointment.staff_user_id,
                "customer_user_id": appointment.customer_user_id,
                "customer_name": appointment.customer_name,
                "customer_email": appointment.customer_email,
                "price": minor_to_major(appointment.price, &appointment.currency),
                "currency": appointment.currency,
                "starts_at": appointment.starts_at.to_rfc3339(),
                "ends_at": appointment.ends_at.to_rfc3339(),
            }),
        );
        // Integration contract for billing: request an invoice for
        // the completed appointment. No billing listener exists for
        // this event name today (checked during this build); the
        // billing module owns fulfilment once the integrator wires
        // a consumer, exactly like the events module's
        // `events.registration.refund_requested`.
        state.emit(
            "billing.invoice.requested",
            json!({
                "appointment_id": appointment.id,
                "service_id": appointment.service_id,
                "service_name": service.name,
                "customer_user_id": appointment.customer_user_id,
                "customer_name": appointment.customer_name,
                "customer_email": appointment.customer_email,
                "amount": minor_to_major(appointment.price, &appointment.currency),
                "currency": appointment.currency,
                "description": format!("Appointment: {}", service.name),
            }),
        );
        // Integration contract for CRM: record the completed visit
        // as a customer activity. The CRM module owns fulfilment.
        state.emit(
            "crm.activity.requested",
            json!({
                "appointment_id": appointment.id,
                "customer_user_id": appointment.customer_user_id,
                "customer_name": appointment.customer_name,
                "customer_email": appointment.customer_email,
                "activity_type": "appointment_completed",
                "subject": format!("Completed appointment: {}", service.name),
                "starts_at": appointment.starts_at.to_rfc3339(),
            }),
        );
        Ok(appointment)
    })();
    match result {
        Ok(appointment) => ok(appointment),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn no_show_appointment(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<AppointmentsState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Appointment> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_APPOINTMENTS_MANAGE)?;
        let mut appointment = load_appointment(&state, &path.into_inner())?;
        require_record_in_company_scope(&appointment.company_id, &_scope)?;
        if appointment.status != AppointmentStatus::Confirmed {
            return Err(UnifiedError::conflict(format!(
                "Only a confirmed appointment can be marked no-show (current status: {})",
                appointment.status.as_str()
            )));
        }
        appointment.status = AppointmentStatus::NoShow;
        appointment.updated_at = Utc::now();
        state
            .appointments
            .insert(appointment.id, appointment.clone());
        cancel_pending_reminders(&state, appointment.id);
        let service = load_service_by_id(&state, appointment.service_id)?;
        require_record_in_company_scope(&service.company_id, &_scope)?;
        state.emit(
            "appointments.no_show",
            json!({
                "appointment_id": appointment.id,
                "service_id": appointment.service_id,
                "starts_at": appointment.starts_at.to_rfc3339(),
            }),
        );
        let title = format!("Missed appointment: {}", service.name);
        let body = format!(
            "You missed your appointment \"{}\" on {}. Please book again if you still need it.",
            service.name,
            appointment.starts_at.to_rfc3339()
        );
        notify_parties(&state, &appointment, "appointments.no_show", &title, &body);
        Ok(appointment)
    })();
    match result {
        Ok(appointment) => ok(appointment),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn appointment_ics(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<AppointmentsState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();

    let _tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return json::error_response(&error, debug),
    };
    let _scope = match caller_company(&_tenant, &req) {
        Ok(scope) => scope,
        Err(error) => return json::error_response(&error, debug),
    };
    let caller_result = caller(&req).and_then(|caller| {
        require_permission(&ctx, &caller.role, PERM_APPOINTMENTS_VIEW)?;
        Ok(())
    });
    if let Err(error) = caller_result {
        return err(error, debug);
    }
    let result: UnifiedResult<String> = (|| {
        let appointment = load_appointment(&state, &path.into_inner())?;
        require_record_in_company_scope(&appointment.company_id, &_scope)?;
        let service = load_service_by_id(&state, appointment.service_id)?;
        require_record_in_company_scope(&service.company_id, &_scope)?;
        let block = ics_appointment_block(&appointment, &service);
        Ok(build_ics(&[block], &format!("Appointment — {}", service.name)))
    })();
    match result {
        Ok(body) => ics_response(body),
        Err(error) => err(error, debug),
    }
}

// ---------------------------------------------------------------------------
// Handlers: public booking
// ---------------------------------------------------------------------------

/// True when a guest booking must be rejected before any
/// rate-limit quota is spent: guest booking is disabled, or the
/// honeypot field was filled. Rejected-before-validation attempts
/// never consume quota.
pub(crate) fn guest_blocked_before_rate_limit(
    settings: &AppointmentSettings,
    website: Option<&str>,
) -> bool {
    !settings.allow_guest_booking
        || website.is_some_and(|value| !value.trim().is_empty())
}

pub(crate) async fn guest_book(
    state: web::Data<AppointmentsState>,
    req: HttpRequest,
    body: web::Json<GuestBookingRequest>,
) -> HttpResponse {
    // The generic failure below is used for every rejection in this
    // handler (honeypot, disabled guest booking, rate limit) so a
    // caller cannot tell the protections apart.
    const GENERIC_FAILURE: &str = "Booking could not be completed. Please try again later.";
    let settings = state.settings_snapshot();
    // Disabled guest booking and honeypot trips are rejected
    // before the rate limit is consulted, so bots and requests to
    // a disabled form never burn quota.
    if guest_blocked_before_rate_limit(&settings, body.website.as_deref()) {
        return err(UnifiedError::new(GENERIC_FAILURE, 400), false);
    }
    let key = client_key(&req);
    if check_guest_rate_limit(&state, &key, settings.guest_booking_rate_limit_per_hour).is_err() {
        return err(UnifiedError::new(GENERIC_FAILURE, 429), false);
    }
    let result: UnifiedResult<Appointment> = (|| {
        let input = body.into_inner();
        let service = load_service_by_id(&state, input.service_id)
            .map_err(|_| UnifiedError::new(GENERIC_FAILURE, 400))?;
        require_record_in_company_scope(&service.company_id, &_scope)?;
        if !service.is_active {
            return Err(UnifiedError::new(GENERIC_FAILURE, 400));
        }
        let staff_user_id = match &input.staff_user_id {
            Some(staff) => Some(valid_user_id(staff, "staff_user_id")?),
            None => None,
        };
        let customer_name = require_text(&input.customer_name, "customer_name", MAX_NAME_LEN)?;
        let customer_email = valid_email(&input.customer_email)?;
        let customer_phone = optional_text(&input.customer_phone, "customer_phone", MAX_PHONE_LEN)?;
        let notes = optional_text(&input.notes, "notes", MAX_NOTES_LEN)?;
        build_appointment(
            &state,
            &service,
            staff_user_id,
            None,
            customer_name,
            customer_email,
            customer_phone,
            input.starts_at,
            notes,
            "guest",
            None,
        )
    })();
    match result {
        Ok(appointment) => {
            let service = match load_service_by_id(&state, appointment.service_id) {
                Ok(service) => service,
                Err(error) => return err(error, false),
            };
            let title = format!("Booking received: {}", service.name);
            let body = format!(
                "Your appointment \"{}\" is {} for {}. Confirmation code: {}.",
                service.name,
                appointment.status.as_str(),
                appointment.starts_at.to_rfc3339(),
                appointment.confirmation_code
            );
            notify_parties(&state, &appointment, "appointments.created", &title, &body);
            created(public_appointment_view(&appointment, &service))
        }
        Err(error) => {
            // Map validation / conflict details to the generic
            // failure so spam probes learn nothing, except keep a
            // real conflict visible: a guest who picks a taken slot
            // needs to know to pick another time.
            if error.code == 409 {
                err(error, false)
            } else {
                err(UnifiedError::new(GENERIC_FAILURE, error.code), false)
            }
        }
    }
}

pub(crate) async fn public_booking_lookup(
    state: web::Data<AppointmentsState>,
    path: web::Path<String>,
) -> HttpResponse {
    let code = path.into_inner();
    let found = state
        .appointments
        .iter()
        .map(|entry| entry.value().clone())
        .find(|appointment| appointment.confirmation_code == code);
    match found {
        Some(appointment) => match load_service_by_id(&state, appointment.service_id) {
            Ok(service) => ok(public_appointment_view(&appointment, &service)),
            Err(error) => err(error, false),
        },
        None => err(UnifiedError::not_found("Booking not found"), false),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn utc(y: i32, mo: u32, d: u32, h: u32, mi: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(y, mo, d, h, mi, 0).unwrap()
    }

    fn appointment(customer: Option<&str>, staff: Option<&str>) -> Appointment {
        Appointment {
            id: Uuid::new_v4(),
            service_id: Uuid::new_v4(),
            staff_user_id: staff.map(str::to_string),
            customer_user_id: customer.map(str::to_string),
            customer_name: "Ada".to_string(),
            customer_email: "ada@example.com".to_string(),
            customer_phone: None,
            starts_at: utc(2026, 11, 23, 10, 0),
            ends_at: utc(2026, 11, 23, 11, 0),
            status: AppointmentStatus::Confirmed,
            source: "staff".to_string(),
            price: 10000,
            currency: "USD".to_string(),
            cancellation_fee_charged: None,
            cancelled_at: None,
            cancel_reason: None,
            notes: None,
            confirmation_code: "ABCDEFGH".to_string(),
            company_id: None,
            created_at: utc(2026, 11, 22, 12, 0),
            updated_at: utc(2026, 11, 22, 12, 0),
        }
    }

    #[test]
    fn appointment_visibility_scopes_to_customer_staff_and_managers() {
        let own = appointment(Some("user-1"), Some("staff-1"));
        assert!(appointment_visible_to(&own, "user-1", false));
        assert!(appointment_visible_to(&own, "staff-1", false));
        assert!(!appointment_visible_to(&own, "user-2", false));
        assert!(appointment_visible_to(&own, "user-2", true));
        // A guest booking names no customer: only a manager sees it.
        let guest = appointment(None, None);
        assert!(!appointment_visible_to(&guest, "user-1", false));
        assert!(appointment_visible_to(&guest, "user-1", true));
    }

    #[test]
    fn guest_precheck_blocks_disabled_and_honeypot_before_rate_limit() {
        let settings = AppointmentSettings::default();
        assert!(!guest_blocked_before_rate_limit(&settings, None));
        assert!(!guest_blocked_before_rate_limit(&settings, Some("")));
        assert!(!guest_blocked_before_rate_limit(&settings, Some("   ")));
        assert!(guest_blocked_before_rate_limit(
            &settings,
            Some("http://spam.example")
        ));
        let mut disabled = AppointmentSettings::default();
        disabled.allow_guest_booking = false;
        assert!(guest_blocked_before_rate_limit(&disabled, None));
    }

    /// The next Monday strictly in the future, so the time is always
    /// beyond the minimum advance booking window.
    fn next_monday() -> NaiveDate {
        let today = Utc::now().date_naive();
        today + Duration::days(7 - today.weekday().num_days_from_monday() as i64)
    }

    fn monday_at(hour: u32, minute: u32) -> DateTime<Utc> {
        Utc.from_utc_datetime(&next_monday().and_hms_opt(hour, minute, 0).unwrap())
    }

    #[test]
    fn build_appointment_rejects_bookings_outside_working_hours() {
        let state = AppointmentsState::default();
        let svc = AppointmentService {
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
            created_at: utc(2026, 11, 22, 12, 0),
            updated_at: utc(2026, 11, 22, 12, 0),
        };
        state.services.insert(svc.id, svc.clone());
        let monday = next_monday();
        let weekday = monday.weekday().num_days_from_monday() as i64;
        state.availability.insert(
            Uuid::new_v4(),
            StaffAvailability {
                id: Uuid::new_v4(),
                staff_user_id: "staff-1".to_string(),
                kind: "working_hours".to_string(),
                weekday: Some(weekday),
                date: None,
                start_minutes: Some(540),
                end_minutes: Some(1020),
                break_start_minutes: Some(720),
                break_end_minutes: Some(780),
                valid_from: None,
                valid_until: None,
                reason: None,
                is_active: true,
                created_at: utc(2026, 11, 22, 12, 0),
                updated_at: utc(2026, 11, 22, 12, 0),
            },
        );
        state.staff_services.insert(
            Uuid::new_v4(),
            StaffService {
                id: Uuid::new_v4(),
                staff_user_id: "staff-1".to_string(),
                service_id: svc.id,
                is_active: true,
                created_at: utc(2026, 11, 22, 12, 0),
                updated_at: utc(2026, 11, 22, 12, 0),
            },
        );
        let book = |staff: Option<String>, starts_at: DateTime<Utc>| {
            build_appointment(
                &state,
                &svc,
                staff,
                Some("user-1".to_string()),
                "Ada".to_string(),
                "ada@example.com".to_string(),
                None,
                starts_at,
                None,
                "staff",
                None,
            )
        };
        // 10:00 fits the assigned staff member's hours.
        assert!(book(None, monday_at(10, 0)).is_ok());
        // 3 AM fits nobody's hours.
        assert!(book(Some("staff-1".to_string()), monday_at(3, 0)).is_err());
        // 12:30 overlaps the lunch break.
        assert!(book(Some("staff-1".to_string()), monday_at(12, 30)).is_err());
        // A day off blocks even an in-hours booking.
        state.availability.insert(
            Uuid::new_v4(),
            StaffAvailability {
                id: Uuid::new_v4(),
                staff_user_id: "staff-1".to_string(),
                kind: "day_off".to_string(),
                weekday: None,
                date: Some(monday),
                start_minutes: None,
                end_minutes: None,
                break_start_minutes: None,
                break_end_minutes: None,
                valid_from: None,
                valid_until: None,
                reason: None,
                is_active: true,
                created_at: utc(2026, 11, 22, 12, 0),
                updated_at: utc(2026, 11, 22, 12, 0),
            },
        );
        assert!(book(Some("staff-1".to_string()), monday_at(10, 0)).is_err());
    }
}
