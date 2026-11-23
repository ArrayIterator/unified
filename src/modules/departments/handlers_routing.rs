//! HTTP handlers for routing rules and route previews.

use super::helpers::{
    PERM_DEPARTMENTS_CREATE, PERM_DEPARTMENTS_MANAGE, PERM_DEPARTMENTS_VIEW, authorize,
    caller_company, require_tenant,
};
use super::state::DepartmentsState;
use super::types::{CreateRoutingRule, RoutePreview, RoutingRule, UpdateRoutingRule};
use super::validation::{new_id, now, paginate, respond_created, valid_match_type};
use actix_web::{HttpRequest, HttpResponse, web};
use serde_json::{Value, json};
use std::sync::Arc;
use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::json;
use unified_core::pagination::PaginationParams;

pub(crate) async fn list_rules_for_department(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<DepartmentsState>>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let _tenant = match require_tenant(&req) {
        Ok(value) => value,
        Err(error) => return json::error_response(&error, ctx.is_debug()),
    };
    let _scope = match caller_company(&_tenant, &req) {
        Ok(value) => value,
        Err(error) => return json::error_response(&error, ctx.is_debug()),
    };
    if let Err(error) = authorize(&ctx, &req, PERM_DEPARTMENTS_VIEW) {
        return json::error_response(&error, ctx.is_debug());
    }
    let params = query.into_inner();
    let department_id = path.as_str().to_string();
    if !state.departments.contains_key(department_id.as_str()) {
        return json::error_response(
            &UnifiedError::not_found("Department not found"),
            ctx.is_debug(),
        );
    }
    let mut items: Vec<RoutingRule> = state
        .rules
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|rule| {
            rule.department_id == department_id
                && params
                    .filters
                    .get("match_type")
                    .is_none_or(|match_type| rule.match_type == *match_type)
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

pub(crate) async fn list_rules(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<DepartmentsState>>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let _tenant = match require_tenant(&req) {
        Ok(value) => value,
        Err(error) => return json::error_response(&error, ctx.is_debug()),
    };
    let _scope = match caller_company(&_tenant, &req) {
        Ok(value) => value,
        Err(error) => return json::error_response(&error, ctx.is_debug()),
    };
    if let Err(error) = authorize(&ctx, &req, PERM_DEPARTMENTS_VIEW) {
        return json::error_response(&error, ctx.is_debug());
    }
    let params = query.into_inner();
    let mut items: Vec<RoutingRule> = state
        .rules
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|rule| {
            params
                .filters
                .get("department_id")
                .is_none_or(|department| rule.department_id == *department)
                && params
                    .filters
                    .get("match_type")
                    .is_none_or(|match_type| rule.match_type == *match_type)
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

pub(crate) async fn create_rule(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<DepartmentsState>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<CreateRoutingRule>,
) -> HttpResponse {
    let _tenant = match require_tenant(&req) {
        Ok(value) => value,
        Err(error) => return json::error_response(&error, ctx.is_debug()),
    };
    let _scope = match caller_company(&_tenant, &req) {
        Ok(value) => value,
        Err(error) => return json::error_response(&error, ctx.is_debug()),
    };
    if let Err(error) = authorize(&ctx, &req, PERM_DEPARTMENTS_CREATE) {
        return json::error_response(&error, ctx.is_debug());
    }
    let result: UnifiedResult<RoutingRule> = (|| {
        let department_id = path.as_str().to_string();
        if !state.departments.contains_key(department_id.as_str()) {
            return Err(UnifiedError::not_found("Department not found"));
        }
        let input = body.into_inner();
        if !valid_match_type(&input.match_type) {
            return Err(UnifiedError::bad_request(
                "match_type must be one of: category, product, keyword",
            ));
        }
        if input.match_value.trim().is_empty() {
            return Err(UnifiedError::bad_request("match_value must not be empty"));
        }
        let rule = RoutingRule {
            id: new_id(),
            department_id,
            match_type: input.match_type,
            match_value: input.match_value.trim().to_string(),
            sort_order: input.sort_order.unwrap_or(0),
            enabled: input.enabled.unwrap_or(true),
            created_at: now(),
            updated_at: now(),
        };
        state.rules.insert(rule.id.clone(), rule.clone());
        let value = serde_json::to_value(&rule).unwrap_or(Value::Null);
        state.emit("department.rule.created", value);
        Ok(rule)
    })();
    respond_created(result)
}

pub(crate) async fn get_rule(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<DepartmentsState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let _tenant = match require_tenant(&req) {
        Ok(value) => value,
        Err(error) => return json::error_response(&error, ctx.is_debug()),
    };
    let _scope = match caller_company(&_tenant, &req) {
        Ok(value) => value,
        Err(error) => return json::error_response(&error, ctx.is_debug()),
    };
    if let Err(error) = authorize(&ctx, &req, PERM_DEPARTMENTS_VIEW) {
        return json::error_response(&error, ctx.is_debug());
    }
    let result: UnifiedResult<RoutingRule> = state
        .rules
        .get(path.as_str())
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found("Routing rule not found"));
    json::respond(result, ctx.is_debug())
}

pub(crate) async fn update_rule(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<DepartmentsState>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdateRoutingRule>,
) -> HttpResponse {
    let _tenant = match require_tenant(&req) {
        Ok(value) => value,
        Err(error) => return json::error_response(&error, ctx.is_debug()),
    };
    let _scope = match caller_company(&_tenant, &req) {
        Ok(value) => value,
        Err(error) => return json::error_response(&error, ctx.is_debug()),
    };
    if let Err(error) = authorize(&ctx, &req, PERM_DEPARTMENTS_MANAGE) {
        return json::error_response(&error, ctx.is_debug());
    }
    let result: UnifiedResult<RoutingRule> = (|| {
        let input = body.into_inner();
        if let Some(match_type) = &input.match_type {
            if !valid_match_type(match_type) {
                return Err(UnifiedError::bad_request(
                    "match_type must be one of: category, product, keyword",
                ));
            }
        }
        let mut rule = state
            .rules
            .get_mut(path.as_str())
            .ok_or_else(|| UnifiedError::not_found("Routing rule not found"))?;
        if let Some(match_type) = input.match_type {
            rule.match_type = match_type;
        }
        if let Some(match_value) = input.match_value {
            if match_value.trim().is_empty() {
                return Err(UnifiedError::bad_request("match_value must not be empty"));
            }
            rule.match_value = match_value.trim().to_string();
        }
        if let Some(sort_order) = input.sort_order {
            rule.sort_order = sort_order;
        }
        if let Some(enabled) = input.enabled {
            rule.enabled = enabled;
        }
        rule.updated_at = now();
        let value = serde_json::to_value(&*rule).unwrap_or(Value::Null);
        let updated = rule.clone();
        drop(rule);
        state.emit("department.rule.updated", value);
        Ok(updated)
    })();
    json::respond(result, ctx.is_debug())
}

pub(crate) async fn delete_rule(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<DepartmentsState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let _tenant = match require_tenant(&req) {
        Ok(value) => value,
        Err(error) => return json::error_response(&error, ctx.is_debug()),
    };
    let _scope = match caller_company(&_tenant, &req) {
        Ok(value) => value,
        Err(error) => return json::error_response(&error, ctx.is_debug()),
    };
    if let Err(error) = authorize(&ctx, &req, PERM_DEPARTMENTS_MANAGE) {
        return json::error_response(&error, ctx.is_debug());
    }
    let result: UnifiedResult<Value> = (|| {
        state
            .rules
            .remove(path.as_str())
            .map(|_| json!({ "deleted": true, "id": path.as_str() }))
            .ok_or_else(|| UnifiedError::not_found("Routing rule not found"))
    })();
    if result.is_ok() {
        state.emit(
            "department.rule.deleted",
            json!({ "rule_id": path.as_str() }),
        );
    }
    json::respond(result, ctx.is_debug())
}

/// Dry-run routing for a ticket draft: which department would receive
/// it, and through which rule (if any).
pub(crate) async fn route_preview(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<DepartmentsState>>,
    req: HttpRequest,
    body: web::Json<RoutePreview>,
) -> HttpResponse {
    let _tenant = match require_tenant(&req) {
        Ok(value) => value,
        Err(error) => return json::error_response(&error, ctx.is_debug()),
    };
    let _scope = match caller_company(&_tenant, &req) {
        Ok(value) => value,
        Err(error) => return json::error_response(&error, ctx.is_debug()),
    };
    if let Err(error) = authorize(&ctx, &req, PERM_DEPARTMENTS_VIEW) {
        return json::error_response(&error, ctx.is_debug());
    }
    let input = body.into_inner();
    let (department_id, matched_rule_id) = state.route_for(
        input.title.as_deref().unwrap_or(""),
        input.body.as_deref().unwrap_or(""),
        input.category.as_deref().unwrap_or("general"),
        input.product.as_deref(),
    );
    let matched_via = match (&department_id, &matched_rule_id) {
        (Some(_), Some(_)) => "rule",
        (Some(_), None) => "default",
        (None, _) => "none",
    };
    json::success_response(
        json!({
            "department_id": department_id,
            "matched_rule_id": matched_rule_id,
            "matched_via": matched_via,
        }),
        200,
    )
}
