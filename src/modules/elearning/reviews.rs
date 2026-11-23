// Elearning — Review handlers.

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
// Review handlers
// ---------------------------------------------------------------------------

pub(crate) fn course_rating_summary(state: &ElearningState, course_id: Uuid) -> Value {
    let ratings: Vec<i64> = state
        .reviews
        .iter()
        .filter(|entry| entry.value().course_id == course_id)
        .map(|entry| entry.value().rating)
        .collect();
    let count = ratings.len() as i64;
    let average = if count == 0 {
        0.0
    } else {
        ratings.iter().sum::<i64>() as f64 / count as f64
    };
    json!({ "course_id": course_id.to_string(), "review_count": count, "average_rating": average })
}

pub(crate) async fn list_course_reviews(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ElearningState>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ELEARNING_VIEW)?;
        let course = load_course(&state, &path.into_inner())?;
        require_record_in_company_scope(&course.company_id, &_scope)?;
        let params = query.into_inner();
        let rating_filter = params
            .filters
            .get("rating")
            .and_then(|raw| raw.parse::<i64>().ok());
        let mut items: Vec<Review> = state
            .reviews
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|row| {
                if row.course_id != course.id {
                    return false;
                }
                if let Some(rating) = rating_filter {
                    if row.rating != rating {
                        return false;
                    }
                }
                true
            })
            .collect();
        items.sort_by_key(|row| row.created_at);
        let page = paginate(items, &params);
        let mut data = page.to_json();
        data["rating_summary"] = course_rating_summary(&state, course.id);
        Ok(data)
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

/// A review needs a real learning relationship: only an active
/// or completed enrollment may review. Learners whose enrollment
/// is still pending payment, or is suspended, have not (or not
/// any longer) taken the course.
pub(crate) fn enrollment_may_review(enrollment: &Enrollment) -> bool {
    enrollment.status == ENROLL_ACTIVE || enrollment.status == ENROLL_COMPLETED
}

pub(crate) async fn create_course_review(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ElearningState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<CreateReviewRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Review> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ELEARNING_VIEW)?;
        let course = load_course(&state, &path.into_inner())?;
        require_record_in_company_scope(&course.company_id, &_scope)?;
        // Reviews are for enrolled learners only, one per user.
        let Some(enrollment) = find_enrollment(&state, course.id, &caller.actor) else {
            return Err(UnifiedError::forbidden(
                "Only enrolled learners may review this course",
            ));
        };
        if !enrollment_may_review(&enrollment) {
            return Err(UnifiedError::forbidden(
                "Only active or completed learners may review this course",
            ));
        }
        if state.reviews.iter().any(|entry| {
            entry.value().course_id == course.id && entry.value().user_id == caller.actor
        }) {
            return Err(UnifiedError::conflict(
                "You already reviewed this course; edit your review instead",
            ));
        }
        let input = body.into_inner();
        if input.rating < 1 || input.rating > 5 {
            return Err(UnifiedError::bad_request("rating must be between 1 and 5"));
        }
        let comment = optional_text(&input.comment, "comment", MAX_COMMENT_LEN)?;
        let now = Utc::now();
        let row = Review {
            id: Uuid::now_v7(),
            course_id: course.id,
            user_id: caller.actor.clone(),
            rating: input.rating,
            comment,
            created_at: now,
            updated_at: now,
        };
        state.reviews.insert(row.id, row.clone());
        state.emit(
            "elearning.review.created",
            json!({
                "review_id": row.id.to_string(),
                "course_id": course.id.to_string(),
                "user_id": row.user_id,
                "rating": row.rating,
            }),
        );
        Ok(row)
    })();
    match result {
        Ok(row) => created(row),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn get_review(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ElearningState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Review> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ELEARNING_VIEW)?;
        let row = load_review(&state, &path.into_inner())?;
        require_course_by_id_in_scope(&state, row.course_id, &_scope)?;
        Ok(row)
    })();
    match result {
        Ok(row) => ok(row),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn update_review(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ElearningState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdateReviewRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Review> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ELEARNING_VIEW)?;
        let mut row = load_review(&state, &path.into_inner())?;
        require_course_by_id_in_scope(&state, row.course_id, &_scope)?;
        let is_manager = check_capability(&ctx, &caller.role, PERM_ELEARNING_MANAGE);
        if !is_manager && row.user_id != caller.actor {
            return Err(UnifiedError::forbidden(
                "Only the author or a manager may edit this review",
            ));
        }
        let input = body.into_inner();
        if let Some(rating) = input.rating {
            if rating < 1 || rating > 5 {
                return Err(UnifiedError::bad_request("rating must be between 1 and 5"));
            }
            row.rating = rating;
        }
        if let Some(comment) = &input.comment {
            row.comment = match comment {
                Some(raw) => Some(require_text(raw, "comment", MAX_COMMENT_LEN)?),
                None => None,
            };
        }
        row.updated_at = Utc::now();
        state.reviews.insert(row.id, row.clone());
        state.emit(
            "elearning.review.updated",
            json!({ "review_id": row.id.to_string(), "course_id": row.course_id.to_string() }),
        );
        Ok(row)
    })();
    match result {
        Ok(row) => ok(row),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn delete_review(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ElearningState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ELEARNING_VIEW)?;
        let row = load_review(&state, &path.into_inner())?;
        require_course_by_id_in_scope(&state, row.course_id, &_scope)?;
        let is_manager = check_capability(&ctx, &caller.role, PERM_ELEARNING_MANAGE);
        if !is_manager && row.user_id != caller.actor {
            return Err(UnifiedError::forbidden(
                "Only the author or a manager may delete this review",
            ));
        }
        state.reviews.remove(&row.id);
        state.emit(
            "elearning.review.deleted",
            json!({ "review_id": row.id.to_string(), "course_id": row.course_id.to_string() }),
        );
        Ok(json!({ "deleted": true, "id": row.id.to_string() }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_enrollment(status: &str) -> Enrollment {
        let now = Utc::now();
        Enrollment {
            id: Uuid::now_v7(),
            course_id: Uuid::now_v7(),
            user_id: "learner-1".to_string(),
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
    fn enrollment_may_review_only_for_active_or_completed() {
        assert!(enrollment_may_review(&test_enrollment(ENROLL_ACTIVE)));
        assert!(enrollment_may_review(&test_enrollment(ENROLL_COMPLETED)));
        assert!(!enrollment_may_review(&test_enrollment(ENROLL_PENDING_PAYMENT)));
        assert!(!enrollment_may_review(&test_enrollment(ENROLL_SUSPENDED)));
        assert!(!enrollment_may_review(&test_enrollment(ENROLL_CANCELLED)));
    }
}
