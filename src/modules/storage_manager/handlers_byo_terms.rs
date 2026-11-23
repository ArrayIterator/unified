// BYO terms handlers.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::json;
use unified_core::pagination::PaginationParams;
use actix_web::{web, HttpRequest, HttpResponse};
use chrono::Utc;
use serde_json::{json, Value};
use std::sync::Arc;
use uuid::Uuid;

use super::*;

// ---------------------------------------------------------------------------
// BYO terms.
// ---------------------------------------------------------------------------

pub(crate) async fn list_byo_terms(
    state: web::Data<StorageManagerState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    params: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();

    let _tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return err_response(error, debug),
    };
    let _scope = match caller_company(&_tenant, &req) {
        Ok(scope) => scope,
        Err(error) => return err_response(error, debug),
    };
    if let Err(error) = authorize(&ctx, &req, PERM_READ) {
        return err_response(error, debug);
    }
    let params = params.into_inner();
    let mut items: Vec<ByoTerms> = state
        .byo_terms
        .iter()
        .map(|entry| entry.value().clone())
        .collect();
    if let Some(filter) = params.filters.get("is_current") {
        let want = filter == "true";
        items.retain(|terms| terms.is_current == want);
    }
    if let Some(filter) = params.filters.get("enabled") {
        let want = filter == "true";
        items.retain(|terms| terms.enabled == want);
    }
    if let Some(search) = &params.search {
        let needle = search.to_lowercase();
        items.retain(|terms| {
            terms.version.to_lowercase().contains(&needle)
                || terms.title.to_lowercase().contains(&needle)
        });
    }
    items.sort_by(|a, b| b.created_at.cmp(&a.created_at));
    ok(paginate(items, &params))
}

pub(crate) async fn get_current_byo_terms(
    state: web::Data<StorageManagerState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
) -> HttpResponse {
    let debug = ctx.is_debug();

    let _tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return err_response(error, debug),
    };
    let _scope = match caller_company(&_tenant, &req) {
        Ok(scope) => scope,
        Err(error) => return err_response(error, debug),
    };
    if let Err(error) = authorize(&ctx, &req, PERM_READ) {
        return err_response(error, debug);
    }
    match current_byo_terms(&state) {
        Some(terms) => ok(terms),
        None => err_response(
            UnifiedError::not_found("No current BYO terms version is published"),
            debug,
        ),
    }
}

pub(crate) async fn create_byo_terms(
    state: web::Data<StorageManagerState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    body: web::Json<CreateByoTermsRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();

    let _tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return err_response(error, debug),
    };
    let _scope = match caller_company(&_tenant, &req) {
        Ok(scope) => scope,
        Err(error) => return err_response(error, debug),
    };
    let found = match authorize(&ctx, &req, PERM_MANAGE_BYO) {
        Ok(found) => found,
        Err(error) => return err_response(error, debug),
    };
    let result: UnifiedResult<ByoTerms> = (|| {
        let input = body.into_inner();
        let version = require_text(&input.version, "version", 32)?;
        if state
            .byo_terms
            .iter()
            .any(|entry| entry.value().version == version)
        {
            return Err(UnifiedError::conflict(format!(
                "BYO terms version \"{version}\" already exists"
            )));
        }
        let title = require_text(&input.title, "title", 256)?;
        let body_text = require_text(&input.body, "body", 100_000)?;
        let is_current = input.is_current.unwrap_or(false);
        if is_current {
            for mut entry in state.byo_terms.iter_mut() {
                entry.value_mut().is_current = false;
            }
        }
        let now = Utc::now();
        let terms = ByoTerms {
            id: Uuid::now_v7(),
            version: version.clone(),
            title,
            body: body_text,
            is_current,
            reacceptance_deadline: input.reacceptance_deadline,
            enabled: true,
            created_by: found.actor.clone(),
            created_at: now,
            updated_at: now,
        };
        state.byo_terms.insert(terms.id, terms.clone());
        state.emit(
            events::BYO_TERMS_CHANGED,
            json!({
                "scope": "terms_created",
                "terms_id": terms.id.to_string(),
                "version": version,
                "is_current": is_current,
                "created_by": found.actor,
            }),
        );
        Ok(terms)
    })();
    match result {
        Ok(terms) => created(terms),
        Err(error) => err_response(error, debug),
    }
}

pub(crate) async fn get_byo_terms(
    state: web::Data<StorageManagerState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    id: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();

    let _tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return err_response(error, debug),
    };
    let _scope = match caller_company(&_tenant, &req) {
        Ok(scope) => scope,
        Err(error) => return err_response(error, debug),
    };
    if let Err(error) = authorize(&ctx, &req, PERM_READ) {
        return err_response(error, debug);
    }
    json::respond(load_byo_terms(&state, &id), debug)
}

pub(crate) async fn update_byo_terms(
    state: web::Data<StorageManagerState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    id: web::Path<String>,
    body: web::Json<UpdateByoTermsRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();

    let _tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return err_response(error, debug),
    };
    let _scope = match caller_company(&_tenant, &req) {
        Ok(scope) => scope,
        Err(error) => return err_response(error, debug),
    };
    let found = match authorize(&ctx, &req, PERM_MANAGE_BYO) {
        Ok(found) => found,
        Err(error) => return err_response(error, debug),
    };
    let result: UnifiedResult<ByoTerms> = (|| {
        let input = body.into_inner();
        let mut terms = load_byo_terms(&state, &id)?;
        if let Some(title) = &input.title {
            terms.title = require_text(title, "title", 256)?;
        }
        if let Some(body_text) = &input.body {
            terms.body = require_text(body_text, "body", 100_000)?;
        }
        if let Some(is_current) = input.is_current {
            if is_current {
                for mut entry in state.byo_terms.iter_mut() {
                    if entry.value().id != terms.id {
                        entry.value_mut().is_current = false;
                    }
                }
            }
            terms.is_current = is_current;
        }
        if let Some(deadline) = input.reacceptance_deadline {
            terms.reacceptance_deadline = deadline;
        }
        if let Some(enabled) = input.enabled {
            terms.enabled = enabled;
        }
        terms.updated_at = Utc::now();
        state.byo_terms.insert(terms.id, terms.clone());
        state.emit(
            events::BYO_TERMS_CHANGED,
            json!({
                "scope": "terms_updated",
                "terms_id": terms.id.to_string(),
                "version": terms.version,
                "is_current": terms.is_current,
                "reacceptance_deadline": terms.reacceptance_deadline.map(|at| at.to_rfc3339()),
                "changed_by": found.actor,
            }),
        );
        Ok(terms)
    })();
    json::respond(result, debug)
}

pub(crate) async fn delete_byo_terms(
    state: web::Data<StorageManagerState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    id: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();

    let _tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return err_response(error, debug),
    };
    let _scope = match caller_company(&_tenant, &req) {
        Ok(scope) => scope,
        Err(error) => return err_response(error, debug),
    };
    let found = match authorize(&ctx, &req, PERM_MANAGE_BYO) {
        Ok(found) => found,
        Err(error) => return err_response(error, debug),
    };
    let result: UnifiedResult<Value> = (|| {
        let terms = load_byo_terms(&state, &id)?;
        if terms.is_current {
            return Err(UnifiedError::conflict(
                "The current BYO terms version cannot be deleted; publish another version as current first",
            ));
        }
        if state
            .byo_acceptances
            .iter()
            .any(|entry| entry.value().terms_id == terms.id)
        {
            return Err(UnifiedError::conflict(format!(
                "BYO terms version \"{}\" cannot be deleted: users have already accepted it",
                terms.version
            )));
        }
        state.byo_terms.remove(&terms.id);
        state.emit(
            events::BYO_TERMS_CHANGED,
            json!({
                "scope": "terms_deleted",
                "terms_id": terms.id.to_string(),
                "version": terms.version,
                "deleted_by": found.actor,
            }),
        );
        Ok(json!({ "deleted": true, "terms_id": terms.id }))
    })();
    json::respond(result, debug)
}

/// Record the caller's acceptance of the current BYO terms
/// version. The acceptance is what activation checks later.
pub(crate) async fn accept_byo_terms(
    state: web::Data<StorageManagerState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    body: web::Json<AcceptTermsRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();

    let _tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return err_response(error, debug),
    };
    let _scope = match caller_company(&_tenant, &req) {
        Ok(scope) => scope,
        Err(error) => return err_response(error, debug),
    };
    let found = match authorize(&ctx, &req, PERM_BYO_USE) {
        Ok(found) => found,
        Err(error) => return err_response(error, debug),
    };
    let result: UnifiedResult<ByoTermsAcceptance> = (|| {
        let input = body.into_inner();
        let terms = current_byo_terms(&state).ok_or_else(|| {
            UnifiedError::not_found("No current BYO terms version is published")
        })?;
        if let Some(connection_id) = input.connection_id {
            let connection = load_byo_connection(&state, &connection_id.to_string())?;
            if connection.owner_user != found.actor {
                return Err(UnifiedError::not_found(format!(
                    "BYO connection not found: {connection_id}"
                )));
            }
        }
        let now = Utc::now();
        let acceptance = ByoTermsAcceptance {
            id: Uuid::now_v7(),
            terms_id: terms.id,
            terms_version: terms.version.clone(),
            user_id: found.actor.clone(),
            // Tenant scoping: the acceptance is recorded under the
            // caller's verified company.
            company_id: _scope.clone(),
            connection_id: input.connection_id,
            accepted_at: now,
        };
        state.byo_acceptances.insert(
            format!("{}|{}", acceptance.user_id, acceptance.terms_version),
            acceptance.clone(),
        );
        if let Some(connection_id) = input.connection_id {
            if let Some(mut connection) = state.byo_connections.get_mut(&connection_id) {
                connection.terms_version = Some(terms.version.clone());
                connection.terms_accepted_at = Some(now);
                connection.updated_at = now;
            }
        }
        state.emit(
            events::BYO_TERMS_ACCEPTED,
            json!({
                "terms_id": terms.id.to_string(),
                "terms_version": terms.version,
                "user_id": found.actor,
                "connection_id": input.connection_id.map(|id| id.to_string()),
                "accepted_at": now.to_rfc3339(),
            }),
        );
        Ok(acceptance)
    })();
    match result {
        Ok(acceptance) => created(acceptance),
        Err(error) => err_response(error, debug),
    }
}

/// Suspend connections whose owner has not accepted the current
/// terms version after its re-acceptance deadline. Existing
/// connections keep working until the deadline; this endpoint (or
/// a scheduler run calling the same check) enforces it afterwards.
/// Only an admin may run it.
pub(crate) async fn enforce_byo_reacceptance(
    state: web::Data<StorageManagerState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
) -> HttpResponse {
    let debug = ctx.is_debug();

    let _tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return err_response(error, debug),
    };
    let _scope = match caller_company(&_tenant, &req) {
        Ok(scope) => scope,
        Err(error) => return err_response(error, debug),
    };
    let found = match authorize(&ctx, &req, PERM_MANAGE_BYO) {
        Ok(found) => found,
        Err(error) => return err_response(error, debug),
    };
    let result: UnifiedResult<Value> = (|| {
        let terms = current_byo_terms(&state).ok_or_else(|| {
            UnifiedError::not_found("No current BYO terms version is published")
        })?;
        let Some(deadline) = terms.reacceptance_deadline else {
            return Ok(json!({
                "terms_version": terms.version,
                "deadline": Value::Null,
                "suspended": 0,
                "suspended_connection_ids": Vec::<String>::new(),
                "note": "No re-acceptance deadline is set for the current terms version",
            }));
        };
        if Utc::now() <= deadline {
            return Ok(json!({
                "terms_version": terms.version,
                "deadline": deadline.to_rfc3339(),
                "suspended": 0,
                "suspended_connection_ids": Vec::<String>::new(),
                "note": "Re-acceptance deadline has not passed yet",
            }));
        }
        // Two passes: collect the connections to suspend first
        // (shared borrows only), then mutate them. Holding a map
        // guard while calling back into state would not compile.
        let mut to_suspend: Vec<Uuid> = Vec::new();
        for entry in state.byo_connections.iter() {
            let connection = entry.value();
            if (connection.status == "active" || connection.status == "validated")
                && !has_accepted_terms(&state, &connection.owner_user, &terms.version)
            {
                to_suspend.push(connection.id);
            }
        }
        let mut suspended: Vec<String> = Vec::new();
        for id in to_suspend {
            let (name, owner_user, company_id) = {
                let Some(mut connection) = state.byo_connections.get_mut(&id) else {
                    continue;
                };
                connection.status = "suspended".to_string();
                connection.status_reason = Some(format!(
                    "Suspended: terms version {} was not accepted before the re-acceptance deadline {}",
                    terms.version,
                    deadline.to_rfc3339()
                ));
                connection.updated_at = Utc::now();
                (
                    connection.name.clone(),
                    connection.owner_user.clone(),
                    connection.company_id.clone(),
                )
            };
            suspended.push(id.to_string());
            state.emit(
                events::BYO_SUSPENDED,
                json!({
                    "connection_id": id.to_string(),
                    "name": name,
                    "owner_user": owner_user,
                    "company_id": company_id,
                    "reason": "terms_reacceptance_deadline",
                    "terms_version": terms.version,
                    "suspended_by": found.actor,
                }),
            );
        }
        Ok(json!({
            "terms_version": terms.version,
            "deadline": deadline.to_rfc3339(),
            "suspended": suspended.len(),
            "suspended_connection_ids": suspended,
        }))
    })();
    json::respond(result, debug)
}
