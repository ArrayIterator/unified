// Elearning — Enrollment and progress handlers.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::{HookBus, HookContext};
use unified_core::json;
use unified_core::modules::{Module, ModuleRegistry, RouteInfo, RouteVersionSupport};
use unified_core::pagination::{PageResult, PaginationParams};
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
// Enrollment and progress handlers
// ---------------------------------------------------------------------------

pub(crate) async fn enroll_course(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ElearningState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<EnrollRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Enrollment> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = require_company_for_write(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ELEARNING_VIEW)?;
        let course = load_course(&state, &path.into_inner())?;
        require_record_in_company_scope(&course.company_id, &_scope)?;
        if course.status != STATUS_PUBLISHED {
            return Err(UnifiedError::unprocessable(
                "Only published courses accept enrollments",
            ));
        }
        if let Some(existing) = find_enrollment(&state, course.id, &caller.actor) {
            if existing.status == ENROLL_ACTIVE || existing.status == ENROLL_COMPLETED {
                return Err(UnifiedError::conflict(
                    "Already enrolled in this course",
                ));
            }
            if existing.status == ENROLL_PENDING_PAYMENT {
                return Err(UnifiedError::conflict(
                    "A paid enrollment is already pending payment for this course",
                ));
            }
            if existing.status == ENROLL_SUSPENDED {
                return Err(UnifiedError::conflict(
                    "Enrollment is suspended; contact support to reactivate it",
                ));
            }
        }
        let input = body.into_inner();
        let learner_name = optional_text(&input.learner_name, "learner_name", MAX_TITLE_LEN)?;
        let now = Utc::now();
        let paid = course.price_minor > 0;
        let enrollment = Enrollment {
            id: Uuid::now_v7(),
            course_id: course.id,
            user_id: caller.actor.clone(),
            status: if paid {
                ENROLL_PENDING_PAYMENT.to_string()
            } else {
                ENROLL_ACTIVE.to_string()
            },
            price_paid_minor: course.price_minor,
            currency: course.currency.clone(),
            order_id: None,
            learner_name,
            enrolled_at: now,
            completed_at: None,
            created_at: now,
            updated_at: now,
        };
        state.enrollments.insert(enrollment.id, enrollment.clone());
        state.emit(
            "elearning.enrollment.created",
            json!({
                "enrollment_id": enrollment.id.to_string(),
                "course_id": course.id.to_string(),
                "user_id": enrollment.user_id,
                "status": enrollment.status,
            }),
        );
        if paid {
            // Paid enrollment never calls the store directly: it
            // emits a purchase request and waits for `order.paid`.
            state.emit(
                "elearning.enrollment.purchase_requested",
                json!({
                    "enrollment_id": enrollment.id.to_string(),
                    "course_id": course.id.to_string(),
                    "user_id": enrollment.user_id,
                    "amount_minor": course.price_minor,
                    "currency": course.currency,
                    "module": "elearning",
                    "item_type": "course_enrollment",
                }),
            );
        } else {
            state.notify(
                "elearning.enrollment.activated",
                &enrollment.user_id,
                &format!("Enrolled: {}", course.title),
                &format!("You are enrolled in \"{}\".", course.title),
                Some(format!("/api/v1/elearning/courses/{}/progress", course.id)),
            );
        }
        Ok(enrollment)
    })();
    match result {
        Ok(enrollment) => created(enrollment),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn list_enrollments(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ElearningState>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ELEARNING_VIEW)?;
        let params = query.into_inner();
        let is_manager = check_capability(&ctx, &caller.role, PERM_ELEARNING_MANAGE);
        let course_filter = params.filters.get("course_id").cloned();
        let user_filter = params.filters.get("user_id").cloned();
        let status_filter = params.filters.get("status").cloned();
        let mut items: Vec<Enrollment> = state
            .enrollments
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|row| {
                if !course_in_scope(&state, row.course_id, &_scope) {
                    return false;
                }
                if !is_manager && row.user_id != caller.actor {
                    return false;
                }
                if let Some(course_id) = &course_filter {
                    if row.course_id.to_string() != *course_id {
                        return false;
                    }
                }
                if let Some(user_id) = &user_filter {
                    if &row.user_id != user_id {
                        return false;
                    }
                }
                if let Some(status) = &status_filter {
                    if &row.status != status {
                        return false;
                    }
                }
                true
            })
            .collect();
        items.sort_by_key(|row| row.enrolled_at);
        Ok(paginate(items, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn get_enrollment(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ElearningState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Enrollment> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ELEARNING_VIEW)?;
        let row = load_enrollment(&state, &path.into_inner())?;
        require_course_by_id_in_scope(&state, row.course_id, &_scope)?;
        let is_manager = check_capability(&ctx, &caller.role, PERM_ELEARNING_MANAGE);
        if !is_manager && row.user_id != caller.actor {
            return Err(UnifiedError::forbidden(
                "Only the learner or a manager may view this enrollment",
            ));
        }
        Ok(row)
    })();
    match result {
        Ok(row) => ok(row),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn cancel_enrollment(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ElearningState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Enrollment> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = require_company_for_write(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ELEARNING_VIEW)?;
        let mut row = load_enrollment(&state, &path.into_inner())?;
        require_course_by_id_in_scope(&state, row.course_id, &_scope)?;
        let is_manager = check_capability(&ctx, &caller.role, PERM_ELEARNING_MANAGE);
        if !is_manager && row.user_id != caller.actor {
            return Err(UnifiedError::forbidden(
                "Only the learner or a manager may cancel this enrollment",
            ));
        }
        if row.status == ENROLL_COMPLETED {
            return Err(UnifiedError::conflict(
                "A completed enrollment cannot be cancelled",
            ));
        }
        if row.status == ENROLL_CANCELLED {
            return Err(UnifiedError::conflict("Enrollment is already cancelled"));
        }
        row.status = ENROLL_CANCELLED.to_string();
        row.updated_at = Utc::now();
        state.enrollments.insert(row.id, row.clone());
        state.emit(
            "elearning.enrollment.cancelled",
            json!({
                "enrollment_id": row.id.to_string(),
                "course_id": row.course_id.to_string(),
                "user_id": row.user_id,
            }),
        );
        Ok(row)
    })();
    match result {
        Ok(row) => ok(row),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn list_my_enrollments(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ElearningState>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ELEARNING_VIEW)?;
        let params = query.into_inner();
        let status_filter = params.filters.get("status").cloned();
        let mut items: Vec<Enrollment> = state
            .enrollments
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|row| {
                if !course_in_scope(&state, row.course_id, &_scope) {
                    return false;
                }
                if row.user_id != caller.actor {
                    return false;
                }
                if let Some(status) = &status_filter {
                    if &row.status != status {
                        return false;
                    }
                }
                true
            })
            .collect();
        items.sort_by_key(|row| row.enrolled_at);
        Ok(paginate(items, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn complete_lesson(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ElearningState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    // The flag is whether the row was newly inserted: an
    // already-completed lesson answers 200, a fresh completion
    // answers 201.
    let result: UnifiedResult<(LessonProgress, bool)> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = require_company_for_write(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ELEARNING_VIEW)?;
        let lesson = load_lesson(&state, &path.into_inner())?;
        require_course_by_id_in_scope(&state, lesson.course_id, &_scope)?;
        let Some(enrollment) = find_enrollment(&state, lesson.course_id, &caller.actor) else {
            return Err(UnifiedError::forbidden(
                "Enroll in this course before completing lessons",
            ));
        };
        if enrollment.status != ENROLL_ACTIVE {
            return Err(UnifiedError::conflict(format!(
                "Enrollment is {}, not active",
                enrollment.status
            )));
        }
        if !lesson_available(&state, &lesson, &enrollment)? {
            return Err(UnifiedError::unprocessable(
                "Lesson is not available yet (drip schedule or previous lesson incomplete)",
            ));
        }
        // A lesson with a quiz is completed by passing the quiz,
        // never by hand.
        let has_quiz = state
            .quizzes
            .iter()
            .any(|entry| entry.value().lesson_id == lesson.id);
        if has_quiz && !quiz_passed(&state, lesson.id, enrollment.id) {
            return Err(UnifiedError::unprocessable(
                "Pass the lesson quiz before completing this lesson",
            ));
        }
        if let Some(existing) = state.progress.iter().find(|entry| {
            let row = entry.value();
            row.enrollment_id == enrollment.id && row.lesson_id == lesson.id && row.completed
        }) {
            return Ok((existing.value().clone(), false));
        }
        let now = Utc::now();
        let row = LessonProgress {
            id: Uuid::now_v7(),
            enrollment_id: enrollment.id,
            lesson_id: lesson.id,
            course_id: lesson.course_id,
            user_id: caller.actor.clone(),
            completed: true,
            completed_at: Some(now),
            created_at: now,
            updated_at: now,
        };
        state.progress.insert(row.id, row.clone());
        state.emit(
            "elearning.lesson.completed",
            json!({
                "lesson_id": lesson.id.to_string(),
                "course_id": lesson.course_id.to_string(),
                "user_id": caller.actor,
                "enrollment_id": enrollment.id.to_string(),
            }),
        );
        maybe_complete_course(&state, &enrollment);
        Ok((row, true))
    })();
    match result {
        Ok((row, true)) => created(row),
        Ok((row, false)) => ok(row),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn get_course_progress(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ElearningState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<CourseProgress> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ELEARNING_VIEW)?;
        let course = load_course(&state, &path.into_inner())?;
        require_record_in_company_scope(&course.company_id, &_scope)?;
        let Some(enrollment) = find_enrollment(&state, course.id, &caller.actor) else {
            return Err(UnifiedError::not_found("No enrollment in this course"));
        };
        Ok(course_progress(&state, &enrollment))
    })();
    match result {
        Ok(progress) => ok(progress),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn get_user_course_progress(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ElearningState>,
    req: HttpRequest,
    path: web::Path<(String, String)>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<CourseProgress> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ELEARNING_VIEW)?;
        let (course_id, user_id) = path.into_inner();
        let course = load_course(&state, &course_id)?;
        require_record_in_company_scope(&course.company_id, &_scope)?;
        let is_manager = check_capability(&ctx, &caller.role, PERM_ELEARNING_MANAGE);
        let is_instructor = check_capability(&ctx, &caller.role, PERM_ELEARNING_INSTRUCT)
            && course.instructor_id == caller.actor;
        if !is_manager && !is_instructor && user_id != caller.actor {
            return Err(UnifiedError::forbidden(
                "Only the learner, the course instructor, or a manager may view this progress",
            ));
        }
        let Some(enrollment) = find_enrollment(&state, course.id, &user_id) else {
            return Err(UnifiedError::not_found("No enrollment in this course"));
        };
        Ok(course_progress(&state, &enrollment))
    })();
    match result {
        Ok(progress) => ok(progress),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn list_my_progress(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ElearningState>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ELEARNING_VIEW)?;
        let params = query.into_inner();
        let mut items: Vec<CourseProgress> = state
            .enrollments
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|row| {
                row.user_id == caller.actor
                    && row.status != ENROLL_CANCELLED
                    && course_in_scope(&state, row.course_id, &_scope)
            })
            .map(|row| course_progress(&state, &row))
            .collect();
        items.sort_by_key(|row| row.course_id);
        Ok(paginate(items, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}
