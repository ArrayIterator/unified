//! HTTP handlers: review cycles and reviews.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::{HookBus, HookContext, HookHandler};
use unified_core::json;
use unified_core::modules::{Module, ModuleRegistry, RouteInfo};
use unified_core::pagination::{PageResult, PaginationParams};
use actix_web::{web, HttpRequest, HttpResponse};
use chrono::{DateTime, NaiveDate, Utc};
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use uuid::Uuid;

use super::*;

// ---------------------------------------------------------------------------
// Handlers: performance reviews
// ---------------------------------------------------------------------------

pub(crate) async fn list_review_cycles(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<HrState>>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PageResult<ReviewCycle>> = (|| {
        let _tenant = require_tenant(&req)?;
        let _caller = caller(&req)?;
        let params = query.into_inner();
        let mut items: Vec<ReviewCycle> = state
            .review_cycles
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|cycle| {
                matches_search(&cycle.name, &params)
                    && params
                        .filters
                        .get("status")
                        .is_none_or(|status| cycle.status == *status)
            })
            .collect();
        items.sort_by(|a, b| b.period_start.cmp(&a.period_start));
        if params.normalized().order == "asc" {
            items.reverse();
        }
        Ok(paginate(items, &params))
    })();
    match result {
        Ok(page) => json::success_response(page, 200),
        Err(error) => json::error_response(&error, debug),
    }
}

pub(crate) async fn create_review_cycle(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<HrState>>,
    req: HttpRequest,
    body: web::Json<CreateReviewCycle>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<ReviewCycle> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_HR_MANAGE)?;
        let input = body.into_inner();
        let name = require_text(&input.name, "name", 255)?;
        let period_start = parse_date(&input.period_start, "period_start")?;
        let period_end = parse_date(&input.period_end, "period_end")?;
        if period_end < period_start {
            return Err(UnifiedError::bad_request(
                "period_end must not be before period_start",
            ));
        }
        let cycle = ReviewCycle {
            id: new_id(),
            name,
            period_start,
            period_end,
            status: "active".to_string(),
            created_at: now(),
            updated_at: now(),
        };
        state.review_cycles.insert(cycle.id.clone(), cycle.clone());
        Ok(cycle)
    })();
    match result {
        Ok(data) => json::success_response(data, 201),
        Err(error) => json::error_response(&error, debug),
    }
}

pub(crate) async fn get_review_cycle(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<HrState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<ReviewCycle> = (|| {
        let _tenant = require_tenant(&req)?;
        let _caller = caller(&req)?;
        state
            .review_cycles
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Review cycle not found"))
    })();
    json::respond(result, debug)
}

pub(crate) async fn close_review_cycle(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<HrState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<ReviewCycle> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_HR_MANAGE)?;
        let cycle_id = path.as_str().to_string();
        if state.reviews.iter().any(|entry| {
            let review = entry.value();
            review.cycle_id == cycle_id && review.status != "completed"
        }) {
            return Err(UnifiedError::conflict(
                "Review cycle still has incomplete reviews; complete them first",
            ));
        }
        let mut cycle = state
            .review_cycles
            .get_mut(cycle_id.as_str())
            .ok_or_else(|| UnifiedError::not_found("Review cycle not found"))?;
        if cycle.status == "closed" {
            return Err(UnifiedError::conflict("Review cycle is already closed"));
        }
        cycle.status = "closed".to_string();
        cycle.updated_at = now();
        Ok(cycle.clone())
    })();
    json::respond(result, debug)
}

pub(crate) async fn list_reviews(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<HrState>>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PageResult<Review>> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_HR_READ)?;
        let params = query.into_inner();
        let read_all = check_capability(&ctx, &caller.role, PERM_HR_READ_ALL);
        let own_employee_id = state
            .employee_by_user(&caller.actor)
            .map(|employee| employee.id);
        let mut items: Vec<Review> = state
            .reviews
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|review| {
                (read_all
                    || Some(review.employee_id.as_str()) == own_employee_id.as_deref()
                    || review.reviewer_id == caller.actor)
                    && params
                        .filters
                        .get("cycle_id")
                        .is_none_or(|cycle| review.cycle_id == *cycle)
                    && params
                        .filters
                        .get("employee_id")
                        .is_none_or(|employee| review.employee_id == *employee)
                    && params
                        .filters
                        .get("status")
                        .is_none_or(|status| review.status == *status)
            })
            .collect();
        items.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        if params.normalized().order == "asc" {
            items.reverse();
        }
        Ok(paginate(items, &params))
    })();
    match result {
        Ok(page) => json::success_response(page, 200),
        Err(error) => json::error_response(&error, debug),
    }
}

pub(crate) async fn create_review(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<HrState>>,
    req: HttpRequest,
    body: web::Json<CreateReview>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Review> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_HR_MANAGE)?;
        let input = body.into_inner();
        let cycle = state
            .review_cycles
            .get(input.cycle_id.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Review cycle not found"))?;
        if cycle.status != "active" {
            return Err(UnifiedError::conflict(format!(
                "Reviews can only be created in an active cycle (current status: {})",
                cycle.status
            )));
        }
        if !state.employees.contains_key(input.employee_id.as_str()) {
            return Err(UnifiedError::not_found("Employee not found"));
        }
        if let Some(rating) = input.rating {
            if !(1..=5).contains(&rating) {
                return Err(UnifiedError::bad_request("rating must be between 1 and 5"));
            }
        }
        if state.reviews.iter().any(|entry| {
            let review = entry.value();
            review.cycle_id == input.cycle_id && review.employee_id == input.employee_id
        }) {
            return Err(UnifiedError::conflict(
                "A review already exists for this employee in this cycle",
            ));
        }
        let goals = match input.goals {
            Some(goals) => {
                let mut clean: Vec<String> = Vec::new();
                for goal in goals {
                    clean.push(require_text(&goal, "goals entry", 500)?);
                }
                clean
            }
            None => Vec::new(),
        };
        let review = Review {
            id: new_id(),
            cycle_id: input.cycle_id,
            employee_id: input.employee_id,
            reviewer_id: require_text(&input.reviewer_id, "reviewer_id", 255)?,
            rating: input.rating,
            goals,
            reviewer_notes: optional_text(&input.reviewer_notes, "reviewer_notes", MAX_TEXT)?
                .unwrap_or_default(),
            status: "draft".to_string(),
            created_at: now(),
            updated_at: now(),
        };
        state.reviews.insert(review.id.clone(), review.clone());
        Ok(review)
    })();
    match result {
        Ok(data) => json::success_response(data, 201),
        Err(error) => json::error_response(&error, debug),
    }
}

pub(crate) async fn get_review(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<HrState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Review> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_HR_READ)?;
        let review = state
            .reviews
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Review not found"))?;
        let employee = state
            .employees
            .get(review.employee_id.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Employee not found"))?;
        if !can_read_employee(&ctx, &caller, &employee) && review.reviewer_id != caller.actor {
            return Err(UnifiedError::not_found("Review not found"));
        }
        Ok(review)
    })();
    json::respond(result, debug)
}

pub(crate) async fn update_review(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<HrState>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdateReview>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Review> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_HR_MANAGE)?;
        let input = body.into_inner();
        if let Some(rating) = input.rating {
            if !(1..=5).contains(&rating) {
                return Err(UnifiedError::bad_request("rating must be between 1 and 5"));
            }
        }
        if let Some(status) = &input.status {
            if status == "completed" {
                return Err(UnifiedError::bad_request(
                    "A review cannot be completed through update; use the complete endpoint",
                ));
            }
            if !matches!(status.as_str(), "draft" | "submitted") {
                return Err(UnifiedError::bad_request(
                    "status must be one of: draft, submitted",
                ));
            }
        }
        let goals = match input.goals {
            Some(goals) => {
                let mut clean: Vec<String> = Vec::new();
                for goal in goals {
                    clean.push(require_text(&goal, "goals entry", 500)?);
                }
                Some(clean)
            }
            None => None,
        };
        let reviewer_notes = match input.reviewer_notes {
            Some(note) => Some(require_text(&note, "reviewer_notes", MAX_TEXT)?),
            None => None,
        };
        let mut review = state
            .reviews
            .get_mut(path.as_str())
            .ok_or_else(|| UnifiedError::not_found("Review not found"))?;
        if review.status == "completed" {
            return Err(UnifiedError::conflict(
                "A completed review can no longer be changed",
            ));
        }
        if let Some(rating) = input.rating {
            review.rating = Some(rating);
        }
        if let Some(goals) = goals {
            review.goals = goals;
        }
        if let Some(reviewer_notes) = reviewer_notes {
            review.reviewer_notes = reviewer_notes;
        }
        if let Some(status) = input.status {
            if !review_status_transition_allowed(&review.status, &status) {
                return Err(UnifiedError::conflict(format!(
                    "A {} review cannot go back to {status}; reviews move draft -> submitted -> completed",
                    review.status
                )));
            }
            review.status = status;
        }
        review.updated_at = now();
        Ok(review.clone())
    })();
    json::respond(result, debug)
}

pub(crate) async fn complete_review(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<HrState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Review> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        let review_id = path.as_str().to_string();
        let review = state
            .reviews
            .get(review_id.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Review not found"))?;
        // The assigned reviewer may complete their own review without
        // holding an HR capability; anyone else needs `hr:manage`.
        // (An employee cannot complete their own review this way:
        // the reviewer is never the employee under review unless HR
        // assigned them as such, in which case the assignment stands.)
        if review.reviewer_id != caller.actor {
            require_permission(&ctx, &caller.role, PERM_HR_MANAGE)?;
        }
        if review.status == "completed" {
            return Err(UnifiedError::conflict("Review is already completed"));
        }
        if review.status != "submitted" {
            return Err(UnifiedError::conflict(format!(
                "Only a submitted review can be completed (current status: {})",
                review.status
            )));
        }
        if review.rating.is_none() {
            return Err(UnifiedError::bad_request(
                "A review needs a rating before it can be completed",
            ));
        }
        let mut review = state
            .reviews
            .get_mut(review_id.as_str())
            .ok_or_else(|| UnifiedError::not_found("Review not found"))?;
        review.status = "completed".to_string();
        review.updated_at = now();
        let completed = review.clone();
        drop(review);
        state.emit(
            "hr.review_completed",
            json!({
                "review_id": completed.id,
                "cycle_id": completed.cycle_id,
                "employee_id": completed.employee_id,
                "rating": completed.rating,
            }),
        );
        let employee_user_id = state
            .employees
            .get(completed.employee_id.as_str())
            .map(|entry| entry.value().user_id.clone());
        if let Some(user_id) = employee_user_id {
            state.emit(
                "notification.requested",
                json!({
                    "user_id": user_id,
                    "title": "Performance review completed",
                    "body": "Your performance review was completed.",
                    "kind": "hr",
                }),
            );
        }
        Ok(completed)
    })();
    json::respond(result, debug)
}

/// Whether an update may move a review from `from` to `to`.
/// Reviews move forward only — draft to submitted — and never
/// backwards. Completion is not an update at all: it goes through
/// the complete endpoint, which emits the completion event.
pub(crate) fn review_status_transition_allowed(from: &str, to: &str) -> bool {
    from == to || (from == "draft" && to == "submitted")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn review_moves_forward_only() {
        assert!(review_status_transition_allowed("draft", "submitted"));
        assert!(review_status_transition_allowed("draft", "draft"));
        assert!(review_status_transition_allowed("submitted", "submitted"));
        assert!(!review_status_transition_allowed("submitted", "draft"));
    }

    #[test]
    fn review_completion_is_not_an_update_transition() {
        assert!(!review_status_transition_allowed("draft", "completed"));
        assert!(!review_status_transition_allowed("submitted", "completed"));
    }
}
