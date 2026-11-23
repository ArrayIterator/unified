// surveys module — Helpers.
//
// Split from the former single-file module `src/modules/surveys.rs`.
// The module root (`surveys.rs`) re-exports the public items, so the
// public API is unchanged.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::{HookBus, HookContext};
use unified_core::json;
use unified_core::modules::{Module, ModuleRegistry, RouteInfo, RouteVersionSupport};
use unified_core::pagination::{PageResult, PaginationParams};
use actix_web::{web, HttpRequest, HttpResponse};
use base64::Engine;
use chrono::{DateTime, Datelike, Duration, Utc};
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use uuid::Uuid;

use super::constants::{MAX_EMAIL_LEN, QUESTION_TYPES};
use super::types::{Survey, SurveyAnswer, SurveyDistribution, SurveyQuestion, SurveyResponse};
use super::state::SurveysState;

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

// ---------------------------------------------------------------------------
// Tenant edge guard (C3 pilot 4, task 316).
// ---------------------------------------------------------------------------

/// Fail-closed tenant gate: resolves the tenant context the
/// tenant-resolution middleware attached to the request. A request
/// with no resolved tenant context is rejected (401) and never falls
/// back to the `public` schema (the shared `TenantContext` contract
/// in `unified_core::tenancy`). Every tenant-data handler in this
/// module calls this FIRST, before caller identity or any capability
/// check.
///
/// The records in this module carry no company field, so the guard
/// is a fail-closed gate only: real per-record partitioning needs a
/// `tenant_id` on the record (C11 follow-up), not a filter here.
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
/// middleware-resolved tenant context are sources.
pub(crate) fn caller_company(
    tenant: &unified_core::tenancy::TenantContext,
    req: &HttpRequest,
) -> UnifiedResult<Option<String>> {
    let verified = unified_core::caller::caller_from(req)
        .company_id
        .filter(|value| !value.is_empty());
    match (&tenant.company_id, &verified) {
        (Some(tenant_company), Some(verified_company)) if tenant_company != verified_company => {
            Err(UnifiedError::forbidden(
                "Verified caller company does not match the tenant company scope",
            ))
        }
        (Some(tenant_company), _) => Ok(Some(tenant_company.clone())),
        (None, verified) => Ok(verified),
    }
}

/// Fail closed (403) when no company resolves for a mutating
/// handler: the tenant context's company first, the verified
/// caller's server-resolved company otherwise. An unresolved caller
/// writes nothing (the shared `CallerScope::Unresolved` rule) — the
/// records in this module carry no company field to stamp, so there
/// is no global-row write to fall back to.
pub(crate) fn require_company(
    tenant: &unified_core::tenancy::TenantContext,
    req: &HttpRequest,
) -> UnifiedResult<String> {
    caller_company(tenant, req)?.ok_or_else(|| {
        UnifiedError::forbidden(
            "No company scope resolved for this request: writes require a resolved company",
        )
    })
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

pub(crate) fn valid_email(email: &str) -> bool {
    if email.is_empty() || email.len() > MAX_EMAIL_LEN || email.contains(' ') {
        return false;
    }
    let mut parts = email.split('@');
    let (Some(local), Some(domain)) = (parts.next(), parts.next()) else {
        return false;
    };
    if parts.next().is_some() || local.is_empty() || domain.is_empty() {
        return false;
    }
    domain.contains('.') && !domain.starts_with('.') && !domain.ends_with('.')
}

/// One unguessable response token: 32 random bytes, base64url
/// encoded (43 characters, no padding). Regenerated while the
/// token already exists, so tokens stay unique.
pub(crate) fn generate_token(state: &SurveysState) -> String {
    loop {
        let bytes: [u8; 32] = rand::random();
        let token = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes);
        if !state.responses.iter().any(|entry| entry.value().token == token) {
            return token;
        }
    }
}

pub(crate) fn load_survey(state: &SurveysState, raw_id: &str) -> UnifiedResult<Survey> {
    let id = parse_uuid(raw_id, "survey")?;
    state
        .surveys
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Survey not found: {id}")))
}

pub(crate) fn load_question(state: &SurveysState, survey_id: Uuid, raw_id: &str) -> UnifiedResult<SurveyQuestion> {
    let id = parse_uuid(raw_id, "question")?;
    state
        .questions
        .get(&id)
        .map(|entry| entry.value().clone())
        .filter(|question| question.survey_id == survey_id)
        .ok_or_else(|| UnifiedError::not_found(format!("Question not found: {id}")))
}

pub(crate) fn load_distribution(state: &SurveysState, raw_id: &str) -> UnifiedResult<SurveyDistribution> {
    let id = parse_uuid(raw_id, "distribution")?;
    state
        .distributions
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Distribution not found: {id}")))
}

pub(crate) fn load_response(state: &SurveysState, raw_id: &str) -> UnifiedResult<SurveyResponse> {
    let id = parse_uuid(raw_id, "response")?;
    state
        .responses
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Response not found: {id}")))
}

pub(crate) fn questions_for(state: &SurveysState, survey_id: Uuid) -> Vec<SurveyQuestion> {
    let mut items: Vec<SurveyQuestion> = state
        .questions
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|question| question.survey_id == survey_id)
        .collect();
    items.sort_by_key(|question| question.position);
    items
}

pub(crate) fn responses_for_survey(state: &SurveysState, survey_id: Uuid) -> Vec<SurveyResponse> {
    let mut items: Vec<SurveyResponse> = state
        .responses
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|response| response.survey_id == survey_id)
        .collect();
    items.sort_by_key(|response| response.created_at);
    items
}

pub(crate) fn answers_for_response(state: &SurveysState, response_id: Uuid) -> Vec<SurveyAnswer> {
    let mut items: Vec<SurveyAnswer> = state
        .answers
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|answer| answer.response_id == response_id)
        .collect();
    items.sort_by_key(|answer| answer.created_at);
    items
}

/// Score bounds for a question type: NPS 0–10, CSAT and rating
/// 1–5, no score bounds for text / multiple choice.
pub(crate) fn score_bounds(question_type: &str) -> (Option<i32>, Option<i32>) {
    match question_type {
        "nps" => (Some(0), Some(10)),
        "csat" | "rating" => (Some(1), Some(5)),
        _ => (None, None),
    }
}

pub(crate) fn valid_question_type(raw: &str) -> UnifiedResult<String> {
    let value = raw.trim().to_lowercase();
    if QUESTION_TYPES.contains(&value.as_str()) {
        Ok(value)
    } else {
        Err(UnifiedError::bad_request(format!(
            "question_type must be one of: {}",
            QUESTION_TYPES.join(", ")
        )))
    }
}

pub(crate) fn opt_string(data: &Value, key: &str) -> Option<String> {
    data.get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::modules::surveys::{SurveyQuestion, SurveysState};
    use chrono::Utc;

    #[test]
    fn score_bounds_match_each_question_type() {
        assert_eq!(score_bounds("nps"), (Some(0), Some(10)));
        assert_eq!(score_bounds("csat"), (Some(1), Some(5)));
        assert_eq!(score_bounds("rating"), (Some(1), Some(5)));
        assert_eq!(score_bounds("text"), (None, None));
        assert_eq!(score_bounds("multiple_choice"), (None, None));
        assert_eq!(score_bounds("bogus"), (None, None));
    }

    #[test]
    fn valid_question_type_normalises_case_and_whitespace() {
        assert_eq!(valid_question_type("nps").unwrap(), "nps");
        assert_eq!(valid_question_type(" NPS ").unwrap(), "nps");
        assert!(valid_question_type("bogus").is_err());
    }

    #[test]
    fn valid_email_checks_shape_only() {
        assert!(valid_email("a@b.co"));
        assert!(!valid_email("a@b"));
        assert!(!valid_email("a b@c.co"));
        assert!(!valid_email("@b.co"));
        assert!(!valid_email(""));
    }

    #[test]
    fn opt_string_trims_and_ignores_non_strings() {
        assert_eq!(
            opt_string(&serde_json::json!({"a": " x "}), "a"),
            Some("x".to_string())
        );
        assert_eq!(opt_string(&serde_json::json!({"a": "   "}), "a"), None);
        assert_eq!(opt_string(&serde_json::json!({"a": 5}), "a"), None);
        assert_eq!(opt_string(&serde_json::json!({}), "a"), None);
    }

    #[test]
    fn generate_token_is_43_chars_and_unique() {
        let state = SurveysState::default();
        let first = generate_token(&state);
        let second = generate_token(&state);
        assert_eq!(first.len(), 43);
        assert_ne!(first, second);
    }

    #[test]
    fn load_survey_rejects_bad_ids_and_reports_missing_surveys() {
        let state = SurveysState::default();
        assert!(load_survey(&state, "not-a-uuid").is_err());
        assert!(load_survey(&state, &Uuid::new_v4().to_string()).is_err());
    }

    #[test]
    fn questions_for_returns_questions_in_position_order() {
        let state = SurveysState::default();
        let survey_id = Uuid::new_v4();
        for position in [2, 1] {
            state.questions.insert(
                Uuid::new_v4(),
                SurveyQuestion {
                    id: Uuid::new_v4(),
                    survey_id,
                    position,
                    question_type: "text".to_string(),
                    text: "Question".to_string(),
                    required: false,
                    options: Vec::new(),
                    min_value: None,
                    max_value: None,
                    created_at: Utc::now(),
                    updated_at: Utc::now(),
                },
            );
        }
        let ordered = questions_for(&state, survey_id);
        assert_eq!(ordered.len(), 2);
        assert_eq!(ordered[0].position, 1);
        assert_eq!(ordered[1].position, 2);
    }

    #[test]
    fn paginate_slices_pages_and_reports_totals() {
        let params = PaginationParams {
            page: Some(2),
            per_page: Some(2),
            ..Default::default()
        };
        let page = paginate(vec![1, 2, 3, 4, 5], &params);
        assert_eq!(page.items, vec![3, 4]);
        assert_eq!(page.pagination.total, 5);
        assert_eq!(page.pagination.total_pages, 3);
    }

    #[test]
    fn require_tenant_fails_closed_without_resolved_context() {
        let bare = actix_web::test::TestRequest::default().to_http_request();
        assert!(require_tenant(&bare).is_err());
    }

    #[test]
    fn require_tenant_resolves_from_middleware_context() {
        use actix_web::HttpMessage;
        let req = actix_web::test::TestRequest::default().to_http_request();
        req.extensions_mut()
            .insert(unified_core::middleware::TenantContext {
                tenant_id: uuid::Uuid::now_v7().to_string(),
                company_id: "company-1".to_string(),
                site_id: String::new(),
                kind: unified_core::middleware::TenantKind::Subdomain,
                host: "tenant.example.test".to_string(),
                scope: None,
                product_id: None,
                package_id: None,
                product_subdomain_id: None,
            });
        let tenant = require_tenant(&req).expect("tenant context must resolve");
        assert_eq!(tenant.company_id.as_deref(), Some("company-1"));
        let company = caller_company(&tenant, &req).expect("company must resolve");
        assert_eq!(company.as_deref(), Some("company-1"));
        assert_eq!(
            require_company(&tenant, &req).expect("company must resolve"),
            "company-1"
        );
    }

    #[test]
    fn require_company_fails_closed_without_a_resolved_company() {
        use actix_web::HttpMessage;
        let req = actix_web::test::TestRequest::default().to_http_request();
        req.extensions_mut()
            .insert(unified_core::middleware::TenantContext {
                tenant_id: uuid::Uuid::now_v7().to_string(),
                company_id: String::new(),
                site_id: String::new(),
                kind: unified_core::middleware::TenantKind::Subdomain,
                host: "tenant.example.test".to_string(),
                scope: None,
                product_id: None,
                package_id: None,
                product_subdomain_id: None,
            });
        let tenant = require_tenant(&req).expect("tenant context must resolve");
        assert!(tenant.company_id.is_none());
        assert!(
            caller_company(&tenant, &req)
                .expect("mismatch check must pass")
                .is_none()
        );
        assert!(require_company(&tenant, &req).is_err());
    }

    #[test]
    fn caller_company_refuses_a_verified_company_mismatch() {
        use actix_web::HttpMessage;
        let req = actix_web::test::TestRequest::default().to_http_request();
        req.extensions_mut()
            .insert(unified_core::middleware::TenantContext {
                tenant_id: uuid::Uuid::now_v7().to_string(),
                company_id: "company-1".to_string(),
                site_id: String::new(),
                kind: unified_core::middleware::TenantKind::Subdomain,
                host: "tenant.example.test".to_string(),
                scope: None,
                product_id: None,
                package_id: None,
                product_subdomain_id: None,
            });
        req.extensions_mut().insert(
            unified_core::auth::Caller::verified(uuid::Uuid::now_v7(), Vec::new())
                .with_company("company-2".to_string()),
        );
        let tenant = require_tenant(&req).expect("tenant context must resolve");
        assert!(caller_company(&tenant, &req).is_err());
    }
}
