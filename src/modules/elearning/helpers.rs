// Elearning — Helpers.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::{HookBus, HookContext};
use unified_core::json;
use unified_core::modules::{Module, ModuleRegistry, RouteInfo, RouteVersionSupport};
use unified_core::pagination::{PageResult, PaginationParams};
use unified_core::storage::validate_media_reference;
use crate::modules::ai;
use actix_web::{web, HttpRequest, HttpResponse};
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use rand::{distr::Alphanumeric, Rng};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use uuid::Uuid;

use super::*;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Caller identity: the server-verified caller (user id plus
/// server-resolved role, both required for capability checks),
/// exactly like the events module.
pub(crate) struct Caller {
    pub(crate) actor: String,
    pub(crate) role: String,
}

pub(crate) fn caller(req: &HttpRequest) -> UnifiedResult<Caller> {
    let verified = unified_core::caller::require_caller(req)?;
    let actor = verified.user_id.clone();
    let role = verified.role().to_string();
    Ok(Caller { actor, role })
}

/// Ask the roles module whether `role` holds `permission` through
/// the `role.check` hook. Fails closed: no answer means not allowed
/// (same fail-closed shape as the events module's capability check).
pub(crate) fn check_capability(ctx: &AppContext, role: &str, permission: &str) -> bool {
    let context = ctx.hooks.emit(
        "role.check",
        json!({ "role": role, "permission": permission, "allowed": false }),
    );
    context
        .get("allowed")
        .and_then(|value| value.as_bool())
        .unwrap_or(false)
}

pub(crate) fn require_permission(ctx: &AppContext, role: &str, permission: &str) -> UnifiedResult<()> {
    if check_capability(ctx, role, permission) {
        Ok(())
    } else {
        Err(UnifiedError::forbidden(format!(
            "Role \"{role}\" does not hold permission \"{permission}\""
        )))
    }
}

/// True when the caller may manage content: the manage capability
/// or the instructor capability.
pub(crate) fn can_teach(ctx: &AppContext, role: &str) -> bool {
    check_capability(ctx, role, PERM_ELEARNING_MANAGE)
        || check_capability(ctx, role, PERM_ELEARNING_INSTRUCT)
}

pub(crate) fn require_teach(ctx: &AppContext, role: &str) -> UnifiedResult<()> {
    if can_teach(ctx, role) {
        Ok(())
    } else {
        Err(UnifiedError::forbidden(format!(
            "Role \"{role}\" does not hold permission \"{PERM_ELEARNING_MANAGE}\" or \"{PERM_ELEARNING_INSTRUCT}\""
        )))
    }
}

// ---------------------------------------------------------------------------
// Tenant edge guard (C3 pilot 3).
// ---------------------------------------------------------------------------

/// Fail-closed tenant gate: resolves the tenant context the
/// tenant-resolution middleware attached to the request. A request
/// with no resolved tenant context is rejected (401) and never falls
/// back to the `public` schema (the shared `TenantContext` contract
/// in `unified_core::tenancy`).
pub(crate) fn require_tenant(
    req: &HttpRequest,
) -> UnifiedResult<unified_core::tenancy::TenantContext> {
    unified_core::tenancy::TenantContext::from_http_request(req)
}

/// The company scope the caller acts in: the tenant context's
/// company first, the verified caller's server-resolved company
/// otherwise. Fails closed (403) when both are present and disagree
/// — a request must not carry one company in its tenant context and
/// another in its verified identity. The `X-Company-Id` header is
/// NEVER trusted here: only the verified caller (company resolved
/// from the database at authentication time) and the
/// middleware-resolved tenant context are sources (header trust
/// removal is task 317's job elsewhere).
pub(crate) fn caller_company(
    tenant: &unified_core::tenancy::TenantContext,
    req: &HttpRequest,
) -> UnifiedResult<Option<String>> {
    let verified = unified_core::caller::caller_from(req)
        .company_id
        .filter(|value| !value.is_empty());
    match (&tenant.company_id, &verified) {
        (Some(tenant_company), Some(verified_company))
            if tenant_company != verified_company =>
        {
            Err(UnifiedError::forbidden(
                "Verified caller company does not match the tenant company scope",
            ))
        }
        (Some(tenant_company), _) => Ok(Some(tenant_company.clone())),
        (None, verified) => Ok(verified),
    }
}

/// Resolve the caller's company for a WRITE (C3 task 315, strict by
/// default): a write requires a resolved company scope, so an
/// unresolved caller is refused with 403 instead of silently
/// writing a global (unscoped) record. Mutation handlers call
/// this instead of `caller_company`; read handlers keep
/// `caller_company`, and the named carve-outs (self-service
/// writes, settings with no company field, scan/test triggers,
/// platform surfaces) keep it too.
pub(crate) fn require_company_for_write(
    tenant: &unified_core::tenancy::TenantContext,
    req: &actix_web::HttpRequest,
) -> UnifiedResult<Option<String>> {
    match caller_company(tenant, req)? {
        Some(company) => Ok(Some(company)),
        None => Err(UnifiedError::forbidden(
            "A write requires a resolved company scope",
        )),
    }
}


/// Verify a request body's company id against the caller's
/// company scope and return the company id to store. Fails closed
/// (403) when the body names a company other than the caller's.
/// When the body omits the company, the caller's scope is adopted
/// so the record is never stored unscoped under a scoped caller.
/// When no caller scope is known, a body company is refused: an
/// unresolved caller may not claim a company (the shared
/// `CallerScope::Unresolved` rule); omitting the company is refused (C3 task 315):
/// a write requires a resolved company scope.
pub(crate) fn require_company_match(
    tenant: &unified_core::tenancy::TenantContext,
    req: &HttpRequest,
    body_company_id: &Option<String>,
) -> UnifiedResult<Option<String>> {
    let caller = caller_company(tenant, req)?;
    require_company_match_for(&caller, body_company_id)
}

/// `require_company_match` against an already-resolved caller
/// scope, for handlers that computed `caller_company` first.
pub(crate) fn require_company_match_for(
    caller: &Option<String>,
    body_company_id: &Option<String>,
) -> UnifiedResult<Option<String>> {
    match (body_company_id, caller) {
        (Some(body), Some(caller_company)) if body != caller_company => Err(
            UnifiedError::forbidden("company_id does not match the caller's company scope"),
        ),
        (Some(body), Some(_)) => Ok(Some(body.clone())),
        (Some(body), None) => Err(UnifiedError::forbidden(
            "company_id does not match the caller's company scope",
        )),
        (None, Some(caller)) => Ok(Some(caller.clone())),
        (None, None) => Err(UnifiedError::forbidden(
            "A write requires a resolved company scope",
        )),
    }
}

/// Verify an UPDATE body's newly named company against the caller
/// scope and return the company id to store. Unlike
/// `require_company_match_for` (create: an omitted company adopts
/// the caller scope), an explicit clear (`None`) stays a clear and
/// is never re-stamped with the caller scope.
pub(crate) fn require_company_change_for(
    caller: &Option<String>,
    new_company_id: &Option<String>,
) -> UnifiedResult<Option<String>> {
    match (new_company_id, caller) {
        (Some(body), Some(caller_company)) if body != caller_company => Err(
            UnifiedError::forbidden("company_id does not match the caller's company scope"),
        ),
        (Some(body), Some(_)) => Ok(Some(body.clone())),
        (Some(_body), None) => Err(UnifiedError::forbidden(
            "company_id does not match the caller's company scope",
        )),
        (None, _) => Ok(None),
    }
}

/// True when a stored record belongs to the caller's company
/// scope. With no caller scope (an unresolved caller) only
/// global records match: unscoped records are visible, every
/// company-owned record is denied (the shared
/// `CallerScope::Unresolved` rule). With a scope, only records
/// carrying that company match, plus unscoped (global/legacy) records,
/// which are shared content visible to any resolved scope (C3 task 320, rule A).
pub(crate) fn in_company_scope(
    record_company_id: &Option<String>,
    caller_company: &Option<String>,
) -> bool {
    match caller_company {
        None => record_company_id.is_none(),
        Some(company) => record_company_id.is_none()
                || record_company_id.as_deref() == Some(company.as_str()),
    }
}

/// Fail closed (403) when a stored record is outside the caller's
/// company scope. Single-record reads and mutations call this
/// right after loading the record; list handlers use
/// `in_company_scope` as a filter instead.
pub(crate) fn require_record_in_company_scope(
    record_company_id: &Option<String>,
    caller_company: &Option<String>,
) -> UnifiedResult<()> {
    if in_company_scope(record_company_id, caller_company) {
        Ok(())
    } else {
        Err(UnifiedError::forbidden(
            "The record belongs to a different company",
        ))
    }
}

/// Fail closed (403) when a stored record is outside the caller's
/// company scope FOR A WRITE (C3 task 315, strict by default).
/// Unlike the read check above (rule A), a write requires an exact
/// company match: a company caller may write only their own
/// company's records — global (unscoped) records are platform-only
/// — and an unresolved caller may write nothing. Mutation handlers
/// call this right after loading the record they mutate; handlers
/// that only read the record, or load a parent to scope a child
/// write, keep the read check.
pub(crate) fn require_record_writable_in_company_scope(
    record_company_id: &Option<String>,
    caller_company: &Option<String>,
) -> UnifiedResult<()> {
    match (record_company_id, caller_company) {
        (Some(record), Some(caller)) if record == caller => Ok(()),
        _ => Err(UnifiedError::forbidden(
            "A write requires an exact company match: the record belongs to a different company or is a shared (global) record",
        )),
    }
}


/// True when the named course is in the caller scope. A missing
/// course is out of scope (fail-closed), so list filters never
/// surface rows whose parent course is gone.
pub(crate) fn course_in_scope(
    state: &ElearningState,
    course_id: Uuid,
    scope: &Option<String>,
) -> bool {
    state
        .courses
        .get(&course_id)
        .is_some_and(|entry| in_company_scope(&entry.value().company_id, scope))
}

/// Load a course by id and fail closed (403) when it is outside
/// the caller scope. Sub-record handlers (modules, lessons,
/// quizzes, attempts, certificates, reviews, enrollments) scope
/// through their parent course with this.
pub(crate) fn require_course_by_id_in_scope(
    state: &ElearningState,
    course_id: Uuid,
    scope: &Option<String>,
) -> UnifiedResult<Course> {
    let course = state
        .courses
        .get(&course_id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Course not found: {course_id}")))?;
    require_record_in_company_scope(&course.company_id, scope)?;
    Ok(course)
}

/// Load a course by its path id and fail closed (403) when it is
/// outside the caller scope.
pub(crate) fn require_course_in_scope(
    state: &ElearningState,
    raw_id: &str,
    scope: &Option<String>,
) -> UnifiedResult<Course> {
    let course = load_course(state, raw_id)?;
    require_record_in_company_scope(&course.company_id, scope)?;
    Ok(course)
}

/// True when a quiz's course is in the caller scope (attempt
/// lists scope through the quiz's course).
pub(crate) fn quiz_course_in_scope(
    state: &ElearningState,
    quiz_id: Uuid,
    scope: &Option<String>,
) -> bool {
    state
        .quizzes
        .get(&quiz_id)
        .is_some_and(|entry| course_in_scope(state, entry.value().course_id, scope))
}

/// True when a learning path is visible to the caller scope.
/// Paths carry no company of their own, so a path shows when it
/// has no courses yet (unscoped) or at least one course in
/// scope; a path whose courses are all out of scope stays hidden.
pub(crate) fn path_visible(
    state: &ElearningState,
    path_id: Uuid,
    scope: &Option<String>,
) -> bool {
    let course_ids: Vec<Uuid> = state
        .path_courses
        .iter()
        .filter(|entry| entry.value().path_id == path_id)
        .map(|entry| entry.value().course_id)
        .collect();
    course_ids.is_empty()
        || course_ids
            .iter()
            .any(|course_id| course_in_scope(state, *course_id, scope))
}

/// Instructors may only change their own courses; managers may
/// change any course.
pub(crate) fn require_course_write(
    ctx: &AppContext,
    caller: &Caller,
    course: &Course,
) -> UnifiedResult<()> {
    if check_capability(ctx, &caller.role, PERM_ELEARNING_MANAGE) {
        return Ok(());
    }
    if check_capability(ctx, &caller.role, PERM_ELEARNING_INSTRUCT)
        && course.instructor_id == caller.actor
    {
        return Ok(());
    }
    Err(UnifiedError::forbidden(
        "Only a manager or the course instructor may change this course",
    ))
}

/// Whether `caller` may see `course` at all. Published courses
/// are visible to everyone holding the view permission; a draft
/// or archived course is visible only to a manager or to its own
/// instructor. Anything else is hidden, never merely forbidden,
/// so a learner cannot tell a draft course exists by probing its
/// modules or lessons. This is the single visibility rule for
/// courses: list and single-resource handlers share it.
pub(crate) fn course_visible_to(is_manager: bool, caller: &Caller, course: &Course) -> bool {
    course.status == STATUS_PUBLISHED
        || is_manager
        || course.instructor_id == caller.actor
}

/// Hide a course the caller may not see as a 404.
pub(crate) fn require_course_visible(
    ctx: &AppContext,
    caller: &Caller,
    course: &Course,
) -> UnifiedResult<()> {
    let is_manager = check_capability(ctx, &caller.role, PERM_ELEARNING_MANAGE);
    if course_visible_to(is_manager, caller, course) {
        Ok(())
    } else {
        Err(UnifiedError::not_found("Course not found"))
    }
}

pub(crate) fn parse_uuid(raw: &str, what: &str) -> UnifiedResult<Uuid> {
    Uuid::parse_str(raw).map_err(|_| UnifiedError::bad_request(format!("Invalid {what} id: {raw}")))
}

pub(crate) fn require_text(value: &str, field: &str, max: usize) -> UnifiedResult<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err(UnifiedError::bad_request(format!("{field} must not be empty")));
    }
    if trimmed.len() > max {
        return Err(UnifiedError::bad_request(format!(
            "{field} must be at most {max} characters"
        )))
    }
    Ok(trimmed.to_string())
}

pub(crate) fn optional_text(value: &Option<String>, field: &str, max: usize) -> UnifiedResult<Option<String>> {
    match value {
        Some(raw) => Ok(Some(require_text(raw, field, max)?)),
        None => Ok(None),
    }
}

/// Validate a media reference against the shared documents store
/// contract (document id or `/files/{public,private}/{key}` URL
/// from the media picker). Length is checked first, so the error
/// names the field; the shared validator then checks the form.
pub(crate) fn require_media_reference(
    value: &str,
    field: &str,
    max: usize,
) -> UnifiedResult<String> {
    let checked = require_text(value, field, max)?;
    validate_media_reference(&checked)
        .map_err(|error| UnifiedError::bad_request(format!("{field}: {}", error.message)))
}

/// Optional form of [`require_media_reference`].
pub(crate) fn optional_media_reference(
    value: &Option<String>,
    field: &str,
    max: usize,
) -> UnifiedResult<Option<String>> {
    match value {
        Some(raw) => Ok(Some(require_media_reference(raw, field, max)?)),
        None => Ok(None),
    }
}

/// Validate a value against a settings list. The list is the live
/// admin-configured one — never a constant in this file.
pub(crate) fn require_in_list(value: &str, field: &str, allowed: &[String]) -> UnifiedResult<String> {
    let checked = require_text(value, field, MAX_SLUG_LEN)?;
    if !allowed.iter().any(|item| item == &checked) {
        return Err(UnifiedError::bad_request(format!(
            "{field} \"{checked}\" is not allowed; allowed values: {}",
            allowed.join(", ")
        )));
    }
    Ok(checked)
}

pub(crate) fn require_currency(value: &str) -> UnifiedResult<String> {
    let checked = require_text(value, "currency", 3)?;
    if checked.len() != 3 || !checked.chars().all(|c| c.is_ascii_alphabetic()) {
        return Err(UnifiedError::bad_request(
            "currency must be a 3-letter ISO code",
        ));
    }
    Ok(checked.to_ascii_uppercase())
}

pub(crate) fn ok<T: Serialize>(data: T) -> HttpResponse {
    json::success_response(data, 200)
}

pub(crate) fn created<T: Serialize>(data: T) -> HttpResponse {
    json::success_response(data, 201)
}

pub(crate) fn err(error: UnifiedError, debug: bool) -> HttpResponse {
    json::error_response(&error, debug)
}

pub(crate) fn paginate<T: Serialize + Clone>(items: Vec<T>, params: &PaginationParams) -> PageResult<T> {
    let pagination = params.normalized();
    let total = items.len() as u64;
    let start = ((pagination.page - 1) * pagination.per_page) as usize;
    let page_items: Vec<T> = items
        .into_iter()
        .skip(start)
        .take(pagination.per_page as usize)
        .collect();
    PageResult::new(page_items, total, &pagination)
}

pub(crate) fn opt_string(data: &Value, key: &str) -> Option<String> {
    data.get(key)
        .and_then(|value| value.as_str())
        .map(str::to_string)
}

/// Allowed course status transitions. Anything not listed is
/// rejected with a 422 so a course can never jump backwards.
pub(crate) fn valid_course_transition(from: &str, to: &str) -> bool {
    matches!(
        (from, to),
        (STATUS_DRAFT, STATUS_PUBLISHED)
            | (STATUS_DRAFT, STATUS_ARCHIVED)
            | (STATUS_PUBLISHED, STATUS_ARCHIVED)
            | (STATUS_ARCHIVED, STATUS_DRAFT)
    )
}

// --- Loaders ---------------------------------------------------------------

pub(crate) fn load_course(state: &ElearningState, raw_id: &str) -> UnifiedResult<Course> {
    let id = parse_uuid(raw_id, "course")?;
    state
        .courses
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Course not found: {id}")))
}

pub(crate) fn load_module(state: &ElearningState, raw_id: &str) -> UnifiedResult<CourseModule> {
    let id = parse_uuid(raw_id, "module")?;
    state
        .modules
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Module not found: {id}")))
}

pub(crate) fn load_lesson(state: &ElearningState, raw_id: &str) -> UnifiedResult<Lesson> {
    let id = parse_uuid(raw_id, "lesson")?;
    state
        .lessons
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Lesson not found: {id}")))
}

pub(crate) fn load_enrollment(state: &ElearningState, raw_id: &str) -> UnifiedResult<Enrollment> {
    let id = parse_uuid(raw_id, "enrollment")?;
    state
        .enrollments
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Enrollment not found: {id}")))
}

pub(crate) fn load_quiz_for_lesson(state: &ElearningState, lesson_id: Uuid) -> UnifiedResult<Quiz> {
    state
        .quizzes
        .iter()
        .map(|entry| entry.value().clone())
        .find(|quiz| quiz.lesson_id == lesson_id)
        .ok_or_else(|| UnifiedError::not_found("No quiz is defined for this lesson"))
}

pub(crate) fn load_question(state: &ElearningState, raw_id: &str) -> UnifiedResult<QuizQuestion> {
    let id = parse_uuid(raw_id, "question")?;
    state
        .questions
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Question not found: {id}")))
}

pub(crate) fn load_attempt(state: &ElearningState, raw_id: &str) -> UnifiedResult<QuizAttempt> {
    let id = parse_uuid(raw_id, "attempt")?;
    state
        .attempts
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Attempt not found: {id}")))
}

pub(crate) fn load_certificate(state: &ElearningState, raw_id: &str) -> UnifiedResult<Certificate> {
    let id = parse_uuid(raw_id, "certificate")?;
    state
        .certificates
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Certificate not found: {id}")))
}

pub(crate) fn load_review(state: &ElearningState, raw_id: &str) -> UnifiedResult<Review> {
    let id = parse_uuid(raw_id, "review")?;
    state
        .reviews
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Review not found: {id}")))
}

pub(crate) fn load_path(state: &ElearningState, raw_id: &str) -> UnifiedResult<LearningPath> {
    let id = parse_uuid(raw_id, "learning path")?;
    state
        .paths
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Learning path not found: {id}")))
}

/// The caller's live (not cancelled) enrollment in a course, if any.
pub(crate) fn find_enrollment(state: &ElearningState, course_id: Uuid, user_id: &str) -> Option<Enrollment> {
    state
        .enrollments
        .iter()
        .map(|entry| entry.value().clone())
        .find(|row| {
            row.course_id == course_id
                && row.user_id == user_id
                && row.status != ENROLL_CANCELLED
        })
}

/// All lessons of a course in course order (module position,
/// then lesson position).
pub(crate) fn ordered_lessons(state: &ElearningState, course_id: Uuid) -> Vec<Lesson> {
    let module_positions: HashMap<Uuid, i64> = state
        .modules
        .iter()
        .map(|entry| (entry.value().id, entry.value().position))
        .collect();
    let mut lessons: Vec<Lesson> = state
        .lessons
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|lesson| lesson.course_id == course_id)
        .collect();
    lessons.sort_by_key(|lesson| {
        (
            module_positions.get(&lesson.module_id).copied().unwrap_or(i64::MAX),
            lesson.position,
        )
    });
    lessons
}

/// Whether a lesson is currently available to a learner under the
/// drip rules: the drip date must have passed, and when the lesson
/// requires the previous one, that lesson must be complete.
pub(crate) fn lesson_available(
    state: &ElearningState,
    lesson: &Lesson,
    enrollment: &Enrollment,
) -> UnifiedResult<bool> {
    if let Some(from) = lesson.available_from {
        if Utc::now() < from {
            return Ok(false);
        }
    }
    if lesson.requires_previous {
        let lessons = ordered_lessons(state, lesson.course_id);
        if let Some(index) = lessons.iter().position(|row| row.id == lesson.id) {
            if index > 0 {
                let previous = &lessons[index - 1];
                let done = state.progress.iter().any(|entry| {
                    let row = entry.value();
                    row.enrollment_id == enrollment.id
                        && row.lesson_id == previous.id
                        && row.completed
                });
                if !done {
                    return Ok(false);
                }
            }
        }
    }
    Ok(true)
}

/// Whether the lesson's quiz (when one exists) has a passed
/// attempt for this enrollment. Quiz lessons cannot be completed
/// by hand until the quiz is passed.
pub(crate) fn quiz_passed(state: &ElearningState, lesson_id: Uuid, enrollment_id: Uuid) -> bool {
    let Some(quiz) = state
        .quizzes
        .iter()
        .map(|entry| entry.value().clone())
        .find(|quiz| quiz.lesson_id == lesson_id)
    else {
        return false;
    };
    state.attempts.iter().any(|entry| {
        let row = entry.value();
        row.quiz_id == quiz.id && row.enrollment_id == enrollment_id && row.passed
    })
}

/// Progress of one enrollment, computed on read from lesson
/// progress rows. A course with no required lessons is complete
/// as soon as the learner enrolls (progress 100).
pub(crate) fn course_progress(state: &ElearningState, enrollment: &Enrollment) -> CourseProgress {
    let lessons = ordered_lessons(state, enrollment.course_id);
    let required: Vec<&Lesson> = lessons.iter().filter(|lesson| lesson.required).collect();
    let total_required = required.len();
    let completed_required = required
        .iter()
        .filter(|lesson| {
            state.progress.iter().any(|entry| {
                let row = entry.value();
                row.enrollment_id == enrollment.id
                    && row.lesson_id == lesson.id
                    && row.completed
            })
        })
        .count();
    let percent = if total_required == 0 {
        100
    } else {
        ((completed_required as i64 * 100) / total_required as i64).clamp(0, 100)
    };
    CourseProgress {
        course_id: enrollment.course_id,
        user_id: enrollment.user_id.clone(),
        enrollment_id: enrollment.id,
        enrollment_status: enrollment.status.clone(),
        total_required,
        completed_required,
        progress_percent: percent,
        completed: total_required == 0 || completed_required == total_required,
    }
}

/// A random verifiable certificate code. Only the code is ever
/// returned to the learner; nothing secret is derived from it.
pub(crate) fn new_certificate_code() -> String {
    let mut rng = rand::rng();
    let groups: Vec<String> = (0..3)
        .map(|_| {
            (&mut rng)
                .sample_iter(&Alphanumeric)
                .take(4)
                .map(char::from)
                .collect::<String>()
                .to_ascii_uppercase()
        })
        .collect();
    format!("ELRN-{}", groups.join("-"))
}

/// SHA-256 fingerprint of the canonical certificate payload. This
/// is the value handed to the anchoring flow — never the payload
/// itself, so no personal data leaves the application.
pub(crate) fn certificate_fingerprint(
    code: &str,
    course_id: Uuid,
    user_id: &str,
    learner_name: &str,
    course_title: &str,
    issuer_name: &str,
    issued_at: DateTime<Utc>,
) -> String {
    let canonical = format!(
        "elearning-certificate|{code}|{course_id}|{user_id}|{learner_name}|{course_title}|{issuer_name}|{}",
        issued_at.to_rfc3339()
    );
    let digest = Sha256::digest(canonical.as_bytes());
    format!("{digest:x}")
}

/// Issue a certificate for a completed enrollment, unless a valid
/// one already exists for the same (course, user). Emits
/// `elearning.certificate.issued` and, when anchoring is enabled
/// in settings, `web3.anchor.requested` plus
/// `elearning.certificate.anchor.requested`. Anchoring is events
/// only — this module never talks to a chain.
pub(crate) fn issue_certificate(
    state: &ElearningState,
    enrollment: &Enrollment,
    course: &Course,
    learner_name: &str,
) -> Option<Certificate> {
    if !course.certificate_enabled {
        return None;
    }
    if state.certificates.iter().any(|entry| {
        let row = entry.value();
        row.course_id == course.id && row.user_id == enrollment.user_id && row.status == "valid"
    }) {
        return None;
    }
    let settings = state.settings();
    let now = Utc::now();
    let code = new_certificate_code();
    let fingerprint = certificate_fingerprint(
        &code,
        course.id,
        &enrollment.user_id,
        learner_name,
        &course.title,
        &settings.certificate_issuer_name,
        now,
    );
    let certificate = Certificate {
        id: Uuid::now_v7(),
        code: code.clone(),
        course_id: course.id,
        user_id: enrollment.user_id.clone(),
        learner_name: learner_name.to_string(),
        course_title: course.title.clone(),
        issuer_name: settings.certificate_issuer_name.clone(),
        issued_at: now,
        content_sha256: fingerprint.clone(),
        status: "valid".to_string(),
        anchor_status: if settings.certificate_anchoring_enabled {
            "requested".to_string()
        } else {
            "none".to_string()
        },
        anchor_tx: None,
        revoked_at: None,
        created_at: now,
        updated_at: now,
    };
    state
        .certificates
        .insert(certificate.id, certificate.clone());
    state.emit(
        "elearning.certificate.issued",
        json!({
            "certificate_id": certificate.id.to_string(),
            "code": code,
            "course_id": course.id.to_string(),
            "user_id": enrollment.user_id,
            "content_sha256": fingerprint,
        }),
    );
    if settings.certificate_anchoring_enabled {
        state.emit(
            "web3.anchor.requested",
            json!({
                "record_type": "elearning.certificate",
                "record_id": certificate.id.to_string(),
                "content_sha256": fingerprint,
                "network_id": settings.anchoring_network_id,
            }),
        );
        state.emit(
            "elearning.certificate.anchor.requested",
            json!({
                "certificate_id": certificate.id.to_string(),
                "code": code,
                "content_sha256": fingerprint,
            }),
        );
    }
    state.notify(
        "elearning.certificate.issued",
        &enrollment.user_id,
        &format!("Certificate issued: {}", course.title),
        &format!(
            "Your certificate for \"{}\" is ready. Verification code: {code}.",
            course.title
        ),
        Some(format!("/api/v1/elearning/certificates/verify/{code}")),
    );
    Some(certificate)
}

/// Paths whose last course just completed for `user_id`:
/// every course in the path has a completed enrollment for this
/// user. Called from `maybe_complete_course` right after a
/// course completes, so the returned paths are exactly the ones
/// this completion finished.
pub(crate) fn paths_completed_by_course(
    state: &ElearningState,
    course_id: Uuid,
    user_id: &str,
) -> Vec<Uuid> {
    let mut path_ids: Vec<Uuid> = state
        .path_courses
        .iter()
        .filter(|entry| entry.value().course_id == course_id)
        .map(|entry| entry.value().path_id)
        .collect();
    path_ids.sort();
    path_ids.dedup();
    path_ids
        .into_iter()
        .filter(|path_id| {
            let courses: Vec<Uuid> = state
                .path_courses
                .iter()
                .filter(|entry| entry.value().path_id == *path_id)
                .map(|entry| entry.value().course_id)
                .collect();
            !courses.is_empty()
                && courses.iter().all(|id| {
                    find_enrollment(state, *id, user_id)
                        .is_some_and(|row| row.status == ENROLL_COMPLETED)
                })
        })
        .collect()
}

/// After a lesson completion, finish the enrollment (and issue a
/// certificate) when every required lesson is done.
pub(crate) fn maybe_complete_course(state: &ElearningState, enrollment: &Enrollment) {
    let progress = course_progress(state, enrollment);
    if !progress.completed || enrollment.status != ENROLL_ACTIVE {
        return;
    }
    let Some(mut row) = state.enrollments.get_mut(&enrollment.id) else {
        return;
    };
    if row.status != ENROLL_ACTIVE {
        return;
    }
    let now = Utc::now();
    row.status = ENROLL_COMPLETED.to_string();
    row.completed_at = Some(now);
    row.updated_at = now;
    let updated = row.clone();
    drop(row);
    let Some(course) = state.courses.get(&enrollment.course_id).map(|entry| entry.value().clone()) else {
        return;
    };
    state.emit(
        "elearning.enrollment.completed",
        json!({
            "enrollment_id": updated.id.to_string(),
            "course_id": updated.course_id.to_string(),
            "user_id": updated.user_id,
        }),
    );
    state.notify(
        "elearning.enrollment.completed",
        &updated.user_id,
        &format!("Course completed: {}", course.title),
        &format!("You completed \"{}\".", course.title),
        Some(format!("/api/v1/elearning/courses/{}/progress", course.id)),
    );
    let learner_name = updated
        .learner_name
        .clone()
        .unwrap_or_else(|| updated.user_id.clone());
    issue_certificate(state, &updated, &course, &learner_name);
    // A learning path completes when its last course completes:
    // announce every path this completion finished.
    for path_id in paths_completed_by_course(state, course.id, &updated.user_id) {
        state.emit(
            "elearning.path.completed",
            json!({
                "path_id": path_id.to_string(),
                "course_id": course.id.to_string(),
                "user_id": updated.user_id,
            }),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn require_text_trims_surrounding_whitespace() {
        assert_eq!(require_text("  hello  ", "title", 10).unwrap(), "hello");
    }

    #[test]
    fn require_text_rejects_empty_value() {
        assert!(require_text("   ", "title", 10).is_err());
        assert!(require_text("", "title", 10).is_err());
    }

    #[test]
    fn require_text_rejects_value_over_max() {
        assert!(require_text("toolong", "title", 3).is_err());
    }

    #[test]
    fn require_currency_uppercases_iso_code() {
        assert_eq!(require_currency("usd").unwrap(), "USD");
        assert_eq!(require_currency("EUR").unwrap(), "EUR");
    }

    #[test]
    fn require_currency_rejects_short_code() {
        assert!(require_currency("US").is_err());
    }

    #[test]
    fn require_currency_rejects_non_alpha_code() {
        assert!(require_currency("U1D").is_err());
    }

    #[test]
    fn course_transition_allows_forward_and_reopen_moves() {
        assert!(valid_course_transition("draft", "published"));
        assert!(valid_course_transition("draft", "archived"));
        assert!(valid_course_transition("published", "archived"));
        assert!(valid_course_transition("archived", "draft"));
    }

    #[test]
    fn course_transition_rejects_backwards_and_same_state() {
        assert!(!valid_course_transition("published", "draft"));
        assert!(!valid_course_transition("archived", "published"));
        assert!(!valid_course_transition("draft", "draft"));
        assert!(!valid_course_transition("published", "published"));
    }

    #[test]
    fn optional_text_none_stays_none() {
        assert_eq!(optional_text(&None, "note", 10).unwrap(), None);
    }

    #[test]
    fn optional_text_rejects_empty_value() {
        assert!(optional_text(&Some("  ".to_string()), "note", 10).is_err());
    }

    fn test_course(status: &str, instructor_id: &str) -> Course {
        let now = Utc::now();
        Course {
            id: Uuid::now_v7(),
            title: "Course".to_string(),
            description: "Description".to_string(),
            category: "business".to_string(),
            level: "beginner".to_string(),
            language: "en".to_string(),
            thumbnail_ref: None,
            instructor_id: instructor_id.to_string(),
            status: status.to_string(),
            price_minor: 0,
            currency: "USD".to_string(),
            certificate_enabled: false,
            company_id: None,
            created_at: now,
            updated_at: now,
        }
    }

    fn test_caller(actor: &str) -> Caller {
        Caller {
            actor: actor.to_string(),
            role: "instructor".to_string(),
        }
    }

    fn test_enrollment(course_id: Uuid, user_id: &str, status: &str) -> Enrollment {
        let now = Utc::now();
        Enrollment {
            id: Uuid::now_v7(),
            course_id,
            user_id: user_id.to_string(),
            status: status.to_string(),
            price_paid_minor: 0,
            currency: "USD".to_string(),
            order_id: None,
            learner_name: None,
            enrolled_at: now,
            completed_at: None,
            created_at: now,
            updated_at: now,
        }
    }

    #[test]
    fn course_visible_to_shows_published_courses_to_everyone() {
        let course = test_course(STATUS_PUBLISHED, "instructor-1");
        assert!(course_visible_to(false, &test_caller("learner-9"), &course));
        assert!(course_visible_to(false, &test_caller("instructor-2"), &course));
    }

    #[test]
    fn course_visible_to_shows_drafts_only_to_manager_or_owner() {
        let course = test_course(STATUS_DRAFT, "instructor-1");
        assert!(course_visible_to(true, &test_caller("manager-1"), &course));
        assert!(course_visible_to(false, &test_caller("instructor-1"), &course));
        // Another instructor must not see this draft.
        assert!(!course_visible_to(false, &test_caller("instructor-2"), &course));
        // Neither must a learner.
        assert!(!course_visible_to(false, &test_caller("learner-9"), &course));
    }

    #[test]
    fn course_visible_to_hides_archived_courses_from_other_instructors() {
        let course = test_course(STATUS_ARCHIVED, "instructor-1");
        assert!(!course_visible_to(false, &test_caller("instructor-2"), &course));
        assert!(course_visible_to(false, &test_caller("instructor-1"), &course));
        assert!(course_visible_to(true, &test_caller("manager-1"), &course));
    }

    #[test]
    fn paths_completed_by_course_needs_every_course_completed() {
        let state = ElearningState::default();
        let now = Utc::now();
        let path = LearningPath {
            id: Uuid::now_v7(),
            title: "Path".to_string(),
            description: None,
            created_by: "admin".to_string(),
            created_at: now,
            updated_at: now,
        };
        let first = Uuid::now_v7();
        let second = Uuid::now_v7();
        state.paths.insert(path.id, path.clone());
        for (position, course_id) in [(0, first), (1, second)] {
            state.path_courses.insert(
                Uuid::now_v7(),
                LearningPathCourse {
                    id: Uuid::now_v7(),
                    path_id: path.id,
                    course_id,
                    position,
                    created_at: now,
                },
            );
        }
        let first_done = test_enrollment(first, "learner-1", ENROLL_COMPLETED);
        state.enrollments.insert(first_done.id, first_done);
        let second_active = test_enrollment(second, "learner-1", ENROLL_ACTIVE);
        state.enrollments.insert(second_active.id, second_active.clone());
        assert!(paths_completed_by_course(&state, first, "learner-1").is_empty());
        let mut finished = second_active.clone();
        finished.status = ENROLL_COMPLETED.to_string();
        state.enrollments.insert(finished.id, finished);
        assert_eq!(paths_completed_by_course(&state, first, "learner-1"), vec![path.id]);
    }

    #[test]
    fn maybe_complete_course_emits_path_completed_for_last_course() {
        let state = ElearningState::default();
        let bus = Arc::new(HookBus::new());
        let seen: Arc<std::sync::Mutex<Vec<Value>>> = Arc::new(std::sync::Mutex::new(Vec::new()));
        let seen_clone = seen.clone();
        bus.register(
            "elearning.path.completed",
            "test",
            Arc::new(move |ctx: &mut HookContext| {
                seen_clone
                    .lock()
                    .expect("recorder lock poisoned")
                    .push(ctx.data.clone());
            }),
        );
        *state
            .hooks
            .write()
            .expect("hooks lock poisoned") = Some(bus);
        let now = Utc::now();
        let course = test_course(STATUS_PUBLISHED, "instructor-1");
        state.courses.insert(course.id, course.clone());
        let module = CourseModule {
            id: Uuid::now_v7(),
            course_id: course.id,
            title: "Module".to_string(),
            description: None,
            position: 0,
            created_at: now,
            updated_at: now,
        };
        state.modules.insert(module.id, module.clone());
        let lesson = Lesson {
            id: Uuid::now_v7(),
            module_id: module.id,
            course_id: course.id,
            title: "Lesson".to_string(),
            lesson_type: "text".to_string(),
            content_ref: None,
            body: Some("Body".to_string()),
            position: 0,
            required: true,
            available_from: None,
            requires_previous: false,
            duration_minutes: None,
            created_at: now,
            updated_at: now,
        };
        state.lessons.insert(lesson.id, lesson.clone());
        let enrollment = test_enrollment(course.id, "learner-1", ENROLL_ACTIVE);
        state.enrollments.insert(enrollment.id, enrollment.clone());
        state.progress.insert(
            Uuid::now_v7(),
            LessonProgress {
                id: Uuid::now_v7(),
                enrollment_id: enrollment.id,
                lesson_id: lesson.id,
                course_id: course.id,
                user_id: "learner-1".to_string(),
                completed: true,
                completed_at: Some(now),
                created_at: now,
                updated_at: now,
            },
        );
        let path = LearningPath {
            id: Uuid::now_v7(),
            title: "Path".to_string(),
            description: None,
            created_by: "admin".to_string(),
            created_at: now,
            updated_at: now,
        };
        state.paths.insert(path.id, path.clone());
        state.path_courses.insert(
            Uuid::now_v7(),
            LearningPathCourse {
                id: Uuid::now_v7(),
                path_id: path.id,
                course_id: course.id,
                position: 0,
                created_at: now,
            },
        );
        maybe_complete_course(&state, &enrollment);
        let finished = state
            .enrollments
            .get(&enrollment.id)
            .expect("enrollment still present");
        assert_eq!(finished.status, ENROLL_COMPLETED);
        let events = seen.lock().expect("recorder lock poisoned");
        assert_eq!(events.len(), 1);
        assert_eq!(
            events[0].get("path_id").and_then(|value| value.as_str()),
            Some(path.id.to_string()).as_deref()
        );
        assert_eq!(
            events[0].get("course_id").and_then(|value| value.as_str()),
            Some(course.id.to_string()).as_deref()
        );
    }
}

#[cfg(test)]
mod tenant_guard_tests {
    use super::*;
    use actix_web::test::TestRequest;
    use actix_web::HttpMessage;

    /// Request carrying the middleware-resolved tenant context the
    /// tenant-resolution middleware would attach, plus a verified
    /// caller (optionally with a server-resolved company).
    fn tenant_request(company_id: &str, caller_company: Option<&str>) -> HttpRequest {
        let req = TestRequest::default().to_http_request();
        req.extensions_mut()
            .insert(unified_core::middleware::TenantContext {
                tenant_id: uuid::Uuid::now_v7().to_string(),
                company_id: company_id.to_string(),
                site_id: String::new(),
                kind: unified_core::middleware::TenantKind::Subdomain,
                host: "tenant.example.test".to_string(),
                scope: None,
                product_id: None,
                package_id: None,
                product_subdomain_id: None,
            });
        let mut caller =
            unified_core::auth::Caller::verified(test_user_uuid("alice"), Vec::new());
        if let Some(company) = caller_company {
            caller = caller.with_company(company.to_string());
        }
        req.extensions_mut().insert(caller);
        req
    }

    /// Deterministic test-user identity: distinct names map to
    /// distinct UUIDs (FNV-1a folded into the low bits), so
    /// ownership tests can still tell two users apart. A value
    /// that already parses as a UUID is used as-is.
    fn test_user_uuid(name: &str) -> uuid::Uuid {
        if let Ok(id) = uuid::Uuid::parse_str(name) {
            return id;
        }
        let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
        for byte in name.bytes() {
            hash ^= u64::from(byte);
            hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        }
        uuid::Uuid::from_u128(u128::from(hash) | 0x0000_0000_0000_4000_8000_0000_0000_0000)
    }

    #[test]
    fn require_tenant_fails_closed_without_resolved_context() {
        let bare = TestRequest::default().to_http_request();
        assert!(require_tenant(&bare).is_err());
    }

    #[test]
    fn require_tenant_resolves_from_middleware_context() {
        let req = tenant_request("acme", None);
        let tenant = require_tenant(&req).expect("tenant context must resolve");
        assert!(!tenant.tenant_id.to_string().is_empty());
        assert_eq!(
            caller_company(&tenant, &req).unwrap(),
            Some("acme".to_string())
        );
    }

    #[test]
    fn caller_company_prefers_verified_caller_when_tenant_has_none() {
        let req = tenant_request("", Some("acme"));
        let tenant = require_tenant(&req).expect("tenant context must resolve");
        assert_eq!(
            caller_company(&tenant, &req).unwrap(),
            Some("acme".to_string())
        );
    }

    #[test]
    fn caller_company_rejects_verified_caller_mismatch() {
        let req = tenant_request("acme", Some("other"));
        let tenant = require_tenant(&req).expect("tenant context must resolve");
        assert!(caller_company(&tenant, &req).is_err());
    }

    #[test]
    fn caller_company_ignores_the_x_company_id_header() {
        // A forged header naming another company must not move
        // the scope: only the tenant context and the verified
        // caller count.
        let req = TestRequest::default()
            .insert_header(("x-company-id", "other"))
            .to_http_request();
        req.extensions_mut()
            .insert(unified_core::middleware::TenantContext {
                tenant_id: uuid::Uuid::now_v7().to_string(),
                company_id: "acme".to_string(),
                site_id: String::new(),
                kind: unified_core::middleware::TenantKind::Subdomain,
                host: "tenant.example.test".to_string(),
                scope: None,
                product_id: None,
                package_id: None,
                product_subdomain_id: None,
            });
        req.extensions_mut().insert(
            unified_core::auth::Caller::verified(test_user_uuid("alice"), Vec::new()),
        );
        let tenant = require_tenant(&req).expect("tenant context must resolve");
        assert_eq!(
            caller_company(&tenant, &req).unwrap(),
            Some("acme".to_string())
        );
    }

    #[test]
    fn in_company_scope_shows_global_records_to_scoped_callers() {
        let scope = Some("acme".to_string());
        assert!(in_company_scope(&Some("acme".to_string()), &scope));
        assert!(!in_company_scope(&Some("other".to_string()), &scope));
        assert!(in_company_scope(&None, &scope));
        assert!(in_company_scope(&None, &None));
        assert!(!in_company_scope(&Some("other".to_string()), &None));
    }

    #[test]
    fn require_company_match_for_refuses_writes_from_unresolved_callers() {
        let body = Some("c1".to_string());
        assert!(require_company_match_for(&None, &body).is_err());
        assert!(require_company_match_for(&None, &None).is_err());
        assert_eq!(
            require_company_match_for(&Some("c1".to_string()), &body).unwrap(),
            Some("c1".to_string())
        );
        assert!(
            require_company_match_for(&Some("c1".to_string()), &Some("c2".to_string())).is_err()
        );
    }

    #[test]
    fn require_company_change_for_keeps_an_explicit_clear_a_clear() {
        let scope = Some("c1".to_string());
        assert_eq!(
            require_company_change_for(&scope, &None).unwrap(),
            None
        );
        assert_eq!(
            require_company_change_for(&scope, &Some("c1".to_string())).unwrap(),
            Some("c1".to_string())
        );
        assert!(require_company_change_for(&scope, &Some("c2".to_string())).is_err());
        assert!(require_company_change_for(&None, &Some("c1".to_string())).is_err());
        assert_eq!(require_company_change_for(&None, &None).unwrap(), None);
    }

    #[test]
    fn course_in_scope_is_fail_closed_for_missing_courses() {
        let state = ElearningState::default();
        let scope = Some("acme".to_string());
        assert!(!course_in_scope(&state, Uuid::now_v7(), &scope));
        assert!(!course_in_scope(&state, Uuid::now_v7(), &None));
    }
}
