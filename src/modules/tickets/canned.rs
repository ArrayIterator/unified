// Tickets module — canned response handlers.
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
// Canned responses
// ---------------------------------------------------------------------------

pub(crate) async fn list_canned_responses(
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
    let mut items: Vec<CannedResponse> = state
        .canned_responses
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|response| {
            matches_search(&format!("{} {} {}", response.title, response.body, response.category), &params)
                && params
                    .filters
                    .get("category")
                    .is_none_or(|category| response.category == *category)
                && params
                    .filters
                    .get("department_id")
                    .is_none_or(|department| {
                        response.department_id.as_deref() == Some(department.as_str())
                    })
        })
        .collect();
    items.sort_by(|a, b| a.title.cmp(&b.title));
    if params.normalized().order == "desc" {
        items.reverse();
    }
    let page = paginate(items, &params);
    json::success_response(page, 200)
}

pub(crate) async fn create_canned_response(
    state: web::Data<Arc<TicketsState>>,
    req: HttpRequest,
    body: web::Json<CreateCannedResponse>,
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
    let result: UnifiedResult<CannedResponse> = (|| {
        let input = body.into_inner();
        if input.title.trim().is_empty() {
            return Err(UnifiedError::bad_request(
                "Canned response title must not be empty",
            ));
        }
        if input.body.trim().is_empty() {
            return Err(UnifiedError::bad_request("Canned response body must not be empty"));
        }
        let response = CannedResponse {
            id: new_id(),
            title: input.title.trim().to_string(),
            body: input.body.trim().to_string(),
            category: input
                .category
                .map(|value| value.trim().to_string())
                .filter(|value| !value.is_empty())
                .unwrap_or_else(|| "general".to_string()),
            department_id: input
                .department_id
                .map(|value| value.trim().to_string())
                .filter(|value| !value.is_empty()),
            created_at: now(),
            updated_at: now(),
        };
        state
            .canned_responses
            .insert(response.id.clone(), response.clone());
        Ok(response)
    })();
    respond_created(result)
}

pub(crate) async fn get_canned_response(
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
    let result: UnifiedResult<CannedResponse> = state
        .canned_responses
        .get(path.as_str())
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found("Canned response not found"));
    json::respond(result, false)
}

pub(crate) async fn update_canned_response(
    state: web::Data<Arc<TicketsState>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdateCannedResponse>,
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
    let result: UnifiedResult<CannedResponse> = (|| {
        let input = body.into_inner();
        let mut response = state
            .canned_responses
            .get_mut(path.as_str())
            .ok_or_else(|| UnifiedError::not_found("Canned response not found"))?;
        if let Some(title) = input.title {
            if title.trim().is_empty() {
                return Err(UnifiedError::bad_request(
                    "Canned response title must not be empty",
                ));
            }
            response.title = title.trim().to_string();
        }
        if let Some(body_text) = input.body {
            if body_text.trim().is_empty() {
                return Err(UnifiedError::bad_request(
                    "Canned response body must not be empty",
                ));
            }
            response.body = body_text.trim().to_string();
        }
        if let Some(category) = input.category {
            response.category = category;
        }
        if let Some(department_id) = input.department_id {
            response.department_id = Some(department_id);
        }
        response.updated_at = now();
        Ok(response.clone())
    })();
    json::respond(result, false)
}

pub(crate) async fn delete_canned_response(
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
            .canned_responses
            .remove(path.as_str())
            .map(|_| json!({ "deleted": true, "id": path.as_str() }))
            .ok_or_else(|| UnifiedError::not_found("Canned response not found"))
    })();
    json::respond(result, false)
}

/// Insert a canned response as an agent reply on a ticket.
pub(crate) async fn apply_canned_response(
    state: web::Data<Arc<TicketsState>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<ApplyCannedResponse>,
) -> HttpResponse {
    let _tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return json::error_response(&error, false),
    };
    let _scope = match caller_company(&_tenant, &req) {
        Ok(scope) => scope,
        Err(error) => return json::error_response(&error, false),
    };

    let caller = match authenticate(&req, &state, PERM_TICKETS_MANAGE) {
        Ok(caller) => caller,
        Err(response) => return response,
    };
    let result: UnifiedResult<Ticket> = (|| {
        let input = body.into_inner();
        let canned = state
            .canned_responses
            .get(input.canned_response_id.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Canned response not found"))?;
        let mut ticket = state
            .tickets
            .get_mut(path.as_str())
            .ok_or_else(|| UnifiedError::not_found("Ticket not found"))?;
        // A canned reply is an agent reply: it cannot continue a
        // resolved or closed ticket.
        if ticket.status == "resolved" || ticket.status == "closed" {
            return Err(UnifiedError::unprocessable(format!(
                "Cannot add a reply to a {} ticket; reopen it first",
                ticket.status
            )));
        }
        ticket.comments.push(TicketComment {
            id: new_id(),
            author_id: caller.actor.clone(),
            body: canned.body.clone(),
            kind: "agent_reply".to_string(),
            created_at: now(),
        });
        if ticket.first_response_at.is_none() {
            ticket.first_response_at = Some(now());
        }
        if ticket.status == "open" || ticket.status == "waiting" {
            ticket.status = "in_progress".to_string();
        }
        ticket.updated_at = now();
        let ticket_value = serde_json::to_value(&*ticket).unwrap_or(Value::Null);
        let notify_user = ticket.user_id.clone();
        let title = ticket.title.clone();
        let ticket_id = ticket.id.clone();
        drop(ticket);
        state.emit(
            "ticket.replied",
            json!({ "ticket_id": &ticket_id, "author_kind": "agent", "ticket": ticket_value }),
        );
        state.notify(
            &notify_user,
            format!("Reply on your ticket: {title}"),
            format!("An agent replied to ticket {ticket_id}."),
        );
        Ok(state
            .tickets
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .expect("ticket still present"))
    })();
    json::respond(result, false)
}

#[cfg(test)]
mod tests {

    /// Build a request carrying the server-verified caller the auth
    /// middleware would attach for this user and role. The old
    /// self-asserted identity headers grant nothing,
    /// so tests attach the verified caller to the request
    /// extensions instead.
    fn verified_request(user_id: &str, role: &str) -> actix_web::HttpRequest {
        use actix_web::HttpMessage;
        let req = actix_web::test::TestRequest::default().to_http_request();
        req.extensions_mut().insert(
            unified_core::auth::Caller::verified(test_user_uuid(user_id), Vec::new())
                .with_roles(vec![role.to_string()]),
        );
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
            },
        );
        req
    }

    /// Build a request carrying a verified caller with no roles
    /// resolved: the caller resolves, but holds the anonymous role.
    /// This is the "no role" case in the new model (roles are
    /// resolved server-side, never claimed in a header).
    fn verified_request_without_roles(user_id: &str) -> actix_web::HttpRequest {
        use actix_web::HttpMessage;
        let req = actix_web::test::TestRequest::default().to_http_request();
        req.extensions_mut()
            .insert(unified_core::auth::Caller::verified(
                test_user_uuid(user_id),
                Vec::new(),
            ));
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
            },
        );
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

    use super::*;
    use actix_web::http::StatusCode;
    use actix_web::test::TestRequest;

    fn allowing_state() -> Arc<TicketsState> {
        let state = Arc::new(TicketsState::new());
        let handler: HookHandler = Arc::new(|ctx: &mut HookContext| {
            ctx.set("allowed", json!(true));
        });
        state.bus().register("role.check", "tickets-test", handler);
        state
    }

    #[tokio::test]
    async fn apply_canned_response_rejects_resolved_tickets_and_moves_waiting_ones() {
        let state = allowing_state();
        let ticket = state
            .create_ticket(CreateTicket {
                title: "Help".to_string(),
                body: "Something broke".to_string(),
                user_id: "user-1".to_string(),
                company_id: None,
                priority: Some("low".to_string()),
                category: None,
                product: None,
                department_id: None,
            })
            .expect("seed ticket must be created");
        let canned = CannedResponse {
            id: new_id(),
            title: "Greeting".to_string(),
            body: "Hello, we are on it.".to_string(),
            category: "general".to_string(),
            department_id: None,
            created_at: now(),
            updated_at: now(),
        };
        state
            .canned_responses
            .insert(canned.id.clone(), canned.clone());
        let request = || verified_request("agent-1", "admin");

        // A waiting ticket moves to in progress on the canned reply.
        state
            .tickets
            .get_mut(&ticket.id)
            .expect("ticket must exist")
            .status = "waiting".to_string();
        let response = apply_canned_response(
            web::Data::new(state.clone()),
            request(),
            web::Path::from(ticket.id.clone()),
            web::Json(ApplyCannedResponse {
                canned_response_id: canned.id.clone(),
                author_id: "someone-else".to_string(),
            }),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        let updated = state.tickets.get(&ticket.id).expect("ticket must exist");
        assert_eq!(updated.status, "in_progress");
        assert_eq!(
            updated.comments.last().expect("reply must be recorded").author_id,
            test_user_uuid("agent-1").to_string()
        );

        // A resolved ticket rejects the canned reply.
        state
            .tickets
            .get_mut(&ticket.id)
            .expect("ticket must exist")
            .status = "resolved".to_string();
        let before = state
            .tickets
            .get(&ticket.id)
            .expect("ticket must exist")
            .comments
            .len();
        let response = apply_canned_response(
            web::Data::new(state.clone()),
            request(),
            web::Path::from(ticket.id.clone()),
            web::Json(ApplyCannedResponse {
                canned_response_id: canned.id.clone(),
                author_id: "agent-1".to_string(),
            }),
        )
        .await;
        assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
        let updated = state.tickets.get(&ticket.id).expect("ticket must exist");
        assert_eq!(updated.comments.len(), before);
    }
}
