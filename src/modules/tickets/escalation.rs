// Tickets module — escalation rule handlers.
// Split from the former single-file module; items keep their original names and behaviour. Private items are `pub(crate)` so sibling files in this module can use them.

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
use std::collections::HashSet;
use std::sync::{Arc, RwLock};
use uuid::Uuid;
use super::*;

// ---------------------------------------------------------------------------
// Escalation rules
// ---------------------------------------------------------------------------

pub(crate) fn validate_escalation_trigger(trigger: &str) -> UnifiedResult<()> {
    if !matches!(trigger, "sla_breach" | "no_response") {
        return Err(UnifiedError::bad_request(
            "trigger must be one of: sla_breach, no_response",
        ));
    }
    Ok(())
}

pub(crate) async fn list_escalation_rules(
    state: web::Data<Arc<TicketsState>>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let _tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return json::error_response(&error, false),
    };
    let _scope = match caller_company(&_tenant, &req) {
        Ok(scope) => scope,
        Err(error) => return json::error_response(&error, false),
    };

    let _caller = match authenticate(&req, &state, PERM_TICKETS_READ) {
        Ok(caller) => caller,
        Err(response) => return response,
    };
    let params = query.into_inner();
    let mut items: Vec<EscalationRule> = state
        .escalation_rules
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|rule| {
            matches_search(&format!("{} {}", rule.name, rule.trigger), &params)
                && params.filters.get("trigger").is_none_or(|trigger| {
                    rule.trigger == *trigger
                })
                && params.filters.get("department_id").is_none_or(|department| {
                    rule.department_id.as_deref() == Some(department.as_str())
                })
                && params
                    .filters
                    .get("enabled")
                    .is_none_or(|flag| rule.enabled == (flag == "true"))
        })
        .collect();
    items.sort_by(|a, b| {
        a.sort_order
            .cmp(&b.sort_order)
            .then(a.created_at.cmp(&b.created_at))
    });
    if params.normalized().order == "desc" {
        items.reverse();
    }
    let page = paginate(items, &params);
    json::success_response(page, 200)
}

pub(crate) async fn create_escalation_rule(
    state: web::Data<Arc<TicketsState>>,
    req: HttpRequest,
    body: web::Json<CreateEscalationRule>,
) -> HttpResponse {
    let _tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return json::error_response(&error, false),
    };
    let _scope = match caller_company(&_tenant, &req) {
        Ok(scope) => scope,
        Err(error) => return json::error_response(&error, false),
    };

    let _caller = match authenticate(&req, &state, PERM_TICKETS_MANAGE) {
        Ok(caller) => caller,
        Err(response) => return response,
    };
    let result: UnifiedResult<EscalationRule> = (|| {
        let input = body.into_inner();
        if input.name.trim().is_empty() {
            return Err(UnifiedError::bad_request("Rule name must not be empty"));
        }
        validate_escalation_trigger(&input.trigger)?;
        if input.trigger == "no_response" && input.threshold_minutes.is_none() {
            return Err(UnifiedError::bad_request(
                "threshold_minutes is required for the no_response trigger",
            ));
        }
        if let Some(to_priority) = &input.to_priority {
            if !valid_priority(to_priority) {
                return Err(UnifiedError::bad_request(
                    "to_priority must be one of: low, normal, high, urgent",
                ));
            }
        }
        if let Some(from_priority) = &input.from_priority {
            if !valid_priority(from_priority) {
                return Err(UnifiedError::bad_request(
                    "from_priority must be one of: low, normal, high, urgent",
                ));
            }
        }
        let rule = EscalationRule {
            id: new_id(),
            name: input.name.trim().to_string(),
            department_id: input
                .department_id
                .map(|value| value.trim().to_string())
                .filter(|value| !value.is_empty()),
            trigger: input.trigger,
            sort_order: input.sort_order.unwrap_or(0),
            threshold_minutes: input.threshold_minutes,
            from_priority: input.from_priority,
            to_priority: input.to_priority,
            assign_to: input
                .assign_to
                .map(|value| value.trim().to_string())
                .filter(|value| !value.is_empty()),
            escalate_to_department_id: input
                .escalate_to_department_id
                .map(|value| value.trim().to_string())
                .filter(|value| !value.is_empty()),
            enabled: input.enabled.unwrap_or(true),
            created_at: now(),
            updated_at: now(),
        };
        state
            .escalation_rules
            .insert(rule.id.clone(), rule.clone());
        Ok(rule)
    })();
    respond_created(result)
}

pub(crate) async fn get_escalation_rule(
    state: web::Data<Arc<TicketsState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let _tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return json::error_response(&error, false),
    };
    let _scope = match caller_company(&_tenant, &req) {
        Ok(scope) => scope,
        Err(error) => return json::error_response(&error, false),
    };

    let _caller = match authenticate(&req, &state, PERM_TICKETS_READ) {
        Ok(caller) => caller,
        Err(response) => return response,
    };
    let result: UnifiedResult<EscalationRule> = state
        .escalation_rules
        .get(path.as_str())
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found("Escalation rule not found"));
    json::respond(result, false)
}

pub(crate) async fn update_escalation_rule(
    state: web::Data<Arc<TicketsState>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdateEscalationRule>,
) -> HttpResponse {
    let _tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return json::error_response(&error, false),
    };
    let _scope = match caller_company(&_tenant, &req) {
        Ok(scope) => scope,
        Err(error) => return json::error_response(&error, false),
    };

    let _caller = match authenticate(&req, &state, PERM_TICKETS_MANAGE) {
        Ok(caller) => caller,
        Err(response) => return response,
    };
    let result: UnifiedResult<EscalationRule> = (|| {
        let input = body.into_inner();
        let mut rule = state
            .escalation_rules
            .get_mut(path.as_str())
            .ok_or_else(|| UnifiedError::not_found("Escalation rule not found"))?;
        if let Some(name) = input.name {
            if name.trim().is_empty() {
                return Err(UnifiedError::bad_request("Rule name must not be empty"));
            }
            rule.name = name.trim().to_string();
        }
        if let Some(trigger) = input.trigger {
            validate_escalation_trigger(&trigger)?;
            rule.trigger = trigger;
        }
        if let Some(sort_order) = input.sort_order {
            rule.sort_order = sort_order;
        }
        if let Some(threshold) = input.threshold_minutes {
            rule.threshold_minutes = Some(threshold);
        }
        if let Some(from_priority) = input.from_priority {
            if !valid_priority(&from_priority) {
                return Err(UnifiedError::bad_request(
                    "from_priority must be one of: low, normal, high, urgent",
                ));
            }
            rule.from_priority = Some(from_priority);
        }
        if let Some(to_priority) = input.to_priority {
            if !valid_priority(&to_priority) {
                return Err(UnifiedError::bad_request(
                    "to_priority must be one of: low, normal, high, urgent",
                ));
            }
            rule.to_priority = Some(to_priority);
        }
        if let Some(assign_to) = input.assign_to {
            rule.assign_to = Some(assign_to);
        }
        if let Some(department_id) = input.escalate_to_department_id {
            rule.escalate_to_department_id = Some(department_id);
        }
        if let Some(department_id) = input.department_id {
            rule.department_id = Some(department_id);
        }
        if let Some(enabled) = input.enabled {
            rule.enabled = enabled;
        }
        rule.updated_at = now();
        Ok(rule.clone())
    })();
    json::respond(result, false)
}

pub(crate) async fn delete_escalation_rule(
    state: web::Data<Arc<TicketsState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let _tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return json::error_response(&error, false),
    };
    let _scope = match caller_company(&_tenant, &req) {
        Ok(scope) => scope,
        Err(error) => return json::error_response(&error, false),
    };

    let _caller = match authenticate(&req, &state, PERM_TICKETS_MANAGE) {
        Ok(caller) => caller,
        Err(response) => return response,
    };
    let result: UnifiedResult<Value> = (|| {
        state
            .escalation_rules
            .remove(path.as_str())
            .map(|_| json!({ "deleted": true, "id": path.as_str() }))
            .ok_or_else(|| UnifiedError::not_found("Escalation rule not found"))
    })();
    json::respond(result, false)
}
