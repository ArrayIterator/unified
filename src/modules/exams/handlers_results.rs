//! exams - handlers results.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::json;
use unified_core::pagination::{PageResult, PaginationParams};
use actix_web::{web, HttpRequest, HttpResponse};
use chrono::Utc;
use serde_json::{json, Value};
use std::sync::Arc;
use super::{constants::*, settings::*, types::*, module::*, helpers::*, inputs::*, handlers_banks::*, handlers_questions::*, handlers_exams::*, handlers_attempts::*, handlers_grading::*, handlers_ai::*, handlers_settings::*, hooks::*};



// ---------------------------------------------------------------------------
// Handlers: results and analytics
// ---------------------------------------------------------------------------
pub(crate) fn result_visible(
    _state: &ExamsState,
    ctx: &AppContext,
    caller: &Caller,
    result: &ExamResult,
) -> UnifiedResult<()> {
    let is_manager = check_capability(ctx, &caller.role, PERM_EXAMS_MANAGE);
    let is_grader = check_capability(ctx, &caller.role, PERM_EXAMS_GRADE);
    if is_manager || is_grader || result.user_id == caller.actor {
        return Ok(());
    }
    Err(UnifiedError::not_found("Result not found"))
}


pub(crate) async fn list_exam_results(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ExamsStateRef>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PageResult<ExamResult>> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        require_permission(&ctx, &caller.role, PERM_EXAMS_VIEW)?;
        let is_manager = check_capability(&ctx, &caller.role, PERM_EXAMS_MANAGE);
        let is_grader = check_capability(&ctx, &caller.role, PERM_EXAMS_GRADE);
        if !is_manager && !is_grader {
            return Err(UnifiedError::forbidden(
                "Only a manager or grader may list all results of an exam",
            ));
        }
        let exam = require_exam_by_id_in_scope(&state, path.as_str(), &_scope)?;
        let params = query.into_inner();
        let mut items: Vec<ExamResult> = state
            .results
            .iter()
            .filter(|entry| {
                let result = entry.value();
                result.exam_id == exam.id
                    && (params
                        .filters
                        .get("passed")
                        .map(|v| result.passed.to_string() == *v)
                        .unwrap_or(true))
                    && (params
                        .filters
                        .get("status")
                        .map(|v| &result.status == v)
                        .unwrap_or(true))
                    && (params
                        .filters
                        .get("user_id")
                        .map(|v| &result.user_id == v)
                        .unwrap_or(true))
            })
            .map(|entry| entry.value().clone())
            .collect();
        items.sort_by(|a, b| b.graded_at.cmp(&a.graded_at));
        if params.order.as_deref() == Some("asc") {
            items.reverse();
        }
        Ok(page_of(items, &params))
    })();
    json::respond(result, debug)
}


pub(crate) async fn get_result(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ExamsStateRef>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<ExamResult> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        require_permission(&ctx, &caller.role, PERM_EXAMS_VIEW)?;
        let result = require_result_in_scope(&state, path.as_str(), &_scope)?;
        result_visible(&state, &ctx, &caller, &result)?;
        Ok(result)
    })();
    json::respond(result, debug)
}


pub(crate) async fn list_my_results(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ExamsStateRef>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PageResult<ExamResult>> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        require_permission(&ctx, &caller.role, PERM_EXAMS_VIEW)?;
        let params = query.into_inner();
        let mut items: Vec<ExamResult> = state
            .results
            .iter()
            .filter(|entry| {
                let result = entry.value();
                exam_in_scope(&state, &result.exam_id, &_scope)
                    && result.user_id == caller.actor
                    && (params
                        .filters
                        .get("exam_id")
                        .map(|v| &result.exam_id == v)
                        .unwrap_or(true))
                    && (params
                        .filters
                        .get("status")
                        .map(|v| &result.status == v)
                        .unwrap_or(true))
                    && (params
                        .filters
                        .get("passed")
                        .map(|v| result.passed.to_string() == *v)
                        .unwrap_or(true))
            })
            .map(|entry| entry.value().clone())
            .collect();
        items.sort_by(|a, b| b.graded_at.cmp(&a.graded_at));
        if params.order.as_deref() == Some("asc") {
            items.reverse();
        }
        Ok(page_of(items, &params))
    })();
    json::respond(result, debug)
}


pub(crate) async fn publish_exam_results(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ExamsStateRef>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        require_permission(&ctx, &caller.role, PERM_EXAMS_MANAGE)?;
        let exam = require_exam_by_id_in_scope(&state, path.as_str(), &_scope)?;
        let mut published = 0usize;
        let mut published_ids: Vec<String> = Vec::new();
        for entry in state.results.iter() {
            let result = entry.value();
            if result.exam_id == exam.id && result.status == RESULT_DRAFT {
                if let Some(mut result) = state.results.get_mut(&result.id) {
                    result.status = RESULT_PUBLISHED.to_string();
                    result.published_at = Some(now());
                    result.updated_at = now();
                    published_ids.push(result.id.clone());
                    published += 1;
                }
            }
        }
        state.emit(
            EVT_RESULT_PUBLISHED,
            json!({
                "exam_id": exam.id,
                "published_count": published,
                "result_ids": published_ids,
                "actor": caller.actor,
            }),
        );
        for result_id in &published_ids {
            if let Some(result) = state.results.get(result_id) {
                state.notify(
                    "exams.result_published",
                    &result.user_id,
                    "Your exam result is published",
                    &format!(
                        "Your result for exam \"{}\" is now published.",
                        exam.title
                    ),
                    Some(format!("/api/v1/exams/results/{result_id}")),
                );
            }
        }
        Ok(json!({
            "exam_id": exam.id,
            "published_count": published,
            "result_ids": published_ids,
        }))
    })();
    json::respond(result, debug)
}


/// Why a certificate cannot be requested for this result, if any:
/// certificates are issued only for passed, published results. The
/// gate reads the result only - a paid exam's passed result is
/// eligible exactly like a free exam's, so pricing never blocks a
/// certificate.
pub(crate) fn certificate_block_reason(result: &ExamResult) -> Option<&'static str> {
    if !result.passed {
        return Some("Certificates are requested only for passed results");
    }
    if result.status != RESULT_PUBLISHED {
        return Some("Result must be published before a certificate is requested");
    }
    None
}


pub(crate) async fn request_result_certificate(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ExamsStateRef>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        require_permission(&ctx, &caller.role, PERM_EXAMS_VIEW)?;
        let result = require_result_in_scope(&state, path.as_str(), &_scope)?;
        result_visible(&state, &ctx, &caller, &result)?;
        if let Some(reason) = certificate_block_reason(&result) {
            return Err(UnifiedError::conflict(reason));
        }
        state.emit(
            EVT_CERTIFICATE_REQUESTED,
            json!({
                "attempt_id": result.attempt_id,
                "exam_id": result.exam_id,
                "user_id": result.user_id,
                "result_id": result.id,
                "score": result.score,
                "passed": result.passed,
                "module": "exams",
                "requested_by": caller.actor,
            }),
        );
        Ok(json!({
            "result_id": result.id,
            "certificate_requested": true,
        }))
    })();
    json::respond(result, debug)
}


pub(crate) async fn get_exam_analytics(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ExamsStateRef>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        require_permission(&ctx, &caller.role, PERM_EXAMS_VIEW)?;
        let is_manager = check_capability(&ctx, &caller.role, PERM_EXAMS_MANAGE);
        let is_grader = check_capability(&ctx, &caller.role, PERM_EXAMS_GRADE);
        if !is_manager && !is_grader {
            return Err(UnifiedError::forbidden(
                "Only a manager or grader may read exam analytics",
            ));
        }
        let exam = require_exam_by_id_in_scope(&state, path.as_str(), &_scope)?;
        let settings = state.settings();
        let results: Vec<ExamResult> = state
            .results
            .iter()
            .filter(|entry| entry.value().exam_id == exam.id)
            .map(|entry| entry.value().clone())
            .collect();
        let total = results.len();
        let passed_count = results.iter().filter(|r| r.passed).count();
        let pass_rate = if total > 0 {
            passed_count as f64 / total as f64 * 100.0
        } else {
            0.0
        };
        let mean = if total > 0 {
            results.iter().map(|r| r.percentage).sum::<f64>() / total as f64
        } else {
            0.0
        };
        let mut sorted: Vec<f64> = results.iter().map(|r| r.percentage).collect();
        sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        let median = if sorted.is_empty() {
            0.0
        } else if sorted.len() % 2 == 1 {
            sorted[sorted.len() / 2]
        } else {
            (sorted[sorted.len() / 2 - 1] + sorted[sorted.len() / 2]) / 2.0
        };
        let buckets = settings.distribution_buckets.max(1) as usize;
        let mut distribution: Vec<u64> = vec![0; buckets];
        for result in &results {
            let index = ((result.percentage / 100.0) * buckets as f64)
                .floor()
                .min((buckets - 1) as f64) as usize;
            distribution[index] += 1;
        }
        let stats = question_statistics(&state, &exam.id);
        let attempts_total = state
            .attempts
            .iter()
            .filter(|entry| entry.value().exam_id == exam.id)
            .count();
        let signals_total = state
            .proctor_signals
            .iter()
            .filter(|entry| entry.value().exam_id == exam.id)
            .count();
        Ok(json!({
            "exam_id": exam.id,
            "attempts_total": attempts_total,
            "results_total": total,
            "passed_count": passed_count,
            "pass_rate": pass_rate,
            "mean_percentage": mean,
            "median_percentage": median,
            "score_distribution": {
                "buckets": buckets,
                "counts": distribution,
            },
            "proctor_signals_total": signals_total,
            "question_statistics": stats,
        }))
    })();
    json::respond(result, debug)
}


pub(crate) async fn get_question_stats(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ExamsStateRef>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        require_permission(&ctx, &caller.role, PERM_EXAMS_VIEW)?;
        let is_manager = check_capability(&ctx, &caller.role, PERM_EXAMS_MANAGE);
        let is_grader = check_capability(&ctx, &caller.role, PERM_EXAMS_GRADE);
        if !is_manager && !is_grader {
            return Err(UnifiedError::forbidden(
                "Only a manager or grader may read question statistics",
            ));
        }
        let exam = require_exam_by_id_in_scope(&state, path.as_str(), &_scope)?;
        Ok(question_statistics(&state, &exam.id))
    })();
    json::respond(result, debug)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_result(exam_id: &str, passed: bool, status: &str) -> ExamResult {
        ExamResult {
            id: "result-1".to_string(),
            attempt_id: "attempt-1".to_string(),
            exam_id: exam_id.to_string(),
            user_id: "user-1".to_string(),
            score: 80.0,
            max_score: 100.0,
            percentage: 80.0,
            passed,
            status: status.to_string(),
            per_question: serde_json::json!({}),
            graded_at: Utc::now(),
            published_at: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }

    #[test]
    fn certificate_eligible_for_paid_exam_passed_published_result() {
        // The gate reads the result only: a paid exam's passed,
        // published result is eligible exactly like a free exam's,
        // so pricing never blocks a certificate.
        let paid = test_result("exam-paid", true, RESULT_PUBLISHED);
        assert_eq!(certificate_block_reason(&paid), None);
        let free = test_result("exam-free", true, RESULT_PUBLISHED);
        assert_eq!(certificate_block_reason(&free), None);
    }

    #[test]
    fn certificate_blocked_for_failed_result() {
        let result = test_result("exam-paid", false, RESULT_PUBLISHED);
        assert_eq!(
            certificate_block_reason(&result),
            Some("Certificates are requested only for passed results")
        );
    }

    #[test]
    fn certificate_blocked_for_unpublished_result() {
        let result = test_result("exam-paid", true, RESULT_DRAFT);
        assert_eq!(
            certificate_block_reason(&result),
            Some("Result must be published before a certificate is requested")
        );
    }
}
