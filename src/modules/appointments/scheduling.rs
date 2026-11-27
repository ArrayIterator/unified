// appointments - scheduling.

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
use super::reminders::*;
use super::ics::*;
use super::validation::*;
use super::handlers_services::*;
use super::handlers_staff::*;
use super::handlers_appointments::*;
use super::handlers_reminders::*;
use super::module::*;

// Availability, slots, and conflicts

/// Effective buffer / cancellation values for one service: the
/// per-service override wins, otherwise the module setting.
pub(crate) fn effective_buffer_before(service: &AppointmentService, settings: &AppointmentSettings) -> i64 {
    service
        .buffer_before_minutes
        .unwrap_or(settings.default_buffer_before_minutes)
}

pub(crate) fn effective_buffer_after(service: &AppointmentService, settings: &AppointmentSettings) -> i64 {
    service
        .buffer_after_minutes
        .unwrap_or(settings.default_buffer_after_minutes)
}

pub(crate) fn effective_notice_hours(service: &AppointmentService, settings: &AppointmentSettings) -> i64 {
    service
        .cancellation_notice_hours
        .unwrap_or(settings.default_cancellation_notice_hours)
}

pub(crate) fn effective_fee_percent(service: &AppointmentService, settings: &AppointmentSettings) -> f64 {
    service
        .cancellation_fee_percent
        .unwrap_or(settings.default_cancellation_fee_percent)
}

/// Appointments that block a slot: pending confirmation or
/// confirmed. Cancelled, completed, and no-show appointments free
/// the time again.
pub(crate) fn is_blocking(status: AppointmentStatus) -> bool {
    matches!(
        status,
        AppointmentStatus::PendingConfirmation | AppointmentStatus::Confirmed
    )
}

/// True when the half-open windows [a_start, a_end) and
/// [b_start, b_end) overlap.
pub(crate) fn windows_overlap(
    a_start: DateTime<Utc>,
    a_end: DateTime<Utc>,
    b_start: DateTime<Utc>,
    b_end: DateTime<Utc>,
) -> bool {
    a_start < b_end && b_start < a_end
}

/// Buffered window of one appointment: its own service's buffers
/// extend the occupied time on both sides.
pub(crate) fn buffered_window(
    state: &AppointmentsState,
    appointment: &Appointment,
    settings: &AppointmentSettings,
) -> (DateTime<Utc>, DateTime<Utc>) {
    let (before, after) = match state.services.get(&appointment.service_id) {
        Some(service) => (
            effective_buffer_before(service.value(), settings),
            effective_buffer_after(service.value(), settings),
        ),
        None => (
            settings.default_buffer_before_minutes,
            settings.default_buffer_after_minutes,
        ),
    };
    (
        appointment.starts_at - Duration::minutes(before),
        appointment.ends_at + Duration::minutes(after),
    )
}

/// Server-side double-booking check for a requested window.
/// Returns a human-readable conflict reason when the booking must
/// be refused, or `None` when the window is free.
///
/// HONESTY: this is a check-then-insert in memory. Two requests
/// that pass the check at the same instant can both insert, so
/// this prevents ordinary double-booking but is not an atomic
/// guarantee. A database exclusion constraint (or a serialisable
/// transaction around the check) is an integrator follow-up; the
/// migration carries the same note.
pub(crate) fn find_conflict(
    state: &AppointmentsState,
    service: &AppointmentService,
    staff_user_id: Option<&str>,
    starts_at: DateTime<Utc>,
    ends_at: DateTime<Utc>,
    except_id: Option<Uuid>,
) -> Option<String> {
    let settings = state.settings_snapshot();
    let new_before = effective_buffer_before(service, &settings);
    let new_after = effective_buffer_after(service, &settings);
    let new_start = starts_at - Duration::minutes(new_before);
    let new_end = ends_at + Duration::minutes(new_after);

    // One staff member cannot be in two places at once (buffers
    // included on both sides).
    if let Some(staff) = staff_user_id {
        for entry in state.appointments.iter() {
            let existing = entry.value();
            if Some(existing.id) == except_id || !is_blocking(existing.status) {
                continue;
            }
            if existing.staff_user_id.as_deref() != Some(staff) {
                continue;
            }
            let (existing_start, existing_end) = buffered_window(state, existing, &settings);
            if windows_overlap(new_start, new_end, existing_start, existing_end) {
                return Some(format!(
                    "Staff member already has a booking overlapping {}",
                    existing.starts_at.to_rfc3339()
                ));
            }
        }
    }

    // Group services: the number of overlapping bookings for the
    // service must stay below its capacity. The overlap test uses
    // the buffered windows on both sides, exactly like the staff
    // branch above: two group bookings placed back-to-back inside
    // each other's buffer must count as overlapping.
    let overlapping: i64 = state
        .appointments
        .iter()
        .filter(|entry| {
            let existing = entry.value();
            Some(existing.id) != except_id
                && is_blocking(existing.status)
                && existing.service_id == service.id
                && {
                    let (existing_start, existing_end) =
                        buffered_window(state, existing, &settings);
                    windows_overlap(new_start, new_end, existing_start, existing_end)
                }
        })
        .count() as i64;
    if overlapping >= service.capacity {
        return Some(format!(
            "Service \"{}\" is fully booked for the requested time",
            service.name
        ));
    }
    None
}

/// Validate a requested start time against the advance-booking
/// rules: far enough in the future, and not beyond the configured
/// booking horizon.
pub(crate) fn validate_start_time(
    settings: &AppointmentSettings,
    starts_at: DateTime<Utc>,
    now: DateTime<Utc>,
) -> UnifiedResult<()> {
    let earliest = now + Duration::minutes(settings.min_advance_booking_minutes);
    if starts_at < earliest {
        return Err(UnifiedError::bad_request(format!(
            "Bookings must be made at least {} minute(s) in advance",
            settings.min_advance_booking_minutes
        )));
    }
    let latest = now + Duration::days(settings.guest_booking_max_advance_days);
    if starts_at > latest {
        return Err(UnifiedError::bad_request(format!(
            "Bookings cannot be made more than {} day(s) in advance",
            settings.guest_booking_max_advance_days
        )));
    }
    Ok(())
}

/// True when the staff member is assigned to the service and the
/// assignment is active.
pub(crate) fn staff_assigned_to(state: &AppointmentsState, staff_user_id: &str, service_id: Uuid) -> bool {
    state.staff_services.iter().any(|entry| {
        let row = entry.value();
        row.staff_user_id == staff_user_id && row.service_id == service_id && row.is_active
    })
}

/// Staff members assigned to a service (active assignments only).
pub(crate) fn staff_for_service(state: &AppointmentsState, service_id: Uuid) -> Vec<String> {
    let mut staff: Vec<String> = state
        .staff_services
        .iter()
        .filter(|entry| {
            let row = entry.value();
            row.service_id == service_id && row.is_active
        })
        .map(|entry| entry.value().staff_user_id.clone())
        .collect();
    staff.sort();
    staff.dedup();
    staff
}

/// True when the staff member has a day off on `date`.
pub(crate) fn has_day_off(state: &AppointmentsState, staff_user_id: &str, date: NaiveDate) -> bool {
    state.availability.iter().any(|entry| {
        let row = entry.value();
        row.staff_user_id == staff_user_id
            && row.kind == "day_off"
            && row.is_active
            && row.date == Some(date)
    })
}

/// Working-hours windows for one staff member on one weekday
/// (0 = Monday .. 6 = Sunday), limited to rows whose validity
/// range contains `date`. Each window is (start, end, break) in
/// minutes from midnight (UTC).
pub(crate) fn working_windows(
    state: &AppointmentsState,
    staff_user_id: &str,
    weekday: i64,
    date: NaiveDate,
) -> Vec<(i64, i64, Option<(i64, i64)>)> {
    let mut windows: Vec<(i64, i64, Option<(i64, i64)>)> = state
        .availability
        .iter()
        .filter(|entry| {
            let row = entry.value();
            row.staff_user_id == staff_user_id
                && row.kind == "working_hours"
                && row.is_active
                && row.weekday == Some(weekday)
                && row.valid_from.is_none_or(|from| date >= from)
                && row.valid_until.is_none_or(|until| date <= until)
        })
        .filter_map(|entry| {
            let row = entry.value();
            match (row.start_minutes, row.end_minutes) {
                (Some(start), Some(end)) if start < end => {
                    let break_window = match (row.break_start_minutes, row.break_end_minutes) {
                        (Some(break_start), Some(break_end)) if break_start < break_end => {
                            Some((break_start, break_end))
                        }
                        _ => None,
                    };
                    Some((start, end, break_window))
                }
                _ => None,
            }
        })
        .collect();
    windows.sort();
    windows.dedup();
    windows
}

/// Check one staff member's requested window against their working
/// hours for `date`: the window must lie inside one working window,
/// must not overlap that window's break, and the date must not be
/// a day off. Returns a human-readable reason when it does not.
fn check_staff_window(
    state: &AppointmentsState,
    staff_user_id: &str,
    date: NaiveDate,
    starts_at: DateTime<Utc>,
    ends_at: DateTime<Utc>,
) -> UnifiedResult<()> {
    if has_day_off(state, staff_user_id, date) {
        return Err(UnifiedError::bad_request(format!(
            "Staff member has a day off on {date}"
        )));
    }
    let weekday = date.weekday().num_days_from_monday() as i64;
    let windows = working_windows(state, staff_user_id, weekday, date);
    if windows.is_empty() {
        return Err(UnifiedError::bad_request(format!(
            "Staff member has no working hours on {date}"
        )));
    }
    let day_start = date
        .and_hms_opt(0, 0, 0)
        .map(|naive| Utc.from_utc_datetime(&naive))
        .ok_or_else(|| UnifiedError::bad_request(format!("Invalid date: {date}")))?;
    for (window_start, window_end, break_window) in windows {
        let window_start = day_start + Duration::minutes(window_start);
        let window_end = day_start + Duration::minutes(window_end);
        if starts_at < window_start || ends_at > window_end {
            continue;
        }
        if let Some((break_start, break_end)) = break_window {
            let break_start = day_start + Duration::minutes(break_start);
            let break_end = day_start + Duration::minutes(break_end);
            if windows_overlap(starts_at, ends_at, break_start, break_end) {
                continue;
            }
        }
        return Ok(());
    }
    Err(UnifiedError::bad_request(format!(
        "The requested time is outside the staff member's working hours on {date}"
    )))
}

/// Validate a requested booking window against working hours, breaks,
/// and days off. A booking with an assigned staff member must fit
/// that member's hours. A staff-less booking must fit at least one
/// staff member assigned to the service; a service with no staff
/// assigned at all is intentionally unregulated, because there are
/// no working hours to check it against.
pub(crate) fn validate_working_hours(
    state: &AppointmentsState,
    service_id: Uuid,
    staff_user_id: Option<&str>,
    starts_at: DateTime<Utc>,
    ends_at: DateTime<Utc>,
) -> UnifiedResult<()> {
    let date = starts_at.date_naive();
    if ends_at.date_naive() != date {
        return Err(UnifiedError::bad_request(
            "The booking must start and end on the same day",
        ));
    }
    match staff_user_id {
        Some(staff) => check_staff_window(state, staff, date, starts_at, ends_at),
        None => {
            let assigned = staff_for_service(state, service_id);
            if assigned.is_empty() {
                return Ok(());
            }
            if assigned
                .iter()
                .any(|staff| check_staff_window(state, staff, date, starts_at, ends_at).is_ok())
            {
                Ok(())
            } else {
                Err(UnifiedError::bad_request(
                    "The requested time is outside the working hours of every staff member assigned to this service",
                ))
            }
        }
    }
}

/// Generate the bookable slots for one service on one day, for the
/// given staff members. A slot starts every `slot_minutes` from the
/// start of a working window, lasts exactly the service duration,
/// and is kept only when it fits inside the window, avoids breaks,
/// does not overlap a buffered existing booking for that staff
/// member, and does not exceed the service capacity.
pub(crate) fn generate_slots(
    state: &AppointmentsState,
    service: &AppointmentService,
    staff_ids: &[String],
    date: NaiveDate,
    slot_minutes: i64,
) -> Vec<BookingSlot> {
    let weekday = date.weekday().num_days_from_monday() as i64;
    let day_start = date
        .and_hms_opt(0, 0, 0)
        .map(|naive| Utc.from_utc_datetime(&naive))
        .unwrap_or_else(Utc::now);
    // Slots that already ended (or end before the minimum advance
    // booking time) are not bookable, so a same-day list never
    // offers a slot in the past.
    let earliest_end =
        Utc::now() + Duration::minutes(state.settings_snapshot().min_advance_booking_minutes);
    let mut slots = Vec::new();
    for staff in staff_ids {
        if has_day_off(state, staff, date) {
            continue;
        }
        for (window_start, window_end, break_window) in working_windows(state, staff, weekday, date)
        {
            let mut cursor = window_start;
            while cursor + service.duration_minutes <= window_end {
                let starts_at = day_start + Duration::minutes(cursor);
                let ends_at = starts_at + Duration::minutes(service.duration_minutes);
                let slot_start_min = cursor;
                let slot_end_min = cursor + service.duration_minutes;
                cursor += slot_minutes;
                if ends_at <= earliest_end {
                    continue;
                }
                // A slot that touches a break is not bookable.
                if let Some((break_start, break_end)) = break_window {
                    if slot_start_min < break_end && break_start < slot_end_min {
                        continue;
                    }
                }
                if find_conflict(state, service, Some(staff), starts_at, ends_at, None).is_some() {
                    continue;
                }
                slots.push(BookingSlot {
                    starts_at,
                    ends_at,
                    staff_user_id: staff.clone(),
                    service_id: service.id,
                });
            }
        }
    }
    slots.sort_by_key(|slot| (slot.starts_at, slot.staff_user_id.clone()));
    slots
}

/// Cancellation fee for one appointment at `now`: zero when the
/// cancellation is outside the notice window, otherwise the
/// configured percentage of the booked price.
pub(crate) fn cancellation_fee(
    service: &AppointmentService,
    settings: &AppointmentSettings,
    appointment: &Appointment,
    now: DateTime<Utc>,
) -> i64 {
    let notice = effective_notice_hours(service, settings);
    let deadline = appointment.starts_at - Duration::hours(notice);
    if now <= deadline {
        0
    } else {
        apply_percent_to_minor(
            appointment.price,
            effective_fee_percent(service, settings),
        )
    }
}
#[cfg(test)]
mod tests {
    use super::*;

    fn utc(y: i32, mo: u32, d: u32, h: u32, mi: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(y, mo, d, h, mi, 0).unwrap()
    }

    fn service() -> AppointmentService {
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
            created_at: utc(2026, 11, 22, 12, 0),
            updated_at: utc(2026, 11, 22, 12, 0),
        }
    }

    fn appointment(
        service_id: Uuid,
        start: DateTime<Utc>,
        end: DateTime<Utc>,
    ) -> Appointment {
        Appointment {
            id: Uuid::new_v4(),
            service_id,
            staff_user_id: None,
            customer_user_id: None,
            customer_name: "Ada".to_string(),
            customer_email: "ada@example.com".to_string(),
            customer_phone: None,
            starts_at: start,
            ends_at: end,
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
    fn blocking_statuses_hold_slots_and_the_rest_free_them() {
        assert!(is_blocking(AppointmentStatus::PendingConfirmation));
        assert!(is_blocking(AppointmentStatus::Confirmed));
        assert!(!is_blocking(AppointmentStatus::Cancelled));
        assert!(!is_blocking(AppointmentStatus::Completed));
        assert!(!is_blocking(AppointmentStatus::NoShow));
    }

    #[test]
    fn status_round_trips_through_strings() {
        assert_eq!(AppointmentStatus::Confirmed.as_str(), "confirmed");
        assert_eq!(
            AppointmentStatus::from_str("no_show"),
            Some(AppointmentStatus::NoShow)
        );
        assert_eq!(AppointmentStatus::from_str("bogus"), None);
    }

    #[test]
    fn windows_overlap_only_on_a_shared_interval() {
        let a_start = utc(2026, 11, 22, 10, 0);
        let a_end = utc(2026, 11, 22, 11, 0);
        assert!(windows_overlap(
            a_start,
            a_end,
            utc(2026, 11, 22, 10, 30),
            utc(2026, 11, 22, 11, 30)
        ));
        assert!(windows_overlap(
            a_start,
            a_end,
            utc(2026, 11, 22, 9, 0),
            utc(2026, 11, 22, 12, 0)
        ));
        // Touching endpoints share no interval.
        assert!(!windows_overlap(
            a_start,
            a_end,
            utc(2026, 11, 22, 11, 0),
            utc(2026, 11, 22, 12, 0)
        ));
        assert!(!windows_overlap(
            a_start,
            a_end,
            utc(2026, 11, 22, 8, 0),
            utc(2026, 11, 22, 9, 0)
        ));
    }

    #[test]
    fn effective_values_prefer_service_overrides() {
        let mut settings = AppointmentSettings::default();
        settings.default_buffer_before_minutes = 5;
        settings.default_buffer_after_minutes = 15;
        settings.default_cancellation_notice_hours = 24;
        settings.default_cancellation_fee_percent = 10.0;
        let plain = service();
        assert_eq!(effective_buffer_before(&plain, &settings), 5);
        assert_eq!(effective_buffer_after(&plain, &settings), 15);
        assert_eq!(effective_notice_hours(&plain, &settings), 24);
        assert_eq!(effective_fee_percent(&plain, &settings), 10.0);
        let mut over = service();
        over.buffer_before_minutes = Some(30);
        over.cancellation_fee_percent = Some(50.0);
        assert_eq!(effective_buffer_before(&over, &settings), 30);
        assert_eq!(effective_fee_percent(&over, &settings), 50.0);
    }

    #[test]
    fn start_time_must_respect_advance_rules() {
        let settings = AppointmentSettings::default();
        let now = utc(2026, 11, 22, 12, 0);
        assert!(validate_start_time(&settings, utc(2026, 11, 22, 12, 30), now).is_err());
        assert!(validate_start_time(&settings, utc(2026, 11, 22, 14, 0), now).is_ok());
        assert!(validate_start_time(&settings, utc(2027, 1, 22, 12, 0), now).is_err());
    }

    #[test]
    fn cancellation_fee_is_zero_outside_the_notice_window() {
        let mut settings = AppointmentSettings::default();
        settings.default_cancellation_fee_percent = 50.0;
        let svc = service();
        let now = utc(2026, 11, 22, 12, 0);
        let early = appointment(svc.id, utc(2026, 11, 23, 12, 0), utc(2026, 11, 23, 13, 0));
        assert_eq!(cancellation_fee(&svc, &settings, &early, now), 0);
        let late = appointment(svc.id, utc(2026, 11, 23, 11, 0), utc(2026, 11, 23, 12, 0));
        assert_eq!(cancellation_fee(&svc, &settings, &late, now), 5000);
    }

    #[test]
    fn capacity_conflict_blocks_double_booking() {
        let state = AppointmentsState::default();
        let svc = service();
        let existing = appointment(svc.id, utc(2026, 11, 22, 10, 0), utc(2026, 11, 22, 11, 0));
        state.appointments.insert(existing.id, existing);
        let clash = find_conflict(
            &state,
            &svc,
            None,
            utc(2026, 11, 22, 10, 30),
            utc(2026, 11, 22, 11, 30),
            None,
        );
        assert!(clash.is_some_and(|m| m.contains("fully booked")));
        let free = find_conflict(
            &state,
            &svc,
            None,
            utc(2026, 11, 22, 12, 0),
            utc(2026, 11, 22, 13, 0),
            None,
        );
        assert!(free.is_none());
    }

    #[test]
    fn staff_conflict_blocks_overlapping_bookings() {
        let state = AppointmentsState::default();
        let mut svc = service();
        svc.capacity = 10;
        let mut existing =
            appointment(svc.id, utc(2026, 11, 22, 10, 0), utc(2026, 11, 22, 11, 0));
        existing.staff_user_id = Some("staff-1".to_string());
        state.appointments.insert(existing.id, existing);
        let clash = find_conflict(
            &state,
            &svc,
            Some("staff-1"),
            utc(2026, 11, 22, 10, 30),
            utc(2026, 11, 22, 11, 30),
            None,
        );
        assert!(clash.is_some_and(|m| m.contains("already has a booking")));
        let other = find_conflict(
            &state,
            &svc,
            Some("staff-2"),
            utc(2026, 11, 22, 10, 30),
            utc(2026, 11, 22, 11, 30),
            None,
        );
        assert!(other.is_none());
    }

    fn availability_row(
        staff: &str,
        kind: &str,
        weekday: Option<i64>,
        date: Option<NaiveDate>,
        start: Option<i64>,
        end: Option<i64>,
        break_window: Option<(i64, i64)>,
    ) -> StaffAvailability {
        StaffAvailability {
            id: Uuid::new_v4(),
            staff_user_id: staff.to_string(),
            kind: kind.to_string(),
            weekday,
            date,
            start_minutes: start,
            end_minutes: end,
            break_start_minutes: break_window.map(|window| window.0),
            break_end_minutes: break_window.map(|window| window.1),
            valid_from: None,
            valid_until: None,
            reason: None,
            is_active: true,
            created_at: utc(2026, 11, 22, 12, 0),
            updated_at: utc(2026, 11, 22, 12, 0),
        }
    }

    /// The next Monday strictly in the future, at 10:00 UTC, so the
    /// time is always beyond the minimum advance booking window.
    fn next_monday() -> NaiveDate {
        let today = Utc::now().date_naive();
        let offset = 7 - today.weekday().num_days_from_monday() as i64;
        today + Duration::days(offset)
    }

    fn monday_at(hour: u32, minute: u32) -> DateTime<Utc> {
        let date = next_monday();
        Utc.from_utc_datetime(&date.and_hms_opt(hour, minute, 0).unwrap())
    }

    #[test]
    fn working_hours_reject_off_hours_breaks_and_days_off() {
        let state = AppointmentsState::default();
        let monday = next_monday();
        let weekday = monday.weekday().num_days_from_monday() as i64;
        state.availability.insert(
            Uuid::new_v4(),
            availability_row(
                "staff-1",
                "working_hours",
                Some(weekday),
                None,
                Some(540),
                Some(1020),
                Some((720, 780)),
            ),
        );
        // Inside working hours.
        assert!(validate_working_hours(
            &state,
            Uuid::new_v4(),
            Some("staff-1"),
            monday_at(10, 0),
            monday_at(11, 0),
        )
        .is_ok());
        // 3 AM is outside working hours.
        assert!(validate_working_hours(
            &state,
            Uuid::new_v4(),
            Some("staff-1"),
            monday_at(3, 0),
            monday_at(4, 0),
        )
        .is_err());
        // Overlapping the lunch break is not bookable.
        assert!(validate_working_hours(
            &state,
            Uuid::new_v4(),
            Some("staff-1"),
            monday_at(12, 30),
            monday_at(13, 30),
        )
        .is_err());
        // A staff member with no working hours that day cannot be booked.
        assert!(validate_working_hours(
            &state,
            Uuid::new_v4(),
            Some("staff-2"),
            monday_at(10, 0),
            monday_at(11, 0),
        )
        .is_err());
        // A day off blocks even an in-hours booking.
        state.availability.insert(
            Uuid::new_v4(),
            availability_row("staff-1", "day_off", None, Some(monday), None, None, None),
        );
        assert!(validate_working_hours(
            &state,
            Uuid::new_v4(),
            Some("staff-1"),
            monday_at(10, 0),
            monday_at(11, 0),
        )
        .is_err());
    }

    #[test]
    fn staff_less_bookings_follow_assigned_staff_hours() {
        let state = AppointmentsState::default();
        let svc = service();
        let monday = next_monday();
        let weekday = monday.weekday().num_days_from_monday() as i64;
        state.availability.insert(
            Uuid::new_v4(),
            availability_row(
                "staff-1",
                "working_hours",
                Some(weekday),
                None,
                Some(540),
                Some(1020),
                None,
            ),
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
        // In hours for the assigned staff member.
        assert!(validate_working_hours(&state, svc.id, None, monday_at(10, 0), monday_at(11, 0))
            .is_ok());
        // 3 AM fits no assigned staff member's hours.
        assert!(validate_working_hours(&state, svc.id, None, monday_at(3, 0), monday_at(4, 0))
            .is_err());
        // With no staff assigned at all the service is unregulated.
        state.staff_services.clear();
        assert!(validate_working_hours(&state, svc.id, None, monday_at(3, 0), monday_at(4, 0))
            .is_ok());
    }

    #[test]
    fn capacity_conflict_honours_buffers() {
        let state = AppointmentsState::default();
        let mut svc = service();
        svc.capacity = 1;
        svc.buffer_after_minutes = Some(15);
        let existing = appointment(svc.id, utc(2026, 11, 22, 10, 0), utc(2026, 11, 22, 11, 0));
        state.appointments.insert(existing.id, existing);
        // 11:05 does not overlap the raw 10:00-11:00 window, but it
        // sits inside the 15-minute buffer after it.
        let clash = find_conflict(
            &state,
            &svc,
            None,
            utc(2026, 11, 22, 11, 5),
            utc(2026, 11, 22, 12, 5),
            None,
        );
        assert!(clash.is_some_and(|message| message.contains("fully booked")));
        // 11:30 starts after the buffered window ends (11:15).
        let free = find_conflict(
            &state,
            &svc,
            None,
            utc(2026, 11, 22, 11, 30),
            utc(2026, 11, 22, 12, 30),
            None,
        );
        assert!(free.is_none());
    }

    #[test]
    fn slot_generation_drops_past_slots_and_keeps_future_days() {
        let state = AppointmentsState::default();
        let svc = service();
        let today = Utc::now().date_naive();
        let tomorrow = today + Duration::days(1);
        let today_weekday = today.weekday().num_days_from_monday() as i64;
        let tomorrow_weekday = tomorrow.weekday().num_days_from_monday() as i64;
        state.availability.insert(
            Uuid::new_v4(),
            availability_row(
                "staff-1",
                "working_hours",
                Some(today_weekday),
                None,
                Some(0),
                Some(1440),
                None,
            ),
        );
        state.availability.insert(
            Uuid::new_v4(),
            availability_row(
                "staff-1",
                "working_hours",
                Some(tomorrow_weekday),
                None,
                Some(540),
                Some(720),
                None,
            ),
        );
        // Same-day slots must all end after now plus the minimum
        // advance booking time: nothing already past is offered.
        let earliest_end =
            Utc::now() + Duration::minutes(AppointmentSettings::default().min_advance_booking_minutes);
        let today_slots = generate_slots(&state, &svc, &["staff-1".to_string()], today, 60);
        assert!(today_slots.iter().all(|slot| slot.ends_at > earliest_end));
        // Tomorrow is untouched by the past filter: 09:00-12:00 at
        // one hour per slot is exactly three slots.
        let tomorrow_slots =
            generate_slots(&state, &svc, &["staff-1".to_string()], tomorrow, 60);
        assert_eq!(tomorrow_slots.len(), 3);
        let expected_first =
            Utc.from_utc_datetime(&tomorrow.and_hms_opt(9, 0, 0).unwrap());
        assert_eq!(tomorrow_slots[0].starts_at, expected_first);
    }
}
