//! advisor - handlers suggestions.

use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::json;
use unified_core::pagination::PaginationParams;
use actix_web::{web, HttpRequest, HttpResponse};
use chrono::{ Duration, Utc };
use serde_json::{json, Value};
use std::sync::Arc;
use super::{rules::*, types::*, state::*, helpers::*, handlers_rules::*, hooks::*, module::*};



// ---------------------------------------------------------------------------
// Handlers: suggestions
// ---------------------------------------------------------------------------
pub(crate) async fn list_suggestions(
    state: web::Data<Arc<AdvisorState>>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let params = query.into_inner();
    let _tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return json::error_response(&error, false),
    };
    let caller = match caller(&req) {
        Ok(caller) => caller,
        Err(error) => return json::error_response(&error, false),
    };
    if let Err(error) = require_capability(&state, Some(caller.role.as_str()), "advisor:view") {
        return json::error_response(&error, false);
    }
    let scope = match caller_company(&_tenant, &req) {
        Ok(scope) => scope,
        Err(error) => return json::error_response(&error, false),
    };
    let mut items: Vec<Suggestion> = state
        .suggestions
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|suggestion| {
            // Company scope first: a scoped caller sees only
            // their own company's suggestions; an unresolved
            // caller sees global suggestions only.
            in_company_scope(&suggestion.company_id, &scope)
                && matches_search(
                &format!("{} {} {} {}", suggestion.rule_id, suggestion.title, suggestion.body, suggestion.recommended_action),
                &params,
            ) && params
                .filters
                .get("severity")
                .is_none_or(|severity| suggestion.severity == *severity)
                && params
                    .filters
                    .get("status")
                    .is_none_or(|status| suggestion.status == *status)
                && params
                    .filters
                    .get("company_id")
                    .is_none_or(|company| suggestion.company_id.as_deref() == Some(company.as_str()))
                && params
                    .filters
                    .get("rule_id")
                    .is_none_or(|rule| suggestion.rule_id == *rule)
        })
        .collect();
    items.sort_by(|a, b| b.created_at.cmp(&a.created_at));
    if params.normalized().order == "asc" {
        items.reverse();
    }
    let page = paginate(items, &params);
    json::success_response(page, 200)
}


pub(crate) async fn get_suggestion(
    state: web::Data<Arc<AdvisorState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let _tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return json::error_response(&error, false),
    };
    let caller = match caller(&req) {
        Ok(caller) => caller,
        Err(error) => return json::error_response(&error, false),
    };
    if let Err(error) = require_capability(&state, Some(caller.role.as_str()), "advisor:view") {
        return json::error_response(&error, false);
    }
    let scope = match caller_company(&_tenant, &req) {
        Ok(scope) => scope,
        Err(error) => return json::error_response(&error, false),
    };
    let result: UnifiedResult<Suggestion> = (|| {
        let suggestion = state
            .suggestions
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Suggestion not found"))?;
        require_record_in_company_scope(&suggestion.company_id, &scope)?;
        Ok(suggestion)
    })();
    json::respond(result, false)
}


pub(crate) async fn re_evaluate(
    state: web::Data<Arc<AdvisorState>>,
    req: HttpRequest,
    body: web::Json<EvaluateRequest>,
) -> HttpResponse {
    let _tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return json::error_response(&error, false),
    };
    let caller = match caller(&req) {
        Ok(caller) => caller,
        Err(error) => return json::error_response(&error, false),
    };
    if let Err(error) = require_capability(&state, Some(caller.role.as_str()), "advisor:manage") {
        return json::error_response(&error, false);
    }
    let scope = match caller_company(&_tenant, &req) {
        Ok(scope) => scope,
        Err(error) => return json::error_response(&error, false),
    };
    let input = body.into_inner();
    // `evaluate` treats a missing company as "all companies" — the
    // system-wide licensing-expiry sweep in `hooks.rs` relies on
    // that shape, so the gate lives HERE, at the handler: the
    // company to evaluate is resolved from the caller scope first,
    // and an unresolved caller may never evaluate at all.
    let requested_company = input
        .company_id
        .clone()
        .filter(|value| !value.trim().is_empty());
    let company_id = match require_company_match_for(&scope, &requested_company) {
        Ok(Some(company)) => company,
        Ok(None) => {
            return json::error_response(
                &UnifiedError::forbidden(
                    "A company scope is required to re-evaluate suggestions",
                ),
                false,
            );
        }
        Err(error) => return json::error_response(&error, false),
    };
    for rule_id in &input.rules {
        if !RULE_DEFINITIONS.iter().any(|rule| rule.id == rule_id) {
            return json::error_response(
                &UnifiedError::bad_request(format!("Unknown rule id: {rule_id}")),
                false,
            );
        }
    }
    for activity in &input.crm_activities {
        if activity.contact_id.trim().is_empty() {
            return json::error_response(
                &UnifiedError::bad_request("crm_activities entries require contact_id"),
                false,
            );
        }
        // Snapshots are stored per contact: a snapshot naming
        // another company would pollute that company's facts, so
        // each snapshot's company is verified against the resolved
        // scope and stamped with it when omitted.
        let mut activity = activity.clone();
        activity.company_id = match require_company_match_for(&scope, &activity.company_id) {
            Ok(company) => company,
            Err(error) => return json::error_response(&error, false),
        };
        state
            .crm_activities
            .insert(activity.contact_id.clone(), activity);
    }
    for subscription in &input.subscriptions {
        if subscription.subscription_id.trim().is_empty() {
            return json::error_response(
                &UnifiedError::bad_request("subscriptions entries require subscription_id"),
                false,
            );
        }
        let mut subscription = subscription.clone();
        subscription.company_id =
            match require_company_match_for(&scope, &subscription.company_id) {
                Ok(company) => company,
                Err(error) => return json::error_response(&error, false),
            };
        if subscription.seats_used > subscription.seats_purchased {
            return json::error_response(
                &UnifiedError::bad_request(
                    "subscriptions entries require seats_used <= seats_purchased",
                ),
                false,
            );
        }
        if subscription.amount < 0 {
            return json::error_response(
                &UnifiedError::bad_request("subscriptions entries require amount >= 0"),
                false,
            );
        }
        state
            .subscriptions
            .insert(subscription.subscription_id.clone(), subscription);
    }
    let (created, updated, evaluated, gaps) =
        state.evaluate(Some(&company_id), &input.rules);
    let _ = caller.actor;
    json::success_response(
        json!({
            "company_id": company_id,
            "rules_evaluated": evaluated,
            "suggestions_created": created,
            "suggestions_updated": updated,
            "data_gaps": gaps,
        }),
        200,
    )
}


pub(crate) async fn acknowledge_suggestion(
    state: web::Data<Arc<AdvisorState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let _tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return json::error_response(&error, false),
    };
    let caller = match caller(&req) {
        Ok(caller) => caller,
        Err(error) => return json::error_response(&error, false),
    };
    if let Err(error) = require_capability(&state, Some(caller.role.as_str()), "advisor:manage") {
        return json::error_response(&error, false);
    }
    let scope = match caller_company(&_tenant, &req) {
        Ok(scope) => scope,
        Err(error) => return json::error_response(&error, false),
    };
    let result: UnifiedResult<Suggestion> = (|| {
        let mut suggestion = state
            .suggestions
            .get_mut(path.as_str())
            .ok_or_else(|| UnifiedError::not_found("Suggestion not found"))?;
        require_record_in_company_scope(&suggestion.company_id, &scope)?;
        if suggestion.status == "dismissed" {
            return Err(UnifiedError::conflict(
                "A dismissed suggestion cannot be acknowledged; re-evaluation creates a fresh one when the rule still fires",
            ));
        }
        suggestion.status = "acknowledged".to_string();
        suggestion.acknowledged_by = Some(caller.actor.clone());
        suggestion.acknowledged_at = Some(Utc::now());
        suggestion.updated_at = Utc::now();
        let value = serde_json::to_value(&*suggestion).unwrap_or(Value::Null);
        let updated = suggestion.clone();
        drop(suggestion);
        state.emit("advisor.suggestion_acknowledged", value);
        Ok(updated)
    })();
    json::respond(result, false)
}


pub(crate) async fn dismiss_suggestion(
    state: web::Data<Arc<AdvisorState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let _tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return json::error_response(&error, false),
    };
    let caller = match caller(&req) {
        Ok(caller) => caller,
        Err(error) => return json::error_response(&error, false),
    };
    if let Err(error) = require_capability(&state, Some(caller.role.as_str()), "advisor:manage") {
        return json::error_response(&error, false);
    }
    let scope = match caller_company(&_tenant, &req) {
        Ok(scope) => scope,
        Err(error) => return json::error_response(&error, false),
    };
    let result: UnifiedResult<Suggestion> = (|| {
        let mut suggestion = state
            .suggestions
            .get_mut(path.as_str())
            .ok_or_else(|| UnifiedError::not_found("Suggestion not found"))?;
        require_record_in_company_scope(&suggestion.company_id, &scope)?;
        suggestion.status = "dismissed".to_string();
        suggestion.dismissed_by = Some(caller.actor.clone());
        suggestion.dismissed_at = Some(Utc::now());
        suggestion.updated_at = Utc::now();
        let value = serde_json::to_value(&*suggestion).unwrap_or(Value::Null);
        let updated = suggestion.clone();
        drop(suggestion);
        state.emit("advisor.suggestion_dismissed", value);
        Ok(updated)
    })();
    json::respond(result, false)
}


pub(crate) async fn snooze_suggestion(
    state: web::Data<Arc<AdvisorState>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<SnoozeRequest>,
) -> HttpResponse {
    let _tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return json::error_response(&error, false),
    };
    let caller = match caller(&req) {
        Ok(caller) => caller,
        Err(error) => return json::error_response(&error, false),
    };
    if let Err(error) = require_capability(&state, Some(caller.role.as_str()), "advisor:manage") {
        return json::error_response(&error, false);
    }
    let scope = match caller_company(&_tenant, &req) {
        Ok(scope) => scope,
        Err(error) => return json::error_response(&error, false),
    };
    let input = body.into_inner();
    let result: UnifiedResult<Suggestion> = (|| {
        let until = match (input.snoozed_until, input.snooze_days) {
            (Some(until), _) => until,
            (None, Some(days)) => Utc::now() + Duration::days(i64::from(days)),
            (None, None) => {
                return Err(UnifiedError::bad_request(
                    "Provide snoozed_until or snooze_days",
                ));
            }
        };
        if until <= Utc::now() {
            return Err(UnifiedError::bad_request(
                "snoozed_until must be in the future",
            ));
        }
        let mut suggestion = state
            .suggestions
            .get_mut(path.as_str())
            .ok_or_else(|| UnifiedError::not_found("Suggestion not found"))?;
        require_record_in_company_scope(&suggestion.company_id, &scope)?;
        if suggestion.status == "dismissed" {
            return Err(UnifiedError::conflict(
                "A dismissed suggestion cannot be snoozed",
            ));
        }
        suggestion.status = "snoozed".to_string();
        suggestion.snoozed_until = Some(until);
        suggestion.updated_at = Utc::now();
        let value = serde_json::to_value(&*suggestion).unwrap_or(Value::Null);
        let updated = suggestion.clone();
        drop(suggestion);
        state.emit("advisor.suggestion_snoozed", value);
        Ok(updated)
    })();
    json::respond(result, false)
}
