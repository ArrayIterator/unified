// Elearning — Course handlers.

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
// Course handlers
// ---------------------------------------------------------------------------

pub(crate) async fn list_courses(
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
        let category_filter = params.filters.get("category").cloned();
        let level_filter = params.filters.get("level").cloned();
        let language_filter = params.filters.get("language").cloned();
        let status_filter = params.filters.get("status").cloned();
        let instructor_filter = params.filters.get("instructor_id").cloned();
        let search = params.search.clone().map(|value| value.to_lowercase());
        let is_manager = check_capability(&ctx, &caller.role, PERM_ELEARNING_MANAGE);
        let mut items: Vec<Course> = state
            .courses
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|course| {
                if !in_company_scope(&course.company_id, &_scope) {
                    return false;
                }
                // Learners only ever see published courses;
                // instructors also see their own drafts, managers
                // see every status. The rule is shared with
                // `require_course_visible`, evaluated here in bulk
                // so the capability hooks run once, not once per
                // course.
                if !course_visible_to(is_manager, &caller, course) {
                    return false;
                }
                if let Some(category) = &category_filter {
                    if &course.category != category {
                        return false;
                    }
                }
                if let Some(level) = &level_filter {
                    if &course.level != level {
                        return false;
                    }
                }
                if let Some(language) = &language_filter {
                    if &course.language != language {
                        return false;
                    }
                }
                if let Some(status) = &status_filter {
                    if &course.status != status {
                        return false;
                    }
                }
                if let Some(instructor) = &instructor_filter {
                    if &course.instructor_id != instructor {
                        return false;
                    }
                }
                if let Some(needle) = &search {
                    let haystack = format!("{} {}", course.title, course.description).to_lowercase();
                    if !haystack.contains(needle) {
                        return false;
                    }
                }
                true
            })
            .collect();
        items.sort_by_key(|course| course.title.clone());
        Ok(paginate(items, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn create_course(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ElearningState>,
    req: HttpRequest,
    body: web::Json<CreateCourseRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Course> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = require_company_for_write(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_teach(&ctx, &caller.role)?;
        let input = body.into_inner();
        let settings = state.settings();
        let title = require_text(&input.title, "title", MAX_TITLE_LEN)?;
        let description = require_text(&input.description, "description", MAX_DESCRIPTION_LEN)?;
        let category = require_in_list(&input.category, "category", &settings.categories)?;
        let level = require_in_list(&input.level, "level", &settings.levels)?;
        let language = match &input.language {
            Some(raw) => require_in_list(raw, "language", &settings.languages)?,
            None => settings.languages.first().cloned().unwrap_or_else(|| "en".to_string()),
        };
        let price_minor = input.price_minor.unwrap_or(0);
        if price_minor < 0 || price_minor > 1_000_000_000 {
            return Err(UnifiedError::bad_request(
                "price_minor must be between 0 and 1000000000",
            ));
        }
        let currency = match &input.currency {
            Some(raw) => require_currency(raw)?,
            None => settings.default_currency.clone(),
        };
        let thumbnail_ref =
            optional_media_reference(&input.thumbnail_ref, "thumbnail_ref", MAX_REF_LEN)?;
        let company_id = require_company_match(
            &_tenant,
            &req,
            &optional_text(&input.company_id, "company_id", MAX_SLUG_LEN)?,
        )?;
        let now = Utc::now();
        let course = Course {
            id: Uuid::now_v7(),
            title,
            description,
            category,
            level,
            language,
            thumbnail_ref,
            instructor_id: caller.actor.clone(),
            status: STATUS_DRAFT.to_string(),
            price_minor,
            currency,
            certificate_enabled: input.certificate_enabled.unwrap_or(true),
            company_id,
            created_at: now,
            updated_at: now,
        };
        state.courses.insert(course.id, course.clone());
        state.emit(
            "elearning.course.created",
            json!({
                "course_id": course.id.to_string(),
                "title": course.title,
                "instructor_id": course.instructor_id,
                "status": course.status,
            }),
        );
        Ok(course)
    })();
    match result {
        Ok(course) => created(course),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn get_course(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ElearningState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Course> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ELEARNING_VIEW)?;
        let course = load_course(&state, &path.into_inner())?;
        require_record_in_company_scope(&course.company_id, &_scope)?;
        require_course_visible(&ctx, &caller, &course)?;
        Ok(course)
    })();
    match result {
        Ok(course) => ok(course),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn update_course(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ElearningState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdateCourseRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Course> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = require_company_for_write(&_tenant, &req)?;
        let caller = caller(&req)?;
        let mut course = load_course(&state, &path.into_inner())?;
        require_record_writable_in_company_scope(&course.company_id, &_scope)?;
        require_course_write(&ctx, &caller, &course)?;
        let input = body.into_inner();
        let settings = state.settings();
        if let Some(title) = &input.title {
            course.title = require_text(title, "title", MAX_TITLE_LEN)?;
        }
        if let Some(description) = &input.description {
            course.description = require_text(description, "description", MAX_DESCRIPTION_LEN)?;
        }
        if let Some(category) = &input.category {
            course.category = require_in_list(category, "category", &settings.categories)?;
        }
        if let Some(level) = &input.level {
            course.level = require_in_list(level, "level", &settings.levels)?;
        }
        if let Some(language) = &input.language {
            course.language = require_in_list(language, "language", &settings.languages)?;
        }
        if let Some(thumbnail_ref) = &input.thumbnail_ref {
            course.thumbnail_ref = match thumbnail_ref {
                Some(raw) => Some(require_media_reference(raw, "thumbnail_ref", MAX_REF_LEN)?),
                None => None,
            };
        }
        if let Some(price) = input.price_minor {
            if price < 0 || price > 1_000_000_000 {
                return Err(UnifiedError::bad_request(
                    "price_minor must be between 0 and 1000000000",
                ));
            }
            if state.enrollments.iter().any(|entry| {
                entry.value().course_id == course.id
                    && entry.value().status == ENROLL_PENDING_PAYMENT
            }) {
                return Err(UnifiedError::conflict(
                    "Course price cannot change while a paid enrollment is pending payment",
                ));
            }
            course.price_minor = price;
        }
        if let Some(currency) = &input.currency {
            course.currency = require_currency(currency)?;
        }
        if let Some(enabled) = input.certificate_enabled {
            course.certificate_enabled = enabled;
        }
        if let Some(company_id) = &input.company_id {
            let checked = match company_id {
                Some(raw) => Some(require_text(raw, "company_id", MAX_SLUG_LEN)?),
                None => None,
            };
            // A newly named company must match the caller scope;
            // an explicit clear stays a clear and is never
            // re-stamped with the caller scope.
            course.company_id = require_company_change_for(&_scope, &checked)?;
        }
        course.updated_at = Utc::now();
        state.courses.insert(course.id, course.clone());
        state.emit(
            "elearning.course.updated",
            json!({ "course_id": course.id.to_string(), "title": course.title }),
        );
        Ok(course)
    })();
    match result {
        Ok(course) => ok(course),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn delete_course(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ElearningState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = require_company_for_write(&_tenant, &req)?;
        let caller = caller(&req)?;
        let course = load_course(&state, &path.into_inner())?;
        require_record_writable_in_company_scope(&course.company_id, &_scope)?;
        require_course_write(&ctx, &caller, &course)?;
        if state.enrollments.iter().any(|entry| {
            entry.value().course_id == course.id
                && entry.value().status != ENROLL_CANCELLED
        }) {
            return Err(UnifiedError::conflict(
                "Course cannot be deleted while enrollments exist; archive it instead",
            ));
        }
        delete_course_rows(&state, course.id);
        state.emit(
            "elearning.course.deleted",
            json!({ "course_id": course.id.to_string(), "title": course.title }),
        );
        Ok(json!({ "deleted": true, "id": course.id.to_string() }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

/// Remove a course and everything attached to it: its modules
/// and lessons (with their quizzes, attempts, and progress
/// rows), reviews, path memberships, certificates, and
/// enrollments. Only called once the guard in `delete_course`
/// has confirmed every enrollment is cancelled, so no live
/// learner loses their record.
pub(crate) fn delete_course_rows(state: &ElearningState, course_id: Uuid) {
    let module_ids: Vec<Uuid> = state
        .modules
        .iter()
        .filter(|entry| entry.value().course_id == course_id)
        .map(|entry| *entry.key())
        .collect();
    for module_id in &module_ids {
        let lesson_ids: Vec<Uuid> = state
            .lessons
            .iter()
            .filter(|entry| entry.value().module_id == *module_id)
            .map(|entry| *entry.key())
            .collect();
        for lesson_id in lesson_ids {
            delete_lesson_rows(state, lesson_id);
        }
        state.modules.remove(module_id);
    }
    state.reviews.retain(|_, row| row.course_id != course_id);
    state.path_courses.retain(|_, row| row.course_id != course_id);
    state.certificates.retain(|_, row| row.course_id != course_id);
    state.enrollments.retain(|_, row| row.course_id != course_id);
    state.courses.remove(&course_id);
}

/// Remove a lesson and everything attached to it (its quiz,
/// questions, attempts, and progress rows).
pub(crate) fn delete_lesson_rows(state: &ElearningState, lesson_id: Uuid) {
    let quiz_ids: Vec<Uuid> = state
        .quizzes
        .iter()
        .filter(|entry| entry.value().lesson_id == lesson_id)
        .map(|entry| *entry.key())
        .collect();
    for quiz_id in quiz_ids {
        state.questions.retain(|_, row| row.quiz_id != quiz_id);
        state.attempts.retain(|_, row| row.quiz_id != quiz_id);
        state.quizzes.remove(&quiz_id);
    }
    state.progress.retain(|_, row| row.lesson_id != lesson_id);
    state.lessons.remove(&lesson_id);
}

pub(crate) async fn transition_course(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ElearningState>,
    req: HttpRequest,
    path: web::Path<String>,
    target: &str,
    event: &str,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Course> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = require_company_for_write(&_tenant, &req)?;
        let caller = caller(&req)?;
        let mut course = load_course(&state, &path.into_inner())?;
        require_record_writable_in_company_scope(&course.company_id, &_scope)?;
        require_course_write(&ctx, &caller, &course)?;
        if !valid_course_transition(&course.status, target) {
            return Err(UnifiedError::unprocessable(format!(
                "Cannot move course from {} to {target}",
                course.status
            )));
        }
        if target == STATUS_PUBLISHED {
            let has_lesson = state
                .lessons
                .iter()
                .any(|entry| entry.value().course_id == course.id);
            if !has_lesson {
                return Err(UnifiedError::unprocessable(
                    "Cannot publish a course with no lessons",
                ));
            }
        }
        course.status = target.to_string();
        course.updated_at = Utc::now();
        state.courses.insert(course.id, course.clone());
        state.emit(
            event,
            json!({ "course_id": course.id.to_string(), "status": course.status }),
        );
        Ok(course)
    })();
    match result {
        Ok(course) => ok(course),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn publish_course(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ElearningState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    transition_course(ctx, state, req, path, STATUS_PUBLISHED, "elearning.course.published").await
}

pub(crate) async fn archive_course(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ElearningState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    transition_course(ctx, state, req, path, STATUS_ARCHIVED, "elearning.course.archived").await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_course() -> Course {
        let now = Utc::now();
        Course {
            id: Uuid::now_v7(),
            title: "Course".to_string(),
            description: "Description".to_string(),
            category: "business".to_string(),
            level: "beginner".to_string(),
            language: "en".to_string(),
            thumbnail_ref: None,
            instructor_id: "instructor-1".to_string(),
            status: STATUS_DRAFT.to_string(),
            price_minor: 0,
            currency: "USD".to_string(),
            certificate_enabled: false,
            company_id: None,
            created_at: now,
            updated_at: now,
        }
    }

    #[test]
    fn delete_course_rows_removes_certificates_and_enrollments() {
        let state = ElearningState::default();
        let now = Utc::now();
        let course = test_course();
        let other = test_course();
        state.courses.insert(course.id, course.clone());
        state.courses.insert(other.id, other.clone());
        // A cancelled enrollment and a certificate of this course.
        let enrollment = Enrollment {
            id: Uuid::now_v7(),
            course_id: course.id,
            user_id: "learner-1".to_string(),
            status: ENROLL_CANCELLED.to_string(),
            price_paid_minor: 0,
            currency: "USD".to_string(),
            order_id: None,
            learner_name: None,
            enrolled_at: now,
            completed_at: None,
            created_at: now,
            updated_at: now,
        };
        state.enrollments.insert(enrollment.id, enrollment);
        let certificate = Certificate {
            id: Uuid::now_v7(),
            code: "ELRN-AAAA-BBBB-CCCC".to_string(),
            course_id: course.id,
            user_id: "learner-1".to_string(),
            learner_name: "Learner".to_string(),
            course_title: course.title.clone(),
            issuer_name: "Unified Academy".to_string(),
            issued_at: now,
            content_sha256: "fingerprint".to_string(),
            status: "valid".to_string(),
            anchor_status: "none".to_string(),
            anchor_tx: None,
            revoked_at: None,
            created_at: now,
            updated_at: now,
        };
        state.certificates.insert(certificate.id, certificate);
        // A review and a path membership of this course.
        state.reviews.insert(
            Uuid::now_v7(),
            Review {
                id: Uuid::now_v7(),
                course_id: course.id,
                user_id: "learner-1".to_string(),
                rating: 5,
                comment: None,
                created_at: now,
                updated_at: now,
            },
        );
        state.path_courses.insert(
            Uuid::now_v7(),
            LearningPathCourse {
                id: Uuid::now_v7(),
                path_id: Uuid::now_v7(),
                course_id: course.id,
                position: 0,
                created_at: now,
            },
        );
        // Rows of another course must survive the purge.
        let other_enrollment = Enrollment {
            id: Uuid::now_v7(),
            course_id: other.id,
            user_id: "learner-2".to_string(),
            status: ENROLL_CANCELLED.to_string(),
            price_paid_minor: 0,
            currency: "USD".to_string(),
            order_id: None,
            learner_name: None,
            enrolled_at: now,
            completed_at: None,
            created_at: now,
            updated_at: now,
        };
        state.enrollments.insert(other_enrollment.id, other_enrollment);

        delete_course_rows(&state, course.id);

        assert!(!state.courses.contains_key(&course.id));
        assert!(state.courses.contains_key(&other.id));
        assert!(state.enrollments.iter().all(|entry| entry.value().course_id != course.id));
        assert!(state.certificates.iter().all(|entry| entry.value().course_id != course.id));
        assert!(state.reviews.iter().all(|entry| entry.value().course_id != course.id));
        assert!(state.path_courses.iter().all(|entry| entry.value().course_id != course.id));
        assert_eq!(state.enrollments.len(), 1);
    }
}
