// surveys module — Results and response handlers.
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

use super::constants::{CSAT_FOLLOWUP_MAX, CSAT_SATISFIED_MIN, NPS_DETRACTOR_MAX, NPS_PROMOTER_MIN, PERM_SURVEYS_VIEW};
use super::types::SurveyResponse;
use super::state::SurveysState;
use super::helpers::{
    answers_for_response, caller, caller_company, err, load_response, load_survey, ok, paginate,
    questions_for, require_company, require_permission, require_tenant, responses_for_survey,
};
use super::service::{question_stats, trend_buckets};

// ---------------------------------------------------------------------------
// Results and response handlers
// ---------------------------------------------------------------------------

pub(crate) fn parse_rfc3339(raw: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(raw)
        .ok()
        .map(|value| value.with_timezone(&Utc))
}

pub(crate) async fn survey_results(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<SurveysState>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_SURVEYS_VIEW)?;
        let survey = load_survey(&state, &path)?;
        let params = query.into_inner();
        let bucket = params
            .filters
            .get("bucket")
            .cloned()
            .unwrap_or_else(|| "daily".to_string());
        if bucket != "daily" && bucket != "weekly" {
            return Err(UnifiedError::bad_request("bucket must be daily or weekly"));
        }
        let from = params.filters.get("from").and_then(|raw| parse_rfc3339(raw));
        let to = params.filters.get("to").and_then(|raw| parse_rfc3339(raw));
        let agent_filter = params.filters.get("agent_id").cloned();
        let department_filter = params.filters.get("department_id").cloned();
        let product_filter = params.filters.get("product_id").cloned();
        let mut responses = responses_for_survey(&state, survey.id);
        responses.retain(|response| {
            if let Some(agent) = &agent_filter {
                if response.agent_id.as_deref() != Some(agent.as_str()) {
                    return false;
                }
            }
            if let Some(department) = &department_filter {
                if response.department_id.as_deref() != Some(department.as_str()) {
                    return false;
                }
            }
            if let Some(product) = &product_filter {
                if response.product_id.as_deref() != Some(product.as_str()) {
                    return false;
                }
            }
            if let Some(submitted_at) = response.submitted_at {
                if let Some(from) = from {
                    if submitted_at < from {
                        return false;
                    }
                }
                if let Some(to) = to {
                    if submitted_at > to {
                        return false;
                    }
                }
            } else if from.is_some() || to.is_some() {
                return false;
            }
            true
        });
        let submitted = responses.iter().filter(|response| response.submitted_at.is_some()).count() as u64;
        let questions = questions_for(&state, survey.id);
        let per_question: Vec<Value> = questions
            .iter()
            .map(|question| question_stats(&state, question, &responses))
            .collect();
        // Overall NPS / CSAT across all scored questions of each
        // type (computed on read, never stored).
        let overall = |question_type: &str, score_fn: &dyn Fn(&[i32]) -> Option<f64>| -> Value {
            let mut scores: Vec<i32> = Vec::new();
            for question in questions.iter().filter(|question| question.question_type == question_type) {
                for response in responses.iter().filter(|response| response.submitted_at.is_some()) {
                    for answer in answers_for_response(&state, response.id) {
                        if answer.question_id == question.id {
                            if let Some(score) = answer.score {
                                scores.push(score);
                            }
                        }
                    }
                }
            }
            match score_fn(&scores) {
                Some(value) => json!(value),
                None => Value::Null,
            }
        };
        let nps_value = overall("nps", &|scores: &[i32]| {
            if scores.is_empty() {
                return None;
            }
            let total = scores.len() as f64;
            let promoters = scores.iter().filter(|score| **score >= NPS_PROMOTER_MIN).count() as f64;
            let detractors = scores.iter().filter(|score| **score <= NPS_DETRACTOR_MAX).count() as f64;
            Some(((promoters - detractors) / total * 100.0).round())
        });
        let csat_value = overall("csat", &|scores: &[i32]| {
            if scores.is_empty() {
                return None;
            }
            let total = scores.len() as f64;
            let satisfied = scores.iter().filter(|score| **score >= CSAT_SATISFIED_MIN).count() as f64;
            Some((satisfied / total * 100.0).round())
        });
        // Breakdowns by agent, department, and product: submitted
        // count plus NPS / CSAT within each group.
        let breakdown = |key_fn: &dyn Fn(&SurveyResponse) -> Option<String>| -> Value {
            let mut groups: HashMap<String, Vec<Uuid>> = HashMap::new();
            for response in responses.iter().filter(|response| response.submitted_at.is_some()) {
                if let Some(key) = key_fn(response) {
                    groups.entry(key).or_default().push(response.id);
                }
            }
            let mut keys: Vec<String> = groups.keys().cloned().collect();
            keys.sort();
            let mut out = serde_json::Map::new();
            for key in keys {
                let response_ids = groups.remove(&key).unwrap_or_default();
                let mut nps_scores: Vec<i32> = Vec::new();
                let mut csat_scores: Vec<i32> = Vec::new();
                for response_id in &response_ids {
                    for answer in answers_for_response(&state, *response_id) {
                        let Some(question) = state.questions.get(&answer.question_id) else {
                            continue;
                        };
                        let Some(score) = answer.score else {
                            continue;
                        };
                        if question.question_type == "nps" {
                            nps_scores.push(score);
                        } else if question.question_type == "csat" {
                            csat_scores.push(score);
                        }
                    }
                }
                let nps = if nps_scores.is_empty() {
                    Value::Null
                } else {
                    let total = nps_scores.len() as f64;
                    let promoters = nps_scores.iter().filter(|score| **score >= NPS_PROMOTER_MIN).count() as f64;
                    let detractors = nps_scores.iter().filter(|score| **score <= NPS_DETRACTOR_MAX).count() as f64;
                    json!(((promoters - detractors) / total * 100.0).round())
                };
                let csat = if csat_scores.is_empty() {
                    Value::Null
                } else {
                    let total = csat_scores.len() as f64;
                    let satisfied = csat_scores.iter().filter(|score| **score >= CSAT_SATISFIED_MIN).count() as f64;
                    json!((satisfied / total * 100.0).round())
                };
                out.insert(
                    key,
                    json!({
                        "responses": response_ids.len() as u64,
                        "nps": nps,
                        "csat": csat,
                    }),
                );
            }
            Value::Object(out)
        };
        Ok(json!({
            "survey_id": survey.id.to_string(),
            "family_id": survey.family_id.to_string(),
            "version": survey.version,
            "invited": responses.len() as u64,
            "submitted": submitted,
            "response_rate": if responses.is_empty() { Value::Null } else { json!(((submitted as f64 / responses.len() as f64) * 100.0).round()) },
            "nps": nps_value,
            "csat": csat_value,
            "questions": per_question,
            "by_agent": breakdown(&|response| response.agent_id.clone()),
            "by_department": breakdown(&|response| response.department_id.clone()),
            "by_product": breakdown(&|response| response.product_id.clone()),
            "trends": trend_buckets(&state, &responses, &bucket),
        }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) fn response_view(state: &SurveysState, response: &SurveyResponse) -> Value {
    let mut value = serde_json::to_value(response).unwrap_or(Value::Null);
    if let Value::Object(map) = &mut value {
        // The token is never returned in admin views: it is the
        // bearer credential for the public response link.
        map.remove("token");
        map.insert(
            "answers".to_string(),
            serde_json::to_value(answers_for_response(state, response.id)).unwrap_or(Value::Null),
        );
    }
    value
}

pub(crate) async fn list_responses(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<SurveysState>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_SURVEYS_VIEW)?;
        let survey = load_survey(&state, &path)?;
        let params = query.into_inner();
        let status_filter = params.filters.get("status").cloned();
        let score_band = params.filters.get("score_band").cloned();
        let agent_filter = params.filters.get("agent_id").cloned();
        let department_filter = params.filters.get("department_id").cloned();
        let product_filter = params.filters.get("product_id").cloned();
        let mut items: Vec<Value> = responses_for_survey(&state, survey.id)
            .into_iter()
            .filter(|response| {
                if let Some(status) = &status_filter {
                    let submitted = response.submitted_at.is_some();
                    match status.as_str() {
                        "submitted" if !submitted => return false,
                        "pending" if submitted => return false,
                        _ => {}
                    }
                }
                if let Some(agent) = &agent_filter {
                    if response.agent_id.as_deref() != Some(agent.as_str()) {
                        return false;
                    }
                }
                if let Some(department) = &department_filter {
                    if response.department_id.as_deref() != Some(department.as_str()) {
                        return false;
                    }
                }
                if let Some(product) = &product_filter {
                    if response.product_id.as_deref() != Some(product.as_str()) {
                        return false;
                    }
                }
                if let Some(band) = &score_band {
                    let scores: Vec<i32> = answers_for_response(&state, response.id)
                        .iter()
                        .filter_map(|answer| answer.score)
                        .collect();
                    if scores.is_empty() {
                        return false;
                    }
                    let matches = match band.as_str() {
                        "promoter" => scores.iter().any(|score| *score >= NPS_PROMOTER_MIN),
                        "detractor" => scores.iter().any(|score| *score <= NPS_DETRACTOR_MAX),
                        "satisfied" => scores.iter().any(|score| *score >= CSAT_SATISFIED_MIN),
                        "dissatisfied" => scores.iter().any(|score| *score <= CSAT_FOLLOWUP_MAX),
                        _ => true,
                    };
                    if !matches {
                        return false;
                    }
                }
                true
            })
            .map(|response| response_view(&state, &response))
            .collect();
        items.sort_by(|a, b| {
            b.get("created_at")
                .and_then(Value::as_str)
                .cmp(&a.get("created_at").and_then(Value::as_str))
        });
        Ok(paginate(items, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn get_response(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<SurveysState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_SURVEYS_VIEW)?;
        let response = load_response(&state, &path)?;
        Ok(response_view(&state, &response))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}
