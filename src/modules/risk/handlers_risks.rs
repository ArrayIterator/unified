// risk - handlers risks.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::{HookBus, HookContext, HookHandler};
use unified_core::json;
use unified_core::modules::{Module, ModuleRegistry, RouteInfo};
use unified_core::pagination::{PageResult, PaginationParams};
use actix_web::{web, HttpRequest, HttpResponse};
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::sync::{Arc, RwLock};
use uuid::Uuid;
use super::*;
use super::types::*;
use super::store::*;
use super::common::*;
use super::handlers_plans::*;
use super::handlers_templates::*;
use super::handlers_instances::*;
use super::module::*;

// Handlers: risks

/// Parse the `min_score` list filter up front: an unparsable
/// value is a 400, never a silently ignored filter.
pub(crate) fn parse_min_score(params: &PaginationParams) -> UnifiedResult<Option<u8>> {
    match params.filters.get("min_score") {
        None => Ok(None),
        Some(raw) => raw.parse::<u8>().map(Some).map_err(|_| {
            UnifiedError::bad_request(format!(
                "min_score must be an integer between 0 and 255, got \"{raw}\""
            ))
        }),
    }
}

pub(crate) async fn list_risks(
    state: web::Data<Arc<RiskState>>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let params = query.into_inner();
    let _tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return json::error_response(&error, false),
    };
    let (_viewer_id, viewer_role) = viewer_of(&req);
    let min_score = match parse_min_score(&params) {
        Ok(min_score) => min_score,
        Err(error) => return json::error_response(&error, false),
    };
    if let Err(error) = require_capability(&state, viewer_role.as_deref(), "risk:view") {
        return json::error_response(&error, false);
    }
    let scope = match caller_company(&_tenant, &req) {
        Ok(scope) => scope,
        Err(error) => return json::error_response(&error, false),
    };
    let mut items: Vec<Risk> = state
        .risks
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|risk| {
            // Company scope first: a scoped caller sees only
            // their own company's risks; an unresolved caller
            // sees global risks only.
            in_company_scope(&risk.company_id, &scope)
                && matches_search(
                &format!("{} {} {}", risk.title, risk.description, risk.category),
                &params,
            ) && params
                .filters
                .get("company_id")
                .is_none_or(|company| risk.company_id.as_deref() == Some(company.as_str()))
                && params
                    .filters
                    .get("category")
                    .is_none_or(|category| risk.category == *category)
                && params
                    .filters
                    .get("status")
                    .is_none_or(|status| risk.status == *status)
                && params
                    .filters
                    .get("band")
                    .is_none_or(|band| risk.band == *band)
                && params
                    .filters
                    .get("owner_id")
                    .is_none_or(|owner| risk.owner_id == *owner)
                && min_score.is_none_or(|min| risk.score >= min)
        })
        .collect();
    items.sort_by(|a, b| b.score.cmp(&a.score).then(b.updated_at.cmp(&a.updated_at)));
    if params.normalized().order == "asc" {
        items.reverse();
    }
    let page = paginate(items, &params);
    json::success_response(page, 200)
}

pub(crate) async fn create_risk(
    state: web::Data<Arc<RiskState>>,
    req: HttpRequest,
    body: web::Json<CreateRisk>,
) -> HttpResponse {
    let _tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return json::error_response(&error, false),
    };
    let (actor_id, actor_role) = actor_of(&req);
    if let Err(error) = require_capability(&state, actor_role.as_deref(), "risk:create") {
        return json::error_response(&error, false);
    }
    let scope = match require_company_for_write(&_tenant, &req) {
        Ok(scope) => scope,
        Err(error) => return json::error_response(&error, false),
    };
    let result: UnifiedResult<Risk> = (|| {
        let input = body.into_inner();
        if input.title.trim().is_empty() {
            return Err(UnifiedError::bad_request("Risk title must not be empty"));
        }
        if !RISK_CATEGORIES.contains(&input.category.as_str()) {
            return Err(UnifiedError::bad_request(format!(
                "category must be one of: {}",
                RISK_CATEGORIES.join(", ")
            )));
        }
        if !valid_rating(input.likelihood) || !valid_rating(input.impact) {
            return Err(UnifiedError::bad_request(
                "likelihood and impact must each be between 1 and 5",
            ));
        }
        if input.owner_id.trim().is_empty() {
            return Err(UnifiedError::bad_request("owner_id must not be empty"));
        }
        let status = input.status.unwrap_or_else(|| "identified".to_string());
        if !RISK_STATUSES.contains(&status.as_str()) {
            return Err(UnifiedError::bad_request(format!(
                "status must be one of: {}",
                RISK_STATUSES.join(", ")
            )));
        }
        let alert_threshold = input.alert_threshold.unwrap_or(DEFAULT_ALERT_THRESHOLD);
        if !(1..=25).contains(&alert_threshold) {
            return Err(UnifiedError::bad_request(
                "alert_threshold must be between 1 and 25",
            ));
        }
        let score = score_for(input.likelihood, input.impact);
        let risk = Risk {
            id: new_id(),
            company_id: require_company_match_for(
                &scope,
                &input.company_id.filter(|value| !value.trim().is_empty()),
            )?, 
            title: input.title.trim().to_string(),
            description: input
                .description
                .map(|value| value.trim().to_string())
                .unwrap_or_default(),
            category: input.category,
            likelihood: input.likelihood,
            impact: input.impact,
            score,
            band: band_for(score).to_string(),
            owner_id: input.owner_id.trim().to_string(),
            status,
            next_review_at: input.next_review_at,
            alert_threshold,
            created_by: actor_id,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        };
        state.risks.insert(risk.id.clone(), risk.clone());
        state.emit(
            "risk.risk_created",
            serde_json::to_value(&risk).unwrap_or(Value::Null),
        );
        state.check_threshold(&risk);
        Ok(risk)
    })();
    respond_created(result)
}

pub(crate) async fn get_risk(
    state: web::Data<Arc<RiskState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let _tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return json::error_response(&error, false),
    };
    let (_actor_id, actor_role) = actor_of(&req);
    if let Err(error) = require_capability(&state, actor_role.as_deref(), "risk:view") {
        return json::error_response(&error, false);
    }
    let scope = match caller_company(&_tenant, &req) {
        Ok(scope) => scope,
        Err(error) => return json::error_response(&error, false),
    };
    let result: UnifiedResult<Risk> = (|| {
        let risk = state.get_risk(path.as_str())?;
        require_record_in_company_scope(&risk.company_id, &scope)?;
        Ok(risk)
    })();
    json::respond(result, false)
}

pub(crate) async fn update_risk(
    state: web::Data<Arc<RiskState>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdateRisk>,
) -> HttpResponse {
    let _tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return json::error_response(&error, false),
    };
    let (_actor_id, actor_role) = actor_of(&req);
    if let Err(error) = require_capability(&state, actor_role.as_deref(), "risk:edit") {
        return json::error_response(&error, false);
    }
    let scope = match require_company_for_write(&_tenant, &req) {
        Ok(scope) => scope,
        Err(error) => return json::error_response(&error, false),
    };
    let result: UnifiedResult<Risk> = (|| {
        let input = body.into_inner();
        if let Some(category) = &input.category {
            if !RISK_CATEGORIES.contains(&category.as_str()) {
                return Err(UnifiedError::bad_request(format!(
                    "category must be one of: {}",
                    RISK_CATEGORIES.join(", ")
                )));
            }
        }
        if let Some(likelihood) = input.likelihood {
            if !valid_rating(likelihood) {
                return Err(UnifiedError::bad_request(
                    "likelihood must be between 1 and 5",
                ));
            }
        }
        if let Some(impact) = input.impact {
            if !valid_rating(impact) {
                return Err(UnifiedError::bad_request("impact must be between 1 and 5"));
            }
        }
        if let Some(status) = &input.status {
            if !RISK_STATUSES.contains(&status.as_str()) {
                return Err(UnifiedError::bad_request(format!(
                    "status must be one of: {}",
                    RISK_STATUSES.join(", ")
                )));
            }
        }
        if let Some(threshold) = input.alert_threshold {
            if !(1..=25).contains(&threshold) {
                return Err(UnifiedError::bad_request(
                    "alert_threshold must be between 1 and 25",
                ));
            }
        }
        let mut risk = state
            .risks
            .get_mut(path.as_str())
            .ok_or_else(|| UnifiedError::not_found("Risk not found"))?;
        require_record_writable_in_company_scope(&risk.company_id, &scope)?;
        let old_status = risk.status.clone();
        // Validate the transition before mutating anything: the
        // guard writes straight into the stored entry, so a
        // rejected move must not leave a half-applied update.
        if let Some(status) = &input.status {
            if !valid_risk_transition(&old_status, status) {
                return Err(UnifiedError::unprocessable(format!(
                    "Risk status cannot move from \"{old_status}\" to \"{status}\""
                )));
            }
        }
        if let Some(title) = input.title {
            if title.trim().is_empty() {
                return Err(UnifiedError::bad_request("Risk title must not be empty"));
            }
            risk.title = title.trim().to_string();
        }
        if let Some(description) = input.description {
            risk.description = description.trim().to_string();
        }
        if let Some(category) = input.category {
            risk.category = category;
        }
        if let Some(likelihood) = input.likelihood {
            risk.likelihood = likelihood;
        }
        if let Some(impact) = input.impact {
            risk.impact = impact;
        }
        // Score and band are always recomputed server-side; a
        // client-supplied score is never accepted.
        risk.score = score_for(risk.likelihood, risk.impact);
        risk.band = band_for(risk.score).to_string();
        if let Some(owner_id) = input.owner_id {
            if owner_id.trim().is_empty() {
                return Err(UnifiedError::bad_request("owner_id must not be empty"));
            }
            risk.owner_id = owner_id.trim().to_string();
        }
        if let Some(status) = input.status {
            risk.status = status;
        }
        if let Some(next_review_at) = input.next_review_at {
            risk.next_review_at = Some(next_review_at);
        }
        if let Some(alert_threshold) = input.alert_threshold {
            risk.alert_threshold = alert_threshold;
        }
        risk.updated_at = Utc::now();
        let value = serde_json::to_value(&*risk).unwrap_or(Value::Null);
        let updated = risk.clone();
        let status_changed = old_status != updated.status;
        drop(risk);
        state.emit("risk.risk_updated", value.clone());
        if status_changed {
            state.emit(
                "risk.risk_status_changed",
                json!({
                    "risk_id": updated.id,
                    "company_id": updated.company_id,
                    "title": updated.title,
                    "old_status": old_status,
                    "new_status": updated.status,
                    "score": updated.score,
                    "band": updated.band,
                    "owner_id": updated.owner_id,
                }),
            );
        }
        state.check_threshold(&updated);
        Ok(updated)
    })();
    json::respond(result, false)
}

pub(crate) async fn delete_risk(
    state: web::Data<Arc<RiskState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let _tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return json::error_response(&error, false),
    };
    let (_actor_id, actor_role) = actor_of(&req);
    if let Err(error) = require_capability(&state, actor_role.as_deref(), "risk:delete") {
        return json::error_response(&error, false);
    }
    let scope = match require_company_for_write(&_tenant, &req) {
        Ok(scope) => scope,
        Err(error) => return json::error_response(&error, false),
    };
    let risk_id = path.as_str().to_string();
    let result: UnifiedResult<Value> = (|| {
        let risk = state.get_risk(&risk_id)?;
        require_record_writable_in_company_scope(&risk.company_id, &scope)?;
        state
            .risks
            .remove(&risk_id)
            .map(|_| json!({ "deleted": true, "id": &risk_id }))
            .ok_or_else(|| UnifiedError::not_found("Risk not found"))
    })();
    if result.is_ok() {
        // Plans, tasks, and alerts belong to the risk; they go
        // with it. Checklist instances linked to the risk keep
        // their records but lose the link.
        let plan_ids: Vec<String> = state
            .plans
            .iter()
            .filter(|entry| entry.value().risk_id == risk_id)
            .map(|entry| entry.key().clone())
            .collect();
        for plan_id in &plan_ids {
            state.plans.remove(plan_id);
        }
        state
            .tasks
            .retain(|_, task| task.risk_id != risk_id);
        state.alerts.retain(|_, alert| alert.risk_id != risk_id);
        for mut entry in state.instances.iter_mut() {
            if entry.value().risk_id.as_deref() == Some(risk_id.as_str()) {
                entry.value_mut().risk_id = None;
            }
        }
        state.emit(
            "risk.risk_deleted",
            json!({ "risk_id": risk_id }),
        );
    }
    json::respond(result, false)
}

/// The 5x5 risk matrix: for every likelihood (1-5) by impact
/// (1-5) cell, the score, the band, how many risks sit in it, and
/// their ids. Band thresholds are returned alongside so a client
/// never has to hard-code them.
pub(crate) async fn risk_matrix(
    state: web::Data<Arc<RiskState>>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let params = query.into_inner();
    let _tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return json::error_response(&error, false),
    };
    let (_viewer_id, viewer_role) = viewer_of(&req);
    if let Err(error) = require_capability(&state, viewer_role.as_deref(), "risk:view") {
        return json::error_response(&error, false);
    }
    let scope = match caller_company(&_tenant, &req) {
        Ok(scope) => scope,
        Err(error) => return json::error_response(&error, false),
    };
    // Read-side company filter (C3, rule A - the shared
    // `CallerScope::scoped_read_company` rule, not the write helper:
    // `require_company_match_for` is a write verify and refuses an
    // unresolved caller with no body company). The query filter names
    // a company only when it equals the caller scope; a scoped
    // caller without a filter sees their own company's risks plus
    // global ones, an unresolved caller only global ones - never
    // another company's risk ids.
    let company_filter = {
        let requested = params
            .filters
            .get("company_id")
            .cloned()
            .unwrap_or_default();
        let caller_scope = unified_core::tenancy::CallerScope::from_company(scope);
        match caller_scope.scoped_read_company(&requested) {
            Ok(resolved) => {
                if resolved.is_empty() {
                    None
                } else {
                    Some(resolved)
                }
            }
            Err(error) => return json::error_response(&error, false),
        }
    };
    let mut cells: Vec<Value> = Vec::new();
    for likelihood in 1..=5u8 {
        for impact in 1..=5u8 {
            let score = score_for(likelihood, impact);
            let risk_ids: Vec<String> = state
                .risks
                .iter()
                .filter(|entry| {
                    let risk = entry.value();
                    risk.likelihood == likelihood
                        && risk.impact == impact
                        && in_company_scope(&risk.company_id, &company_filter)
                })
                .map(|entry| entry.key().clone())
                .collect();
            cells.push(json!({
                "likelihood": likelihood,
                "impact": impact,
                "score": score,
                "band": band_for(score),
                "count": risk_ids.len(),
                "risk_ids": risk_ids,
            }));
        }
    }
    json::success_response(
        json!({
            "company_id": company_filter,
            "cells": cells,
            "bands": [
                { "band": "low", "min_score": 1, "max_score": BAND_LOW_MAX },
                { "band": "medium", "min_score": BAND_LOW_MAX + 1, "max_score": BAND_MEDIUM_MAX },
                { "band": "high", "min_score": BAND_MEDIUM_MAX + 1, "max_score": BAND_HIGH_MAX },
                { "band": "critical", "min_score": BAND_HIGH_MAX + 1, "max_score": 25 },
            ],
            "default_alert_threshold": DEFAULT_ALERT_THRESHOLD,
        }),
        200,
    )
}
#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn params_with_min_score(raw: Option<&str>) -> PaginationParams {
        let mut filters = HashMap::new();
        if let Some(raw) = raw {
            filters.insert("min_score".to_string(), raw.to_string());
        }
        PaginationParams {
            page: None,
            per_page: None,
            sort: None,
            order: None,
            search: None,
            filters,
        }
    }

    #[test]
    fn unparsable_min_score_is_a_bad_request_not_an_ignored_filter() {
        assert_eq!(parse_min_score(&params_with_min_score(None)).unwrap(), None);
        assert_eq!(
            parse_min_score(&params_with_min_score(Some("10"))).unwrap(),
            Some(10)
        );
        let error = parse_min_score(&params_with_min_score(Some("abc"))).unwrap_err();
        assert_eq!(error.code, 400);
        let error = parse_min_score(&params_with_min_score(Some("300"))).unwrap_err();
        assert_eq!(error.code, 400);
        let error = parse_min_score(&params_with_min_score(Some(""))).unwrap_err();
        assert_eq!(error.code, 400);
    }
}
