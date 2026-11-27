//! exams - helpers.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::pagination::{PageResult, PaginationParams};
use unified_core::tenancy;
use actix_web::HttpRequest;
use chrono::{DateTime, Utc};
use serde::Serialize;
use serde_json::{json, Value};
use std::collections::HashMap;
use uuid::Uuid;
use super::{constants::*, settings::*, types::*, module::*, inputs::*, handlers_banks::*, handlers_questions::*, handlers_exams::*, handlers_attempts::*, handlers_grading::*, handlers_results::*, handlers_ai::*, handlers_settings::*, hooks::*};



// Helpers
/// Caller identity: the server-verified caller (user id plus
/// server-resolved role, both required for capability checks),
/// exactly like the e-learning module.
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


/// Resolve the tenant for this request, failing closed when the
/// tenant-resolution middleware attached no tenant context (401)
/// or the resolved tenant id is not a uuidv7 UUID (403). Every
/// tenant-data handler in this module calls this FIRST, before
/// caller identity or any capability check, so an unresolved
/// request never reaches a store or an emit. There is no fallback
/// to the `public` schema and no default tenant. See
/// `unified_core::tenancy` (`TENANT_SCOPE_CONTRACT`).
///
/// Diesel persistence is not wired yet (the DashMap stores are
/// authoritative; migration `2026-11-22-000090_exams` is the
/// persistence target), so there is no `DbPool` in this module
/// and `with_tenant_connection` has no call site here yet - when
/// the Diesel writers land, all data access goes through
/// `tenancy::with_tenant_connection(pool, tenant.tenant_id, ...)`.
/// Until then the guard rejects unresolved tenants, but the
/// in-memory stores are not yet partitioned by tenant: records
/// carry no `tenant_id` yet, so per-tenant isolation of the maps
/// themselves is still OPEN for the integrator (see the module
/// header convergence note).
pub(crate) fn require_tenant(req: &HttpRequest) -> UnifiedResult<tenancy::TenantContext> {
    tenancy::TenantContext::from_http_request(req)
}

// Tenant edge guard (C3 pilot 5, group B).

/// The company scope the caller acts in: the tenant context's
/// company first, the verified caller's server-resolved company
/// otherwise. Fails closed (403) when both are present and disagree
/// - a request must not carry one company in its tenant context and
/// another in its verified identity. The `X-Company-Id` header is
/// NEVER trusted here: only the verified caller (company resolved
/// from the database at authentication time) and the
/// middleware-resolved tenant context are sources (header trust
/// removal is handled elsewhere).
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
        (None, verified) => Ok(verified.clone()),
    }
}

/// Resolve the caller's company for a WRITE (C3, strict by
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
/// `CallerScope::Unresolved` rule); omitting the company is refused (C3):
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

/// True when a stored record belongs to the caller's company
/// scope. With no caller scope (an unresolved caller) only
/// global records match: unscoped records are visible, every
/// company-owned record is denied (the shared
/// `CallerScope::Unresolved` rule). With a scope, only records
/// carrying that company match, plus unscoped (global/legacy) records,
/// which are shared content visible to any resolved scope (C3, rule A). Write strictness here is
/// mismatch-only (the pilot-3 elearning model): updates carry no
/// company field in this module, so there is no company change to
/// verify, and an unresolved caller may still write a global row
/// - requiring a resolved company on every write is a task-315
/// decision, not a C3 gate decision.
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
/// company scope FOR A WRITE (C3, strict by default).
/// Unlike the read check above (rule A), a write requires an exact
/// company match: a company caller may write only their own
/// company's records - global (unscoped) records are platform-only
/// - and an unresolved caller may write nothing. Mutation handlers
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


/// True when the named question bank is in the caller scope. A
/// missing bank is out of scope (fail-closed), so list filters
/// never surface rows whose parent bank is gone.
pub(crate) fn bank_in_scope(
    state: &ExamsState,
    bank_id: &str,
    scope: &Option<String>,
) -> bool {
    state
        .banks
        .get(bank_id)
        .is_some_and(|entry| in_company_scope(&entry.value().company_id, scope))
}

/// Load a question bank by id and fail closed (403) when it is
/// outside the caller scope. Sub-record handlers (questions,
/// imports, exports) scope through their parent bank with this.
pub(crate) fn require_bank_by_id_in_scope(
    state: &ExamsState,
    bank_id: &str,
    scope: &Option<String>,
) -> UnifiedResult<QuestionBank> {
    let bank = state
        .banks
        .get(bank_id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found("Question bank not found"))?;
    require_record_in_company_scope(&bank.company_id, scope)?;
    Ok(bank)
}

/// True when the named exam is in the caller scope. A missing
/// exam is out of scope (fail-closed).
pub(crate) fn exam_in_scope(
    state: &ExamsState,
    exam_id: &str,
    scope: &Option<String>,
) -> bool {
    state
        .exams
        .get(exam_id)
        .is_some_and(|entry| in_company_scope(&entry.value().company_id, scope))
}

/// Load an exam by id and fail closed (403) when it is outside
/// the caller scope. Sub-record handlers (exam questions,
/// attempts, rubrics, grading, results, analytics) scope through
/// their parent exam with this.
pub(crate) fn require_exam_by_id_in_scope(
    state: &ExamsState,
    exam_id: &str,
    scope: &Option<String>,
) -> UnifiedResult<Exam> {
    let exam = state
        .exams
        .get(exam_id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found("Exam not found"))?;
    require_record_in_company_scope(&exam.company_id, scope)?;
    Ok(exam)
}

/// Load an exam by id for a WRITE and fail closed (403) when it is
/// outside the caller scope (C3, strict by default).
/// Unlike `require_exam_by_id_in_scope` (rule A reads), a write
/// requires an exact company match: a global exam is platform-only.
/// Mutation-only loaders call this; read handlers keep
/// `require_exam_by_id_in_scope`.
pub(crate) fn require_exam_writable_by_id_in_scope(
    state: &ExamsState,
    exam_id: &str,
    scope: &Option<String>,
) -> UnifiedResult<Exam> {
    let exam = state
        .exams
        .get(exam_id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found("Exam not found"))?;
    require_record_writable_in_company_scope(&exam.company_id, scope)?;
    Ok(exam)
}


/// Load a question by id and fail closed (403) when its parent
/// bank is outside the caller scope.
pub(crate) fn require_question_in_scope(
    state: &ExamsState,
    question_id: &str,
    scope: &Option<String>,
) -> UnifiedResult<Question> {
    let question = state
        .questions
        .get(question_id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found("Question not found"))?;
    require_bank_by_id_in_scope(state, &question.bank_id, scope)?;
    Ok(question)
}

/// Load an attempt by id and fail closed (403) when its parent
/// exam is outside the caller scope. Ownership (the attempt
/// belongs to the caller, or the caller manages/grades) is still
/// checked by the handler after this.
pub(crate) fn require_attempt_in_scope(
    state: &ExamsState,
    attempt_id: &str,
    scope: &Option<String>,
) -> UnifiedResult<ExamAttempt> {
    let attempt = state
        .attempts
        .get(attempt_id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found("Attempt not found"))?;
    require_exam_by_id_in_scope(state, &attempt.exam_id, scope)?;
    Ok(attempt)
}

/// Load an attempt by id for a WRITE and fail closed (403) when its
/// parent exam is outside the caller scope (C3, strict by
/// default: the parent exam must match the caller's company exactly
/// - an attempt under a global exam is platform-only). Mutation-only;
/// read handlers keep `require_attempt_in_scope`.
pub(crate) fn require_attempt_writable_in_scope(
    state: &ExamsState,
    attempt_id: &str,
    scope: &Option<String>,
) -> UnifiedResult<ExamAttempt> {
    let attempt = state
        .attempts
        .get(attempt_id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found("Attempt not found"))?;
    require_exam_writable_by_id_in_scope(state, &attempt.exam_id, scope)?;
    Ok(attempt)
}


/// Load a rubric by id and fail closed (403) when its parent
/// exam is outside the caller scope.
pub(crate) fn require_rubric_in_scope(
    state: &ExamsState,
    rubric_id: &str,
    scope: &Option<String>,
) -> UnifiedResult<Rubric> {
    let rubric = state
        .rubrics
        .get(rubric_id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found("Rubric not found"))?;
    require_exam_by_id_in_scope(state, &rubric.exam_id, scope)?;
    Ok(rubric)
}

/// Load a manual grade by id for a WRITE and fail closed (403) when
/// its parent attempt's exam is outside the caller scope (C3 task
/// 315, strict by default: the parent exam must match the caller's
/// company exactly - a grade under a global exam is platform-only).
/// Mutation-only.
pub(crate) fn require_grade_in_scope(
    state: &ExamsState,
    grade_id: &str,
    scope: &Option<String>,
) -> UnifiedResult<ExamGrade> {
    let grade = state
        .grades
        .get(grade_id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found("Grade not found"))?;
    require_attempt_writable_in_scope(state, &grade.attempt_id, scope)?;
    Ok(grade)
}

/// Load a result by id and fail closed (403) when its parent
/// exam is outside the caller scope. Ownership (the result
/// belongs to the caller, or the caller manages/grades) is still
/// checked by the handler after this.
pub(crate) fn require_result_in_scope(
    state: &ExamsState,
    result_id: &str,
    scope: &Option<String>,
) -> UnifiedResult<ExamResult> {
    let result = state
        .results
        .get(result_id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found("Result not found"))?;
    require_exam_by_id_in_scope(state, &result.exam_id, scope)?;
    Ok(result)
}


/// Ask the roles module whether `role` holds `permission` through
/// the `role.check` hook. Fails closed: no answer means not allowed.
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


pub(crate) fn new_id() -> String {
    Uuid::new_v4().to_string()
}


pub(crate) fn now() -> DateTime<Utc> {
    Utc::now()
}


pub(crate) fn require_text(value: &str, field: &str, max: usize) -> UnifiedResult<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err(UnifiedError::bad_request(format!("{field} must not be empty")));
    }
    if trimmed.len() > max {
        return Err(UnifiedError::bad_request(format!(
            "{field} must be at most {max} characters"
        )));
    }
    Ok(trimmed.to_string())
}


pub(crate) fn optional_text(value: &Option<String>, field: &str, max: usize) -> UnifiedResult<Option<String>> {
    match value {
        Some(raw) => Ok(Some(require_text(raw, field, max)?)),
        None => Ok(None),
    }
}


pub(crate) fn require_marks(value: f64, field: &str) -> UnifiedResult<f64> {
    if !value.is_finite() || value < 0.0 || value > 1000.0 {
        return Err(UnifiedError::bad_request(format!(
            "{field} must be a number between 0 and 1000"
        )));
    }
    Ok(value)
}


pub(crate) fn require_percentage(value: f64, field: &str) -> UnifiedResult<f64> {
    if !value.is_finite() || value < 0.0 || value > 100.0 {
        return Err(UnifiedError::bad_request(format!(
            "{field} must be a number between 0 and 100"
        )));
    }
    Ok(value)
}


/// Validate a paid-exam price pair: the price is an integer minor
/// amount and must never be negative; a paid exam must carry a price
/// above zero, and a priced exam must be marked paid, so the two
/// fields can never disagree about whether the exam costs money.
pub(crate) fn validate_pricing(price_minor: i64, is_paid: bool) -> UnifiedResult<()> {
    if price_minor < 0 {
        return Err(UnifiedError::bad_request(
            "price_minor must not be negative",
        ));
    }
    if is_paid && price_minor == 0 {
        return Err(UnifiedError::bad_request(
            "A paid exam must have a price_minor above zero",
        ));
    }
    if !is_paid && price_minor > 0 {
        return Err(UnifiedError::bad_request(
            "An exam with a price_minor above zero must be marked paid (is_paid)",
        ));
    }
    Ok(())
}


/// Read a top-level string field from a hook payload defensively:
/// missing, null, or non-string values read as `None` instead of
/// failing the handler (same convention as the e-learning module).
pub(crate) fn opt_string(data: &Value, key: &str) -> Option<String> {
    data.get(key)
        .and_then(|value| value.as_str())
        .map(str::to_string)
}


/// Validate a value against a settings list. The list is the live
/// admin-configured one - never a constant in this file.
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


/// Validate a list of strings against a settings list.
pub(crate) fn require_list(values: &[String], field: &str, allowed: &[String]) -> UnifiedResult<Vec<String>> {
    let mut out = Vec::with_capacity(values.len());
    for value in values {
        out.push(require_in_list(value, field, allowed)?);
    }
    Ok(out)
}


/// Paginate an already sorted vector into a `PageResult`.
pub(crate) fn page_of<T: Serialize + Clone>(
    items: Vec<T>,
    params: &PaginationParams,
) -> PageResult<T> {
    let pagination = params.normalized();
    let total = items.len() as u64;
    let start = ((pagination.page - 1) * pagination.per_page) as usize;
    let end = (start + pagination.per_page as usize).min(items.len());
    let page_items = if start >= items.len() {
        Vec::new()
    } else {
        items[start..end].to_vec()
    };
    PageResult::new(page_items, total, &pagination)
}


pub(crate) fn matches_search(haystack: &str, search: &Option<String>) -> bool {
    match search {
        Some(term) => haystack.to_lowercase().contains(&term.to_lowercase()),
        None => true,
    }
}


/// Validate question options and the correct answer for a question
/// type. The allowed question types come from the live settings.
pub(crate) fn validate_question_payload(
    settings: &ExamsSettings,
    question_type: &str,
    options: &[QuestionOptionInput],
    correct_answer: &Value,
) -> UnifiedResult<Vec<QuestionOption>> {
    if !settings.question_types.iter().any(|t| t == question_type) {
        return Err(UnifiedError::bad_request(format!(
            "question_type \"{question_type}\" is not allowed; allowed values: {}",
            settings.question_types.join(", ")
        )));
    }
    if options.len() > settings.max_options_per_question {
        return Err(UnifiedError::bad_request(format!(
            "A question may have at most {} options",
            settings.max_options_per_question
        )));
    }
    let mut seen_texts: Vec<String> = Vec::new();
    let mut out: Vec<QuestionOption> = Vec::with_capacity(options.len());
    for (index, option) in options.iter().enumerate() {
        let text = require_text(&option.text, "option text", MAX_TEXT_LEN)?;
        if seen_texts.iter().any(|t| t == &text) {
            return Err(UnifiedError::bad_request(
                "Question options must not repeat the same text",
            ));
        }
        seen_texts.push(text.clone());
        out.push(QuestionOption {
            id: option.id.clone().unwrap_or_else(new_id),
            text,
            position: option.position.unwrap_or(index as u32),
        });
    }
    match question_type {
        "multiple_choice_single" => {
            if out.len() < 2 {
                return Err(UnifiedError::bad_request(
                    "A single-answer multiple-choice question needs at least 2 options",
                ));
            }
            let selected = correct_answer
                .as_str()
                .ok_or_else(|| UnifiedError::bad_request(
                    "correct_answer must be the id of the correct option",
                ))?;
            if !out.iter().any(|o| o.id == selected) {
                return Err(UnifiedError::bad_request(
                    "correct_answer must be the id of one of the options",
                ));
            }
        }
        "multiple_choice_multiple" => {
            if out.len() < 2 {
                return Err(UnifiedError::bad_request(
                    "A multiple-answer multiple-choice question needs at least 2 options",
                ));
            }
            let selected = correct_answer
                .as_array()
                .ok_or_else(|| UnifiedError::bad_request(
                    "correct_answer must be a list of correct option ids",
                ))?;
            if selected.is_empty() {
                return Err(UnifiedError::bad_request(
                    "correct_answer must list at least one correct option",
                ));
            }
            for entry in selected {
                let id = entry.as_str().ok_or_else(|| UnifiedError::bad_request(
                    "correct_answer entries must be option ids",
                ))?;
                if !out.iter().any(|o| o.id == id) {
                    return Err(UnifiedError::bad_request(
                        "correct_answer must only list ids of the options",
                    ));
                }
            }
        }
        "true_false" => {
            if !correct_answer.is_boolean() {
                return Err(UnifiedError::bad_request(
                    "correct_answer must be true or false for a true/false question",
                ));
            }
        }
        "short_answer" => {
            let accepted = match correct_answer {
                Value::String(text) => vec![text.clone()],
                Value::Array(items) => {
                    let mut texts = Vec::new();
                    for item in items {
                        texts.push(item.as_str().ok_or_else(|| UnifiedError::bad_request(
                            "correct_answer entries must be answer texts",
                        ))?.to_string());
                    }
                    texts
                }
                _ => {
                    return Err(UnifiedError::bad_request(
                        "correct_answer must be the accepted answer text (or a list of accepted texts)",
                    ));
                }
            };
            if accepted.iter().any(|text| text.trim().is_empty()) {
                return Err(UnifiedError::bad_request(
                    "correct_answer must not contain an empty answer",
                ));
            }
        }
        "essay" => {
            if !correct_answer.is_null() {
                return Err(UnifiedError::bad_request(
                    "correct_answer must be null for an essay question; essays are graded by a human",
                ));
            }
        }
        _ => {
            return Err(UnifiedError::bad_request(format!(
                "Unsupported question_type \"{question_type}\""
            )));
        }
    }
    Ok(out)
}


/// Score one objective answer against the pinned question. Returns
/// the marks awarded (0 when wrong or unanswered). Essays return
/// `None`: they need a human grader.
pub(crate) fn auto_score(
    question: &Question,
    answer: &Value,
    case_insensitive: bool,
) -> Option<f64> {
    match question.question_type.as_str() {
        "multiple_choice_single" => {
            let selected = answer.as_str()?;
            let correct = question.correct_answer.as_str()?;
            if selected == correct {
                Some(question.marks)
            } else {
                Some(0.0)
            }
        }
        "multiple_choice_multiple" => {
            let selected = answer.as_array()?;
            let correct = question.correct_answer.as_array()?;
            let mut selected_ids: Vec<&str> = selected
                .iter()
                .filter_map(|v| v.as_str())
                .collect();
            selected_ids.sort_unstable();
            selected_ids.dedup();
            let mut correct_ids: Vec<&str> = correct
                .iter()
                .filter_map(|v| v.as_str())
                .collect();
            correct_ids.sort_unstable();
            correct_ids.dedup();
            if selected_ids == correct_ids {
                Some(question.marks)
            } else {
                Some(0.0)
            }
        }
        "true_false" => {
            let selected = answer.as_bool()?;
            let correct = question.correct_answer.as_bool()?;
            if selected == correct {
                Some(question.marks)
            } else {
                Some(0.0)
            }
        }
        "short_answer" => {
            let given = answer.as_str()?.trim();
            if given.is_empty() {
                return Some(0.0);
            }
            let accepted: Vec<String> = match &question.correct_answer {
                Value::String(text) => vec![text.clone()],
                Value::Array(items) => items
                    .iter()
                    .filter_map(|v| v.as_str().map(str::to_string))
                    .collect(),
                _ => return Some(0.0),
            };
            let matches = accepted.iter().any(|text| {
                if case_insensitive {
                    text.trim().eq_ignore_ascii_case(given)
                } else {
                    text.trim() == given
                }
            });
            if matches {
                Some(question.marks)
            } else {
                Some(0.0)
            }
        }
        _ => None,
    }
}


/// Does this exam already have attempts? Used to decide whether
/// editing a question must create a new version.
pub(crate) fn question_has_attempts(state: &ExamsState, question_id: &str) -> bool {
    state.attempts.iter().any(|entry| {
        let attempt = entry.value();
        attempt.question_order.iter().any(|id| id == question_id)
    })
}


/// Snapshot a question for its version history.
pub(crate) fn snapshot_question(question: &Question) -> Value {
    json!({
        "id": question.id,
        "bank_id": question.bank_id,
        "version": question.version,
        "question_type": question.question_type,
        "category": question.category,
        "difficulty": question.difficulty,
        "text": question.text,
        "options": question.options,
        "correct_answer": question.correct_answer,
        "answer_key": question.answer_key,
        "marks": question.marks,
        "status": question.status,
    })
}


pub(crate) fn exam_total_marks(state: &ExamsState, exam_id: &str) -> f64 {
    state
        .exam_questions
        .iter()
        .filter(|entry| entry.value().exam_id == exam_id)
        .map(|entry| entry.value().marks)
        .sum()
}


pub(crate) fn recompute_exam_total(state: &ExamsState, exam_id: &str) {
    if let Some(mut exam) = state.exams.get_mut(exam_id) {
        exam.total_marks = exam_total_marks(state, exam_id);
        exam.updated_at = now();
    }
}


/// Expire an in-progress attempt whose server deadline has passed.
/// Returns true when the attempt was expired by this call.
pub(crate) fn expire_if_due(state: &ExamsState, attempt_id: &str) -> bool {
    let due = state
        .attempts
        .get(attempt_id)
        .map(|entry| {
            entry.value().status == ATTEMPT_IN_PROGRESS && entry.value().expires_at <= Utc::now()
        })
        .unwrap_or(false);
    if !due {
        return false;
    }
    if let Some(mut attempt) = state.attempts.get_mut(attempt_id) {
        attempt.status = ATTEMPT_EXPIRED.to_string();
        attempt.submitted_at = Some(now());
        let exam_id = attempt.exam_id.clone();
        let user_id = attempt.user_id.clone();
        drop(attempt);
        state.emit(
            EVT_ATTEMPT_EXPIRED,
            json!({
                "attempt_id": attempt_id,
                "exam_id": exam_id,
                "user_id": user_id,
                "expired_by": "server",
            }),
        );
        return true;
    }
    false
}


/// Auto-grade the objective answers of a submitted attempt, create
/// pending manual grades for essay answers, and finish the attempt
/// when nothing needs a human grader. Returns the attempt's current
/// objective score and whether manual grading is still pending.
pub(crate) fn auto_grade_attempt(state: &ExamsState, attempt: &mut ExamAttempt) -> (f64, bool) {
    let settings = state.settings();
    let mut objective_score = 0.0;
    let mut needs_manual = false;
    let mut per_question = serde_json::Map::new();
    let exam_questions: Vec<ExamQuestion> = state
        .exam_questions
        .iter()
        .filter(|entry| entry.value().exam_id == attempt.exam_id)
        .map(|entry| entry.value().clone())
        .collect();
    for link in &exam_questions {
        let question = match state.questions.get(&link.question_id) {
            Some(entry) => entry.value().clone(),
            None => continue,
        };
        let answer = attempt
            .answers
            .get(&link.question_id)
            .cloned()
            .unwrap_or(Value::Null);
        match auto_score(&question, &answer, settings.short_answer_case_insensitive) {
            Some(awarded) => {
                objective_score += awarded;
                per_question.insert(
                    link.question_id.clone(),
                    json!({
                        "awarded": awarded,
                        "max": link.marks,
                        "correct": awarded >= link.marks,
                        "graded_by": "auto",
                    }),
                );
            }
            None => {
                needs_manual = true;
                per_question.insert(
                    link.question_id.clone(),
                    json!({
                        "awarded": null,
                        "max": link.marks,
                        "correct": null,
                        "graded_by": "manual",
                    }),
                );
                let already = state.grades.iter().any(|entry| {
                    entry.value().attempt_id == attempt.id
                        && entry.value().question_id == link.question_id
                });
                if !already {
                    let grade = ExamGrade {
                        id: new_id(),
                        attempt_id: attempt.id.clone(),
                        exam_id: attempt.exam_id.clone(),
                        question_id: link.question_id.clone(),
                        grader_id: None,
                        rubric_id: None,
                        criterion_scores: HashMap::new(),
                        marks_awarded: None,
                        max_marks: link.marks,
                        status: GRADE_PENDING.to_string(),
                        comment: None,
                        second_grader_id: None,
                        audit: vec![AuditEntry {
                            at: now(),
                            actor: "system".to_string(),
                            action: "manual_grade_created".to_string(),
                            detail: "Essay answer queued for manual grading".to_string(),
                        }],
                        created_at: now(),
                        updated_at: now(),
                    };
                    state.grades.insert(grade.id.clone(), grade);
                }
            }
        }
    }
    attempt.score = Some(objective_score);
    (objective_score, needs_manual)
}


/// Finish grading for an attempt: build the result, decide pass or
/// fail, emit the grading, certificate, and academic events, and
/// notify the student. Called once all manual grades are in.
pub(crate) fn finalize_grading(state: &ExamsState, attempt_id: &str) -> UnifiedResult<ExamResult> {
    let mut attempt = state
        .attempts
        .get(attempt_id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found("Attempt not found"))?;
    let exam = state
        .exams
        .get(&attempt.exam_id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found("Exam not found"))?;
    let mut total = 0.0;
    let mut per_question = serde_json::Map::new();
    let exam_questions: Vec<ExamQuestion> = state
        .exam_questions
        .iter()
        .filter(|entry| entry.value().exam_id == attempt.exam_id)
        .map(|entry| entry.value().clone())
        .collect();
    for link in &exam_questions {
        let question = match state.questions.get(&link.question_id) {
            Some(entry) => entry.value().clone(),
            None => continue,
        };
        let answer = attempt
            .answers
            .get(&link.question_id)
            .cloned()
            .unwrap_or(Value::Null);
        if let Some(awarded) =
            auto_score(&question, &answer, state.settings().short_answer_case_insensitive)
        {
            // Objective scores are recomputed from the stored answers
            // every time, so regrades and second grades never lose
            // them when the attempt score is cleared on reopen.
            total += awarded;
            per_question.insert(
                link.question_id.clone(),
                json!({
                    "awarded": awarded,
                    "max": link.marks,
                    "correct": awarded >= link.marks,
                    "graded_by": "auto",
                }),
            );
        }
    }
    for entry in state.grades.iter() {
        let grade = entry.value();
        if grade.attempt_id != attempt.id {
            continue;
        }
        let awarded = grade.marks_awarded.unwrap_or(0.0);
        if grade.status == GRADE_GRADED {
            total += awarded;
            per_question.insert(
                grade.question_id.clone(),
                json!({
                    "awarded": awarded,
                    "max": grade.max_marks,
                    "correct": awarded >= grade.max_marks,
                    "graded_by": grade.grader_id.clone(),
                }),
            );
        }
    }
    let max_score = if attempt.max_score > 0.0 {
        attempt.max_score
    } else {
        exam.total_marks
    };
    let percentage = if max_score > 0.0 {
        total / max_score * 100.0
    } else {
        0.0
    };
    let passed = percentage >= exam.pass_mark;
    let result = ExamResult {
        id: new_id(),
        attempt_id: attempt.id.clone(),
        exam_id: attempt.exam_id.clone(),
        user_id: attempt.user_id.clone(),
        score: total,
        max_score,
        percentage,
        passed,
        status: RESULT_DRAFT.to_string(),
        per_question: Value::Object(per_question),
        graded_at: now(),
        published_at: None,
        created_at: now(),
        updated_at: now(),
    };
    attempt.score = Some(total);
    attempt.passed = Some(passed);
    attempt.status = ATTEMPT_GRADED.to_string();
    attempt.graded_at = Some(now());
    attempt.result_id = Some(result.id.clone());
    state.results.insert(result.id.clone(), result.clone());
    state.attempts.insert(attempt.id.clone(), attempt.clone());
    state.emit(
        EVT_ATTEMPT_GRADED,
        json!({
            "attempt_id": attempt.id,
            "exam_id": attempt.exam_id,
            "user_id": attempt.user_id,
            "score": total,
            "max_score": max_score,
            "percentage": percentage,
            "passed": passed,
            "result_id": result.id,
        }),
    );
    // Grades live in the academic record, never duplicated here:
    // emit the academic event with exactly the payload the academic
    // module records itself, only when the exam is linked.
    if let (Some(enrollment_id), Some(component_id)) = (
        exam.academic_enrollment_id.clone(),
        exam.academic_component_id.clone(),
    ) {
        state.emit(
            EVT_ACADEMIC_GRADE,
            json!({
                "grade_id": result.id,
                "enrollment_id": enrollment_id,
                "component_id": component_id,
                "score": percentage,
            }),
        );
    }
    if passed && state.settings().certificate_on_pass {
        state.emit(
            EVT_CERTIFICATE_REQUESTED,
            json!({
                "attempt_id": attempt.id,
                "exam_id": attempt.exam_id,
                "user_id": attempt.user_id,
                "result_id": result.id,
                "score": total,
                "passed": passed,
                "module": "exams",
            }),
        );
    }
    state.notify(
        "exams.graded",
        &attempt.user_id,
        "Your exam has been graded",
        &format!(
            "Exam \"{}\" graded: {:.1}% - {}.",
            exam.title,
            percentage,
            if passed { "passed" } else { "not passed" }
        ),
        Some(format!("/api/v1/exams/results/{}", result.id)),
    );
    Ok(result)
}


/// Per-question statistics for an exam, computed from graded
/// attempts only. No student identity or raw answer is included.
pub(crate) fn question_statistics(state: &ExamsState, exam_id: &str) -> Value {
    let graded: Vec<ExamAttempt> = state
        .attempts
        .iter()
        .filter(|entry| {
            entry.value().exam_id == exam_id && entry.value().status == ATTEMPT_GRADED
        })
        .map(|entry| entry.value().clone())
        .collect();
    let total_attempts = graded.len();
    let mut stats: Vec<Value> = Vec::new();
    let links: Vec<ExamQuestion> = state
        .exam_questions
        .iter()
        .filter(|entry| entry.value().exam_id == exam_id)
        .map(|entry| entry.value().clone())
        .collect();
    // Discrimination index: top 27% of attempts by score versus
    // bottom 27%, per question.
    let mut ranked: Vec<&ExamAttempt> = graded.iter().collect();
    ranked.sort_by(|a, b| {
        b.score
            .unwrap_or(0.0)
            .partial_cmp(&a.score.unwrap_or(0.0))
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    let group_size = ((total_attempts as f64 * 0.27).ceil() as usize).max(1);
    let top: Vec<&ExamAttempt> = ranked.iter().take(group_size).cloned().collect();
    let bottom: Vec<&ExamAttempt> = ranked
        .iter()
        .rev()
        .take(group_size)
        .cloned()
        .collect();
    for link in &links {
        let question = match state.questions.get(&link.question_id) {
            Some(entry) => entry.value().clone(),
            None => continue,
        };
        let mut attempted = 0usize;
        let mut correct = 0usize;
        let mut top_correct = 0usize;
        let mut top_attempted = 0usize;
        let mut bottom_correct = 0usize;
        let mut bottom_attempted = 0usize;
        let case_insensitive = state.settings().short_answer_case_insensitive;
        for attempt in &graded {
            let answer = attempt.answers.get(&link.question_id);
            if let Some(answer) = answer {
                if !answer.is_null() {
                    attempted += 1;
                    if let Some(awarded) = auto_score(&question, answer, case_insensitive) {
                        if awarded >= link.marks {
                            correct += 1;
                        }
                    }
                }
            }
        }
        for attempt in &top {
            let answer = attempt.answers.get(&link.question_id);
            if let Some(answer) = answer {
                if !answer.is_null() {
                    top_attempted += 1;
                    if let Some(awarded) = auto_score(&question, answer, case_insensitive) {
                        if awarded >= link.marks {
                            top_correct += 1;
                        }
                    }
                }
            }
        }
        for attempt in &bottom {
            let answer = attempt.answers.get(&link.question_id);
            if let Some(answer) = answer {
                if !answer.is_null() {
                    bottom_attempted += 1;
                    if let Some(awarded) = auto_score(&question, answer, case_insensitive) {
                        if awarded >= link.marks {
                            bottom_correct += 1;
                        }
                    }
                }
            }
        }
        let attempted_pct = if total_attempts > 0 {
            attempted as f64 / total_attempts as f64 * 100.0
        } else {
            0.0
        };
        let correct_pct = if attempted > 0 {
            correct as f64 / attempted as f64 * 100.0
        } else {
            0.0
        };
        let top_rate = if top_attempted > 0 {
            top_correct as f64 / top_attempted as f64
        } else {
            0.0
        };
        let bottom_rate = if bottom_attempted > 0 {
            bottom_correct as f64 / bottom_attempted as f64
        } else {
            0.0
        };
        stats.push(json!({
            "question_id": link.question_id,
            "question_type": question.question_type,
            "difficulty": question.difficulty,
            "marks": link.marks,
            "attempted_count": attempted,
            "attempted_pct": attempted_pct,
            "correct_count": correct,
            "correct_pct": correct_pct,
            "discrimination_index": top_rate - bottom_rate,
        }));
    }
    json!({
        "exam_id": exam_id,
        "graded_attempts": total_attempts,
        "questions": stats,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validate_pricing_accepts_free_exam() {
        assert!(validate_pricing(0, false).is_ok());
    }

    #[test]
    fn validate_pricing_accepts_paid_exam() {
        assert!(validate_pricing(50_000, true).is_ok());
    }

    #[test]
    fn validate_pricing_rejects_negative_price() {
        assert!(validate_pricing(-1, false).is_err());
        assert!(validate_pricing(-100, true).is_err());
    }

    #[test]
    fn validate_pricing_rejects_paid_exam_without_price() {
        assert!(validate_pricing(0, true).is_err());
    }

    #[test]
    fn validate_pricing_rejects_priced_exam_not_marked_paid() {
        assert!(validate_pricing(1, false).is_err());
    }
}

#[cfg(test)]
mod tenant_guard_tests {
    use super::*;
    use actix_web::HttpMessage;
    use actix_web::test::TestRequest;

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

    fn bank(id: &str, company: Option<&str>) -> QuestionBank {
        QuestionBank {
            id: id.to_string(),
            title: "Bank".to_string(),
            description: String::new(),
            category: "general".to_string(),
            owner_id: "alice".to_string(),
            company_id: company.map(str::to_string),
            created_at: now(),
            updated_at: now(),
        }
    }

    fn exam(id: &str, company: Option<&str>) -> Exam {
        Exam {
            id: id.to_string(),
            title: "Exam".to_string(),
            description: String::new(),
            category: "general".to_string(),
            bank_ids: Vec::new(),
            course_id: None,
            academic_enrollment_id: None,
            academic_component_id: None,
            status: EXAM_DRAFT.to_string(),
            opens_at: None,
            closes_at: None,
            duration_minutes: 60,
            pass_mark: 60.0,
            attempts_allowed: 3,
            price_minor: 0,
            is_paid: false,
            settings: ExamSettings::default(),
            total_marks: 0.0,
            created_by: "alice".to_string(),
            company_id: company.map(str::to_string),
            created_at: now(),
            updated_at: now(),
        }
    }

    fn question(id: &str, bank_id: &str) -> Question {
        Question {
            id: id.to_string(),
            bank_id: bank_id.to_string(),
            version: 1,
            previous_version_id: None,
            superseded_by: None,
            question_type: "multiple_choice_single".to_string(),
            category: "general".to_string(),
            difficulty: "medium".to_string(),
            text: "What?".to_string(),
            options: Vec::new(),
            correct_answer: Value::Null,
            answer_key: None,
            marks: 1.0,
            status: "draft".to_string(),
            created_by: "alice".to_string(),
            created_at: now(),
            updated_at: now(),
        }
    }

    fn attempt(id: &str, exam_id: &str) -> ExamAttempt {
        ExamAttempt {
            id: id.to_string(),
            exam_id: exam_id.to_string(),
            user_id: "alice".to_string(),
            attempt_number: 1,
            status: ATTEMPT_IN_PROGRESS.to_string(),
            started_at: now(),
            expires_at: now(),
            duration_seconds: 3600,
            seed: 1,
            question_order: Vec::new(),
            option_orders: HashMap::new(),
            answers: Value::Object(serde_json::Map::new()),
            autosaved_at: None,
            submitted_at: None,
            score: None,
            max_score: 0.0,
            passed: None,
            graded_at: None,
            result_id: None,
        }
    }

    fn rubric(id: &str, exam_id: &str) -> Rubric {
        Rubric {
            id: id.to_string(),
            exam_id: exam_id.to_string(),
            title: "Rubric".to_string(),
            description: String::new(),
            criteria: Vec::new(),
            created_by: "alice".to_string(),
            created_at: now(),
            updated_at: now(),
        }
    }

    fn grade(id: &str, attempt_id: &str, exam_id: &str) -> ExamGrade {
        ExamGrade {
            id: id.to_string(),
            attempt_id: attempt_id.to_string(),
            exam_id: exam_id.to_string(),
            question_id: "q-1".to_string(),
            grader_id: None,
            rubric_id: None,
            criterion_scores: HashMap::new(),
            marks_awarded: None,
            max_marks: 1.0,
            status: GRADE_PENDING.to_string(),
            comment: None,
            second_grader_id: None,
            audit: Vec::new(),
            created_at: now(),
            updated_at: now(),
        }
    }

    fn result(id: &str, attempt_id: &str, exam_id: &str) -> ExamResult {
        ExamResult {
            id: id.to_string(),
            attempt_id: attempt_id.to_string(),
            exam_id: exam_id.to_string(),
            user_id: "alice".to_string(),
            score: 0.0,
            max_score: 1.0,
            percentage: 0.0,
            passed: false,
            status: RESULT_DRAFT.to_string(),
            per_question: serde_json::json!({}),
            graded_at: now(),
            published_at: None,
            created_at: now(),
            updated_at: now(),
        }
    }

    fn seeded_state() -> ExamsState {
        let state = ExamsState::default();
        state.banks.insert("bank-a".to_string(), bank("bank-a", Some("company-a")));
        state.banks.insert("bank-b".to_string(), bank("bank-b", Some("company-b")));
        state.banks.insert("bank-global".to_string(), bank("bank-global", None));
        state.exams.insert("exam-a".to_string(), exam("exam-a", Some("company-a")));
        state.exams.insert("exam-b".to_string(), exam("exam-b", Some("company-b")));
        state.exams.insert("exam-global".to_string(), exam("exam-global", None));
        state.questions.insert("q-1".to_string(), question("q-1", "bank-a"));
        state.questions.insert("q-2".to_string(), question("q-2", "bank-b"));
        state.attempts.insert("att-1".to_string(), attempt("att-1", "exam-a"));
        state.attempts.insert("att-2".to_string(), attempt("att-2", "exam-b"));
        state.rubrics.insert("rub-1".to_string(), rubric("rub-1", "exam-a"));
        state.grades.insert("gr-1".to_string(), grade("gr-1", "att-1", "exam-a"));
        state.results.insert("res-1".to_string(), result("res-1", "att-1", "exam-a"));
        state
    }

    #[test]
    fn require_tenant_fails_closed_without_resolved_context() {
        let bare = TestRequest::default().to_http_request();
        assert!(require_tenant(&bare).is_err());
    }

    #[test]
    fn require_tenant_resolves_from_middleware_context() {
        let req = tenant_request("company-a", None);
        let tenant = require_tenant(&req).expect("tenant context must resolve");
        assert!(!tenant.tenant_id.to_string().is_empty());
        assert_eq!(
            caller_company(&tenant, &req).unwrap(),
            Some("company-a".to_string())
        );
    }

    #[test]
    fn caller_company_prefers_verified_caller_when_tenant_has_none() {
        let req = tenant_request("", Some("company-a"));
        let tenant = require_tenant(&req).expect("tenant context must resolve");
        assert_eq!(
            caller_company(&tenant, &req).unwrap(),
            Some("company-a".to_string())
        );
    }

    #[test]
    fn caller_company_rejects_verified_caller_mismatch() {
        let req = tenant_request("company-a", Some("company-b"));
        let tenant = require_tenant(&req).expect("tenant context must resolve");
        assert!(caller_company(&tenant, &req).is_err());
    }

    #[test]
    fn caller_company_ignores_the_x_company_id_header() {
        // A forged header naming another company must not move
        // the scope: only the tenant context and the verified
        // caller count.
        let req = TestRequest::default()
            .insert_header(("x-company-id", "company-b"))
            .to_http_request();
        req.extensions_mut()
            .insert(unified_core::middleware::TenantContext {
                tenant_id: uuid::Uuid::now_v7().to_string(),
                company_id: "company-a".to_string(),
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
            Some("company-a".to_string())
        );
    }

    #[test]
    fn in_company_scope_shows_global_records_to_scoped_callers() {
        let scope = Some("company-a".to_string());
        assert!(in_company_scope(&Some("company-a".to_string()), &scope));
        assert!(!in_company_scope(&Some("company-b".to_string()), &scope));
        assert!(in_company_scope(&None, &scope));
        assert!(in_company_scope(&None, &None));
        assert!(!in_company_scope(&Some("company-b".to_string()), &None));
    }

    #[test]
    fn require_company_match_for_rules() {
        let body = Some("company-a".to_string());
        assert!(require_company_match_for(&None, &body).is_err());
        assert!(require_company_match_for(&None, &None).is_err());
        assert_eq!(
            require_company_match_for(&Some("company-a".to_string()), &body).unwrap(),
            Some("company-a".to_string())
        );
        assert!(require_company_match_for(&Some("company-b".to_string()), &body).is_err());
        // An omitted body company adopts the caller scope.
        assert_eq!(
            require_company_match_for(&Some("company-a".to_string()), &None).unwrap(),
            Some("company-a".to_string())
        );
    }

    #[test]
    fn require_bank_by_id_in_scope_rules() {
        let state = seeded_state();
        let scope = Some("company-a".to_string());
        assert!(require_bank_by_id_in_scope(&state, "bank-a", &scope).is_ok());
        assert!(require_bank_by_id_in_scope(&state, "bank-b", &scope).is_err());
        // A scoped caller does not see global banks.
        assert!(require_bank_by_id_in_scope(&state, "bank-global", &scope).is_err());
        // An unresolved caller sees global banks only.
        assert!(require_bank_by_id_in_scope(&state, "bank-global", &None).is_ok());
        assert!(require_bank_by_id_in_scope(&state, "bank-a", &None).is_err());
        // A missing bank fails closed.
        assert!(require_bank_by_id_in_scope(&state, "bank-nope", &scope).is_err());
    }

    #[test]
    fn require_exam_by_id_in_scope_rules() {
        let state = seeded_state();
        let scope = Some("company-a".to_string());
        assert!(require_exam_by_id_in_scope(&state, "exam-a", &scope).is_ok());
        assert!(require_exam_by_id_in_scope(&state, "exam-b", &scope).is_err());
        assert!(require_exam_by_id_in_scope(&state, "exam-global", &scope).is_err());
        assert!(require_exam_by_id_in_scope(&state, "exam-global", &None).is_ok());
        assert!(require_exam_by_id_in_scope(&state, "exam-nope", &scope).is_err());
    }

    #[test]
    fn require_question_in_scope_follows_its_bank() {
        let state = seeded_state();
        let scope = Some("company-a".to_string());
        assert!(require_question_in_scope(&state, "q-1", &scope).is_ok());
        assert!(require_question_in_scope(&state, "q-2", &scope).is_err());
        assert!(require_question_in_scope(&state, "q-nope", &scope).is_err());
    }

    #[test]
    fn require_attempt_in_scope_follows_its_exam() {
        let state = seeded_state();
        let scope = Some("company-a".to_string());
        assert!(require_attempt_in_scope(&state, "att-1", &scope).is_ok());
        assert!(require_attempt_in_scope(&state, "att-2", &scope).is_err());
        assert!(require_attempt_in_scope(&state, "att-nope", &scope).is_err());
    }

    #[test]
    fn require_rubric_grade_and_result_in_scope_follow_the_exam() {
        let state = seeded_state();
        let scope = Some("company-a".to_string());
        assert!(require_rubric_in_scope(&state, "rub-1", &scope).is_ok());
        assert!(require_grade_in_scope(&state, "gr-1", &scope).is_ok());
        assert!(require_result_in_scope(&state, "res-1", &scope).is_ok());
        let other = Some("company-b".to_string());
        assert!(require_rubric_in_scope(&state, "rub-1", &other).is_err());
        assert!(require_grade_in_scope(&state, "gr-1", &other).is_err());
        assert!(require_result_in_scope(&state, "res-1", &other).is_err());
    }
}
