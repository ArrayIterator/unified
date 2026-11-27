//! Academic - hook handlers for timetracker, e-learning, admissions and calendar.
//!
//! Part of the `academic` module split; see `src/modules/academic.rs`.

use unified_core::hooks::HookContext;
use unified_core::json;
use chrono::DateTime;
use chrono::Datelike;
use chrono::Duration;
use chrono::NaiveDate;
use chrono::Utc;
use serde_json::Value;
use serde_json::json;
use std::sync::Arc;

use super::constants::MAX_MINUTES_PER_DAY;
use super::handlers_enrollment::transition_enrollment;
use super::helpers::new_id;
use super::helpers::now;
use super::helpers::time_to_minutes;
use super::state::AcademicState;
use super::types::Attendance;
use super::types::Schedule;
use super::types::Student;

// Hook handlers

/// Accept both TimeTracker event names and both payload shapes (see
/// the module header): `timetracker.entry_logged` with `{user_id,
/// date, minutes, source}` and `timetracker.time.logged` with
/// `{entry_id, user_id, started_at, ended_at, duration_secs, hours,
/// source}`. A matching class session on that date records
/// attendance with source "timetracker".
pub(crate) fn on_timetracker_entry(state: Arc<AcademicState>, ctx: &mut HookContext) {
    let data = ctx.data.clone();
    let Some(user_id) = data.get("user_id").and_then(|v| v.as_str()) else {
        return;
    };
    let user_id = user_id.to_string();
    let work_date = match data.get("date").and_then(|v| v.as_str()) {
        Some(raw) => match NaiveDate::parse_from_str(raw.trim(), "%Y-%m-%d") {
            Ok(date) => date,
            Err(_) => return,
        },
        None => {
            let timestamp = data
                .get("started_at")
                .and_then(|v| v.as_str())
                .or_else(|| data.get("ended_at").and_then(|v| v.as_str()));
            let Some(timestamp) = timestamp else {
                return;
            };
            match NaiveDate::parse_from_str(&timestamp[..timestamp.len().min(10)], "%Y-%m-%d") {
                Ok(date) => date,
                Err(_) => return,
            }
        }
    };
    let minutes: u64 = if let Some(minutes) = data.get("minutes").and_then(|v| v.as_u64()) {
        minutes
    } else if let Some(secs) = data.get("duration_secs").and_then(|v| v.as_u64()) {
        secs.div_ceil(60)
    } else if let Some(hours) = data.get("hours").and_then(|v| v.as_f64()) {
        (hours * 60.0).round() as u64
    } else {
        return;
    };
    if minutes == 0 || minutes > MAX_MINUTES_PER_DAY {
        return;
    }
    // A class match needs the entry's time window, so entries
    // without start and end timestamps cannot be matched to a
    // session and are ignored.
    let (Some(started_at), Some(ended_at)) = (
        data.get("started_at").and_then(|v| v.as_str()),
        data.get("ended_at").and_then(|v| v.as_str()),
    ) else {
        return;
    };
    let (Some(entry_start), Some(entry_end)) = (
        started_at.get(11..16).and_then(time_to_minutes),
        ended_at.get(11..16).and_then(time_to_minutes),
    ) else {
        return;
    };
    let entry_id = data
        .get("entry_id")
        .and_then(|v| v.as_str())
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty());
    let Some(student) = state.student_by_user(&user_id) else {
        return;
    };
    // De-duplicate repeated delivery of the same TimeTracker entry.
    if let Some(entry_id) = &entry_id {
        if state
            .attendance
            .iter()
            .any(|entry| entry.value().entry_id.as_deref() == Some(entry_id.as_str()))
        {
            return;
        }
    }
    let day_of_week = work_date.weekday().number_from_monday() as i32;
    let sessions: Vec<Schedule> = state
        .schedules
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|schedule| {
            schedule.status == "active"
                && schedule.day_of_week == day_of_week
                && state
                    .semesters
                    .get(&schedule.semester_id)
                    .is_some_and(|semester| {
                        work_date >= semester.start_date && work_date <= semester.end_date
                    })
                && state.enrollments.iter().any(|entry| {
                    let enrollment = entry.value();
                    enrollment.student_id == student.id
                        && enrollment.subject_id == schedule.subject_id
                        && enrollment.semester_id == schedule.semester_id
                        && matches!(enrollment.status.as_str(), "approved" | "completed")
                })
        })
        .collect();
    for schedule in sessions {
        let class_start = time_to_minutes(&schedule.start_time).unwrap_or(0);
        let class_end = time_to_minutes(&schedule.end_time).unwrap_or(0);
        if entry_start >= class_end || class_start >= entry_end {
            continue;
        }
        if state.attendance.iter().any(|entry| {
            let record = entry.value();
            record.schedule_id == schedule.id
                && record.student_id == student.id
                && record.date == work_date
        }) {
            continue;
        }
        let record = Attendance {
            id: new_id(),
            schedule_id: schedule.id.clone(),
            student_id: student.id.clone(),
            date: work_date,
            status: "present".to_string(),
            source: "timetracker".to_string(),
            entry_id: entry_id.clone(),
            minutes: Some(minutes),
            notes: None,
            created_at: now(),
            updated_at: now(),
        };
        state.attendance.insert(record.id.clone(), record.clone());
        state.emit(
            "academic.attendance.recorded",
            json!({
                "attendance_id": record.id,
                "student_id": record.student_id,
                "schedule_id": record.schedule_id,
                "date": record.date.format("%Y-%m-%d").to_string(),
                "status": record.status,
                "source": record.source,
            }),
        );
    }
}

/// Keep the e-learning link flag fresh: a created or updated course
/// verifies linked subjects; a deleted course un-verifies them.
pub(crate) fn on_elearning_course_event(
    state: Arc<AcademicState>,
    ctx: &mut HookContext,
    verified: bool,
) {
    let data = ctx.data.clone();
    let Some(course_id) = data.get("course_id").and_then(|v| v.as_str()) else {
        return;
    };
    for entry in state.subjects.iter() {
        if entry.value().elearning_course_id.as_deref() == Some(course_id) {
            let mut subject = entry.value().clone();
            subject.elearning_course_verified = verified;
            subject.updated_at = now();
            state.subjects.insert(subject.id.clone(), subject);
        }
    }
}

/// When a learner completes an e-learning course, mark the linked
/// academic enrollment completed (payload: `{enrollment_id,
/// course_id, user_id}`).
pub(crate) fn on_elearning_enrollment_completed(state: Arc<AcademicState>, ctx: &mut HookContext) {
    let data = ctx.data.clone();
    let (Some(course_id), Some(user_id)) = (
        data.get("course_id").and_then(|v| v.as_str()),
        data.get("user_id").and_then(|v| v.as_str()),
    ) else {
        return;
    };
    let Some(student) = state.student_by_user(user_id) else {
        return;
    };
    let subject_ids: Vec<String> = state
        .subjects
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|subject| subject.elearning_course_id.as_deref() == Some(course_id))
        .map(|subject| subject.id)
        .collect();
    for entry in state.enrollments.iter() {
        let mut enrollment = entry.value().clone();
        if enrollment.student_id == student.id
            && subject_ids.contains(&enrollment.subject_id)
            && enrollment.status == "approved"
        {
            if transition_enrollment(&state, &mut enrollment, "completed").is_ok() {
                state
                    .enrollments
                    .insert(enrollment.id.clone(), enrollment.clone());
                state.emit(
                    "academic.enrollment.completed",
                    json!({
                        "enrollment_id": enrollment.id,
                        "student_id": enrollment.student_id,
                        "subject_id": enrollment.subject_id,
                    }),
                );
            }
        }
    }
}

/// Derive the enrollment year for a student created from an
/// admissions application: the caller-supplied `enrollment_year`
/// when it is a plausible year, otherwise the leading four digits
/// of the free-text `academic_year` (for example "2026/2027"),
/// otherwise the current calendar year. Never fails: a hook
/// handler has no error channel, so an unparseable year falls back
/// instead of dropping the handoff.
fn enrollment_year_for(supplied: Option<i64>, academic_year: Option<&str>) -> i32 {
    if let Some(year) = supplied {
        if (1990..=2100).contains(&year) {
            return year as i32;
        }
    }
    if let Some(raw) = academic_year {
        let digits: String = raw.trim().chars().take(4).collect();
        if let Ok(year) = digits.parse::<i32>() {
            if (1990..=2100).contains(&year) {
                return year;
            }
        }
    }
    Utc::now().date_naive().year()
}

/// Handle `academic.student.create_requested` (admissions ->
/// academic handoff). Creates the student record from the admitted
/// application and answers with `academic.student.created`
/// carrying the same `application_id`, so admissions can link the
/// student back to the application.
///
/// Idempotent: when a student already exists for the applicant
/// user (redelivery, or an earlier manual creation), the existing
/// record is reused and the answer is still emitted - never a
/// duplicate student. Skipped silently, with no student and no
/// answer, only when the payload names no user, names no program,
/// or names a program this module does not know: a student record
/// must reference a real program, and a hook handler cannot report
/// the failure back to the caller.
pub(crate) fn on_admissions_student_create_requested(
    state: Arc<AcademicState>,
    ctx: &mut HookContext,
) {
    let data = ctx.data.clone();
    let user_id = data
        .get("applicant_user_id")
        .or_else(|| data.get("user_id"))
        .and_then(|value| value.as_str())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| value.to_string());
    let Some(user_id) = user_id else {
        return;
    };
    let program_id = data
        .get("program_id")
        .and_then(|value| value.as_str())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| value.to_string());
    let Some(program_id) = program_id else {
        return;
    };
    if !state.programs.contains_key(&program_id) {
        return;
    }
    let application_id = data
        .get("application_id")
        .and_then(|value| value.as_str())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string);
    // Reuse the existing student for this user when there is one.
    let student = match state.student_by_user(&user_id) {
        Some(existing) => existing,
        None => {
            let mut student_number = format!("STU-{}", &new_id()[..8].to_uppercase());
            while state
                .students
                .iter()
                .any(|entry| entry.value().student_number == student_number)
            {
                student_number = format!("STU-{}", &new_id()[..8].to_uppercase());
            }
            let enrollment_year = enrollment_year_for(
                data.get("enrollment_year").and_then(|value| value.as_i64()),
                data.get("academic_year").and_then(|value| value.as_str()),
            );
            let created = Student {
                id: new_id(),
                user_id: user_id.clone(),
                student_number,
                program_id: program_id.clone(),
                advisor_id: None,
                enrollment_year,
                status: "active".to_string(),
                created_at: now(),
                updated_at: now(),
            };
            state.students.insert(created.id.clone(), created.clone());
            created
        }
    };
    state.emit(
        "academic.student.created",
        json!({
            "student_id": student.id,
            "user_id": student.user_id,
            "program_id": student.program_id,
            "application_id": application_id,
        }),
    );
}

/// Answer the `academic.student.lookup` hook query. The reference
/// may be a student id, a linked user id, or a student number -
/// curriculum's `student_ref` is opaque, so all three are accepted.
/// Answers with one result `{exists, student_id?, user_id?,
/// program_id?}` (the tree's hook-query convention) and also sets
/// the `exists` key on the payload, so callers that read the
/// payload instead of the results see the same answer. No answer
/// is added when the reference is empty: an empty reference is not
/// a lookup.
pub(crate) fn on_academic_student_lookup(state: Arc<AcademicState>, ctx: &mut HookContext) {
    let data = ctx.data.clone();
    let reference = data
        .get("student_ref")
        .or_else(|| data.get("student_id"))
        .or_else(|| data.get("user_id"))
        .and_then(|value| value.as_str())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| value.to_string());
    let Some(reference) = reference else {
        return;
    };
    let found = state
        .students
        .get(&reference)
        .map(|entry| entry.value().clone())
        .or_else(|| state.student_by_user(&reference))
        .or_else(|| {
            state
                .students
                .iter()
                .find(|entry| entry.value().student_number == reference)
                .map(|entry| entry.value().clone())
        });
    match found {
        Some(student) => {
            ctx.set("exists", json!(true));
            ctx.add_result(json!({
                "exists": true,
                "student_id": student.id,
                "user_id": student.user_id,
                "program_id": student.program_id,
            }));
        }
        None => {
            ctx.set("exists", json!(false));
            ctx.add_result(json!({ "exists": false }));
        }
    }
}

/// Parse an RFC 3339 bound from a `calendar.items_requested`
/// request. Returns `None` when the value is missing or malformed;
/// the caller then contributes nothing rather than guessing a
/// range.
fn parse_calendar_bound(raw: Option<&Value>) -> Option<DateTime<Utc>> {
    let text = raw.and_then(|value| value.as_str())?;
    DateTime::parse_from_rfc3339(text)
        .ok()
        .map(|fixed| fixed.with_timezone(&Utc))
}

/// Answer `calendar.items_requested` with this module's dated
/// items in the shared calendar shape (`{id, module: "academic",
/// title, start, end?, all_day, label?, deep_link?, visibility:
/// "shared", owner_user_id?}`). Contributed, in the requested
/// `[start, end]` window:
///   - academic calendar events (holidays, exam periods,
///     registration windows) as all-day items spanning
///     `start_date` through `end_date` inclusive;
///   - one item per occurrence of each active weekly class
///     schedule inside its semester's dates;
///   - thesis defenses with a scheduled date.
/// Semester start/end dates themselves are not contributed: they
/// are boundaries, and an administrator records them as calendar
/// events when they should appear. The window expansion is capped
/// at 366 days and 1,000 items so a wide-open range cannot explode
/// into unbounded occurrences. Academic items carry no tenant or
/// company scope (this store has no tenant partition yet), so no
/// `tenant_id`/`company_id` is set and the calendar's foreign-scope
/// exclusion never drops them. When the request's `modules` filter
/// does not include `academic`, nothing is contributed.
pub(crate) fn on_calendar_items_requested(state: Arc<AcademicState>, ctx: &mut HookContext) {
    let data = ctx.data.clone();
    if let Some(modules) = data.get("modules").and_then(|value| value.as_array()) {
        let wanted = modules
            .iter()
            .any(|value| value.as_str() == Some("academic"));
        if !wanted {
            return;
        }
    }
    let (Some(start), Some(end)) = (
        parse_calendar_bound(data.get("start")),
        parse_calendar_bound(data.get("end")),
    ) else {
        return;
    };
    if end < start {
        return;
    }
    let mut items: Vec<Value> = Vec::new();
    // Academic calendar events: all-day, start_date 00:00 UTC
    // through the day after end_date 00:00 UTC (end exclusive).
    for event in state
        .calendar_events
        .iter()
        .map(|entry| entry.value().clone())
    {
        if items.len() >= 1000 {
            break;
        }
        let Some(item_start) = event
            .start_date
            .and_hms_opt(0, 0, 0)
            .map(|time| time.and_utc())
        else {
            continue;
        };
        let Some(item_end) = (event.end_date + Duration::days(1))
            .and_hms_opt(0, 0, 0)
            .map(|time| time.and_utc())
        else {
            continue;
        };
        if item_start > end || item_end < start {
            continue;
        }
        items.push(json!({
            "id": format!("calendar-event:{}", event.id),
            "module": "academic",
            "title": event.title,
            "start": item_start.to_rfc3339(),
            "end": item_end.to_rfc3339(),
            "all_day": true,
            "label": event.event_type,
            "deep_link": format!("/api/v1/academic/calendar/{}", event.id),
            "visibility": "shared",
        }));
    }
    // Class schedules: one item per weekly occurrence inside the
    // semester's dates and the requested window.
    let window_days = (end.date_naive() - start.date_naive()).num_days().min(366);
    for schedule in state
        .schedules
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|schedule| schedule.status == "active")
    {
        if items.len() >= 1000 {
            break;
        }
        let Some(semester) = state
            .semesters
            .get(&schedule.semester_id)
            .map(|entry| entry.value().clone())
        else {
            continue;
        };
        let (Some(class_start_minutes), Some(class_end_minutes)) = (
            time_to_minutes(&schedule.start_time),
            time_to_minutes(&schedule.end_time),
        ) else {
            continue;
        };
        let first = start.date_naive().max(semester.start_date);
        let last = end
            .date_naive()
            .min(semester.end_date)
            .min(first + Duration::days(window_days));
        let mut date = first;
        while date <= last {
            if items.len() >= 1000 {
                break;
            }
            if date.weekday().number_from_monday() as i32 == schedule.day_of_week {
                let start_time = (class_start_minutes / 60, class_start_minutes % 60);
                let end_time = (class_end_minutes / 60, class_end_minutes % 60);
                if let (Some(item_start), Some(item_end)) = (
                    date.and_hms_opt(start_time.0 as u32, start_time.1 as u32, 0)
                        .map(|time| time.and_utc()),
                    date.and_hms_opt(end_time.0 as u32, end_time.1 as u32, 0)
                        .map(|time| time.and_utc()),
                ) {
                    if item_start >= start && item_start <= end {
                        let title = state
                            .subjects
                            .get(&schedule.subject_id)
                            .map(|entry| entry.value().title.clone())
                            .unwrap_or_else(|| "Class".to_string());
                        let owner_user_id = state
                            .lecturers
                            .get(&schedule.lecturer_id)
                            .map(|entry| entry.value().user_id.clone());
                        items.push(json!({
                            "id": format!("schedule:{}:{}", schedule.id, date.format("%Y-%m-%d")),
                            "module": "academic",
                            "title": title,
                            "start": item_start.to_rfc3339(),
                            "end": item_end.to_rfc3339(),
                            "all_day": false,
                            "label": "Class",
                            "deep_link": format!("/api/v1/academic/schedules/{}", schedule.id),
                            "visibility": "shared",
                            "owner_user_id": owner_user_id,
                        }));
                    }
                }
            }
            date += Duration::days(1);
        }
    }
    // Thesis defenses with a scheduled date: all-day items.
    for thesis in state.theses.iter().map(|entry| entry.value().clone()) {
        if items.len() >= 1000 {
            break;
        }
        let Some(defense_date) = thesis.defense_date else {
            continue;
        };
        let Some(item_start) = defense_date.and_hms_opt(0, 0, 0).map(|time| time.and_utc()) else {
            continue;
        };
        let Some(item_end) = (defense_date + Duration::days(1))
            .and_hms_opt(0, 0, 0)
            .map(|time| time.and_utc())
        else {
            continue;
        };
        if item_start < start || item_start > end {
            continue;
        }
        let owner_user_id = state
            .students
            .get(&thesis.student_id)
            .map(|entry| entry.value().user_id.clone());
        items.push(json!({
            "id": format!("thesis-defense:{}", thesis.id),
            "module": "academic",
            "title": format!("Thesis defense: {}", thesis.title),
            "start": item_start.to_rfc3339(),
            "end": item_end.to_rfc3339(),
            "all_day": true,
            "label": "Thesis defense",
            "deep_link": format!("/api/v1/academic/theses/{}", thesis.id),
            "visibility": "shared",
            "owner_user_id": owner_user_id,
        }));
    }
    ctx.add_result(json!({ "items": items }));
}
