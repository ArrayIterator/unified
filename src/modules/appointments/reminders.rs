// appointments - reminders.

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
use super::ics::*;
use super::validation::*;
use super::handlers_services::*;
use super::handlers_staff::*;
use super::handlers_appointments::*;
use super::handlers_reminders::*;
use super::module::*;

// Reminders

/// Queue one reminder through `scheduler.enqueue` with kind
/// `appointments.reminders.send`. The kind is registered in the
/// scheduler's `KNOWN_KINDS` (`scheduler/types.rs`) and dispatched
/// in `scheduler/service.rs`, which emits
/// `appointments.reminders.send_requested`. The scheduler answers
/// `{"queued": true, "job_id": ...}` when it accepts the job; the
/// reminder is stored either way and `scheduler_status` records
/// the outcome honestly (`queued` / `declined`).
pub(crate) fn schedule_reminder(state: &AppointmentsState, reminder: &mut AppointmentReminder, appointment: &Appointment) {
    let Some(bus) = state.bus() else {
        reminder.scheduler_job_id = None;
        reminder.scheduler_status = "declined".to_string();
        return;
    };
    let send_at = appointment.starts_at - Duration::minutes(reminder.offset_minutes);
    let answer = bus.emit(
        "scheduler.enqueue",
        json!({
            "kind": "appointments.reminders.send",
            "payload": {
                "appointment_id": appointment.id.to_string(),
                "reminder_id": reminder.id.to_string(),
                "send_at": send_at.to_rfc3339(),
                "offset_minutes": reminder.offset_minutes,
            },
        }),
    );
    match answer.results.first() {
        Some(result) if result.get("queued").and_then(Value::as_bool) == Some(true) => {
            reminder.scheduler_job_id = result
                .get("job_id")
                .and_then(|value| value.as_str())
                .map(str::to_string);
            reminder.scheduler_status = "queued".to_string();
        }
        _ => {
            reminder.scheduler_job_id = None;
            reminder.scheduler_status = "declined".to_string();
        }
    }
}

/// Materialise one reminder row per active reminder rule that
/// applies to the appointment's service (a rule with
/// `service_id: None` applies to all services). Existing pending
/// rows for the same rule offset are not duplicated.
pub(crate) fn materialise_reminders(state: &AppointmentsState, appointment: &Appointment) {
    let now = Utc::now();
    let mut rules: Vec<ReminderRule> = state
        .reminder_rules
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|rule| {
            rule.is_active
                && (rule.service_id.is_none() || rule.service_id == Some(appointment.service_id))
        })
        .collect();
    rules.sort_by_key(|rule| rule.offset_minutes);
    for rule in rules {
        let already = state.reminders.iter().any(|entry| {
            let row = entry.value();
            row.appointment_id == appointment.id
                && row.offset_minutes == rule.offset_minutes
                && row.status == "pending"
        });
        if already {
            continue;
        }
        let mut reminder = AppointmentReminder {
            id: Uuid::now_v7(),
            appointment_id: appointment.id,
            offset_minutes: rule.offset_minutes,
            channels: rule.channels.clone(),
            status: "pending".to_string(),
            scheduler_job_id: None,
            scheduler_status: "not_scheduled".to_string(),
            sent_at: None,
            created_at: now,
            updated_at: now,
        };
        schedule_reminder(state, &mut reminder, appointment);
        state.reminders.insert(reminder.id, reminder);
    }
}

/// Cancel every pending reminder row of an appointment (used on
/// cancellation and before re-materialising after a reschedule).
pub(crate) fn cancel_pending_reminders(state: &AppointmentsState, appointment_id: Uuid) {
    let now = Utc::now();
    for mut entry in state.reminders.iter_mut() {
        if entry.value().appointment_id == appointment_id && entry.value().status == "pending" {
            entry.value_mut().status = "cancelled".to_string();
            entry.value_mut().updated_at = now;
        }
    }
}

/// Send one reminder now through `notification.requested`: one
/// notification to the customer and one to the staff member. The
/// reminder row is marked `sent` (or `failed` when the appointment
/// it belongs to is gone).
pub(crate) fn send_reminder(state: &AppointmentsState, reminder: &AppointmentReminder) -> UnifiedResult<usize> {
    let appointment = state
        .appointments
        .get(&reminder.appointment_id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| {
            UnifiedError::not_found(format!("Appointment not found: {}", reminder.appointment_id))
        })?;
    let service = load_service_by_id(state, appointment.service_id)?;
    let (customer_id, customer_email) = AppointmentsState::customer_recipient(&appointment);
    let title = format!("Reminder: {} at {}", service.name, appointment.starts_at.to_rfc3339());
    let body = format!(
        "Your appointment \"{}\" starts at {}. Confirmation code: {}.",
        service.name,
        appointment.starts_at.to_rfc3339(),
        appointment.confirmation_code
    );
    let link = format!("/api/v1/appointments/public/bookings/{}", appointment.confirmation_code);
    state.notify(
        "appointments.reminder",
        &customer_id,
        Some(&customer_email),
        &title,
        &body,
        Some(link),
        &reminder.channels,
    );
    let mut sent = 1;
    if let Some(staff) = &appointment.staff_user_id {
        state.notify(
            "appointments.reminder",
            staff,
            None,
            &title,
            &body,
            None,
            &reminder.channels,
        );
        sent += 1;
    }
    Ok(sent)
}

/// The scheduler's `appointments.reminders.send_requested` handler:
/// send the due reminder through `notification.requested` and mark
/// the row `sent` (or `failed` when the appointment is gone).
pub(crate) fn on_reminders_send_requested(state: AppointmentsState, ctx: &mut HookContext) {
    let reminder_id = ctx
        .get("reminder_id")
        .and_then(|value| value.as_str())
        .unwrap_or("")
        .to_string();
    let Ok(reminder) = load_reminder(&state, &reminder_id) else {
        ctx.add_result(json!({ "error": format!("Reminder not found: {reminder_id}") }));
        return;
    };
    if reminder.status != "pending" {
        ctx.add_result(json!({ "reminder_id": reminder.id, "skipped": true, "status": reminder.status }));
        return;
    }
    match send_reminder(&state, &reminder) {
        Ok(sent) => {
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
                    "appointment_id": reminder.appointment_id,
                    "sent": sent,
                    "sent_at": now.to_rfc3339(),
                }),
            );
            ctx.add_result(json!({ "reminder_id": reminder.id, "sent": sent }));
        }
        Err(error) => {
            let now = Utc::now();
            if let Some(mut row) = state.reminders.get_mut(&reminder.id) {
                row.value_mut().status = "failed".to_string();
                row.value_mut().updated_at = now;
            }
            ctx.add_result(json!({ "reminder_id": reminder.id, "error": error.message }));
        }
    }
}
