//! advisor - handlers rules.

use unified_core::json;
use unified_core::pagination::PaginationParams;
use actix_web::{web, HttpRequest, HttpResponse};
use serde_json::json;
use std::sync::Arc;
use super::{rules::*, types::*, state::*, helpers::*, handlers_suggestions::*, hooks::*, module::*};



// ---------------------------------------------------------------------------
// Handlers: rules and rule runs (read-only reference data)
// ---------------------------------------------------------------------------
pub(crate) async fn list_rules(
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
    let items: Vec<RuleDefinition> = RULE_DEFINITIONS.to_vec();
    let page = paginate(items, &params);
    json::success_response(page, 200)
}


pub(crate) async fn list_rule_runs(
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
    let mut items: Vec<RuleRun> = state
        .rule_runs
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|run| {
            // Company scope first: a scoped caller sees only
            // their own company's runs; an unresolved caller
            // sees global runs only.
            in_company_scope(&run.company_id, &scope)
                && params
                    .filters
                    .get("company_id")
                .is_none_or(|company| run.company_id.as_deref() == Some(company.as_str()))
                && params
                    .filters
                    .get("rule_id")
                    .is_none_or(|rule| run.rule_id == *rule)
        })
        .collect();
    items.sort_by(|a, b| b.ran_at.cmp(&a.ran_at));
    if params.normalized().order == "asc" {
        items.reverse();
    }
    let page = paginate(items, &params);
    json::success_response(page, 200)
}
