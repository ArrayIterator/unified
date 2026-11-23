// appointments — handlers reminders.

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
use super::handlers_appointments::*;
use super::module::*;

// ---------------------------------------------------------------------------
// Handlers: appointment reminders
// ---------------------------------------------------------------------------

pub(crate) async fn list_appointment_reminders(
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
        let appointment = load_appointment(&state, &path.into_inner())?;
        require_record_in_company_scope(&appointment.company_id, &_scope)?;
        let params = query.into_inner();
        let mut items: Vec<AppointmentReminder> = state
            .reminders
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|reminder| {
                reminder.appointment_id == appointment.id
                    && params
                        .filters
                        .get("status")
                        .is_none_or(|value| reminder.status == *value)
            })
            .collect();
        items.sort_by_key(|reminder| reminder.offset_minutes);
        let page = paginate(items, &params);
        Ok(serde_json::to_value(page).unwrap_or(Value::Null))
    })();
    match result {
        Ok(value) => ok(value),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn create_appointment_reminder(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<AppointmentsState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<CreateReminderRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<AppointmentReminder> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = require_company_for_write(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_APPOINTMENTS_MANAGE)?;
        let appointment = load_appointment(&state, &path.into_inner())?;
        require_record_in_company_scope(&appointment.company_id, &_scope)?;
        if matches!(
            appointment.status,
            AppointmentStatus::Cancelled | AppointmentStatus::Completed | AppointmentStatus::NoShow
        ) {
            return Err(UnifiedError::conflict(format!(
                "A {} appointment cannot gain new reminders",
                appointment.status.as_str()
            )));
        }
        let input = body.into_inner();
        if !(0..=MAX_OFFSET_MINUTES).contains(&input.offset_minutes) {
            return Err(UnifiedError::bad_request(format!(
                "offset_minutes must be between 0 and {MAX_OFFSET_MINUTES}"
            )));
        }
        let settings = state.settings_snapshot();
        let channels = if input.channels.is_empty() {
            settings.default_reminder_channels.clone()
        } else {
            valid_channels(&input.channels)?
        };
        let now = Utc::now();
        let mut reminder = AppointmentReminder {
            id: Uuid::now_v7(),
            appointment_id: appointment.id,
            offset_minutes: input.offset_minutes,
            channels,
            status: "pending".to_string(),
            scheduler_job_id: None,
            scheduler_status: "not_scheduled".to_string(),
            sent_at: None,
            created_at: now,
            updated_at: now,
        };
        schedule_reminder(&state, &mut reminder, &appointment);
        state.reminders.insert(reminder.id, reminder.clone());
        state.emit(
            "appointments.reminder.created",
            json!({
                "reminder_id": reminder.id,
                "appointment_id": appointment.id,
                "offset_minutes": reminder.offset_minutes,
            }),
        );
        Ok(reminder)
    })();
    match result {
        Ok(reminder) => created(reminder),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn delete_appointment_reminder(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<AppointmentsState>,
    req: HttpRequest,
    path: web::Path<(String, String)>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = require_company_for_write(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_APPOINTMENTS_MANAGE)?;
        let (appointment_id, reminder_id) = path.into_inner();
        let appointment = load_appointment(&state, &appointment_id)?;
        require_record_in_company_scope(&appointment.company_id, &_scope)?;
        let reminder = load_reminder(&state, &reminder_id)?;
        if reminder.appointment_id != appointment.id {
            return Err(UnifiedError::not_found(format!("Reminder not found: {reminder_id}")));
        }
        if reminder.status == "sent" {
            return Err(UnifiedError::conflict("A reminder that was already sent cannot be deleted"));
        }
        state.reminders.remove(&reminder.id);
        state.emit(
            "appointments.reminder.deleted",
            json!({ "reminder_id": reminder.id, "appointment_id": appointment.id }),
        );
        Ok(json!({ "deleted": true, "id": reminder.id }))
    })();
    match result {
        Ok(value) => ok(value),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn send_appointment_reminder(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<AppointmentsState>,
    req: HttpRequest,
    path: web::Path<(String, String)>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = require_company_for_write(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_APPOINTMENTS_MANAGE)?;
        let (appointment_id, reminder_id) = path.into_inner();
        let appointment = load_appointment(&state, &appointment_id)?;
        require_record_in_company_scope(&appointment.company_id, &_scope)?;
        let reminder = load_reminder(&state, &reminder_id)?;
        if reminder.appointment_id != appointment.id {
            return Err(UnifiedError::not_found(format!("Reminder not found: {reminder_id}")));
        }
        if reminder.status == "sent" {
            return Err(UnifiedError::conflict("This reminder was already sent"));
        }
        let sent = send_reminder(&state, &reminder)?;
        let now = Utc::now();
        if let Some(mut row) = state.reminders.get_mut(&reminder.id) {
            row.value_mut().status = "sent".to_string();
            row.value_mut().sent_at = Some(now);
            row.value_mut().updated_at = now;
        }
        state.emit(
            "appointments.reminder.sent",
            json!({
                "reminder_id": reminder.id,
                "appointment_id": appointment.id,
                "sent": sent,
                "sent_at": now.to_rfc3339(),
            }),
        );
        Ok(json!({ "reminder_id": reminder.id, "sent": sent }))
    })();
    match result {
        Ok(value) => ok(value),
        Err(error) => err(error, debug),
    }
}

// ---------------------------------------------------------------------------
// Handlers: reminder rules
// ---------------------------------------------------------------------------

pub(crate) async fn list_reminder_rules(
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
        let mut items: Vec<ReminderRule> = state
            .reminder_rules
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|rule| {
                params.filters.get("is_active").is_none_or(|value| {
                    rule.is_active == (*value == "true" || *value == "1")
                }) && params.filters.get("service_id").is_none_or(|value| {
                    rule.service_id.map(|id| id.to_string()) == Some(value.clone())
                })
            })
            .collect();
        items.sort_by_key(|rule| rule.offset_minutes);
        let page = paginate(items, &params);
        Ok(serde_json::to_value(page).unwrap_or(Value::Null))
    })();
    match result {
        Ok(value) => ok(value),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn create_reminder_rule(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<AppointmentsState>,
    req: HttpRequest,
    body: web::Json<CreateReminderRuleRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<ReminderRule> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = require_company_for_write(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_APPOINTMENTS_MANAGE)?;
        let input = body.into_inner();
        let name = require_text(&input.name, "name", MAX_NAME_LEN)?;
        if !(0..=MAX_OFFSET_MINUTES).contains(&input.offset_minutes) {
            return Err(UnifiedError::bad_request(format!(
                "offset_minutes must be between 0 and {MAX_OFFSET_MINUTES}"
            )));
        }
        let settings = state.settings_snapshot();
        let channels = if input.channels.is_empty() {
            settings.default_reminder_channels.clone()
        } else {
            valid_channels(&input.channels)?
        };
        if let Some(service_id) = input.service_id {
            load_service_by_id(&state, service_id)?;
        }
        let now = Utc::now();
        let rule = ReminderRule {
            id: Uuid::now_v7(),
            name,
            offset_minutes: input.offset_minutes,
            channels,
            service_id: input.service_id,
            is_active: input.is_active.unwrap_or(true),
            created_at: now,
            updated_at: now,
        };
        state.reminder_rules.insert(rule.id, rule.clone());
        state.emit(
            "appointments.reminder_rule.created",
            json!({ "rule_id": rule.id, "name": rule.name }),
        );
        Ok(rule)
    })();
    match result {
        Ok(rule) => created(rule),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn delete_reminder_rule(
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
        let rule = load_rule(&state, &path.into_inner())?;
        state.reminder_rules.remove(&rule.id);
        state.emit(
            "appointments.reminder_rule.deleted",
            json!({ "rule_id": rule.id, "name": rule.name }),
        );
        Ok(json!({ "deleted": true, "id": rule.id }))
    })();
    match result {
        Ok(value) => ok(value),
        Err(error) => err(error, debug),
    }
}

// ---------------------------------------------------------------------------
// Handlers: settings
// ---------------------------------------------------------------------------

pub(crate) async fn get_settings(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<AppointmentsState>,
    req: HttpRequest,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<AppointmentSettings> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_APPOINTMENTS_VIEW)?;
        Ok(state.settings_snapshot())
    })();
    match result {
        Ok(settings) => ok(settings),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn update_settings(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<AppointmentsState>,
    req: HttpRequest,
    body: web::Json<UpdateSettingsRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<AppointmentSettings> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_APPOINTMENTS_MANAGE)?;
        let input = body.into_inner();
        let mut settings = state.settings_snapshot();
        if let Some(slot) = input.default_slot_minutes {
            if !(MIN_SLOT_MINUTES..=MAX_SLOT_MINUTES).contains(&slot) {
                return Err(UnifiedError::bad_request(format!(
                    "default_slot_minutes must be between {MIN_SLOT_MINUTES} and {MAX_SLOT_MINUTES}"
                )));
            }
            settings.default_slot_minutes = slot;
        }
        for (value, field) in [
            (input.default_buffer_before_minutes, "default_buffer_before_minutes"),
            (input.default_buffer_after_minutes, "default_buffer_after_minutes"),
        ] {
            if let Some(minutes) = value {
                if !(0..=MAX_BUFFER_MINUTES).contains(&minutes) {
                    return Err(UnifiedError::bad_request(format!(
                        "{field} must be between 0 and {MAX_BUFFER_MINUTES}"
                    )));
                }
            }
        }
        if let Some(minutes) = input.default_buffer_before_minutes {
            settings.default_buffer_before_minutes = minutes;
        }
        if let Some(minutes) = input.default_buffer_after_minutes {
            settings.default_buffer_after_minutes = minutes;
        }
        if let Some(hours) = input.default_cancellation_notice_hours {
            if !(0..=MAX_NOTICE_HOURS).contains(&hours) {
                return Err(UnifiedError::bad_request(format!(
                    "default_cancellation_notice_hours must be between 0 and {MAX_NOTICE_HOURS}"
                )));
            }
            settings.default_cancellation_notice_hours = hours;
        }
        if let Some(percent) = input.default_cancellation_fee_percent {
            if !percent.is_finite() || !(0.0..=MAX_FEE_PERCENT).contains(&percent) {
                return Err(UnifiedError::bad_request(
                    "default_cancellation_fee_percent must be between 0 and 100",
                ));
            }
            settings.default_cancellation_fee_percent = percent;
        }
        if let Some(allow) = input.allow_guest_booking {
            settings.allow_guest_booking = allow;
        }
        if let Some(limit) = input.guest_booking_rate_limit_per_hour {
            if !(1..=MAX_RATE_LIMIT_PER_HOUR).contains(&limit) {
                return Err(UnifiedError::bad_request(format!(
                    "guest_booking_rate_limit_per_hour must be between 1 and {MAX_RATE_LIMIT_PER_HOUR}"
                )));
            }
            settings.guest_booking_rate_limit_per_hour = limit;
        }
        if let Some(days) = input.guest_booking_max_advance_days {
            if !(1..=MAX_ADVANCE_DAYS).contains(&days) {
                return Err(UnifiedError::bad_request(format!(
                    "guest_booking_max_advance_days must be between 1 and {MAX_ADVANCE_DAYS}"
                )));
            }
            settings.guest_booking_max_advance_days = days;
        }
        if let Some(minutes) = input.min_advance_booking_minutes {
            if !(0..=MAX_OFFSET_MINUTES).contains(&minutes) {
                return Err(UnifiedError::bad_request(
                    "min_advance_booking_minutes must be between 0 and 43200",
                ));
            }
            settings.min_advance_booking_minutes = minutes;
        }
        if let Some(channels) = input.default_reminder_channels {
            settings.default_reminder_channels = valid_channels(&channels)?;
        }
        if let Some(types) = input.allowed_location_types {
            if types.is_empty() {
                return Err(UnifiedError::bad_request(
                    "allowed_location_types must not be empty",
                ));
            }
            let mut cleaned = Vec::with_capacity(types.len());
            for location_type in types {
                let trimmed = location_type.trim().to_string();
                if trimmed.is_empty() || trimmed.len() > MAX_LOCATION_LEN {
                    return Err(UnifiedError::bad_request(
                        "allowed_location_types entries must be non-empty and at most 200 characters",
                    ));
                }
                if !cleaned.contains(&trimmed) {
                    cleaned.push(trimmed);
                }
            }
            settings.allowed_location_types = cleaned;
        }
        if let Some(require) = input.require_confirmation {
            settings.require_confirmation = require;
        }
        settings.updated_at = Utc::now();
        *state
            .settings
            .write()
            .expect("settings lock poisoned") = settings.clone();
        state.emit(
            "appointments.settings.updated",
            json!({ "updated_at": settings.updated_at.to_rfc3339() }),
        );
        Ok(settings)
    })();
    match result {
        Ok(settings) => ok(settings),
        Err(error) => err(error, debug),
    }
}
