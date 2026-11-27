//! Academic - enrollment and attendance handlers.
//!
//! Part of the `academic` module split; see `src/modules/academic.rs`.

use unified_core::context::AppContext;
use unified_core::error::UnifiedError;
use unified_core::error::UnifiedResult;
use unified_core::json;
use unified_core::pagination::PageResult;
use unified_core::pagination::PaginationParams;
use actix_web::HttpRequest;
use actix_web::HttpResponse;
use actix_web::web;
use serde_json::json;
use std::sync::Arc;

use super::constants::KIND_ATTENDANCE_STATUS;
use super::constants::KIND_ENROLLMENT_TRANSITION;
use super::constants::MAX_CODE;
use super::constants::MAX_MINUTES_PER_DAY;
use super::constants::MAX_TEXT;
use super::constants::PERM_ACADEMIC_MANAGE;
use super::constants::PERM_ACADEMIC_READ;
use super::constants::PERM_ACADEMIC_READ_ALL;
use super::helpers::caller;
use super::helpers::caller_company;
use super::helpers::can_approve_enrollment;
use super::helpers::can_read_student;
use super::helpers::check_capability;
use super::helpers::filter_matches;
use super::helpers::is_staff;
use super::helpers::load_student_for_read;
use super::helpers::new_id;
use super::helpers::now;
use super::helpers::optional_text;
use super::helpers::paginate;
use super::helpers::parse_date;
use super::helpers::require_company;
use super::helpers::require_permission;
use super::helpers::require_tenant;
use super::helpers::require_text;
use super::helpers::teaches_subject;
use super::helpers::today;
use super::state::AcademicState;
use super::types::Attendance;
use super::types::CreateAttendance;
use super::types::CreateEnrollment;
use super::types::Enrollment;
use super::types::RejectEnrollment;
use super::types::UpdateAttendance;

// Handlers: enrollments (course registration)

pub(crate) async fn list_enrollments(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<AcademicState>>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PageResult<Enrollment>> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ACADEMIC_READ)?;
        let params = query.into_inner();
        let read_all = check_capability(&ctx, &caller.role, PERM_ACADEMIC_READ_ALL);
        let own_student = state.student_by_user(&caller.actor);
        let own_lecturer = state.lecturer_by_user(&caller.actor);
        let mut items: Vec<Enrollment> = state
            .enrollments
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|enrollment| {
                let visible = read_all
                    || own_student
                        .as_ref()
                        .is_some_and(|student| student.id == enrollment.student_id)
                    || own_lecturer
                        .as_ref()
                        .is_some_and(|_| teaches_subject(&state, &caller, &enrollment.subject_id))
                    || check_capability(&ctx, &caller.role, PERM_ACADEMIC_MANAGE);
                visible
                    && filter_matches(&params, "student_id", &enrollment.student_id)
                    && filter_matches(&params, "subject_id", &enrollment.subject_id)
                    && filter_matches(&params, "semester_id", &enrollment.semester_id)
                    && filter_matches(&params, "status", &enrollment.status)
            })
            .collect();
        items.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        Ok(paginate(items, &params))
    })();
    json::respond(result, debug)
}

pub(crate) async fn list_student_enrollments(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<AcademicState>>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PageResult<Enrollment>> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ACADEMIC_READ)?;
        let student = load_student_for_read(&state, &ctx, &caller, path.as_str())?;
        let params = query.into_inner();
        let mut items: Vec<Enrollment> = state
            .enrollments
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|enrollment| {
                enrollment.student_id == student.id
                    && filter_matches(&params, "subject_id", &enrollment.subject_id)
                    && filter_matches(&params, "semester_id", &enrollment.semester_id)
                    && filter_matches(&params, "status", &enrollment.status)
            })
            .collect();
        items.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        Ok(paginate(items, &params))
    })();
    json::respond(result, debug)
}

pub(crate) async fn create_enrollment(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<AcademicState>>,
    req: HttpRequest,
    body: web::Json<CreateEnrollment>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Enrollment> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = require_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ACADEMIC_READ)?;
        let body = body.into_inner();
        let student = state
            .students
            .get(&body.student_id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Student not found"))?;
        // A student registers themself; staff may register any
        // student. Anyone else gets the same 404 as a missing
        // student, so ids cannot be probed.
        let self_registration = student.user_id == caller.actor;
        let staff = is_staff(&ctx, &caller.role);
        if !self_registration && !staff {
            return Err(UnifiedError::not_found("Student not found"));
        }
        if student.status != "active" {
            return Err(UnifiedError::conflict(
                "Only students with status \"active\" may register for courses",
            ));
        }
        let subject = state
            .subjects
            .get(&body.subject_id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Subject not found"))?;
        if subject.status != "active" {
            return Err(UnifiedError::conflict(
                "Only active subjects accept registrations",
            ));
        }
        if subject.program_id != student.program_id {
            return Err(UnifiedError::bad_request(
                "Subject belongs to a different program than the student",
            ));
        }
        let semester = state
            .semesters
            .get(&body.semester_id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Semester not found"))?;
        if semester.status == "completed" {
            return Err(UnifiedError::conflict(
                "Registrations are closed for a completed semester",
            ));
        }
        if state.enrollments.iter().any(|entry| {
            let enrollment = entry.value();
            enrollment.student_id == student.id
                && enrollment.subject_id == subject.id
                && enrollment.semester_id == semester.id
                && matches!(enrollment.status.as_str(), "requested" | "approved")
        }) {
            return Err(UnifiedError::conflict(
                "Student is already registered for this subject in this semester",
            ));
        }
        let cap = state.credit_cap_for(&student.program_id);
        let used = state.registered_credits(&student.id, &semester.id);
        if used + subject.credits > cap {
            return Err(UnifiedError::conflict(format!(
                "Credit limit exceeded: {used} registered + {} for this subject exceeds the cap of {cap} credits for this program",
                subject.credits
            )));
        }
        let enrollment = Enrollment {
            id: new_id(),
            student_id: student.id.clone(),
            subject_id: subject.id.clone(),
            semester_id: semester.id.clone(),
            credits: subject.credits,
            status: "requested".to_string(),
            advisor_id: student.advisor_id.clone(),
            rejection_reason: None,
            created_at: now(),
            updated_at: now(),
        };
        state
            .enrollments
            .insert(enrollment.id.clone(), enrollment.clone());
        state.emit(
            "academic.enrollment.requested",
            json!({
                "enrollment_id": enrollment.id,
                "student_id": enrollment.student_id,
                "subject_id": enrollment.subject_id,
                "semester_id": enrollment.semester_id,
                "credits": enrollment.credits,
            }),
        );
        Ok(enrollment)
    })();
    json::respond(result, debug)
}

pub(crate) async fn get_enrollment(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<AcademicState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Enrollment> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ACADEMIC_READ)?;
        let enrollment = state
            .enrollments
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Enrollment not found"))?;
        let student = state
            .students
            .get(&enrollment.student_id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Enrollment not found"))?;
        let visible = can_read_student(&state, &ctx, &caller, &student)
            || teaches_subject(&state, &caller, &enrollment.subject_id);
        if !visible {
            return Err(UnifiedError::not_found("Enrollment not found"));
        }
        Ok(enrollment)
    })();
    json::respond(result, debug)
}

pub(crate) fn transition_enrollment(
    state: &AcademicState,
    enrollment: &mut Enrollment,
    to: &str,
) -> UnifiedResult<()> {
    let allowed = state.allowed_transitions(KIND_ENROLLMENT_TRANSITION);
    if !allowed
        .iter()
        .any(|(from, target)| from == &enrollment.status && target == to)
    {
        return Err(UnifiedError::conflict(format!(
            "Enrollment cannot move from \"{}\" to \"{to}\"",
            enrollment.status
        )));
    }
    enrollment.status = to.to_string();
    enrollment.updated_at = now();
    Ok(())
}

pub(crate) async fn approve_enrollment(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<AcademicState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Enrollment> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = require_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        let mut enrollment = state
            .enrollments
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Enrollment not found"))?;
        if !can_approve_enrollment(&state, &ctx, &caller, &enrollment) {
            return Err(UnifiedError::forbidden(
                "Only the student's advisor or a manager may approve this registration",
            ));
        }
        transition_enrollment(&state, &mut enrollment, "approved")?;
        state
            .enrollments
            .insert(enrollment.id.clone(), enrollment.clone());
        state.emit(
            "academic.enrollment.approved",
            json!({
                "enrollment_id": enrollment.id,
                "student_id": enrollment.student_id,
                "subject_id": enrollment.subject_id,
                "semester_id": enrollment.semester_id,
            }),
        );
        if let Some(student) = state.students.get(&enrollment.student_id) {
            state.notify(
                &student.user_id,
                "Course registration approved",
                "Your course registration was approved by your advisor.",
            );
        }
        Ok(enrollment)
    })();
    json::respond(result, debug)
}

pub(crate) async fn reject_enrollment(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<AcademicState>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<RejectEnrollment>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Enrollment> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = require_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        let reason = require_text(&body.reason, "reason", MAX_TEXT)?;
        let mut enrollment = state
            .enrollments
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Enrollment not found"))?;
        if !can_approve_enrollment(&state, &ctx, &caller, &enrollment) {
            return Err(UnifiedError::forbidden(
                "Only the student's advisor or a manager may reject this registration",
            ));
        }
        transition_enrollment(&state, &mut enrollment, "rejected")?;
        enrollment.rejection_reason = Some(reason.clone());
        state
            .enrollments
            .insert(enrollment.id.clone(), enrollment.clone());
        state.emit(
            "academic.enrollment.rejected",
            json!({
                "enrollment_id": enrollment.id,
                "student_id": enrollment.student_id,
                "subject_id": enrollment.subject_id,
                "reason": reason,
            }),
        );
        if let Some(student) = state.students.get(&enrollment.student_id) {
            state.notify(
                &student.user_id,
                "Course registration rejected",
                &format!("Your course registration was rejected: {reason}"),
            );
        }
        Ok(enrollment)
    })();
    json::respond(result, debug)
}

pub(crate) async fn drop_enrollment(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<AcademicState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Enrollment> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = require_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        let mut enrollment = state
            .enrollments
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Enrollment not found"))?;
        let student = state
            .students
            .get(&enrollment.student_id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Enrollment not found"))?;
        let own = student.user_id == caller.actor;
        let staff = is_staff(&ctx, &caller.role);
        if !own && !staff {
            return Err(UnifiedError::not_found("Enrollment not found"));
        }
        if enrollment.status == "completed" {
            return Err(UnifiedError::conflict(
                "A completed enrollment cannot be dropped",
            ));
        }
        transition_enrollment(&state, &mut enrollment, "dropped")?;
        state
            .enrollments
            .insert(enrollment.id.clone(), enrollment.clone());
        state.emit(
            "academic.enrollment.dropped",
            json!({
                "enrollment_id": enrollment.id,
                "student_id": enrollment.student_id,
                "subject_id": enrollment.subject_id,
            }),
        );
        Ok(enrollment)
    })();
    json::respond(result, debug)
}

// Handlers: attendance

pub(crate) async fn list_attendance(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<AcademicState>>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PageResult<Attendance>> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ACADEMIC_READ)?;
        let params = query.into_inner();
        let read_all = check_capability(&ctx, &caller.role, PERM_ACADEMIC_READ_ALL);
        let own_student = state.student_by_user(&caller.actor);
        let own_lecturer = state.lecturer_by_user(&caller.actor);
        let mut items: Vec<Attendance> = state
            .attendance
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|record| {
                let visible = read_all
                    || own_student
                        .as_ref()
                        .is_some_and(|student| student.id == record.student_id)
                    || own_lecturer.as_ref().is_some_and(|lecturer| {
                        state
                            .schedules
                            .get(&record.schedule_id)
                            .is_some_and(|schedule| schedule.lecturer_id == lecturer.id)
                    })
                    || check_capability(&ctx, &caller.role, PERM_ACADEMIC_MANAGE);
                visible
                    && filter_matches(&params, "schedule_id", &record.schedule_id)
                    && filter_matches(&params, "student_id", &record.student_id)
                    && filter_matches(&params, "status", &record.status)
                    && filter_matches(&params, "source", &record.source)
                    && params
                        .filters
                        .get("date")
                        .is_none_or(|date| record.date.format("%Y-%m-%d").to_string() == *date)
            })
            .collect();
        items.sort_by(|a, b| b.date.cmp(&a.date));
        Ok(paginate(items, &params))
    })();
    json::respond(result, debug)
}

pub(crate) async fn create_attendance(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<AcademicState>>,
    req: HttpRequest,
    body: web::Json<CreateAttendance>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Attendance> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = require_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ACADEMIC_MANAGE)?;
        let body = body.into_inner();
        let schedule = state
            .schedules
            .get(&body.schedule_id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Schedule not found"))?;
        let student = state
            .students
            .get(&body.student_id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Student not found"))?;
        if !state.enrollments.iter().any(|entry| {
            let enrollment = entry.value();
            enrollment.student_id == student.id
                && enrollment.subject_id == schedule.subject_id
                && enrollment.semester_id == schedule.semester_id
                && matches!(enrollment.status.as_str(), "approved" | "completed")
        }) {
            return Err(UnifiedError::conflict(
                "Student is not enrolled in this class",
            ));
        }
        let date = parse_date(&body.date, "date")?;
        if date > today() {
            return Err(UnifiedError::bad_request(
                "Attendance cannot be recorded for a future date",
            ));
        }
        let status = match &body.status {
            None => "present".to_string(),
            Some(value) => {
                let status = require_text(value, "status", MAX_CODE)?;
                if !state.valid_status(KIND_ATTENDANCE_STATUS, &status) {
                    return Err(UnifiedError::bad_request(format!(
                        "Unknown attendance status \"{status}\""
                    )));
                }
                status
            }
        };
        if state.attendance.iter().any(|entry| {
            let record = entry.value();
            record.schedule_id == schedule.id
                && record.student_id == student.id
                && record.date == date
        }) {
            return Err(UnifiedError::conflict(
                "Attendance for this student, class, and date already exists",
            ));
        }
        let minutes = match body.minutes {
            None => None,
            Some(value) => {
                if value > MAX_MINUTES_PER_DAY {
                    return Err(UnifiedError::bad_request("minutes must not exceed 1440"));
                }
                Some(value)
            }
        };
        let record = Attendance {
            id: new_id(),
            schedule_id: schedule.id.clone(),
            student_id: student.id.clone(),
            date,
            status: status.clone(),
            source: "manual".to_string(),
            entry_id: None,
            minutes,
            notes: optional_text(&body.notes, "notes", MAX_TEXT)?,
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
        Ok(record)
    })();
    json::respond(result, debug)
}

pub(crate) async fn get_attendance(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<AcademicState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Attendance> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ACADEMIC_READ)?;
        let record = state
            .attendance
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Attendance record not found"))?;
        let student = state
            .students
            .get(&record.student_id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Attendance record not found"))?;
        if !can_read_student(&state, &ctx, &caller, &student) {
            return Err(UnifiedError::not_found("Attendance record not found"));
        }
        Ok(record)
    })();
    json::respond(result, debug)
}

pub(crate) async fn update_attendance(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<AcademicState>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdateAttendance>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Attendance> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = require_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ACADEMIC_MANAGE)?;
        let body = body.into_inner();
        let mut record = state
            .attendance
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Attendance record not found"))?;
        if let Some(status) = &body.status {
            let status = require_text(status, "status", MAX_CODE)?;
            if !state.valid_status(KIND_ATTENDANCE_STATUS, &status) {
                return Err(UnifiedError::bad_request(format!(
                    "Unknown attendance status \"{status}\""
                )));
            }
            record.status = status;
        }
        if let Some(minutes) = &body.minutes {
            record.minutes = match minutes {
                None => None,
                Some(value) => {
                    if *value > MAX_MINUTES_PER_DAY {
                        return Err(UnifiedError::bad_request("minutes must not exceed 1440"));
                    }
                    Some(*value)
                }
            };
        }
        if let Some(notes) = &body.notes {
            record.notes = optional_text(notes, "notes", MAX_TEXT)?;
        }
        record.updated_at = now();
        state.attendance.insert(record.id.clone(), record.clone());
        Ok(record)
    })();
    json::respond(result, debug)
}

pub(crate) async fn list_schedule_attendance(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<AcademicState>>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PageResult<Attendance>> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ACADEMIC_READ)?;
        let schedule_id = path.into_inner();
        let schedule = state
            .schedules
            .get(&schedule_id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Schedule not found"))?;
        let allowed = check_capability(&ctx, &caller.role, PERM_ACADEMIC_READ_ALL)
            || check_capability(&ctx, &caller.role, PERM_ACADEMIC_MANAGE)
            || state
                .lecturer_by_user(&caller.actor)
                .is_some_and(|lecturer| lecturer.id == schedule.lecturer_id);
        if !allowed {
            return Err(UnifiedError::not_found("Schedule not found"));
        }
        let params = query.into_inner();
        let mut items: Vec<Attendance> = state
            .attendance
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|record| {
                record.schedule_id == schedule_id
                    && filter_matches(&params, "student_id", &record.student_id)
                    && filter_matches(&params, "status", &record.status)
                    && params
                        .filters
                        .get("date")
                        .is_none_or(|date| record.date.format("%Y-%m-%d").to_string() == *date)
            })
            .collect();
        items.sort_by(|a, b| b.date.cmp(&a.date));
        Ok(paginate(items, &params))
    })();
    json::respond(result, debug)
}
