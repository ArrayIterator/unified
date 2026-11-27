//! Academic - grade, appeal and transcript handlers.
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

use super::constants::MAX_CODE;
use super::constants::MAX_TEXT;
use super::constants::PERM_ACADEMIC_MANAGE;
use super::constants::PERM_ACADEMIC_READ;
use super::constants::PERM_ACADEMIC_READ_ALL;
use super::constants::PERM_ACADEMIC_TRANSCRIPT;
use super::helpers::caller;
use super::helpers::caller_company;
use super::helpers::can_grade;
use super::helpers::can_read_student;
use super::helpers::check_capability;
use super::helpers::filter_matches;
use super::helpers::is_staff;
use super::helpers::load_student_for_read;
use super::helpers::new_id;
use super::helpers::now;
use super::helpers::optional_text;
use super::helpers::paginate;
use super::helpers::parse_score;
use super::helpers::require_company;
use super::helpers::require_permission;
use super::helpers::require_read_student;
use super::helpers::require_tenant;
use super::helpers::require_text;
use super::helpers::teaches_subject;
use super::state::AcademicState;
use super::types::CreateGrade;
use super::types::CreateGradeAppeal;
use super::types::Enrollment;
use super::types::FinalGrade;
use super::types::Grade;
use super::types::GradeAppeal;
use super::types::ResolveGradeAppeal;
use super::types::Transcript;
use super::types::UpdateGrade;

// Handlers: grades

pub(crate) async fn list_grades_for_enrollment(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<AcademicState>>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PageResult<Grade>> = (|| {
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
        let params = query.into_inner();
        let mut items: Vec<Grade> = state
            .grades
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|grade| grade.enrollment_id == enrollment.id)
            .collect();
        items.sort_by(|a, b| a.created_at.cmp(&b.created_at));
        Ok(paginate(items, &params))
    })();
    json::respond(result, debug)
}

pub(crate) async fn create_grade(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<AcademicState>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<CreateGrade>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Grade> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = require_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        let enrollment = state
            .enrollments
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Enrollment not found"))?;
        if !can_grade(&state, &ctx, &caller, &enrollment) {
            return Err(UnifiedError::forbidden(
                "Only the subject's lecturer or a manager may record grades",
            ));
        }
        if !matches!(enrollment.status.as_str(), "approved" | "completed") {
            return Err(UnifiedError::conflict(
                "Grades can only be recorded for approved or completed enrollments",
            ));
        }
        let body = body.into_inner();
        let component = state
            .grade_components
            .get(&body.component_id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Grade component not found"))?;
        if component.subject_id != enrollment.subject_id {
            return Err(UnifiedError::bad_request(
                "Grade component belongs to a different subject",
            ));
        }
        if state.grades.iter().any(|entry| {
            let grade = entry.value();
            grade.enrollment_id == enrollment.id && grade.component_id == component.id
        }) {
            return Err(UnifiedError::conflict(
                "A grade for this enrollment and component already exists; update it instead",
            ));
        }
        let grade = Grade {
            id: new_id(),
            enrollment_id: enrollment.id.clone(),
            component_id: component.id.clone(),
            score: parse_score(body.score, "score")?,
            graded_by: state
                .lecturer_by_user(&caller.actor)
                .map(|lecturer| lecturer.id),
            created_at: now(),
            updated_at: now(),
        };
        state.grades.insert(grade.id.clone(), grade.clone());
        state.emit(
            "academic.grade.recorded",
            json!({
                "grade_id": grade.id,
                "enrollment_id": grade.enrollment_id,
                "component_id": grade.component_id,
                "score": grade.score,
            }),
        );
        Ok(grade)
    })();
    json::respond(result, debug)
}

pub(crate) async fn update_grade(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<AcademicState>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdateGrade>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Grade> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = require_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        let mut grade = state
            .grades
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Grade not found"))?;
        let enrollment = state
            .enrollments
            .get(&grade.enrollment_id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Grade not found"))?;
        if !can_grade(&state, &ctx, &caller, &enrollment) {
            return Err(UnifiedError::forbidden(
                "Only the subject's lecturer or a manager may update grades",
            ));
        }
        grade.score = parse_score(body.score, "score")?;
        grade.updated_at = now();
        state.grades.insert(grade.id.clone(), grade.clone());
        state.emit(
            "academic.grade.recorded",
            json!({
                "grade_id": grade.id,
                "enrollment_id": grade.enrollment_id,
                "component_id": grade.component_id,
                "score": grade.score,
            }),
        );
        Ok(grade)
    })();
    json::respond(result, debug)
}

pub(crate) async fn delete_grade(
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
        let grade = state
            .grades
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Grade not found"))?;
        let enrollment = state
            .enrollments
            .get(&grade.enrollment_id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Grade not found"))?;
        if !can_grade(&state, &ctx, &caller, &enrollment) {
            return Err(UnifiedError::forbidden(
                "Only the subject's lecturer or a manager may delete grades",
            ));
        }
        state.grades.remove(&grade.id);
        Ok(json!({ "deleted": true, "id": grade.id }))
    })();
    json::respond(result, debug)
}

pub(crate) async fn get_final_grade(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<AcademicState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<FinalGrade> = (|| {
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
        Ok(state.final_grade(&enrollment))
    })();
    json::respond(result, debug)
}

// Handlers: grade appeals

pub(crate) async fn list_grade_appeals(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<AcademicState>>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PageResult<GradeAppeal>> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ACADEMIC_READ)?;
        let params = query.into_inner();
        let read_all = check_capability(&ctx, &caller.role, PERM_ACADEMIC_READ_ALL);
        let own_student = state.student_by_user(&caller.actor);
        let mut items: Vec<GradeAppeal> = state
            .grade_appeals
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|appeal| {
                let visible = read_all
                    || own_student
                        .as_ref()
                        .is_some_and(|student| student.id == appeal.student_id)
                    || check_capability(&ctx, &caller.role, PERM_ACADEMIC_MANAGE);
                visible
                    && filter_matches(&params, "enrollment_id", &appeal.enrollment_id)
                    && filter_matches(&params, "student_id", &appeal.student_id)
                    && filter_matches(&params, "status", &appeal.status)
            })
            .collect();
        items.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        Ok(paginate(items, &params))
    })();
    json::respond(result, debug)
}

pub(crate) async fn create_grade_appeal(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<AcademicState>>,
    req: HttpRequest,
    body: web::Json<CreateGradeAppeal>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<GradeAppeal> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = require_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ACADEMIC_READ)?;
        let body = body.into_inner();
        let enrollment = state
            .enrollments
            .get(&body.enrollment_id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Enrollment not found"))?;
        let student = state
            .students
            .get(&enrollment.student_id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Enrollment not found"))?;
        // A student appeals their own grade; staff may file for any
        // student. Anyone else gets the same 404 as a missing
        // enrollment.
        if student.user_id != caller.actor && !is_staff(&ctx, &caller.role) {
            return Err(UnifiedError::not_found("Enrollment not found"));
        }
        if let Some(component_id) = &body.component_id {
            let component = state
                .grade_components
                .get(component_id)
                .map(|entry| entry.value().clone())
                .ok_or_else(|| UnifiedError::not_found("Grade component not found"))?;
            if component.subject_id != enrollment.subject_id {
                return Err(UnifiedError::bad_request(
                    "Grade component belongs to a different subject",
                ));
            }
            if !state.grades.iter().any(|entry| {
                let grade = entry.value();
                grade.enrollment_id == enrollment.id && grade.component_id == component.id
            }) {
                return Err(UnifiedError::conflict(
                    "No grade exists yet for the appealed component",
                ));
            }
        }
        if state.grade_appeals.iter().any(|entry| {
            let appeal = entry.value();
            appeal.enrollment_id == enrollment.id
                && appeal.component_id == body.component_id
                && matches!(appeal.status.as_str(), "open" | "under_review")
        }) {
            return Err(UnifiedError::conflict(
                "An open appeal already exists for this grade",
            ));
        }
        let appeal = GradeAppeal {
            id: new_id(),
            enrollment_id: enrollment.id.clone(),
            component_id: body.component_id.clone(),
            student_id: student.id.clone(),
            reason: require_text(&body.reason, "reason", MAX_TEXT)?,
            status: "open".to_string(),
            reviewed_by: None,
            decision: None,
            corrected_score: None,
            resolution_notes: None,
            created_at: now(),
            updated_at: now(),
        };
        state
            .grade_appeals
            .insert(appeal.id.clone(), appeal.clone());
        state.emit(
            "academic.grade.appeal_filed",
            json!({
                "appeal_id": appeal.id,
                "enrollment_id": appeal.enrollment_id,
                "student_id": appeal.student_id,
            }),
        );
        Ok(appeal)
    })();
    json::respond(result, debug)
}

pub(crate) async fn get_grade_appeal(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<AcademicState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<GradeAppeal> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ACADEMIC_READ)?;
        let appeal = state
            .grade_appeals
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Grade appeal not found"))?;
        let student = state
            .students
            .get(&appeal.student_id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Grade appeal not found"))?;
        if !can_read_student(&state, &ctx, &caller, &student) {
            return Err(UnifiedError::not_found("Grade appeal not found"));
        }
        Ok(appeal)
    })();
    json::respond(result, debug)
}

pub(crate) async fn resolve_grade_appeal(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<AcademicState>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<ResolveGradeAppeal>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<GradeAppeal> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = require_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        let body = body.into_inner();
        let decision = require_text(&body.decision, "decision", MAX_CODE)?;
        if decision != "upheld" && decision != "denied" {
            return Err(UnifiedError::bad_request(
                "decision must be \"upheld\" or \"denied\"",
            ));
        }
        let corrected_score = match body.corrected_score {
            None => None,
            Some(value) => Some(parse_score(value, "corrected_score")?),
        };
        let mut appeal = state
            .grade_appeals
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Grade appeal not found"))?;
        if !matches!(appeal.status.as_str(), "open" | "under_review") {
            return Err(UnifiedError::conflict(
                "Only an open appeal can be resolved",
            ));
        }
        let enrollment = state
            .enrollments
            .get(&appeal.enrollment_id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Grade appeal not found"))?;
        if !can_grade(&state, &ctx, &caller, &enrollment) {
            return Err(UnifiedError::forbidden(
                "Only the subject's lecturer or a manager may resolve an appeal",
            ));
        }
        if decision == "upheld" {
            if let (Some(component_id), Some(score)) = (&appeal.component_id, corrected_score) {
                if let Some(mut grade) = state
                    .grades
                    .iter()
                    .find(|entry| {
                        let grade = entry.value();
                        grade.enrollment_id == enrollment.id && grade.component_id == *component_id
                    })
                    .map(|entry| entry.value().clone())
                {
                    grade.score = score;
                    grade.updated_at = now();
                    state.grades.insert(grade.id.clone(), grade);
                }
            }
        }
        appeal.status = if decision == "upheld" {
            "upheld"
        } else {
            "denied"
        }
        .to_string();
        appeal.decision = Some(decision.clone());
        appeal.corrected_score = corrected_score;
        appeal.resolution_notes =
            optional_text(&body.resolution_notes, "resolution_notes", MAX_TEXT)?;
        appeal.reviewed_by = state
            .lecturer_by_user(&caller.actor)
            .map(|lecturer| lecturer.id);
        appeal.updated_at = now();
        state
            .grade_appeals
            .insert(appeal.id.clone(), appeal.clone());
        state.emit(
            "academic.grade.appeal_resolved",
            json!({
                "appeal_id": appeal.id,
                "enrollment_id": appeal.enrollment_id,
                "decision": decision,
            }),
        );
        if let Some(student) = state.students.get(&appeal.student_id) {
            state.notify(
                &student.user_id,
                "Grade appeal resolved",
                &format!("Your grade appeal was {decision}."),
            );
        }
        Ok(appeal)
    })();
    json::respond(result, debug)
}

// Handlers: transcripts

pub(crate) async fn list_transcripts(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<AcademicState>>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PageResult<Transcript>> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ACADEMIC_READ)?;
        let student = load_student_for_read(&state, &ctx, &caller, path.as_str())?;
        let params = query.into_inner();
        let mut items: Vec<Transcript> = state
            .transcripts
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|transcript| transcript.student_id == student.id)
            .collect();
        items.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        Ok(paginate(items, &params))
    })();
    json::respond(result, debug)
}

pub(crate) async fn issue_transcript(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<AcademicState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Transcript> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = require_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ACADEMIC_TRANSCRIPT)?;
        let student = state
            .students
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Student not found"))?;
        let (gpa, total_credits, _) = state.gpa_for(&student.id);
        let enrollments: Vec<Enrollment> = state
            .enrollments
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|enrollment| enrollment.student_id == student.id)
            .collect();
        let mut lines: Vec<Value> = Vec::new();
        for enrollment in &enrollments {
            let grade = state.final_grade(enrollment);
            let subject = state
                .subjects
                .get(&enrollment.subject_id)
                .map(|entry| entry.value().clone());
            let semester = state
                .semesters
                .get(&enrollment.semester_id)
                .map(|entry| entry.value().clone());
            lines.push(json!({
                "subject_code": subject.as_ref().map(|subject| subject.code.clone()),
                "subject_title": subject.as_ref().map(|subject| subject.title.clone()),
                "semester_code": semester.as_ref().map(|semester| semester.code.clone()),
                "credits": enrollment.credits,
                "status": enrollment.status,
                "weighted_score": grade.weighted_score,
                "letter": grade.letter,
                "points": grade.points,
            }));
        }
        let transcript = Transcript {
            id: new_id(),
            student_id: student.id.clone(),
            issued_by: caller.actor.clone(),
            gpa,
            total_credits,
            lines: Value::Array(lines),
            print_job_id: None,
            created_at: now(),
        };
        state
            .transcripts
            .insert(transcript.id.clone(), transcript.clone());
        state.emit(
            "academic.transcript.issued",
            json!({
                "transcript_id": transcript.id,
                "student_id": transcript.student_id,
                "gpa": transcript.gpa,
                "total_credits": transcript.total_credits,
            }),
        );
        Ok(transcript)
    })();
    json::respond(result, debug)
}

pub(crate) async fn get_transcript(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<AcademicState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Transcript> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ACADEMIC_READ)?;
        let transcript = state
            .transcripts
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Transcript not found"))?;
        let student = state
            .students
            .get(&transcript.student_id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Transcript not found"))?;
        require_read_student(&state, &ctx, &caller, &student)?;
        Ok(transcript)
    })();
    json::respond(result, debug)
}

pub(crate) async fn print_transcript(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<AcademicState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Transcript> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = require_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ACADEMIC_TRANSCRIPT)?;
        let transcript = state
            .transcripts
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Transcript not found"))?;
        // This module never prints. It only asks the printing module
        // (see the module header: no printing module exists in this
        // tree yet, so the event is the contract).
        state.emit(
            "academic.transcript.print_requested",
            json!({
                "transcript_id": transcript.id,
                "student_id": transcript.student_id,
            }),
        );
        state.emit(
            "print.job.requested",
            json!({
                "source": "academic",
                "document_type": "transcript",
                "transcript_id": transcript.id,
                "student_id": transcript.student_id,
                "title": "Official Transcript",
                "issued_at": transcript.created_at.to_rfc3339(),
            }),
        );
        Ok(transcript)
    })();
    json::respond(result, debug)
}
