//! Academic - program, student, lecturer and subject handlers.
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
use serde_json::Value;
use serde_json::json;
use std::sync::Arc;

use super::constants::KIND_LECTURER_STATUS;
use super::constants::KIND_STUDENT_STATUS;
use super::constants::MAX_CODE;
use super::constants::MAX_TEXT;
use super::constants::PERM_ACADEMIC_MANAGE;
use super::constants::PERM_ACADEMIC_READ;
use super::constants::PERM_ACADEMIC_READ_ALL;
use super::helpers::caller;
use super::helpers::caller_company;
use super::helpers::check_capability;
use super::helpers::filter_matches;
use super::helpers::load_student_for_read;
use super::helpers::matches_search;
use super::helpers::new_id;
use super::helpers::now;
use super::helpers::optional_text;
use super::helpers::paginate;
use super::helpers::parse_credits;
use super::helpers::parse_year;
use super::helpers::require_code;
use super::helpers::require_company;
use super::helpers::require_permission;
use super::helpers::require_tenant;
use super::helpers::require_text;
use super::state::AcademicState;
use super::types::Attendance;
use super::types::CreateLecturer;
use super::types::CreateProgram;
use super::types::CreateStudent;
use super::types::CreateSubject;
use super::types::Enrollment;
use super::types::Lecturer;
use super::types::LinkElearningCourse;
use super::types::Program;
use super::types::Schedule;
use super::types::Student;
use super::types::StudentSummary;
use super::types::Subject;
use super::types::UpdateLecturer;
use super::types::UpdateProgram;
use super::types::UpdateStudent;
use super::types::UpdateSubject;

// ---------------------------------------------------------------------------
// Handlers: programs
// ---------------------------------------------------------------------------

pub(crate) async fn list_programs(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<AcademicState>>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PageResult<Program>> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ACADEMIC_READ)?;
        let params = query.into_inner();
        let mut items: Vec<Program> = state
            .programs
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|program| {
                matches_search(&format!("{} {}", program.code, program.name), &params)
                    && filter_matches(&params, "status", &program.status)
            })
            .collect();
        items.sort_by(|a, b| a.code.cmp(&b.code));
        Ok(paginate(items, &params))
    })();
    json::respond(result, debug)
}

pub(crate) async fn create_program(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<AcademicState>>,
    req: HttpRequest,
    body: web::Json<CreateProgram>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Program> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = require_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ACADEMIC_MANAGE)?;
        let body = body.into_inner();
        let code = require_code(&body.code, "code")?;
        let name = require_text(&body.name, "name", MAX_TEXT)?;
        if state
            .programs
            .iter()
            .any(|entry| entry.value().code.eq_ignore_ascii_case(&code))
        {
            return Err(UnifiedError::conflict(format!(
                "A program with code \"{code}\" already exists"
            )));
        }
        let max_credits = match body.max_credits_per_semester {
            None => None,
            Some(value) => Some(parse_credits(value, "max_credits_per_semester")?),
        };
        let status = match &body.status {
            None => "active".to_string(),
            Some(value) => {
                let status = require_text(value, "status", MAX_CODE)?;
                if status != "active" && status != "inactive" {
                    return Err(UnifiedError::bad_request(
                        "status must be \"active\" or \"inactive\"",
                    ));
                }
                status
            }
        };
        let program = Program {
            id: new_id(),
            code: code.clone(),
            name,
            department_id: optional_text(&body.department_id, "department_id", MAX_CODE)?,
            max_credits_per_semester: max_credits,
            status,
            created_at: now(),
            updated_at: now(),
        };
        state.programs.insert(program.id.clone(), program.clone());
        state.emit(
            "academic.program.created",
            json!({ "program_id": program.id, "code": program.code }),
        );
        Ok(program)
    })();
    json::respond(result, debug)
}

pub(crate) async fn get_program(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<AcademicState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Program> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ACADEMIC_READ)?;
        state
            .programs
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Program not found"))
    })();
    json::respond(result, debug)
}

pub(crate) async fn update_program(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<AcademicState>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdateProgram>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Program> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = require_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ACADEMIC_MANAGE)?;
        let body = body.into_inner();
        let mut program = state
            .programs
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Program not found"))?;
        if let Some(name) = &body.name {
            program.name = require_text(name, "name", MAX_TEXT)?;
        }
        if let Some(department_id) = &body.department_id {
            program.department_id = optional_text(department_id, "department_id", MAX_CODE)?;
        }
        if let Some(max_credits) = &body.max_credits_per_semester {
            program.max_credits_per_semester = match max_credits {
                None => None,
                Some(value) => Some(parse_credits(*value, "max_credits_per_semester")?),
            };
        }
        if let Some(status) = &body.status {
            let status = require_text(status, "status", MAX_CODE)?;
            if status != "active" && status != "inactive" {
                return Err(UnifiedError::bad_request(
                    "status must be \"active\" or \"inactive\"",
                ));
            }
            program.status = status;
        }
        program.updated_at = now();
        state.programs.insert(program.id.clone(), program.clone());
        Ok(program)
    })();
    json::respond(result, debug)
}

pub(crate) async fn delete_program(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<AcademicState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = require_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ACADEMIC_MANAGE)?;
        let id = path.into_inner();
        if !state.programs.contains_key(&id) {
            return Err(UnifiedError::not_found("Program not found"));
        }
        if state
            .students
            .iter()
            .any(|entry| entry.value().program_id == id)
        {
            return Err(UnifiedError::conflict(
                "Program cannot be deleted while students are enrolled in it",
            ));
        }
        if state
            .subjects
            .iter()
            .any(|entry| entry.value().program_id == id)
        {
            return Err(UnifiedError::conflict(
                "Program cannot be deleted while subjects belong to it",
            ));
        }
        state.programs.remove(&id);
        Ok(json!({ "deleted": true, "id": id }))
    })();
    json::respond(result, debug)
}

// ---------------------------------------------------------------------------
// Handlers: students
// ---------------------------------------------------------------------------

pub(crate) async fn list_students(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<AcademicState>>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PageResult<Student>> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ACADEMIC_READ)?;
        let params = query.into_inner();
        let read_all = check_capability(&ctx, &caller.role, PERM_ACADEMIC_READ_ALL);
        let own = state.student_by_user(&caller.actor);
        let advisor = state.lecturer_by_user(&caller.actor);
        let mut items: Vec<Student> = state
            .students
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|student| {
                let visible = read_all
                    || own.as_ref().is_some_and(|me| me.id == student.id)
                    || advisor.as_ref().is_some_and(|lecturer| {
                        student.advisor_id.as_deref() == Some(lecturer.id.as_str())
                    });
                visible
                    && matches_search(
                        &format!("{} {}", student.student_number, student.user_id),
                        &params,
                    )
                    && filter_matches(&params, "status", &student.status)
                    && filter_matches(&params, "program_id", &student.program_id)
            })
            .collect();
        items.sort_by(|a, b| a.student_number.cmp(&b.student_number));
        Ok(paginate(items, &params))
    })();
    json::respond(result, debug)
}

pub(crate) async fn create_student(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<AcademicState>>,
    req: HttpRequest,
    body: web::Json<CreateStudent>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Student> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = require_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ACADEMIC_MANAGE)?;
        let body = body.into_inner();
        let user_id = require_text(&body.user_id, "user_id", MAX_CODE)?;
        if state.student_by_user(&user_id).is_some() {
            return Err(UnifiedError::conflict(
                "A student record already exists for this user",
            ));
        }
        if !state.programs.contains_key(&body.program_id) {
            return Err(UnifiedError::not_found("Program not found"));
        }
        if let Some(advisor_id) = &body.advisor_id {
            if !state.lecturers.contains_key(advisor_id) {
                return Err(UnifiedError::not_found("Advisor not found"));
            }
        }
        let student_number = match &body.student_number {
            Some(number) => require_code(number, "student_number")?,
            None => format!("STU-{}", &new_id()[..8].to_uppercase()),
        };
        if state
            .students
            .iter()
            .any(|entry| entry.value().student_number == student_number)
        {
            return Err(UnifiedError::conflict(format!(
                "A student with number \"{student_number}\" already exists"
            )));
        }
        let student = Student {
            id: new_id(),
            user_id: user_id.clone(),
            student_number,
            program_id: body.program_id.clone(),
            advisor_id: body.advisor_id.clone(),
            enrollment_year: parse_year(body.enrollment_year, "enrollment_year")?,
            status: "active".to_string(),
            created_at: now(),
            updated_at: now(),
        };
        state.students.insert(student.id.clone(), student.clone());
        state.emit(
            "academic.student.created",
            json!({
                "student_id": student.id,
                "user_id": student.user_id,
                "program_id": student.program_id,
                // No admissions application backs a directly created
                // student; the key is always present so consumers
                // can rely on one canonical payload shape.
                "application_id": Value::Null,
            }),
        );
        Ok(student)
    })();
    json::respond(result, debug)
}

pub(crate) async fn get_student(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<AcademicState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Student> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ACADEMIC_READ)?;
        load_student_for_read(&state, &ctx, &caller, path.as_str())
    })();
    json::respond(result, debug)
}

pub(crate) async fn update_student(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<AcademicState>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdateStudent>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Student> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = require_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ACADEMIC_MANAGE)?;
        let body = body.into_inner();
        let mut student = state
            .students
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Student not found"))?;
        if let Some(program_id) = &body.program_id {
            if !state.programs.contains_key(program_id) {
                return Err(UnifiedError::not_found("Program not found"));
            }
            student.program_id = program_id.clone();
        }
        if let Some(advisor_id) = &body.advisor_id {
            match advisor_id {
                None => student.advisor_id = None,
                Some(id) => {
                    if !state.lecturers.contains_key(id) {
                        return Err(UnifiedError::not_found("Advisor not found"));
                    }
                    student.advisor_id = Some(id.clone());
                }
            }
        }
        if let Some(status) = &body.status {
            let status = require_text(status, "status", MAX_CODE)?;
            if !state.valid_status(KIND_STUDENT_STATUS, &status) {
                return Err(UnifiedError::bad_request(format!(
                    "Unknown student status \"{status}\""
                )));
            }
            student.status = status;
        }
        student.updated_at = now();
        state.students.insert(student.id.clone(), student.clone());
        Ok(student)
    })();
    json::respond(result, debug)
}

pub(crate) async fn delete_student(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<AcademicState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = require_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ACADEMIC_MANAGE)?;
        let id = path.into_inner();
        if !state.students.contains_key(&id) {
            return Err(UnifiedError::not_found("Student not found"));
        }
        if state
            .enrollments
            .iter()
            .any(|entry| entry.value().student_id == id)
        {
            return Err(UnifiedError::conflict(
                "Student cannot be deleted while enrollments exist",
            ));
        }
        if state
            .theses
            .iter()
            .any(|entry| entry.value().student_id == id)
        {
            return Err(UnifiedError::conflict(
                "Student cannot be deleted while a thesis record exists",
            ));
        }
        state.students.remove(&id);
        Ok(json!({ "deleted": true, "id": id }))
    })();
    json::respond(result, debug)
}

pub(crate) async fn get_student_summary(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<AcademicState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<StudentSummary> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ACADEMIC_READ)?;
        let student = load_student_for_read(&state, &ctx, &caller, path.as_str())?;
        let (gpa, total_credits, earned_credits) = state.gpa_for(&student.id);
        let enrollments: Vec<Enrollment> = state
            .enrollments
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|enrollment| enrollment.student_id == student.id)
            .collect();
        let completed = enrollments
            .iter()
            .filter(|enrollment| enrollment.status == "completed")
            .count();
        let attendance: Vec<Attendance> = state
            .attendance
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|record| record.student_id == student.id)
            .collect();
        let present = attendance
            .iter()
            .filter(|record| record.status == "present" || record.status == "late")
            .count();
        let rate = if attendance.is_empty() {
            None
        } else {
            Some((present as f64 / attendance.len() as f64 * 100.0).round() / 100.0)
        };
        Ok(StudentSummary {
            student_id: student.id.clone(),
            gpa,
            total_credits,
            earned_credits,
            standing: state.standing_for_gpa(gpa),
            enrollments: enrollments.len(),
            completed_enrollments: completed,
            attendance_present: present,
            attendance_total: attendance.len(),
            attendance_rate: rate,
        })
    })();
    json::respond(result, debug)
}

pub(crate) async fn get_student_gpa(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<AcademicState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ACADEMIC_READ)?;
        let student = load_student_for_read(&state, &ctx, &caller, path.as_str())?;
        let (gpa, total_credits, earned_credits) = state.gpa_for(&student.id);
        Ok(json!({
            "student_id": student.id,
            "gpa": gpa,
            "total_credits": total_credits,
            "earned_credits": earned_credits,
            "standing": state.standing_for_gpa(gpa),
        }))
    })();
    json::respond(result, debug)
}

// ---------------------------------------------------------------------------
// Handlers: lecturers
// ---------------------------------------------------------------------------

pub(crate) async fn list_lecturers(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<AcademicState>>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PageResult<Lecturer>> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ACADEMIC_READ)?;
        let params = query.into_inner();
        let read_all = check_capability(&ctx, &caller.role, PERM_ACADEMIC_READ_ALL);
        let own = state.lecturer_by_user(&caller.actor);
        let mut items: Vec<Lecturer> = state
            .lecturers
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|lecturer| {
                let visible = read_all
                    || own.as_ref().is_some_and(|me| me.id == lecturer.id)
                    || check_capability(&ctx, &caller.role, PERM_ACADEMIC_MANAGE);
                visible
                    && matches_search(
                        &format!("{} {}", lecturer.lecturer_number, lecturer.user_id),
                        &params,
                    )
                    && filter_matches(&params, "status", &lecturer.status)
            })
            .collect();
        items.sort_by(|a, b| a.lecturer_number.cmp(&b.lecturer_number));
        Ok(paginate(items, &params))
    })();
    json::respond(result, debug)
}

pub(crate) async fn create_lecturer(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<AcademicState>>,
    req: HttpRequest,
    body: web::Json<CreateLecturer>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Lecturer> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = require_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ACADEMIC_MANAGE)?;
        let body = body.into_inner();
        let user_id = require_text(&body.user_id, "user_id", MAX_CODE)?;
        if state.lecturer_by_user(&user_id).is_some() {
            return Err(UnifiedError::conflict(
                "A lecturer record already exists for this user",
            ));
        }
        let lecturer_number = match &body.lecturer_number {
            Some(number) => require_code(number, "lecturer_number")?,
            None => format!("LEC-{}", &new_id()[..8].to_uppercase()),
        };
        if state
            .lecturers
            .iter()
            .any(|entry| entry.value().lecturer_number == lecturer_number)
        {
            return Err(UnifiedError::conflict(format!(
                "A lecturer with number \"{lecturer_number}\" already exists"
            )));
        }
        let lecturer = Lecturer {
            id: new_id(),
            user_id: user_id.clone(),
            lecturer_number,
            department_id: optional_text(&body.department_id, "department_id", MAX_CODE)?,
            title: optional_text(&body.title, "title", MAX_TEXT)?,
            status: "active".to_string(),
            created_at: now(),
            updated_at: now(),
        };
        state
            .lecturers
            .insert(lecturer.id.clone(), lecturer.clone());
        state.emit(
            "academic.lecturer.created",
            json!({ "lecturer_id": lecturer.id, "user_id": lecturer.user_id }),
        );
        Ok(lecturer)
    })();
    json::respond(result, debug)
}

pub(crate) async fn get_lecturer(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<AcademicState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Lecturer> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ACADEMIC_READ)?;
        let lecturer = state
            .lecturers
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Lecturer not found"))?;
        let allowed = check_capability(&ctx, &caller.role, PERM_ACADEMIC_READ_ALL)
            || check_capability(&ctx, &caller.role, PERM_ACADEMIC_MANAGE)
            || lecturer.user_id == caller.actor;
        if !allowed {
            return Err(UnifiedError::not_found("Lecturer not found"));
        }
        Ok(lecturer)
    })();
    json::respond(result, debug)
}

pub(crate) async fn update_lecturer(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<AcademicState>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdateLecturer>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Lecturer> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = require_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ACADEMIC_MANAGE)?;
        let body = body.into_inner();
        let mut lecturer = state
            .lecturers
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Lecturer not found"))?;
        if let Some(department_id) = &body.department_id {
            lecturer.department_id = optional_text(department_id, "department_id", MAX_CODE)?;
        }
        if let Some(title) = &body.title {
            lecturer.title = optional_text(title, "title", MAX_TEXT)?;
        }
        if let Some(status) = &body.status {
            let status = require_text(status, "status", MAX_CODE)?;
            if !state.valid_status(KIND_LECTURER_STATUS, &status) {
                return Err(UnifiedError::bad_request(format!(
                    "Unknown lecturer status \"{status}\""
                )));
            }
            lecturer.status = status;
        }
        lecturer.updated_at = now();
        state
            .lecturers
            .insert(lecturer.id.clone(), lecturer.clone());
        Ok(lecturer)
    })();
    json::respond(result, debug)
}

pub(crate) async fn delete_lecturer(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<AcademicState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = require_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ACADEMIC_MANAGE)?;
        let id = path.into_inner();
        if !state.lecturers.contains_key(&id) {
            return Err(UnifiedError::not_found("Lecturer not found"));
        }
        if state
            .schedules
            .iter()
            .any(|entry| entry.value().lecturer_id == id && entry.value().status == "active")
        {
            return Err(UnifiedError::conflict(
                "Lecturer cannot be deleted while assigned to active schedules",
            ));
        }
        if state
            .students
            .iter()
            .any(|entry| entry.value().advisor_id.as_deref() == Some(id.as_str()))
        {
            return Err(UnifiedError::conflict(
                "Lecturer cannot be deleted while advising students",
            ));
        }
        state.lecturers.remove(&id);
        Ok(json!({ "deleted": true, "id": id }))
    })();
    json::respond(result, debug)
}

pub(crate) async fn list_lecturer_classes(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<AcademicState>>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PageResult<Schedule>> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ACADEMIC_READ)?;
        let lecturer_id = path.into_inner();
        let lecturer = state
            .lecturers
            .get(&lecturer_id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Lecturer not found"))?;
        let allowed = check_capability(&ctx, &caller.role, PERM_ACADEMIC_READ_ALL)
            || check_capability(&ctx, &caller.role, PERM_ACADEMIC_MANAGE)
            || lecturer.user_id == caller.actor;
        if !allowed {
            return Err(UnifiedError::not_found("Lecturer not found"));
        }
        let params = query.into_inner();
        let mut items: Vec<Schedule> = state
            .schedules
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|schedule| {
                schedule.lecturer_id == lecturer_id
                    && filter_matches(&params, "semester_id", &schedule.semester_id)
                    && filter_matches(&params, "status", &schedule.status)
            })
            .collect();
        items.sort_by(|a, b| {
            a.day_of_week
                .cmp(&b.day_of_week)
                .then_with(|| a.start_time.cmp(&b.start_time))
        });
        Ok(paginate(items, &params))
    })();
    json::respond(result, debug)
}

// ---------------------------------------------------------------------------
// Handlers: subjects
// ---------------------------------------------------------------------------

pub(crate) async fn list_subjects(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<AcademicState>>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PageResult<Subject>> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ACADEMIC_READ)?;
        let params = query.into_inner();
        let mut items: Vec<Subject> = state
            .subjects
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|subject| {
                matches_search(&format!("{} {}", subject.code, subject.title), &params)
                    && filter_matches(&params, "status", &subject.status)
                    && filter_matches(&params, "program_id", &subject.program_id)
            })
            .collect();
        items.sort_by(|a, b| a.code.cmp(&b.code));
        Ok(paginate(items, &params))
    })();
    json::respond(result, debug)
}

pub(crate) async fn create_subject(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<AcademicState>>,
    req: HttpRequest,
    body: web::Json<CreateSubject>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Subject> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = require_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ACADEMIC_MANAGE)?;
        let body = body.into_inner();
        let code = require_code(&body.code, "code")?;
        let title = require_text(&body.title, "title", MAX_TEXT)?;
        if !state.programs.contains_key(&body.program_id) {
            return Err(UnifiedError::not_found("Program not found"));
        }
        if state
            .subjects
            .iter()
            .any(|entry| entry.value().code.eq_ignore_ascii_case(&code))
        {
            return Err(UnifiedError::conflict(format!(
                "A subject with code \"{code}\" already exists"
            )));
        }
        let credits = parse_credits(body.credits, "credits")?;
        let semester_number = match body.semester_number {
            None => None,
            Some(value) => {
                if !(1..=20).contains(&value) {
                    return Err(UnifiedError::bad_request(
                        "semester_number must be between 1 and 20",
                    ));
                }
                Some(value)
            }
        };
        let elearning_course_id =
            optional_text(&body.elearning_course_id, "elearning_course_id", MAX_CODE)?;
        let verified = match &elearning_course_id {
            None => false,
            Some(course_id) => state.elearning_course_exists(course_id),
        };
        let status = match &body.status {
            None => "active".to_string(),
            Some(value) => {
                let status = require_text(value, "status", MAX_CODE)?;
                if status != "active" && status != "inactive" {
                    return Err(UnifiedError::bad_request(
                        "status must be \"active\" or \"inactive\"",
                    ));
                }
                status
            }
        };
        let subject = Subject {
            id: new_id(),
            code: code.clone(),
            title,
            program_id: body.program_id.clone(),
            credits,
            semester_number,
            elearning_course_id,
            elearning_course_verified: verified,
            status,
            created_at: now(),
            updated_at: now(),
        };
        state.subjects.insert(subject.id.clone(), subject.clone());
        state.emit(
            "academic.subject.created",
            json!({
                "subject_id": subject.id,
                "code": subject.code,
                "program_id": subject.program_id,
            }),
        );
        Ok(subject)
    })();
    json::respond(result, debug)
}

pub(crate) async fn get_subject(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<AcademicState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Subject> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ACADEMIC_READ)?;
        state
            .subjects
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Subject not found"))
    })();
    json::respond(result, debug)
}

pub(crate) async fn update_subject(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<AcademicState>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdateSubject>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Subject> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = require_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ACADEMIC_MANAGE)?;
        let body = body.into_inner();
        let mut subject = state
            .subjects
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Subject not found"))?;
        if let Some(title) = &body.title {
            subject.title = require_text(title, "title", MAX_TEXT)?;
        }
        if let Some(credits) = body.credits {
            subject.credits = parse_credits(credits, "credits")?;
        }
        if let Some(semester_number) = &body.semester_number {
            subject.semester_number = match semester_number {
                None => None,
                Some(value) => {
                    if !(1..=20).contains(value) {
                        return Err(UnifiedError::bad_request(
                            "semester_number must be between 1 and 20",
                        ));
                    }
                    Some(*value)
                }
            };
        }
        if let Some(elearning_course_id) = &body.elearning_course_id {
            match elearning_course_id {
                None => {
                    subject.elearning_course_id = None;
                    subject.elearning_course_verified = false;
                }
                Some(course_id) => {
                    let course_id = require_text(course_id, "elearning_course_id", MAX_CODE)?;
                    subject.elearning_course_verified = state.elearning_course_exists(&course_id);
                    subject.elearning_course_id = Some(course_id);
                }
            }
        }
        if let Some(status) = &body.status {
            let status = require_text(status, "status", MAX_CODE)?;
            if status != "active" && status != "inactive" {
                return Err(UnifiedError::bad_request(
                    "status must be \"active\" or \"inactive\"",
                ));
            }
            subject.status = status;
        }
        subject.updated_at = now();
        state.subjects.insert(subject.id.clone(), subject.clone());
        state.emit(
            "academic.subject.updated",
            json!({ "subject_id": subject.id }),
        );
        Ok(subject)
    })();
    json::respond(result, debug)
}

pub(crate) async fn delete_subject(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<AcademicState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = require_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ACADEMIC_MANAGE)?;
        let id = path.into_inner();
        if !state.subjects.contains_key(&id) {
            return Err(UnifiedError::not_found("Subject not found"));
        }
        if state
            .enrollments
            .iter()
            .any(|entry| entry.value().subject_id == id)
        {
            return Err(UnifiedError::conflict(
                "Subject cannot be deleted while enrollments exist",
            ));
        }
        if state
            .schedules
            .iter()
            .any(|entry| entry.value().subject_id == id && entry.value().status == "active")
        {
            return Err(UnifiedError::conflict(
                "Subject cannot be deleted while active schedules exist",
            ));
        }
        state.subjects.remove(&id);
        Ok(json!({ "deleted": true, "id": id }))
    })();
    json::respond(result, debug)
}

pub(crate) async fn link_elearning_course(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<AcademicState>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<LinkElearningCourse>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Subject> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = require_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ACADEMIC_MANAGE)?;
        let course_id = require_text(&body.elearning_course_id, "elearning_course_id", MAX_CODE)?;
        let mut subject = state
            .subjects
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Subject not found"))?;
        let answer = state.elearning_course(&course_id);
        let verified = answer
            .as_ref()
            .and_then(|result| result.get("exists").and_then(|value| value.as_bool()))
            .unwrap_or(false);
        subject.elearning_course_id = Some(course_id);
        subject.elearning_course_verified = verified;
        subject.updated_at = now();
        state.subjects.insert(subject.id.clone(), subject.clone());
        state.emit(
            "academic.subject.updated",
            json!({ "subject_id": subject.id, "elearning_course_verified": verified }),
        );
        Ok(subject)
    })();
    json::respond(result, debug)
}
