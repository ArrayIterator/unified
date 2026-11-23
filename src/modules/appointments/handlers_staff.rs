// appointments — handlers staff.

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
use super::handlers_appointments::*;
use super::handlers_reminders::*;
use super::module::*;

// ---------------------------------------------------------------------------
// Handlers: staff availability
// ---------------------------------------------------------------------------

pub(crate) fn validate_availability(
    kind: &str,
    weekday: Option<i64>,
    date: Option<NaiveDate>,
    start_minutes: Option<i64>,
    end_minutes: Option<i64>,
    break_start: Option<i64>,
    break_end: Option<i64>,
    valid_from: Option<NaiveDate>,
    valid_until: Option<NaiveDate>,
) -> UnifiedResult<()> {
    match kind {
        "working_hours" => {
            let weekday = weekday.ok_or_else(|| {
                UnifiedError::bad_request("weekday (0 = Monday .. 6 = Sunday) is required for working_hours")
            })?;
            if !(0..=6).contains(&weekday) {
                return Err(UnifiedError::bad_request("weekday must be between 0 and 6"));
            }
            let (Some(start), Some(end)) = (start_minutes, end_minutes) else {
                return Err(UnifiedError::bad_request(
                    "start_minutes and end_minutes are required for working_hours",
                ));
            };
            if !(0..=1440).contains(&start) || !(0..=1440).contains(&end) || start >= end {
                return Err(UnifiedError::bad_request(
                    "start_minutes must be < end_minutes and both within 0..=1440",
                ));
            }
            match (break_start, break_end) {
                (Some(break_start), Some(break_end)) => {
                    if break_start < start || break_end > end || break_start >= break_end {
                        return Err(UnifiedError::bad_request(
                            "break must lie inside the working window with break_start < break_end",
                        ));
                    }
                }
                (None, None) => {}
                _ => {
                    return Err(UnifiedError::bad_request(
                        "break_start_minutes and break_end_minutes must be given together",
                    ));
                }
            }
            if date.is_some() {
                return Err(UnifiedError::bad_request(
                    "date must not be given for working_hours; use day_off for a single date",
                ));
            }
        }
        "day_off" => {
            if date.is_none() {
                return Err(UnifiedError::bad_request("date is required for day_off"));
            }
            if weekday.is_some() || start_minutes.is_some() || end_minutes.is_some() {
                return Err(UnifiedError::bad_request(
                    "weekday and start/end minutes must not be given for day_off",
                ));
            }
        }
        other => {
            return Err(UnifiedError::bad_request(format!(
                "kind must be \"working_hours\" or \"day_off\", not \"{other}\""
            )));
        }
    }
    if let (Some(from), Some(until)) = (valid_from, valid_until) {
        if from > until {
            return Err(UnifiedError::bad_request("valid_from must not be after valid_until"));
        }
    }
    Ok(())
}

pub(crate) fn parse_optional_date(raw: &Option<String>, field: &str) -> UnifiedResult<Option<NaiveDate>> {
    match raw {
        Some(text) => Ok(Some(parse_date(text, field)?)),
        None => Ok(None),
    }
}

pub(crate) async fn list_staff_availability(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<AppointmentsState>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_APPOINTMENTS_VIEW)?;
        let staff_user_id = valid_user_id(&path.into_inner(), "staff_user_id")?;
        let params = query.into_inner();
        let mut items: Vec<StaffAvailability> = state
            .availability
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|row| {
                row.staff_user_id == staff_user_id
                    && params
                        .filters
                        .get("kind")
                        .is_none_or(|value| row.kind == *value)
                    && params.filters.get("is_active").is_none_or(|value| {
                        row.is_active == (*value == "true" || *value == "1")
                    })
                    && params.filters.get("weekday").is_none_or(|value| {
                        row.weekday.map(|day| day.to_string()) == Some(value.clone())
                    })
            })
            .collect();
        items.sort_by_key(|row| (row.kind.clone(), row.weekday.unwrap_or(0), row.date));
        let page = paginate(items, &params);
        Ok(serde_json::to_value(page).unwrap_or(Value::Null))
    })();
    match result {
        Ok(value) => ok(value),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn create_staff_availability(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<AppointmentsState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<CreateAvailabilityRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<StaffAvailability> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_APPOINTMENTS_MANAGE)?;
        let staff_user_id = valid_user_id(&path.into_inner(), "staff_user_id")?;
        let input = body.into_inner();
        let kind = input.kind.trim().to_string();
        let date = parse_optional_date(&input.date, "date")?;
        let valid_from = parse_optional_date(&input.valid_from, "valid_from")?;
        let valid_until = parse_optional_date(&input.valid_until, "valid_until")?;
        let reason = optional_text(&input.reason, "reason", MAX_REASON_LEN)?;
        validate_availability(
            &kind,
            input.weekday,
            date,
            input.start_minutes,
            input.end_minutes,
            input.break_start_minutes,
            input.break_end_minutes,
            valid_from,
            valid_until,
        )?;
        let now = Utc::now();
        let row = StaffAvailability {
            id: Uuid::now_v7(),
            staff_user_id: staff_user_id.clone(),
            kind,
            weekday: input.weekday,
            date,
            start_minutes: input.start_minutes,
            end_minutes: input.end_minutes,
            break_start_minutes: input.break_start_minutes,
            break_end_minutes: input.break_end_minutes,
            valid_from,
            valid_until,
            reason,
            is_active: input.is_active.unwrap_or(true),
            created_at: now,
            updated_at: now,
        };
        state.availability.insert(row.id, row.clone());
        state.emit(
            "appointments.availability.created",
            json!({ "availability_id": row.id, "staff_user_id": staff_user_id }),
        );
        Ok(row)
    })();
    match result {
        Ok(row) => created(row),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn update_staff_availability(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<AppointmentsState>,
    req: HttpRequest,
    path: web::Path<(String, String)>,
    body: web::Json<UpdateAvailabilityRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<StaffAvailability> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_APPOINTMENTS_MANAGE)?;
        let (staff_user_id, availability_id) = path.into_inner();
        let staff_user_id = valid_user_id(&staff_user_id, "staff_user_id")?;
        let input = body.into_inner();
        let mut row = load_availability(&state, &staff_user_id, &availability_id)?;
        if let Some(kind) = &input.kind {
            row.kind = kind.trim().to_string();
        }
        if let Some(weekday) = input.weekday {
            row.weekday = weekday;
        }
        if let Some(date) = &input.date {
            row.date = match date {
                Some(text) => Some(parse_date(text, "date")?),
                None => None,
            };
        }
        if let Some(start) = input.start_minutes {
            row.start_minutes = start;
        }
        if let Some(end) = input.end_minutes {
            row.end_minutes = end;
        }
        if let Some(break_start) = input.break_start_minutes {
            row.break_start_minutes = break_start;
        }
        if let Some(break_end) = input.break_end_minutes {
            row.break_end_minutes = break_end;
        }
        if let Some(valid_from) = &input.valid_from {
            row.valid_from = match valid_from {
                Some(text) => Some(parse_date(text, "valid_from")?),
                None => None,
            };
        }
        if let Some(valid_until) = &input.valid_until {
            row.valid_until = match valid_until {
                Some(text) => Some(parse_date(text, "valid_until")?),
                None => None,
            };
        }
        if let Some(reason) = &input.reason {
            row.reason = optional_text(reason, "reason", MAX_REASON_LEN)?;
        }
        if let Some(is_active) = input.is_active {
            row.is_active = is_active;
        }
        validate_availability(
            &row.kind,
            row.weekday,
            row.date,
            row.start_minutes,
            row.end_minutes,
            row.break_start_minutes,
            row.break_end_minutes,
            row.valid_from,
            row.valid_until,
        )?;
        row.updated_at = Utc::now();
        state.availability.insert(row.id, row.clone());
        state.emit(
            "appointments.availability.updated",
            json!({ "availability_id": row.id, "staff_user_id": staff_user_id }),
        );
        Ok(row)
    })();
    match result {
        Ok(row) => ok(row),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn delete_staff_availability(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<AppointmentsState>,
    req: HttpRequest,
    path: web::Path<(String, String)>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_APPOINTMENTS_MANAGE)?;
        let (staff_user_id, availability_id) = path.into_inner();
        let staff_user_id = valid_user_id(&staff_user_id, "staff_user_id")?;
        let row = load_availability(&state, &staff_user_id, &availability_id)?;
        state.availability.remove(&row.id);
        state.emit(
            "appointments.availability.deleted",
            json!({ "availability_id": row.id, "staff_user_id": staff_user_id }),
        );
        Ok(json!({ "deleted": true, "id": row.id }))
    })();
    match result {
        Ok(value) => ok(value),
        Err(error) => err(error, debug),
    }
}

// ---------------------------------------------------------------------------
// Handlers: staff-service assignments
// ---------------------------------------------------------------------------

pub(crate) async fn list_staff_services(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<AppointmentsState>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_APPOINTMENTS_VIEW)?;
        let staff_user_id = valid_user_id(&path.into_inner(), "staff_user_id")?;
        let params = query.into_inner();
        let mut items: Vec<StaffService> = state
            .staff_services
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|row| {
                row.staff_user_id == staff_user_id
                    && params.filters.get("is_active").is_none_or(|value| {
                        row.is_active == (*value == "true" || *value == "1")
                    })
            })
            .collect();
        items.sort_by_key(|row| row.created_at);
        let page = paginate(items, &params);
        Ok(serde_json::to_value(page).unwrap_or(Value::Null))
    })();
    match result {
        Ok(value) => ok(value),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn assign_staff_service(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<AppointmentsState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<AssignStaffServiceRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<StaffService> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_APPOINTMENTS_MANAGE)?;
        let staff_user_id = valid_user_id(&path.into_inner(), "staff_user_id")?;
        let service = load_service_by_id(&state, body.service_id)?;
        let existing = state.staff_services.iter().find(|entry| {
            let row = entry.value();
            row.staff_user_id == staff_user_id && row.service_id == service.id
        });
        if let Some(entry) = existing {
            let mut row = entry.value().clone();
            row.is_active = true;
            row.updated_at = Utc::now();
            state.staff_services.insert(row.id, row.clone());
            return Ok(row);
        }
        let now = Utc::now();
        let row = StaffService {
            id: Uuid::now_v7(),
            staff_user_id: staff_user_id.clone(),
            service_id: service.id,
            is_active: true,
            created_at: now,
            updated_at: now,
        };
        state.staff_services.insert(row.id, row.clone());
        state.emit(
            "appointments.staff.assigned",
            json!({ "staff_user_id": staff_user_id, "service_id": service.id }),
        );
        Ok(row)
    })();
    match result {
        Ok(row) => created(row),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn unassign_staff_service(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<AppointmentsState>,
    req: HttpRequest,
    path: web::Path<(String, String)>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_APPOINTMENTS_MANAGE)?;
        let (staff_user_id, service_id) = path.into_inner();
        let staff_user_id = valid_user_id(&staff_user_id, "staff_user_id")?;
        let service = load_service(&state, &service_id)?;
        let target = state.staff_services.iter().find(|entry| {
            let row = entry.value();
            row.staff_user_id == staff_user_id && row.service_id == service.id
        });
        match target {
            Some(entry) => {
                let id = *entry.key();
                state.staff_services.remove(&id);
                state.emit(
                    "appointments.staff.unassigned",
                    json!({ "staff_user_id": staff_user_id, "service_id": service.id }),
                );
                Ok(json!({ "deleted": true, "staff_user_id": staff_user_id, "service_id": service.id }))
            }
            None => Err(UnifiedError::not_found(
                "Staff member is not assigned to this service",
            )),
        }
    })();
    match result {
        Ok(value) => ok(value),
        Err(error) => err(error, debug),
    }
}

// ---------------------------------------------------------------------------
// Handlers: staff calendar (JSON + iCalendar)
// ---------------------------------------------------------------------------

pub(crate) fn staff_appointments_in_range(
    state: &AppointmentsState,
    staff_user_id: &str,
    from: Option<DateTime<Utc>>,
    to: Option<DateTime<Utc>>,
) -> Vec<Appointment> {
    let mut items: Vec<Appointment> = state
        .appointments
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|appointment| {
            appointment.staff_user_id.as_deref() == Some(staff_user_id)
                && from.is_none_or(|from| appointment.ends_at >= from)
                && to.is_none_or(|to| appointment.starts_at <= to)
        })
        .collect();
    items.sort_by_key(|appointment| appointment.starts_at);
    items
}

pub(crate) async fn staff_calendar(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<AppointmentsState>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<CalendarQuery>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_APPOINTMENTS_VIEW)?;
        let staff_user_id = valid_user_id(&path.into_inner(), "staff_user_id")?;
        let input = query.into_inner();
        let from = match &input.from {
            Some(raw) => Some(parse_datetime(raw, "from")?),
            None => None,
        };
        let to = match &input.to {
            Some(raw) => Some(parse_datetime(raw, "to")?),
            None => None,
        };
        if let (Some(from), Some(to)) = (from, to) {
            if from > to {
                return Err(UnifiedError::bad_request("from must not be after to"));
            }
        }
        let items = staff_appointments_in_range(&state, &staff_user_id, from, to);
        Ok(json!({ "staff_user_id": staff_user_id, "appointments": items, "count": items.len() }))
    })();
    match result {
        Ok(value) => ok(value),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn staff_calendar_ics(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<AppointmentsState>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<CalendarQuery>,
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
    let staff_user_id = match valid_user_id(&path.into_inner(), "staff_user_id") {
        Ok(id) => id,
        Err(error) => return err(error, debug),
    };
    let caller_result = caller(&req).and_then(|caller| {
        require_permission(&ctx, &caller.role, PERM_APPOINTMENTS_VIEW)?;
        Ok(())
    });
    if let Err(error) = caller_result {
        return err(error, debug);
    }
    let input = query.into_inner();
    let from = input.from.as_ref().and_then(|raw| parse_datetime(raw, "from").ok());
    let to = input.to.as_ref().and_then(|raw| parse_datetime(raw, "to").ok());
    let items = staff_appointments_in_range(&state, &staff_user_id, from, to);
    let mut blocks = Vec::new();
    for appointment in &items {
        if appointment.status == AppointmentStatus::Cancelled {
            continue;
        }
        if let Ok(service) = load_service_by_id(&state, appointment.service_id) {
            blocks.push(ics_appointment_block(appointment, &service));
        }
    }
    ics_response(build_ics(&blocks, &format!("Appointments — {staff_user_id}")))
}
